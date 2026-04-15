// Package state is SQLite-backed persistent storage for config overrides,
// event log, history snapshots, and battery models.
//
// History uses one table per tier (hot/warm/cold) like the Rust version, but
// the aggregation from hot → warm → cold is pure SQL instead of custom
// bucketing code. See Prune() for the aggregation queries.
package state

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"

	_ "modernc.org/sqlite"
)

const (
	// HotRetention = 30 days at 5s resolution
	HotRetention = 30 * 24 * time.Hour
	// WarmRetention = 12 months at 15-min buckets
	WarmRetention = 365 * 24 * time.Hour
	// WarmBucketMS = 15-minute bucket size for warm tier
	WarmBucketMS = 15 * 60 * 1000
	// ColdBucketMS = daily bucket size for cold tier
	ColdBucketMS = 24 * 60 * 60 * 1000
)

// Store is the persistent state DB (thin wrapper around *sql.DB).
type Store struct {
	db *sql.DB
}

// Open initializes (or creates) the DB at path. Runs all migrations.
func Open(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path+"?_pragma=journal_mode(WAL)&_pragma=synchronous(NORMAL)&_pragma=foreign_keys(1)")
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}
	// Single connection — SQLite doesn't benefit from pooling
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	s := &Store{db: db}
	if err := s.migrate(); err != nil {
		db.Close()
		return nil, err
	}
	return s, nil
}

// Close releases the DB file. Safe to call multiple times.
func (s *Store) Close() error {
	if s == nil || s.db == nil {
		return nil
	}
	return s.db.Close()
}

func (s *Store) migrate() error {
	stmts := []string{
		// config: small string key-value for mode, grid_target etc.
		`CREATE TABLE IF NOT EXISTS config (
			key TEXT PRIMARY KEY NOT NULL,
			value TEXT NOT NULL
		)`,
		// events: operational log, ms-precision key (seconds collided)
		`CREATE TABLE IF NOT EXISTS events (
			ts_ms INTEGER PRIMARY KEY NOT NULL,
			event TEXT NOT NULL
		)`,
		// telemetry snapshots for crash recovery
		`CREATE TABLE IF NOT EXISTS telemetry (
			key TEXT PRIMARY KEY NOT NULL,
			json TEXT NOT NULL
		)`,
		// battery models (JSON-serialized), keyed by driver name
		`CREATE TABLE IF NOT EXISTS battery_models (
			name TEXT PRIMARY KEY NOT NULL,
			json TEXT NOT NULL
		)`,
		// History tiers — hot/warm/cold, all keyed by ms timestamp
		`CREATE TABLE IF NOT EXISTS history_hot (
			ts_ms INTEGER PRIMARY KEY NOT NULL,
			grid_w REAL, pv_w REAL, bat_w REAL, load_w REAL, bat_soc REAL,
			json TEXT NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS history_warm (
			ts_ms INTEGER PRIMARY KEY NOT NULL,
			grid_w REAL, pv_w REAL, bat_w REAL, load_w REAL, bat_soc REAL,
			json TEXT NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS history_cold (
			ts_ms INTEGER PRIMARY KEY NOT NULL,
			grid_w REAL, pv_w REAL, bat_w REAL, load_w REAL, bat_soc REAL,
			json TEXT NOT NULL
		)`,
		`CREATE INDEX IF NOT EXISTS idx_hot_ts ON history_hot(ts_ms)`,
		`CREATE INDEX IF NOT EXISTS idx_warm_ts ON history_warm(ts_ms)`,
		`CREATE INDEX IF NOT EXISTS idx_cold_ts ON history_cold(ts_ms)`,
		`CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts_ms DESC)`,

		// Spot prices — one row per time slot per zone.
		// Slot duration is provider-dependent: NordPool went to 15-min PTU
		// in late 2025; ENTSOE is mixed. The table just stores timestamps —
		// slot_len_min tells consumers what duration each row represents.
		`CREATE TABLE IF NOT EXISTS prices (
			zone TEXT NOT NULL,
			slot_ts_ms INTEGER NOT NULL,
			slot_len_min INTEGER NOT NULL DEFAULT 60,
			spot_ore_kwh REAL NOT NULL,
			total_ore_kwh REAL NOT NULL,
			source TEXT NOT NULL,
			fetched_at_ms INTEGER NOT NULL,
			PRIMARY KEY (zone, slot_ts_ms)
		)`,
		`CREATE INDEX IF NOT EXISTS idx_prices_slot ON prices(slot_ts_ms)`,

		// Weather + PV forecasts — one row per hour (met.no/openweather
		// both default to hourly; can downsample to 15-min if needed later).
		`CREATE TABLE IF NOT EXISTS forecasts (
			slot_ts_ms INTEGER PRIMARY KEY,
			slot_len_min INTEGER NOT NULL DEFAULT 60,
			cloud_cover_pct REAL,
			temp_c REAL,
			solar_wm2 REAL,
			pv_w_estimated REAL,
			source TEXT NOT NULL,
			fetched_at_ms INTEGER NOT NULL
		)`,

		// ---- Long-format time-series ("recent" tier, last 14 days) ----
		// Drivers + metrics are interned to integer ids to keep rows small.
		// Composite PK is (driver_id, metric_id, ts) WITHOUT ROWID so storage
		// is clustered by driver+metric — typical access pattern is "give me
		// metric X for driver Y over time range Z".
		`CREATE TABLE IF NOT EXISTS ts_drivers (
			id INTEGER PRIMARY KEY,
			name TEXT NOT NULL UNIQUE
		)`,
		`CREATE TABLE IF NOT EXISTS ts_metrics (
			id INTEGER PRIMARY KEY,
			name TEXT NOT NULL UNIQUE,
			unit TEXT
		)`,
		`CREATE TABLE IF NOT EXISTS ts_samples (
			driver_id INTEGER NOT NULL,
			metric_id INTEGER NOT NULL,
			ts_ms     INTEGER NOT NULL,
			value     REAL NOT NULL,
			PRIMARY KEY (driver_id, metric_id, ts_ms)
		) WITHOUT ROWID, STRICT`,
		`CREATE INDEX IF NOT EXISTS idx_ts_samples_ts ON ts_samples(ts_ms)`,
	}
	for _, stmt := range stmts {
		if _, err := s.db.Exec(stmt); err != nil {
			return fmt.Errorf("migration %q: %w", stmt[:40]+"…", err)
		}
	}
	return nil
}

// ---- Config key-value ----

// SaveConfig writes a config k/v. Upserts on conflict.
func (s *Store) SaveConfig(key, value string) error {
	_, err := s.db.Exec(`INSERT INTO config (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value`, key, value)
	return err
}

// LoadConfig returns the value for key, or ok=false if missing.
func (s *Store) LoadConfig(key string) (string, bool) {
	var v string
	err := s.db.QueryRow(`SELECT value FROM config WHERE key = ?`, key).Scan(&v)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false
	}
	if err != nil {
		return "", false
	}
	return v, true
}

// ---- Events ----

// RecordEvent appends an event at the current ms timestamp. Collision-safe up to 1 per ms.
func (s *Store) RecordEvent(event string) error {
	ts := time.Now().UnixMilli()
	_, err := s.db.Exec(`INSERT OR REPLACE INTO events (ts_ms, event) VALUES (?, ?)`, ts, event)
	return err
}

// RecentEvents returns the N most recent events (most recent first).
func (s *Store) RecentEvents(limit int) ([]Event, error) {
	rows, err := s.db.Query(`SELECT ts_ms, event FROM events ORDER BY ts_ms DESC LIMIT ?`, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := make([]Event, 0, limit)
	for rows.Next() {
		var e Event
		if err := rows.Scan(&e.TsMs, &e.Event); err != nil {
			return out, err
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

// Event is one entry from the events log.
type Event struct {
	TsMs  int64
	Event string
}

// ---- Telemetry snapshots ----

// SaveTelemetry stores the latest known state of one DER key for crash recovery.
func (s *Store) SaveTelemetry(key, json string) error {
	_, err := s.db.Exec(`INSERT INTO telemetry (key, json) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET json = excluded.json`, key, json)
	return err
}

// LoadTelemetry returns the most recent saved JSON blob for a key.
func (s *Store) LoadTelemetry(key string) (string, bool) {
	var v string
	err := s.db.QueryRow(`SELECT json FROM telemetry WHERE key = ?`, key).Scan(&v)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false
	}
	if err != nil {
		return "", false
	}
	return v, true
}

// ---- Battery models ----

// SaveBatteryModel stores the JSON-serialized model state for a driver.
func (s *Store) SaveBatteryModel(name, json string) error {
	_, err := s.db.Exec(`INSERT INTO battery_models (name, json) VALUES (?, ?) ON CONFLICT (name) DO UPDATE SET json = excluded.json`, name, json)
	return err
}

// LoadAllBatteryModels returns all stored model states keyed by driver name.
func (s *Store) LoadAllBatteryModels() (map[string]string, error) {
	rows, err := s.db.Query(`SELECT name, json FROM battery_models`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := make(map[string]string)
	for rows.Next() {
		var name, js string
		if err := rows.Scan(&name, &js); err != nil {
			return out, err
		}
		out[name] = js
	}
	return out, rows.Err()
}

// DeleteBatteryModel removes a stored model (used when resetting).
func (s *Store) DeleteBatteryModel(name string) error {
	_, err := s.db.Exec(`DELETE FROM battery_models WHERE name = ?`, name)
	return err
}

// ---- History tiers ----

// HistoryPoint is one row of the history table.
type HistoryPoint struct {
	TsMs   int64
	GridW  float64
	PVW    float64
	BatW   float64
	LoadW  float64
	BatSoC float64
	JSON   string
}

// RecordHistory inserts a new hot-tier entry.
func (s *Store) RecordHistory(p HistoryPoint) error {
	_, err := s.db.Exec(
		`INSERT OR REPLACE INTO history_hot (ts_ms, grid_w, pv_w, bat_w, load_w, bat_soc, json)
		 VALUES (?, ?, ?, ?, ?, ?, ?)`,
		p.TsMs, p.GridW, p.PVW, p.BatW, p.LoadW, p.BatSoC, p.JSON,
	)
	return err
}

// LoadHistory returns points from ALL tiers in [sinceMs, untilMs], merged + sorted.
// maxPoints=0 means no limit. With a limit, we return at most that many evenly-spaced rows.
func (s *Store) LoadHistory(sinceMs, untilMs int64, maxPoints int) ([]HistoryPoint, error) {
	// Union across all three tiers. Dedupe on ts_ms preferring hot over warm over cold.
	// COALESCE to 0 so NULL columns (from partial aggregations) scan cleanly.
	query := `
		WITH all_rows AS (
			SELECT ts_ms, grid_w, pv_w, bat_w, load_w, bat_soc, json, 0 AS tier FROM history_hot
			WHERE ts_ms BETWEEN ? AND ?
			UNION ALL
			SELECT ts_ms, grid_w, pv_w, bat_w, load_w, bat_soc, json, 1 FROM history_warm
			WHERE ts_ms BETWEEN ? AND ?
			UNION ALL
			SELECT ts_ms, grid_w, pv_w, bat_w, load_w, bat_soc, json, 2 FROM history_cold
			WHERE ts_ms BETWEEN ? AND ?
		),
		deduped AS (
			SELECT * FROM all_rows
			GROUP BY ts_ms
			HAVING tier = MIN(tier)
		)
		SELECT ts_ms,
		       COALESCE(grid_w, 0), COALESCE(pv_w, 0), COALESCE(bat_w, 0),
		       COALESCE(load_w, 0), COALESCE(bat_soc, 0), json
		FROM deduped
		ORDER BY ts_ms ASC
	`
	rows, err := s.db.Query(query, sinceMs, untilMs, sinceMs, untilMs, sinceMs, untilMs)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	all := make([]HistoryPoint, 0)
	for rows.Next() {
		var p HistoryPoint
		if err := rows.Scan(&p.TsMs, &p.GridW, &p.PVW, &p.BatW, &p.LoadW, &p.BatSoC, &p.JSON); err != nil {
			return all, err
		}
		all = append(all, p)
	}
	if err := rows.Err(); err != nil {
		return all, err
	}

	// Downsample by evenly picking maxPoints rows
	if maxPoints > 0 && len(all) > maxPoints {
		step := float64(len(all)) / float64(maxPoints)
		out := make([]HistoryPoint, 0, maxPoints)
		for i := 0; i < maxPoints; i++ {
			idx := int(float64(i) * step)
			if idx >= len(all) {
				idx = len(all) - 1
			}
			out = append(out, all[idx])
		}
		return out, nil
	}
	return all, nil
}

// HistoryCounts returns the number of rows in (hot, warm, cold) tiers.
func (s *Store) HistoryCounts() (hot, warm, cold int, err error) {
	row := s.db.QueryRow(`SELECT
		(SELECT COUNT(*) FROM history_hot),
		(SELECT COUNT(*) FROM history_warm),
		(SELECT COUNT(*) FROM history_cold)`)
	err = row.Scan(&hot, &warm, &cold)
	return
}

// Prune ages old hot rows into warm buckets, old warm into cold daily buckets.
// This is pure SQL — no custom Go bucketing needed. Idempotent; safe to call often.
func (s *Store) Prune(ctx context.Context) error {
	nowMs := time.Now().UnixMilli()
	hotCutoff := nowMs - int64(HotRetention.Milliseconds())
	warmCutoff := nowMs - int64(WarmRetention.Milliseconds())

	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()

	// 1. hot → warm (15-min buckets)
	if _, err := tx.ExecContext(ctx, fmt.Sprintf(`
		INSERT OR REPLACE INTO history_warm (ts_ms, grid_w, pv_w, bat_w, load_w, bat_soc, json)
		SELECT
			(ts_ms / %d) * %d + %d AS bucket_ts,
			AVG(grid_w), AVG(pv_w), AVG(bat_w), AVG(load_w), AVG(bat_soc),
			-- Pick any JSON from the bucket; aggregation via SQL is too fiddly.
			(SELECT json FROM history_hot h2 WHERE h2.ts_ms / %d = h.ts_ms / %d LIMIT 1)
		FROM history_hot h
		WHERE ts_ms < ?
		GROUP BY ts_ms / %d
	`, WarmBucketMS, WarmBucketMS, WarmBucketMS/2, WarmBucketMS, WarmBucketMS, WarmBucketMS), hotCutoff); err != nil {
		return fmt.Errorf("aggregate hot→warm: %w", err)
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM history_hot WHERE ts_ms < ?`, hotCutoff); err != nil {
		return fmt.Errorf("delete old hot: %w", err)
	}

	// 2. warm → cold (1-day buckets)
	if _, err := tx.ExecContext(ctx, fmt.Sprintf(`
		INSERT OR REPLACE INTO history_cold (ts_ms, grid_w, pv_w, bat_w, load_w, bat_soc, json)
		SELECT
			(ts_ms / %d) * %d + %d AS bucket_ts,
			AVG(grid_w), AVG(pv_w), AVG(bat_w), AVG(load_w), AVG(bat_soc),
			(SELECT json FROM history_warm w2 WHERE w2.ts_ms / %d = w.ts_ms / %d LIMIT 1)
		FROM history_warm w
		WHERE ts_ms < ?
		GROUP BY ts_ms / %d
	`, ColdBucketMS, ColdBucketMS, ColdBucketMS/2, ColdBucketMS, ColdBucketMS, ColdBucketMS), warmCutoff); err != nil {
		return fmt.Errorf("aggregate warm→cold: %w", err)
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM history_warm WHERE ts_ms < ?`, warmCutoff); err != nil {
		return fmt.Errorf("delete old warm: %w", err)
	}

	return tx.Commit()
}

// ---- Prices ----

// PricePoint is one time-slot's spot price row. Slot length varies by source:
// NordPool/elprisetjustnu is 15 min since late 2025; ENTSOE is mostly still
// hourly. Consumers should honor SlotLenMin when plotting or aggregating.
type PricePoint struct {
	Zone        string  `json:"zone"`
	SlotTsMs    int64   `json:"slot_ts_ms"`
	SlotLenMin  int     `json:"slot_len_min"`
	SpotOreKwh  float64 `json:"spot_ore_kwh"`
	TotalOreKwh float64 `json:"total_ore_kwh"`
	Source      string  `json:"source"`
	FetchedAtMs int64   `json:"fetched_at_ms"`
}

// SavePrices upserts a batch of price rows (slot duration per-row).
func (s *Store) SavePrices(pts []PricePoint) error {
	if len(pts) == 0 { return nil }
	tx, err := s.db.Begin()
	if err != nil { return err }
	defer tx.Rollback()
	stmt, err := tx.Prepare(`INSERT INTO prices
		(zone, slot_ts_ms, slot_len_min, spot_ore_kwh, total_ore_kwh, source, fetched_at_ms)
		VALUES (?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT (zone, slot_ts_ms) DO UPDATE SET
			slot_len_min = excluded.slot_len_min,
			spot_ore_kwh = excluded.spot_ore_kwh,
			total_ore_kwh = excluded.total_ore_kwh,
			source = excluded.source,
			fetched_at_ms = excluded.fetched_at_ms`)
	if err != nil { return err }
	defer stmt.Close()
	for _, p := range pts {
		slot := p.SlotLenMin
		if slot <= 0 { slot = 60 }
		if _, err := stmt.Exec(p.Zone, p.SlotTsMs, slot, p.SpotOreKwh, p.TotalOreKwh, p.Source, p.FetchedAtMs); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// LoadPrices returns prices for zone in [sinceMs, untilMs], ordered ascending.
func (s *Store) LoadPrices(zone string, sinceMs, untilMs int64) ([]PricePoint, error) {
	rows, err := s.db.Query(`SELECT zone, slot_ts_ms, slot_len_min, spot_ore_kwh, total_ore_kwh, source, fetched_at_ms
		FROM prices
		WHERE zone = ? AND slot_ts_ms BETWEEN ? AND ?
		ORDER BY slot_ts_ms ASC`, zone, sinceMs, untilMs)
	if err != nil { return nil, err }
	defer rows.Close()
	out := []PricePoint{}
	for rows.Next() {
		var p PricePoint
		if err := rows.Scan(&p.Zone, &p.SlotTsMs, &p.SlotLenMin, &p.SpotOreKwh, &p.TotalOreKwh, &p.Source, &p.FetchedAtMs); err != nil {
			return out, err
		}
		out = append(out, p)
	}
	return out, rows.Err()
}

// ---- Forecasts ----

// ForecastPoint is one slot's weather + derived PV estimate.
type ForecastPoint struct {
	SlotTsMs      int64    `json:"slot_ts_ms"`
	SlotLenMin    int      `json:"slot_len_min"`
	CloudCoverPct *float64 `json:"cloud_cover_pct,omitempty"`
	TempC         *float64 `json:"temp_c,omitempty"`
	SolarWm2      *float64 `json:"solar_wm2,omitempty"`
	PVWEstimated  *float64 `json:"pv_w_estimated,omitempty"`
	Source        string   `json:"source"`
	FetchedAtMs   int64    `json:"fetched_at_ms"`
}

// SaveForecasts upserts a batch of forecast rows.
func (s *Store) SaveForecasts(pts []ForecastPoint) error {
	if len(pts) == 0 { return nil }
	tx, err := s.db.Begin()
	if err != nil { return err }
	defer tx.Rollback()
	stmt, err := tx.Prepare(`INSERT INTO forecasts
		(slot_ts_ms, slot_len_min, cloud_cover_pct, temp_c, solar_wm2, pv_w_estimated, source, fetched_at_ms)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT (slot_ts_ms) DO UPDATE SET
			slot_len_min = excluded.slot_len_min,
			cloud_cover_pct = excluded.cloud_cover_pct,
			temp_c = excluded.temp_c,
			solar_wm2 = excluded.solar_wm2,
			pv_w_estimated = excluded.pv_w_estimated,
			source = excluded.source,
			fetched_at_ms = excluded.fetched_at_ms`)
	if err != nil { return err }
	defer stmt.Close()
	for _, p := range pts {
		slot := p.SlotLenMin
		if slot <= 0 { slot = 60 }
		if _, err := stmt.Exec(p.SlotTsMs, slot, p.CloudCoverPct, p.TempC, p.SolarWm2, p.PVWEstimated, p.Source, p.FetchedAtMs); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// LoadForecasts returns forecasts in [sinceMs, untilMs], ordered ascending.
func (s *Store) LoadForecasts(sinceMs, untilMs int64) ([]ForecastPoint, error) {
	rows, err := s.db.Query(`SELECT slot_ts_ms, slot_len_min, cloud_cover_pct, temp_c, solar_wm2, pv_w_estimated, source, fetched_at_ms
		FROM forecasts
		WHERE slot_ts_ms BETWEEN ? AND ?
		ORDER BY slot_ts_ms ASC`, sinceMs, untilMs)
	if err != nil { return nil, err }
	defer rows.Close()
	out := []ForecastPoint{}
	for rows.Next() {
		var p ForecastPoint
		if err := rows.Scan(&p.SlotTsMs, &p.SlotLenMin, &p.CloudCoverPct, &p.TempC, &p.SolarWm2, &p.PVWEstimated, &p.Source, &p.FetchedAtMs); err != nil {
			return out, err
		}
		out = append(out, p)
	}
	return out, rows.Err()
}
