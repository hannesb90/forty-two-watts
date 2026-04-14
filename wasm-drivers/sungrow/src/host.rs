//! Rust-side glue for the forty-two-watts WASM driver ABI.
//!
//! This is intentionally zero-dependency and small — it's the only thing that
//! touches the raw extern "C" FFI surface. Driver code works with safe Rust
//! wrappers (`host::log()`, `host::emit_telemetry()`, etc.) that handle
//! pointer/length juggling.

use std::ffi::c_void;

// ============================================================================
// Exports — called by the host. We define the memory allocator here so the
// host can pass bytes *into* our module via host.*(ptr, len) patterns.
// ============================================================================

/// Host calls this to allocate `n` bytes in our linear memory, then writes
/// into the returned pointer. We use Box<[u8]> + leak so ownership transfers
/// cleanly; `wasm_dealloc` reclaims it later.
#[no_mangle]
pub extern "C" fn wasm_alloc(n: u32) -> *mut c_void {
    let mut buf: Vec<u8> = Vec::with_capacity(n as usize);
    unsafe { buf.set_len(n as usize); }
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr as *mut c_void
}

/// Reclaim memory allocated via wasm_alloc.
#[no_mangle]
pub extern "C" fn wasm_dealloc(ptr: *mut c_void, n: u32) {
    if ptr.is_null() || n == 0 { return; }
    unsafe {
        let _ = Vec::from_raw_parts(ptr as *mut u8, n as usize, n as usize);
    }
}

// ============================================================================
// Host imports — implemented by forty-two-watts Go host via wazero.
// ============================================================================

// Raw host imports. Names prefixed `h_` so the safe wrappers below can take
// the nice names. wasm_import_name attribute maps back to the symbols the Go
// host exports under the "host" namespace.
#[link(wasm_import_module = "host")]
extern "C" {
    #[link_name = "log"]
    fn h_log(level: i32, ptr: *const u8, len: i32);
    #[link_name = "millis"]
    fn h_millis() -> i64;
    #[link_name = "set_poll_interval"]
    fn h_set_poll_interval(ms: i32);
    #[link_name = "emit_telemetry"]
    fn h_emit_telemetry(ptr: *const u8, len: i32) -> i32;
    #[link_name = "set_sn"]
    fn h_set_sn(ptr: *const u8, len: i32) -> i32;
    #[link_name = "set_make"]
    fn h_set_make(ptr: *const u8, len: i32) -> i32;

    #[link_name = "mqtt_subscribe"]
    fn h_mqtt_subscribe(ptr: *const u8, len: i32) -> i32;
    #[link_name = "mqtt_publish"]
    fn h_mqtt_publish(topic_ptr: *const u8, topic_len: i32, pay_ptr: *const u8, pay_len: i32) -> i32;
    #[link_name = "mqtt_poll_messages"]
    fn h_mqtt_poll_messages(out_ptr: *mut u8, out_cap: i32) -> i32;

    #[link_name = "modbus_read"]
    fn h_modbus_read(addr: i32, count: i32, kind: i32, out_ptr: *mut u8, out_cap: i32) -> i32;
    #[link_name = "modbus_write_single"]
    fn h_modbus_write_single(addr: i32, value: i32) -> i32;
    #[link_name = "modbus_write_multi"]
    fn h_modbus_write_multi(addr: i32, values_ptr: *const u8, count: i32) -> i32;
}

// ============================================================================
// Safe wrappers — these are what the driver code calls.
// ============================================================================

/// Log levels matching the Go host's slog mapping.
#[repr(i32)]
#[allow(dead_code)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info  = 2,
    Warn  = 3,
    Error = 4,
}

/// Write a log message at the given level.
pub fn log_msg(level: Level, s: &str) {
    unsafe { h_log(level as i32, s.as_ptr(), s.len() as i32); }
}

#[allow(dead_code)] pub fn info(s: &str)  { log_msg(Level::Info, s); }
#[allow(dead_code)] pub fn warn(s: &str)  { log_msg(Level::Warn, s); }
#[allow(dead_code)] pub fn error(s: &str) { log_msg(Level::Error, s); }
#[allow(dead_code)] pub fn debug(s: &str) { log_msg(Level::Debug, s); }

/// Monotonic milliseconds since host startup.
pub fn now_ms() -> i64 {
    unsafe { h_millis() }
}

/// Hint to host about desired next poll cadence. Usually the driver returns
/// this from `driver_poll` instead.
#[allow(dead_code)]
pub fn set_poll(ms: i32) {
    unsafe { h_set_poll_interval(ms); }
}

/// Emit a telemetry record. `json` should be a JSON object with at least
/// `{"type":"meter|pv|battery","w":...}`. SoC (0..1) is optional for batteries.
pub fn emit(json: &str) -> i32 {
    unsafe { h_emit_telemetry(json.as_ptr(), json.len() as i32) }
}

#[allow(dead_code)]
pub fn set_serial(sn: &str) { unsafe { h_set_sn(sn.as_ptr(), sn.len() as i32); } }
pub fn set_manufacturer(m: &str) { unsafe { h_set_make(m.as_ptr(), m.len() as i32); } }

/// Subscribe to an MQTT topic. Requires the driver to have been granted
/// `capabilities.mqtt` in the host config.
pub fn mqtt_sub(topic: &str) -> i32 {
    unsafe { h_mqtt_subscribe(topic.as_ptr(), topic.len() as i32) }
}

/// Publish a message to an MQTT topic.
pub fn mqtt_pub(topic: &str, payload: &[u8]) -> i32 {
    unsafe {
        h_mqtt_publish(
            topic.as_ptr(), topic.len() as i32,
            payload.as_ptr(), payload.len() as i32,
        )
    }
}

/// Pop all messages queued since the last call. The host encodes them as
/// JSON: `[{"topic":"...","payload":"..."}, ...]`.
pub fn mqtt_messages() -> String {
    // Large-enough staging buffer. Host returns -1 on overflow; we retry.
    let mut cap: usize = 8192;
    loop {
        let mut buf = vec![0u8; cap];
        let n = unsafe { h_mqtt_poll_messages(buf.as_mut_ptr(), cap as i32) };
        if n < 0 {
            // Overflow — grow and try again (cap at 1 MB to avoid runaway)
            if cap >= 1 << 20 {
                return "[]".to_string();
            }
            cap *= 2;
            continue;
        }
        buf.truncate(n as usize);
        return String::from_utf8(buf).unwrap_or_else(|_| "[]".to_string());
    }
}

/// Convenience: parse the MQTT message JSON returned by `mqtt_messages` into
/// a Vec<(topic, payload)>.
#[allow(dead_code)]
pub fn mqtt_messages_parsed() -> Vec<(String, String)> {
    #[derive(serde::Deserialize)]
    struct Msg { topic: String, payload: String }
    let raw = mqtt_messages();
    match serde_json::from_str::<Vec<Msg>>(&raw) {
        Ok(v) => v.into_iter().map(|m| (m.topic, m.payload)).collect(),
        Err(_) => vec![],
    }
}

// ============================================================================
// Modbus wrappers
// ============================================================================

/// Modbus register kind — matches the host-side ABI enum.
#[repr(i32)]
#[allow(dead_code)]
#[derive(Copy, Clone)]
pub enum ModbusKind {
    Coil     = 0,
    Discrete = 1,
    Holding  = 2,
    Input    = 3,
}

/// Read `count` consecutive registers at `addr`. Returns None on error.
/// The host encodes registers as little-endian bytes into the output buffer.
pub fn modbus_read(addr: u16, count: u16, kind: ModbusKind) -> Option<Vec<u16>> {
    let mut buf = vec![0u8; count as usize * 2];
    let n = unsafe {
        h_modbus_read(addr as i32, count as i32, kind as i32,
                      buf.as_mut_ptr(), buf.len() as i32)
    };
    if n < 0 { return None; }
    let got = (n as usize) / 2;
    let mut regs = Vec::with_capacity(got);
    for i in 0..got {
        let lo = buf[i * 2] as u16;
        let hi = buf[i * 2 + 1] as u16;
        regs.push((hi << 8) | lo);
    }
    Some(regs)
}

/// Write a single holding register. Returns true on success.
pub fn modbus_write_single(addr: u16, value: u16) -> bool {
    unsafe { h_modbus_write_single(addr as i32, value as i32) == 0 }
}

/// Write multiple consecutive holding registers. Returns true on success.
#[allow(dead_code)]
pub fn modbus_write_multiple(addr: u16, values: &[u16]) -> bool {
    let mut buf = Vec::with_capacity(values.len() * 2);
    for &v in values {
        buf.push((v & 0xFF) as u8);
        buf.push((v >> 8) as u8);
    }
    unsafe {
        h_modbus_write_multi(addr as i32, buf.as_ptr(), values.len() as i32) == 0
    }
}
