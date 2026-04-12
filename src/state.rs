use redb::{Database, ReadableDatabase, TableDefinition, ReadableTable};
use tracing::{info, warn, error};

const CONFIG_TABLE: TableDefinition<&str, &str> = TableDefinition::new("config");
const TELEMETRY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("telemetry");
const EVENTS_TABLE: TableDefinition<u64, &str> = TableDefinition::new("events");

/// Persistent state store backed by redb
pub struct StateStore {
    db: Database,
}

impl StateStore {
    pub fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let db = Database::create(path)?;

        // Ensure tables exist
        let txn = db.begin_write()?;
        {
            let _ = txn.open_table(CONFIG_TABLE)?;
            let _ = txn.open_table(TELEMETRY_TABLE)?;
            let _ = txn.open_table(EVENTS_TABLE)?;
        }
        txn.commit()?;

        info!("state store opened: {}", path);
        Ok(Self { db })
    }

    /// Save a config value (mode, grid target, weights, etc.)
    pub fn save_config(&self, key: &str, value: &str) {
        match self.db.begin_write() {
            Ok(txn) => {
                match txn.open_table(CONFIG_TABLE) {
                    Ok(mut table) => {
                        if let Err(e) = table.insert(key, value) {
                            error!("failed to save config {}: {}", key, e);
                        }
                    }
                    Err(e) => error!("failed to open config table: {}", e),
                }
                if let Err(e) = txn.commit() {
                    error!("failed to commit config: {}", e);
                }
            }
            Err(e) => error!("failed to begin write txn: {}", e),
        }
    }

    /// Load a config value
    pub fn load_config(&self, key: &str) -> Option<String> {
        match self.db.begin_read() {
            Ok(txn) => {
                match txn.open_table(CONFIG_TABLE) {
                    Ok(table) => {
                        table.get(key).ok().flatten().map(|v| v.value().to_string())
                    }
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    }

    /// Save last known telemetry for crash recovery
    pub fn save_telemetry(&self, key: &str, json: &str) {
        match self.db.begin_write() {
            Ok(txn) => {
                match txn.open_table(TELEMETRY_TABLE) {
                    Ok(mut table) => {
                        if let Err(e) = table.insert(key, json) {
                            warn!("failed to save telemetry {}: {}", key, e);
                        }
                    }
                    Err(e) => warn!("failed to open telemetry table: {}", e),
                }
                let _ = txn.commit();
            }
            Err(e) => warn!("failed to begin write txn: {}", e),
        }
    }

    /// Load last known telemetry
    pub fn load_telemetry(&self, key: &str) -> Option<String> {
        match self.db.begin_read() {
            Ok(txn) => {
                match txn.open_table(TELEMETRY_TABLE) {
                    Ok(table) => {
                        table.get(key).ok().flatten().map(|v| v.value().to_string())
                    }
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    }

    /// Record an event (mode change, error, recovery)
    pub fn record_event(&self, event: &str) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match self.db.begin_write() {
            Ok(txn) => {
                match txn.open_table(EVENTS_TABLE) {
                    Ok(mut table) => {
                        if let Err(e) = table.insert(timestamp, event) {
                            warn!("failed to record event: {}", e);
                        }
                    }
                    Err(e) => warn!("failed to open events table: {}", e),
                }
                let _ = txn.commit();
            }
            Err(e) => warn!("failed to begin write txn: {}", e),
        }
    }

    /// Load recent events (last N)
    pub fn recent_events(&self, limit: usize) -> Vec<(u64, String)> {
        let mut events = Vec::new();

        match self.db.begin_read() {
            Ok(txn) => {
                match txn.open_table(EVENTS_TABLE) {
                    Ok(table) => {
                        // Iterate in reverse (most recent first)
                        if let Ok(iter) = table.iter() {
                            let all: Vec<_> = iter
                                .filter_map(|r| r.ok())
                                .map(|(k, v)| (k.value(), v.value().to_string()))
                                .collect();
                            let start = if all.len() > limit { all.len() - limit } else { 0 };
                            events = all[start..].to_vec();
                        }
                    }
                    Err(_) => {}
                }
            }
            Err(_) => {}
        }

        events
    }
}
