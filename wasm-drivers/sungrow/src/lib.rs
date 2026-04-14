//! Sungrow SH-series WASM driver.
//!
//! Compiled to `wasm32-wasip1`, loaded by forty-two-watts Go host via
//! `cargo build --target wasm32-wasip1 --release`.
//!
//! Drives Sungrow SH10RT (and family) over Modbus TCP. All binary decoding —
//! U32 LE, I32 LE, status bits, SoC scaling — happens IN THE DRIVER. The
//! host provides only raw uint16 register reads/writes.
//!
//! Commanded battery power is sent as three-register sequence:
//!   reg 13051 = power (W)
//!   reg 13050 = cmd  (0xAA=charge, 0xBB=discharge, 0xCC=stop)
//!   reg 13049 = mode (0=stop, 2=forced)
//!
//! Order matters — write power first, then cmd, then mode. The inverter
//! sees a complete directive when mode = 2 is written.

mod host;

use host::ModbusKind;

// ============================================================================
// Register map constants (all from Sungrow SH-series spec)
// ============================================================================

const REG_SERIAL: u16       = 4990;  // 10 regs ASCII
const REG_DEV_TYPE: u16     = 4999;
const REG_RATED_W: u16      = 5000;
const REG_HEATSINK: u16     = 5007;  // I16 ×0.1 °C
const REG_MPPT: u16         = 5010;  // 4 regs V×0.1, A×0.1 per string
const REG_PV_W: u16         = 5016;  // U32 LE
const REG_GRID_FREQ: u16    = 5241;  // ×0.01 Hz
const REG_METER_W: u16      = 5600;  // I32 LE
const REG_METER_PHASE: u16  = 5602;  // 3× I32 LE per-phase
const REG_METER_V: u16      = 5740;  // 3× U16 ×0.1 V
const REG_METER_A: u16      = 5743;  // 3× U16 ×0.01 A

const REG_STATUS: u16       = 13000; // bit 1=charging, bit 2=discharging
const REG_PV_LIFETIME: u16  = 13002; // U32 LE ×0.1 kWh
const REG_BATTERY_BLOCK: u16 = 13019; // 4 regs: V×0.1, A×0.1, W raw, SoC×0.1%
const REG_BAT_DISCHARGE_WH: u16 = 13026; // U32 LE ×0.1 kWh
const REG_IMPORT_WH: u16    = 13036; // U32 LE ×0.1 kWh
const REG_BAT_CHARGE_WH: u16 = 13040; // U32 LE ×0.1 kWh
const REG_EXPORT_WH: u16    = 13045; // U32 LE ×0.1 kWh

const REG_EMS_MODE: u16     = 13049; // W: 0=stop, 2=forced
const REG_FORCE_CMD: u16    = 13050; // W: 0xAA=charge, 0xBB=discharge, 0xCC=stop
const REG_FORCE_POWER: u16  = 13051; // W: watts
const REG_MAX_CHARGE: u16   = 33046; // W: ×0.01 kW
const REG_MAX_DISCHARGE: u16 = 33047; // W: ×0.01 kW

const CMD_CHARGE: u16 = 0xAA;
const CMD_DISCHARGE: u16 = 0xBB;
const CMD_STOP: u16 = 0xCC;

const EMS_STOP: u16 = 0;
const EMS_FORCED: u16 = 2;

// ============================================================================
// Little-endian U32 / I32 helpers
// ============================================================================

/// Decode two consecutive u16 registers as little-endian u32 (Sungrow convention).
fn u32_le(regs: &[u16], idx: usize) -> u32 {
    let lo = regs.get(idx).copied().unwrap_or(0) as u32;
    let hi = regs.get(idx + 1).copied().unwrap_or(0) as u32;
    (hi << 16) | lo
}

/// Decode as signed 32-bit integer.
fn i32_le(regs: &[u16], idx: usize) -> i32 {
    u32_le(regs, idx) as i32
}

/// Decode a signed 16-bit register.
fn i16_reg(r: u16) -> i16 {
    r as i16
}

// ============================================================================
// Driver exports
// ============================================================================

#[no_mangle]
pub extern "C" fn driver_init(_cfg_ptr: *const u8, _cfg_len: i32) -> i32 {
    host::set_manufacturer("Sungrow");

    // Read device type + serial on init, log them, set serial number
    if let Some(regs) = host::modbus_read(REG_SERIAL, 10, ModbusKind::Input) {
        let sn = serial_from_regs(&regs);
        if !sn.is_empty() {
            host::set_serial(&sn);
            host::info(&format!("sungrow SN: {}", sn));
        }
    }
    if let Some(regs) = host::modbus_read(REG_DEV_TYPE, 1, ModbusKind::Input) {
        host::info(&format!("sungrow device type: {}", regs[0]));
    }

    // Safety: ensure max-power registers are set to at least 5000W so forced-mode
    // commands aren't clamped by a factory default that throttles them.
    if let Some(regs) = host::modbus_read(REG_MAX_CHARGE, 1, ModbusKind::Holding) {
        // reg value is ×0.01 kW (i.e. ×10 W). 500 = 5 kW.
        if regs[0] < 500 {
            host::info("raising max charge to 5 kW");
            host::modbus_write_single(REG_MAX_CHARGE, 500);
        }
    }
    if let Some(regs) = host::modbus_read(REG_MAX_DISCHARGE, 1, ModbusKind::Holding) {
        if regs[0] < 500 {
            host::info("raising max discharge to 5 kW");
            host::modbus_write_single(REG_MAX_DISCHARGE, 500);
        }
    }

    host::info("sungrow driver initialized");
    host::set_poll(2000); // 2s poll — Modbus is slower than MQTT
    0
}

#[no_mangle]
pub extern "C" fn driver_poll() -> i32 {
    emit_pv();
    emit_battery();
    emit_meter();
    2000
}

#[no_mangle]
pub extern "C" fn driver_command(ptr: *const u8, len: i32) -> i32 {
    let body = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    #[derive(serde::Deserialize)]
    struct Cmd {
        action: String,
        #[serde(default)]
        power_w: Option<f64>,
    }
    let cmd: Cmd = match serde_json::from_slice(body) {
        Ok(c) => c,
        Err(e) => {
            host::warn(&format!("invalid command: {}", e));
            return 1;
        }
    };
    if cmd.action != "battery" {
        host::warn(&format!("unsupported action: {}", cmd.action));
        return 2;
    }
    dispatch_battery(cmd.power_w.unwrap_or(0.0));
    0
}

#[no_mangle]
pub extern "C" fn driver_default() -> i32 {
    // Revert to stop mode — inverter's own self-consumption logic takes over
    host::modbus_write_single(REG_FORCE_POWER, 0);
    host::modbus_write_single(REG_FORCE_CMD, CMD_STOP);
    host::modbus_write_single(REG_EMS_MODE, EMS_STOP);
    0
}

#[no_mangle]
pub extern "C" fn driver_cleanup() -> i32 { 0 }

// ============================================================================
// Command dispatch
// ============================================================================

/// Controller issues `power_w` in SITE convention (boundary view):
///   + = battery CHARGES (adds load to site → site imports more)
///   − = battery DISCHARGES (pushes to site → site imports less or exports)
fn dispatch_battery(power_w: f64) {
    if power_w.abs() < 10.0 {
        host::modbus_write_single(REG_FORCE_POWER, 0);
        host::modbus_write_single(REG_FORCE_CMD, CMD_STOP);
        host::modbus_write_single(REG_EMS_MODE, EMS_STOP);
        return;
    }
    let (cmd, watts) = if power_w > 0.0 {
        // Site +  = charge
        (CMD_CHARGE, power_w as u16)
    } else {
        // Site − = discharge
        (CMD_DISCHARGE, power_w.abs() as u16)
    };
    host::modbus_write_single(REG_FORCE_POWER, watts);
    host::modbus_write_single(REG_FORCE_CMD, cmd);
    host::modbus_write_single(REG_EMS_MODE, EMS_FORCED);
}

// ============================================================================
// Telemetry emission
// ============================================================================

fn emit_pv() {
    let pv_regs = match host::modbus_read(REG_PV_W, 2, ModbusKind::Input) {
        Some(r) => r,
        None => return,
    };
    let pv_w = u32_le(&pv_regs, 0) as f64;

    // MPPT strings (2× V×0.1, A×0.1)
    let (mut m1v, mut m1a, mut m2v, mut m2a) = (0.0, 0.0, 0.0, 0.0);
    if let Some(m) = host::modbus_read(REG_MPPT, 4, ModbusKind::Input) {
        m1v = m[0] as f64 * 0.1;
        m1a = m[1] as f64 * 0.1;
        m2v = m[2] as f64 * 0.1;
        m2a = m[3] as f64 * 0.1;
    }

    let lifetime_wh = host::modbus_read(REG_PV_LIFETIME, 2, ModbusKind::Input)
        .map(|r| u32_le(&r, 0) as f64 * 0.1 * 1000.0)
        .unwrap_or(0.0);

    let temp_c = host::modbus_read(REG_HEATSINK, 1, ModbusKind::Input)
        .map(|r| i16_reg(r[0]) as f64 * 0.1)
        .unwrap_or(0.0);

    let rated_w = host::modbus_read(REG_RATED_W, 1, ModbusKind::Input)
        .map(|r| r[0] as f64)
        .unwrap_or(0.0);

    // Site convention (boundary view): PV pushes energy TO the site → NEGATIVE.
    let json = format!(
        r#"{{"type":"pv","w":{},"mppt1_v":{},"mppt1_a":{},"mppt2_v":{},"mppt2_a":{},"lifetime_wh":{},"rated_w":{},"temp_c":{}}}"#,
        -pv_w, m1v, m1a, m2v, m2a, lifetime_wh, rated_w, temp_c
    );
    host::emit(&json);
}

fn emit_battery() {
    let status = host::modbus_read(REG_STATUS, 1, ModbusKind::Input)
        .map(|r| r[0])
        .unwrap_or(0);
    let is_discharging = (status & 0x0004) != 0;

    let bat_regs = match host::modbus_read(REG_BATTERY_BLOCK, 4, ModbusKind::Input) {
        Some(r) => r,
        None => return,
    };
    let bat_v = bat_regs[0] as f64 * 0.1;
    let bat_a = bat_regs[1] as f64 * 0.1;
    // Site convention (boundary view, + = flow INTO site via grid meter):
    //   charging  → consumes from site bus → POSITIVE (load)
    //   discharging → pushes TO site bus → NEGATIVE
    // Sungrow reports unsigned magnitude; direction comes from status bits.
    let bat_w = if is_discharging {
        -(bat_regs[2] as f64)       // discharge → −
    } else {
        bat_regs[2] as f64          // charge → +
    };
    let bat_soc = bat_regs[3] as f64 * 0.001; // ×0.1% → 0..1

    let charge_wh = host::modbus_read(REG_BAT_CHARGE_WH, 2, ModbusKind::Input)
        .map(|r| u32_le(&r, 0) as f64 * 0.1 * 1000.0)
        .unwrap_or(0.0);
    let discharge_wh = host::modbus_read(REG_BAT_DISCHARGE_WH, 2, ModbusKind::Input)
        .map(|r| u32_le(&r, 0) as f64 * 0.1 * 1000.0)
        .unwrap_or(0.0);

    let json = format!(
        r#"{{"type":"battery","w":{},"v":{},"a":{},"soc":{},"charge_wh":{},"discharge_wh":{}}}"#,
        bat_w, bat_v, bat_a, bat_soc, charge_wh, discharge_wh
    );
    host::emit(&json);
}

fn emit_meter() {
    let grid_regs = match host::modbus_read(REG_METER_W, 2, ModbusKind::Input) {
        Some(r) => r,
        None => return,
    };
    let grid_w = i32_le(&grid_regs, 0) as f64;

    let (mut l1, mut l2, mut l3) = (0.0, 0.0, 0.0);
    if let Some(r) = host::modbus_read(REG_METER_PHASE, 6, ModbusKind::Input) {
        l1 = i32_le(&r, 0) as f64;
        l2 = i32_le(&r, 2) as f64;
        l3 = i32_le(&r, 4) as f64;
    }

    let import_wh = host::modbus_read(REG_IMPORT_WH, 2, ModbusKind::Input)
        .map(|r| u32_le(&r, 0) as f64 * 0.1 * 1000.0)
        .unwrap_or(0.0);
    let export_wh = host::modbus_read(REG_EXPORT_WH, 2, ModbusKind::Input)
        .map(|r| u32_le(&r, 0) as f64 * 0.1 * 1000.0)
        .unwrap_or(0.0);

    let hz = host::modbus_read(REG_GRID_FREQ, 1, ModbusKind::Input)
        .map(|r| r[0] as f64 * 0.01)
        .unwrap_or(0.0);

    let json = format!(
        r#"{{"type":"meter","w":{},"l1_w":{},"l2_w":{},"l3_w":{},"hz":{},"import_wh":{},"export_wh":{}}}"#,
        grid_w, l1, l2, l3, hz, import_wh, export_wh
    );
    host::emit(&json);
}

// ============================================================================
// Serial number decoding
// ============================================================================

/// Extract ASCII serial from 10 × u16 registers (2 chars per reg, big-endian).
fn serial_from_regs(regs: &[u16]) -> String {
    let mut bytes = Vec::with_capacity(20);
    for &r in regs {
        let hi = (r >> 8) as u8;
        let lo = (r & 0xFF) as u8;
        if hi != 0 { bytes.push(hi); }
        if lo != 0 { bytes.push(lo); }
    }
    String::from_utf8(bytes).unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .collect()
}

// ============================================================================
// Tests (run on host, not in WASM)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u32_le_decodes_little_endian() {
        // 12345 = 0x00003039 → low=0x3039, high=0x0000
        let regs = vec![0x3039u16, 0x0000u16];
        assert_eq!(u32_le(&regs, 0), 12345);
    }

    #[test]
    fn i32_le_decodes_negative() {
        // -1500 = 0xFFFFFA24 → low=0xFA24, high=0xFFFF
        let regs = vec![0xFA24u16, 0xFFFFu16];
        assert_eq!(i32_le(&regs, 0), -1500);
    }

    #[test]
    fn i16_reg_negative() {
        // -350 = 0xFEA2 (as u16)
        assert_eq!(i16_reg(0xFEA2), -350);
    }

    #[test]
    fn serial_from_regs_sungrow_format() {
        // "SH10RT-SIM-00001" → 8 regs (2 chars each) + 2 padding
        let regs = vec![
            0x5348u16, // 'S' 'H'
            0x3130,    // '1' '0'
            0x5254,    // 'R' 'T'
            0x2D53,    // '-' 'S'
            0x494D,    // 'I' 'M'
            0x2D30,    // '-' '0'
            0x3030,    // '0' '0'
            0x3031,    // '0' '1'
            0x0000,
            0x0000,
        ];
        assert_eq!(serial_from_regs(&regs), "SH10RT-SIM-00001");
    }

    #[test]
    fn status_bit_parsing() {
        assert_eq!(0x0004u16 & 0x0004, 0x0004); // discharging bit set
        assert_eq!(0x0002u16 & 0x0004, 0x0000); // only charging bit
    }
}
