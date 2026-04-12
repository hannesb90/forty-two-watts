use super::HostContext;
use mlua::{Lua, Result as LuaResult, Table, Value};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static BOOT_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn boot_time() -> &'static Instant {
    BOOT_TIME.get_or_init(Instant::now)
}

/// Extract a string from a Lua Value, with a fallback.
fn lua_str(val: &Value, fallback: &str) -> String {
    match val {
        Value::String(s) => s
            .to_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| fallback.to_string()),
        other => format!("{:?}", other),
    }
}

/// Register core host functions: host.log, host.millis, host.timestamp,
/// host.sleep, host.pool_free, host.set_sn, host.set_make.
pub fn register(lua: &Lua, host: &Table, ctx: &HostContext) -> LuaResult<()> {
    // Initialize boot time
    let _ = boot_time();

    // host.log(message) -- single arg, logs at info level
    // host.log(level, message) -- two args, level = "debug"/"info"/"warn"/"error"
    let log_fn = lua.create_function(|_, args: mlua::MultiValue| {
        let (level, msg) = match args.len() {
            0 => return Ok(()),
            1 => ("info".to_string(), lua_str(&args[0], "")),
            _ => (lua_str(&args[0], "info"), lua_str(&args[1], "")),
        };

        match level.as_str() {
            "debug" | "D" => tracing::debug!(target: "lua", "{}", msg),
            "info" | "I" => tracing::info!(target: "lua", "{}", msg),
            "warn" | "W" => tracing::warn!(target: "lua", "{}", msg),
            "error" | "E" => tracing::error!(target: "lua", "{}", msg),
            _ => tracing::info!(target: "lua", "[{}] {}", level, msg),
        }
        Ok(())
    })?;
    host.set("log", log_fn)?;

    // host.millis() -> milliseconds since boot
    let millis_fn = lua.create_function(|_, ()| {
        let elapsed = boot_time().elapsed().as_millis() as u64;
        Ok(elapsed)
    })?;
    host.set("millis", millis_fn)?;

    // host.sleep(ms) -> sleep the current thread (capped at 5000ms)
    let sleep_fn = lua.create_function(|_, ms: u64| {
        let capped = ms.min(5000);
        std::thread::sleep(std::time::Duration::from_millis(capped));
        Ok(())
    })?;
    host.set("sleep", sleep_fn)?;

    // host.timestamp() -> Unix timestamp in seconds (float)
    let timestamp_fn = lua.create_function(|_, ()| {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        Ok(ts)
    })?;
    host.set("timestamp", timestamp_fn)?;

    // host.pool_free() -> (free_bytes, total_bytes)
    let pool_free_fn = lua.create_function(|lua, ()| {
        let used = lua.used_memory();
        let limit: u64 = lua
            .globals()
            .get::<u64>("__MEMORY_LIMIT")
            .unwrap_or(4 * 1024 * 1024);
        let free = (limit as usize).saturating_sub(used);
        Ok((free, limit))
    })?;
    host.set("pool_free", pool_free_fn)?;

    // host.set_sn(serial) -- override device serial for telemetry routing
    {
        let current_driver = ctx.current_driver.clone();
        let driver_serials = ctx.driver_serials.clone();
        let f = lua.create_function(move |_, serial: String| {
            let driver = current_driver.lock().unwrap().clone();
            if !driver.is_empty() {
                tracing::info!("Driver '{}': SN set to '{}'", driver, serial);
                driver_serials.lock().unwrap().insert(driver, serial);
            }
            Ok(())
        })?;
        host.set("set_sn", f)?;
    }

    // host.set_make(make) -- set manufacturer name for this driver
    {
        let current_driver = ctx.current_driver.clone();
        let driver_makes = ctx.driver_makes.clone();
        let f = lua.create_function(move |_, make: String| {
            let driver = current_driver.lock().unwrap().clone();
            if !driver.is_empty() {
                driver_makes.lock().unwrap().insert(driver, make);
            }
            Ok(())
        })?;
        host.set("set_make", f)?;
    }

    Ok(())
}
