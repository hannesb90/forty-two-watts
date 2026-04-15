package state

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"time"

	"github.com/parquet-go/parquet-go"
	"github.com/parquet-go/parquet-go/compress/zstd"
)

// parquetSampleRow mirrors the long-format schema in column-oriented form.
// Driver and Metric are interned strings (parquet's dictionary encoding makes
// the repetition cheap — typically <2 bytes per row after compression).
type parquetSampleRow struct {
	TsMs   int64   `parquet:"ts_ms"`
	Driver string  `parquet:"driver,dict,zstd"`
	Metric string  `parquet:"metric,dict,zstd"`
	Value  float64 `parquet:"value,zstd"`
}

// RolloffToParquet exports samples older than RecentRetention into one
// parquet file per UTC-day, then deletes the rolled-off rows from SQLite.
// Idempotent: re-running for a day that already has a file rewrites it
// (we accumulate strict-cutoff data, never lose anything).
//
// File layout: <coldDir>/YYYY/MM/DD.parquet
func (s *Store) RolloffToParquet(ctx context.Context, coldDir string) (rolledRows int64, files []string, err error) {
	if coldDir == "" {
		return 0, nil, fmt.Errorf("RolloffToParquet: coldDir must be set")
	}
	cutoff := time.Now().Add(-RecentRetention).UnixMilli()

	// Bucket samples by UTC day so each parquet covers one calendar date.
	// We accumulate in memory — typical roll-off is ~50 metrics × 17,280
	// samples/day × ~24 days backlog worst case ≈ 20M rows = ~640 MB.
	// In practice the daily run keeps it to one day = ~30 MB before flush.
	type dayKey struct{ year, month, day int }
	byDay := make(map[dayKey][]parquetSampleRow, 4)

	err = s.SamplesBefore(ctx, cutoff, 50000, func(batch []Sample) error {
		for _, sm := range batch {
			t := time.UnixMilli(sm.TsMs).UTC()
			k := dayKey{t.Year(), int(t.Month()), t.Day()}
			byDay[k] = append(byDay[k], parquetSampleRow{
				TsMs: sm.TsMs, Driver: sm.Driver, Metric: sm.Metric, Value: sm.Value,
			})
		}
		return nil
	})
	if err != nil { return 0, nil, fmt.Errorf("read samples: %w", err) }
	if len(byDay) == 0 { return 0, nil, nil }

	for k, rows := range byDay {
		// Sort by ts to maximize compression and make consumer scans linear.
		sort.Slice(rows, func(i, j int) bool { return rows[i].TsMs < rows[j].TsMs })

		dayDir := filepath.Join(coldDir, fmt.Sprintf("%04d/%02d", k.year, k.month))
		if err := os.MkdirAll(dayDir, 0o755); err != nil {
			return rolledRows, files, fmt.Errorf("mkdir %s: %w", dayDir, err)
		}
		path := filepath.Join(dayDir, fmt.Sprintf("%02d.parquet", k.day))
		if err := writeParquetDay(path, rows); err != nil {
			return rolledRows, files, fmt.Errorf("write %s: %w", path, err)
		}
		files = append(files, path)
		rolledRows += int64(len(rows))
	}

	// Delete the rolled-off rows from SQLite. Single statement, atomic.
	if _, err := s.db.ExecContext(ctx, `DELETE FROM ts_samples WHERE ts_ms < ?`, cutoff); err != nil {
		return rolledRows, files, fmt.Errorf("delete rolled rows: %w", err)
	}
	return rolledRows, files, nil
}

func writeParquetDay(path string, rows []parquetSampleRow) error {
	tmp := path + ".tmp"
	f, err := os.Create(tmp)
	if err != nil { return err }
	w := parquet.NewGenericWriter[parquetSampleRow](f, parquet.Compression(&zstd.Codec{Level: zstd.DefaultLevel}))
	if _, err := w.Write(rows); err != nil {
		f.Close()
		os.Remove(tmp)
		return err
	}
	if err := w.Close(); err != nil {
		f.Close()
		os.Remove(tmp)
		return err
	}
	if err := f.Close(); err != nil {
		os.Remove(tmp)
		return err
	}
	return os.Rename(tmp, path)
}

// LoadSeriesFromParquet reads one (driver, metric) series from cold storage.
// Scans every parquet file whose day overlaps [sinceMs, untilMs]. Filtered in
// process — daily files are small enough that pushdown isn't worth the
// complexity for this dataset size.
func (s *Store) LoadSeriesFromParquet(coldDir, driver, metric string, sinceMs, untilMs int64) ([]Sample, error) {
	if coldDir == "" { return nil, nil }
	since := time.UnixMilli(sinceMs).UTC()
	until := time.UnixMilli(untilMs).UTC()
	out := make([]Sample, 0, 256)

	for d := since; !d.After(until); d = d.AddDate(0, 0, 1) {
		path := filepath.Join(coldDir,
			fmt.Sprintf("%04d/%02d/%02d.parquet", d.Year(), int(d.Month()), d.Day()))
		f, err := os.Open(path)
		if err != nil {
			if os.IsNotExist(err) { continue }
			return out, err
		}
		stat, err := f.Stat()
		if err != nil { f.Close(); return out, err }
		pf, err := parquet.OpenFile(f, stat.Size())
		if err != nil { f.Close(); return out, err }
		reader := parquet.NewGenericReader[parquetSampleRow](pf)
		buf := make([]parquetSampleRow, 1024)
		for {
			n, err := reader.Read(buf)
			for i := 0; i < n; i++ {
				r := buf[i]
				if r.Driver == driver && r.Metric == metric &&
					r.TsMs >= sinceMs && r.TsMs <= untilMs {
					out = append(out, Sample{Driver: r.Driver, Metric: r.Metric, TsMs: r.TsMs, Value: r.Value})
				}
			}
			if err != nil { break }
		}
		reader.Close()
		f.Close()
	}
	sort.Slice(out, func(i, j int) bool { return out[i].TsMs < out[j].TsMs })
	return out, nil
}
