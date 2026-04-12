use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn, error, debug};

use crate::config::HomeAssistantConfig;
use crate::telemetry::{TelemetryStore, DerType};
use crate::control::{ControlState, Mode};

/// Home Assistant MQTT autodiscovery + sensor publishing + command subscription
pub fn start(
    config: HomeAssistantConfig,
    store: Arc<Mutex<TelemetryStore>>,
    control: Arc<Mutex<ControlState>>,
    driver_names: Vec<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("ha-mqtt".to_string())
        .spawn(move || {
            run_ha_bridge(config, store, control, driver_names);
        })
        .expect("failed to start HA MQTT thread")
}

fn run_ha_bridge(
    config: HomeAssistantConfig,
    store: Arc<Mutex<TelemetryStore>>,
    control: Arc<Mutex<ControlState>>,
    driver_names: Vec<String>,
) {
    // Connect to HA MQTT broker
    // For now, use a simple MQTT publish via TCP
    // TODO: use the mqtt::client module once it's ready

    let broker = format!("{}:{}", config.broker, config.port);
    info!("HA MQTT: connecting to {}", broker);

    // Retry loop for HA MQTT connection
    loop {
        match connect_and_run(&broker, &config, &store, &control, &driver_names) {
            Ok(()) => {
                info!("HA MQTT: disconnected cleanly");
                return;
            }
            Err(e) => {
                error!("HA MQTT: connection error: {}, retrying in 10s", e);
                std::thread::sleep(Duration::from_secs(10));
            }
        }
    }
}

fn connect_and_run(
    _broker: &str,
    config: &HomeAssistantConfig,
    store: &Arc<Mutex<TelemetryStore>>,
    control: &Arc<Mutex<ControlState>>,
    driver_names: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let interval = Duration::from_secs(config.publish_interval_s);

    // TODO: establish MQTT connection, publish autodiscovery, subscribe to commands
    // For now, just log what we would publish

    info!("HA MQTT: would publish autodiscovery for {} drivers", driver_names.len());

    loop {
        std::thread::sleep(interval);

        let store = store.lock().unwrap();
        let control = control.lock().unwrap();

        // Aggregate values
        let meters = store.readings_by_type(&DerType::Meter);
        let pvs = store.readings_by_type(&DerType::Pv);
        let bats = store.readings_by_type(&DerType::Battery);

        let grid_w: f64 = meters.iter().map(|m| m.smoothed_w).sum();
        let pv_w: f64 = pvs.iter().map(|p| p.smoothed_w).sum();
        let bat_w: f64 = bats.iter().map(|b| b.smoothed_w).sum();

        debug!("HA MQTT: grid={:.0}W pv={:.0}W bat={:.0}W mode={:?}",
            grid_w, pv_w, bat_w, control.mode);

        // TODO: publish to MQTT topics:
        // homeems/status/grid_w
        // homeems/status/pv_w
        // homeems/status/bat_w
        // homeems/status/mode
        // homeems/drivers/{name}/meter_w, pv_w, bat_w, bat_soc, status
    }
}

/// Generate Home Assistant MQTT autodiscovery payloads
pub fn autodiscovery_configs(
    driver_names: &[String],
    discovery_prefix: &str,
    topic_prefix: &str,
) -> Vec<(String, serde_json::Value)> {
    let device = serde_json::json!({
        "identifiers": ["home_ems"],
        "name": "Home EMS",
        "manufacturer": "Sourceful",
        "model": "home-ems",
        "sw_version": env!("CARGO_PKG_VERSION"),
    });

    let mut configs = Vec::new();

    // Site-level sensors
    let site_sensors = vec![
        ("grid_power", "homeems/status/grid_w", "W", "power", "mdi:transmission-tower"),
        ("pv_power", "homeems/status/pv_w", "W", "power", "mdi:solar-power"),
        ("battery_power", "homeems/status/bat_w", "W", "power", "mdi:battery-charging"),
        ("battery_soc", "homeems/status/bat_soc", "%", "battery", "mdi:battery"),
    ];

    for (id, topic, unit, class, icon) in &site_sensors {
        let config_topic = format!("{}/sensor/homeems_{}/config", discovery_prefix, id);
        let payload = serde_json::json!({
            "name": format!("Home EMS {}", id.replace('_', " ")),
            "unique_id": format!("homeems_{}", id),
            "state_topic": topic,
            "unit_of_measurement": unit,
            "device_class": class,
            "icon": icon,
            "device": device,
        });
        configs.push((config_topic, payload));
    }

    // Mode sensor (select entity via MQTT)
    configs.push((
        format!("{}/select/homeems_mode/config", discovery_prefix),
        serde_json::json!({
            "name": "Home EMS Mode",
            "unique_id": "homeems_mode",
            "state_topic": format!("{}/status/mode", topic_prefix),
            "command_topic": format!("{}/command/mode", topic_prefix),
            "options": ["idle", "self_consumption", "charge", "priority", "weighted"],
            "icon": "mdi:tune",
            "device": device,
        }),
    ));

    // Per-driver sensors
    for name in driver_names {
        let driver_sensors = vec![
            ("meter_w", "W", "power"),
            ("pv_w", "W", "power"),
            ("bat_w", "W", "power"),
            ("bat_soc", "%", "battery"),
        ];

        for (field, unit, class) in &driver_sensors {
            let id = format!("{}_{}", name, field);
            let config_topic = format!("{}/sensor/homeems_{}/config", discovery_prefix, id);
            let state_topic = format!("{}/drivers/{}/{}", topic_prefix, name, field);
            let payload = serde_json::json!({
                "name": format!("Home EMS {} {}", name, field.replace('_', " ")),
                "unique_id": format!("homeems_{}", id),
                "state_topic": state_topic,
                "unit_of_measurement": unit,
                "device_class": class,
                "device": device,
            });
            configs.push((config_topic, payload));
        }
    }

    configs
}
