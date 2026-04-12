mod config;
mod telemetry;
mod control;
mod api;
mod ha;
mod state;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn, error};

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    info!("home-ems v{}", env!("CARGO_PKG_VERSION"));

    // Parse CLI args
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.yaml".to_string());

    // Load config
    let config = match config::Config::load(Path::new(&config_path)) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to load config '{}': {}", config_path, e);
            std::process::exit(1);
        }
    };

    info!("site: {}", config.site.name);
    info!("fuse limit: {}A / {} phases (max {:.0}W)",
        config.fuse.max_amps, config.fuse.phases, config.fuse.max_power_w());
    info!("control interval: {}s, grid target: {}W, tolerance: {}W",
        config.site.control_interval_s, config.site.grid_target_w, config.site.grid_tolerance_w);

    for driver in &config.drivers {
        info!("driver: {} (lua: {}, site_meter: {}, battery: {} Wh)",
            driver.name, driver.lua, driver.is_site_meter, driver.battery_capacity_wh);
    }

    // Open persistent state
    let state_path = config.state.as_ref()
        .map(|s| s.path.clone())
        .unwrap_or_else(|| "state.redb".to_string());
    let state_store = match state::StateStore::open(&state_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            error!("failed to open state store '{}': {}", state_path, e);
            std::process::exit(1);
        }
    };

    // Initialize telemetry store
    let store = Arc::new(Mutex::new(
        telemetry::TelemetryStore::new(config.site.smoothing_alpha)
    ));

    // Initialize control state (restore from DB if available)
    let mut control_state = control::ControlState::new(
        config.site.grid_target_w,
        config.site.grid_tolerance_w,
    );

    // Restore saved mode
    if let Some(mode_str) = state_store.load_config("mode") {
        if let Ok(mode) = serde_json::from_str::<control::Mode>(&format!("\"{}\"", mode_str)) {
            info!("restored mode from state: {:?}", mode);
            control_state.mode = mode;
        }
    }

    let control = Arc::new(Mutex::new(control_state));

    // Build driver capacity map
    let driver_capacities: HashMap<String, f64> = config.drivers.iter()
        .map(|d| (d.name.clone(), d.battery_capacity_wh))
        .collect();

    let driver_names: Vec<String> = config.drivers.iter().map(|d| d.name.clone()).collect();

    // Graceful shutdown
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        info!("shutdown signal received");
        r.store(false, std::sync::atomic::Ordering::SeqCst);
    }).expect("failed to set ctrl-c handler");

    // Start REST API
    let _api_handle = api::start(
        config.api.port,
        store.clone(),
        control.clone(),
        driver_capacities.clone(),
    );

    // Start HA MQTT bridge (if configured)
    if let Some(ha_config) = config.homeassistant {
        if ha_config.enabled {
            let _ha_handle = ha::start(
                ha_config,
                store.clone(),
                control.clone(),
                driver_names.clone(),
            );
        }
    }

    // TODO: Start Lua driver threads (once lua/ module is ready)
    // For each driver in config.drivers:
    //   spawn a thread that loads the .lua file, registers host API,
    //   calls driver_init, then loops calling driver_poll

    info!("home-ems running on http://0.0.0.0:{}", config.api.port);

    // Control loop on main thread
    let control_interval = Duration::from_secs(config.site.control_interval_s);
    let fuse_max_w = config.fuse.max_power_w();

    while running.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(control_interval);

        // Run one control cycle
        let targets = {
            let store_lock = store.lock().unwrap();
            let mut control_lock = control.lock().unwrap();
            control::compute_dispatch(&store_lock, &mut control_lock, &driver_capacities, fuse_max_w)
        };

        // Dispatch targets to drivers
        for target in &targets {
            // TODO: send driver_command("battery", target.target_w, {id:"ems"}) to the driver
            info!("dispatch: {} -> {:.0}W{}", target.driver, target.target_w,
                if target.clamped { " (clamped)" } else { "" });
        }

        // Persist state
        {
            let control_lock = control.lock().unwrap();
            state_store.save_config("mode", &format!("{:?}", control_lock.mode).to_lowercase());
            state_store.save_config("grid_target_w", &control_lock.grid_target_w.to_string());
        }
    }

    // Shutdown: revert all drivers to autonomous mode
    info!("reverting drivers to autonomous mode...");
    // TODO: call driver_default_mode() on each driver

    state_store.record_event("shutdown");
    info!("home-ems stopped");
}
