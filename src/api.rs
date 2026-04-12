use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use tracing::{info, error};

use crate::telemetry::{TelemetryStore, DerType};
use crate::control::{ControlState, Mode};

/// Start the REST API server on a separate thread
pub fn start(
    port: u16,
    store: Arc<Mutex<TelemetryStore>>,
    control: Arc<Mutex<ControlState>>,
    driver_capacities: HashMap<String, f64>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("api".to_string())
        .spawn(move || {
            let addr = format!("0.0.0.0:{}", port);
            let server = match tiny_http::Server::http(&addr) {
                Ok(s) => s,
                Err(e) => {
                    error!("failed to start API server on {}: {}", addr, e);
                    return;
                }
            };
            info!("API server listening on http://{}", addr);

            for mut request in server.incoming_requests() {
                let path = request.url().to_string();
                let method = request.method().to_string();

                let response = match (method.as_str(), path.as_str()) {
                    ("GET", "/api/health") => handle_health(&store),
                    ("GET", "/api/status") => handle_status(&store, &control, &driver_capacities),
                    ("GET", "/api/mode") => handle_get_mode(&control),
                    ("POST", "/api/mode") => handle_set_mode(&control, &mut request),
                    ("POST", "/api/target") => handle_set_target(&control, &mut request),
                    ("GET", "/api/drivers") => handle_drivers(&store),
                    _ => json_response(404, &serde_json::json!({"error": "not found"})),
                };

                if let Err(e) = request.respond(response) {
                    error!("failed to send response: {}", e);
                }
            }
        })
        .expect("failed to start API thread")
}

fn json_response(status: u16, body: &serde_json::Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let body_str = serde_json::to_string(body).unwrap_or_default();
    let data = std::io::Cursor::new(body_str.into_bytes());
    let status_code = tiny_http::StatusCode(status);
    let header = tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap();
    tiny_http::Response::new(status_code, vec![header], data, None, None)
}

fn read_body(request: &mut tiny_http::Request) -> String {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    body
}

fn handle_health(store: &Arc<Mutex<TelemetryStore>>) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let store = store.lock().unwrap();
    let health = store.all_health();

    let drivers_ok = health.values().filter(|h| h.status == crate::telemetry::DriverStatus::Ok).count();
    let drivers_degraded = health.values().filter(|h| h.status == crate::telemetry::DriverStatus::Degraded).count();
    let drivers_offline = health.values().filter(|h| h.status == crate::telemetry::DriverStatus::Offline).count();

    let status = if drivers_offline > 0 { "degraded" } else { "ok" };

    json_response(200, &serde_json::json!({
        "status": status,
        "drivers_ok": drivers_ok,
        "drivers_degraded": drivers_degraded,
        "drivers_offline": drivers_offline,
    }))
}

fn handle_status(
    store: &Arc<Mutex<TelemetryStore>>,
    control: &Arc<Mutex<ControlState>>,
    capacities: &HashMap<String, f64>,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let store = store.lock().unwrap();
    let control = control.lock().unwrap();

    // Aggregate readings
    let meters = store.readings_by_type(&DerType::Meter);
    let pvs = store.readings_by_type(&DerType::Pv);
    let bats = store.readings_by_type(&DerType::Battery);

    let grid_w: f64 = meters.iter().map(|m| m.smoothed_w).sum();
    let pv_w: f64 = pvs.iter().map(|p| p.smoothed_w).sum();
    let bat_w: f64 = bats.iter().map(|b| b.smoothed_w).sum();

    // Weighted average SoC by capacity
    let total_cap: f64 = bats.iter()
        .filter_map(|b| capacities.get(&b.driver).copied())
        .sum();
    let avg_soc = if total_cap > 0.0 {
        bats.iter()
            .filter_map(|b| {
                let cap = capacities.get(&b.driver)?;
                Some(b.soc.unwrap_or(0.0) * cap)
            })
            .sum::<f64>() / total_cap
    } else {
        0.0
    };

    // Per-driver details
    let mut drivers = serde_json::Map::new();
    for (name, health) in store.all_health() {
        let mut d = serde_json::Map::new();
        d.insert("status".into(), serde_json::json!(format!("{:?}", health.status)));
        d.insert("consecutive_errors".into(), serde_json::json!(health.consecutive_errors));
        d.insert("tick_count".into(), serde_json::json!(health.tick_count));

        if let Some(err) = &health.last_error {
            d.insert("last_error".into(), serde_json::json!(err));
        }

        if let Some(r) = store.get(name, &DerType::Meter) {
            d.insert("meter_w".into(), serde_json::json!(r.smoothed_w));
        }
        if let Some(r) = store.get(name, &DerType::Pv) {
            d.insert("pv_w".into(), serde_json::json!(r.smoothed_w));
        }
        if let Some(r) = store.get(name, &DerType::Battery) {
            d.insert("bat_w".into(), serde_json::json!(r.smoothed_w));
            if let Some(soc) = r.soc {
                d.insert("bat_soc".into(), serde_json::json!(soc));
            }
        }

        drivers.insert(name.clone(), serde_json::Value::Object(d));
    }

    // Dispatch targets
    let targets: Vec<_> = control.last_targets.iter().map(|t| {
        serde_json::json!({
            "driver": t.driver,
            "target_w": t.target_w,
            "clamped": t.clamped,
        })
    }).collect();

    json_response(200, &serde_json::json!({
        "mode": control.mode,
        "grid_w": grid_w,
        "pv_w": pv_w,
        "bat_w": bat_w,
        "bat_soc": avg_soc,
        "grid_target_w": control.grid_target_w,
        "drivers": drivers,
        "dispatch": targets,
    }))
}

fn handle_get_mode(control: &Arc<Mutex<ControlState>>) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let control = control.lock().unwrap();
    json_response(200, &serde_json::json!({
        "mode": control.mode,
        "grid_target_w": control.grid_target_w,
    }))
}

fn handle_set_mode(
    control: &Arc<Mutex<ControlState>>,
    request: &mut tiny_http::Request,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let body = read_body(request);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&body);

    match parsed {
        Ok(v) => {
            let mut control = control.lock().unwrap();

            if let Some(mode_str) = v.get("mode").and_then(|m| m.as_str()) {
                match serde_json::from_value::<Mode>(serde_json::json!(mode_str)) {
                    Ok(mode) => {
                        info!("mode changed to {:?}", mode);
                        control.mode = mode;
                    }
                    Err(_) => {
                        return json_response(400, &serde_json::json!({"error": "invalid mode"}));
                    }
                }
            }

            if let Some(order) = v.get("priority_order").and_then(|o| o.as_array()) {
                control.priority_order = order.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
            }

            if let Some(weights) = v.get("weights").and_then(|w| w.as_object()) {
                control.weights = weights.iter()
                    .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                    .collect();
            }

            json_response(200, &serde_json::json!({"mode": control.mode}))
        }
        Err(e) => json_response(400, &serde_json::json!({"error": e.to_string()})),
    }
}

fn handle_set_target(
    control: &Arc<Mutex<ControlState>>,
    request: &mut tiny_http::Request,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let body = read_body(request);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&body);

    match parsed {
        Ok(v) => {
            if let Some(target) = v.get("grid_target_w").and_then(|t| t.as_f64()) {
                let mut control = control.lock().unwrap();
                info!("grid target changed to {}W", target);
                control.grid_target_w = target;
                json_response(200, &serde_json::json!({"grid_target_w": target}))
            } else {
                json_response(400, &serde_json::json!({"error": "missing grid_target_w"}))
            }
        }
        Err(e) => json_response(400, &serde_json::json!({"error": e.to_string()})),
    }
}

fn handle_drivers(store: &Arc<Mutex<TelemetryStore>>) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let store = store.lock().unwrap();
    let drivers: Vec<_> = store.all_health().values().map(|h| {
        serde_json::json!({
            "name": h.name,
            "status": format!("{:?}", h.status),
            "consecutive_errors": h.consecutive_errors,
            "tick_count": h.tick_count,
            "last_error": h.last_error,
        })
    }).collect();

    json_response(200, &serde_json::json!(drivers))
}
