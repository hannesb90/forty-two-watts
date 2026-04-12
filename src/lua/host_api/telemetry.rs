use super::HostContext;
use crate::lua::json::lua_table_to_json;
use crate::telemetry::DerType;
use mlua::{Lua, Result as LuaResult, Table};

/// Register telemetry host function:
/// host.emit(der_type, data) -- writes to the shared TelemetryStore.
///
/// der_type: "meter", "pv", "battery"
/// data: Lua table with float values. Must contain at least "w" for power.
///       Battery data may contain "soc" (0-1 fraction).
pub fn register(lua: &Lua, host: &Table, ctx: &HostContext) -> LuaResult<()> {
    let store = ctx.telemetry_store.clone();
    let current_driver = ctx.current_driver.clone();

    let emit_fn = lua.create_function(move |lua, (der_type_str, data): (String, Table)| {
        let der_type = match DerType::from_str(&der_type_str) {
            Some(dt) => dt,
            None => {
                tracing::warn!("host.emit: unknown DER type '{}'", der_type_str);
                return Ok(());
            }
        };

        // Extract power_w from the data table
        let power_w: f64 = data.get("w").unwrap_or(0.0);

        // Extract optional SoC for batteries
        let soc: Option<f64> = data.get("soc").ok();

        // Convert the full data table to JSON for storage
        let json_data = lua_table_to_json(lua, &data)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));

        let driver = current_driver.lock().unwrap().clone();

        tracing::trace!(
            "host.emit: driver='{}' type='{}' w={:.1}",
            driver,
            der_type_str,
            power_w
        );

        store
            .lock()
            .unwrap()
            .update(&driver, &der_type, json_data, power_w, soc);

        Ok(())
    })?;

    host.set("emit", emit_fn)?;
    Ok(())
}
