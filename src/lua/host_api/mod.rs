pub mod core;
pub mod decode;
pub mod modbus;
pub mod mqtt;
pub mod telemetry;

use crate::modbus::ModbusClient;
use crate::mqtt::client::{MessageQueue, MqttClient};
use crate::telemetry::TelemetryStore;
use mlua::{Lua, Result as LuaResult};
use std::sync::{Arc, Mutex};

/// Shared context for all host API functions.
pub struct HostContext {
    /// Shared telemetry store — host.emit() writes here.
    pub telemetry_store: Arc<Mutex<TelemetryStore>>,
    /// Current driver name — set before each tick so host APIs know which driver is calling.
    pub current_driver: Arc<Mutex<String>>,
    /// Per-driver serial overrides (set by host.set_sn).
    pub driver_serials: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Per-driver make/manufacturer (set by host.set_make).
    pub driver_makes: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Modbus client — auto-created from driver config, one per driver.
    pub modbus_client: Option<Arc<Mutex<ModbusClient>>>,
    /// MQTT client — auto-created from driver config, one per driver.
    pub mqtt_client: Option<Arc<Mutex<MqttClient>>>,
    /// MQTT message queue — shared with the client for non-blocking reads.
    pub mqtt_queue: Option<MessageQueue>,
}

/// Register all host API functions on the Lua state under the `host` table.
pub fn register_all(lua: &Lua, ctx: &HostContext) -> LuaResult<()> {
    let host = lua.create_table()?;

    core::register(lua, &host, ctx)?;
    decode::register(lua, &host)?;
    telemetry::register(lua, &host, ctx)?;
    modbus::register(lua, &host, ctx)?;
    mqtt::register(lua, &host, ctx)?;

    lua.globals().set("host", host)?;
    Ok(())
}
