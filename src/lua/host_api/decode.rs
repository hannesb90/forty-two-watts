use crate::lua::json::lua_table_to_json;
use mlua::{Lua, Result as LuaResult, Table};

/// Register decode helpers: host.decode_f32, decode_u32, decode_i32,
/// decode_u32_le, decode_i32_le, decode_i16, decode_u64,
/// host.json_decode, host.json_encode, host.scale.
pub fn register(lua: &Lua, host: &Table) -> LuaResult<()> {
    // host.decode_i16(val) -> signed 16-bit interpretation
    host.set(
        "decode_i16",
        lua.create_function(|_, val: u32| Ok(val as u16 as i16 as f64))?,
    )?;

    // host.decode_u32(hi, lo) -> unsigned 32-bit from two 16-bit registers (big-endian)
    host.set(
        "decode_u32",
        lua.create_function(|_, (hi, lo): (u32, u32)| {
            let result = ((hi & 0xFFFF) << 16) | (lo & 0xFFFF);
            Ok(result as f64)
        })?,
    )?;

    // host.decode_i32(hi, lo) -> signed 32-bit from two 16-bit registers (big-endian)
    host.set(
        "decode_i32",
        lua.create_function(|_, (hi, lo): (u32, u32)| {
            let result = (((hi & 0xFFFF) << 16) | (lo & 0xFFFF)) as i32;
            Ok(result as f64)
        })?,
    )?;

    // host.decode_u32_le(lo, hi) -> unsigned 32-bit from two 16-bit registers (little-endian)
    host.set(
        "decode_u32_le",
        lua.create_function(|_, (lo, hi): (u32, u32)| {
            let result = ((hi & 0xFFFF) << 16) | (lo & 0xFFFF);
            Ok(result as f64)
        })?,
    )?;

    // host.decode_i32_le(lo, hi) -> signed 32-bit from two 16-bit registers (little-endian)
    host.set(
        "decode_i32_le",
        lua.create_function(|_, (lo, hi): (u32, u32)| {
            let result = (((hi & 0xFFFF) << 16) | (lo & 0xFFFF)) as i32;
            Ok(result as f64)
        })?,
    )?;

    // host.decode_f32(hi, lo) -> IEEE 754 float from two 16-bit registers (big-endian)
    host.set(
        "decode_f32",
        lua.create_function(|_, (hi, lo): (u32, u32)| {
            let bits = ((hi & 0xFFFF) << 16) | (lo & 0xFFFF);
            let f = f32::from_bits(bits);
            Ok(f as f64)
        })?,
    )?;

    // host.decode_u64(w1, w2, w3, w4) -> unsigned 64-bit from four 16-bit registers
    host.set(
        "decode_u64",
        lua.create_function(|_, (w1, w2, w3, w4): (u64, u64, u64, u64)| {
            let result = ((w1 & 0xFFFF) << 48)
                | ((w2 & 0xFFFF) << 32)
                | ((w3 & 0xFFFF) << 16)
                | (w4 & 0xFFFF);
            Ok(result as f64)
        })?,
    )?;

    // host.scale(value, sf) -> value * 10^sf (SunSpec scale factor)
    host.set(
        "scale",
        lua.create_function(|_, (value, sf): (f64, f64)| {
            let sf = if sf.abs() > 10.0 { 0.0 } else { sf };
            Ok(value * 10.0_f64.powf(sf))
        })?,
    )?;

    // host.json_decode(str) -> Lua table (parse JSON string into Lua value)
    host.set(
        "json_decode",
        lua.create_function(|lua, s: String| {
            let val: serde_json::Value = match serde_json::from_str(&s) {
                Ok(v) => v,
                Err(e) => {
                    return Ok((mlua::Value::Nil, Some(e.to_string())));
                }
            };
            let lua_val = json_to_lua(lua, &val)?;
            Ok((lua_val, None))
        })?,
    )?;

    // host.json_encode(table) -> JSON string
    host.set(
        "json_encode",
        lua.create_function(|lua, val: mlua::Value| {
            let json_val = match val {
                mlua::Value::Table(ref t) => lua_table_to_json(lua, t)?,
                mlua::Value::String(ref s) => serde_json::Value::String(s.to_str()?.to_string()),
                mlua::Value::Integer(i) => serde_json::Value::Number(i.into()),
                mlua::Value::Number(n) => serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
                mlua::Value::Boolean(b) => serde_json::Value::Bool(b),
                mlua::Value::Nil => serde_json::Value::Null,
                _ => {
                    return Err(mlua::Error::external("json_encode: unsupported type"));
                }
            };
            match serde_json::to_string(&json_val) {
                Ok(s) => Ok(s),
                Err(e) => Err(mlua::Error::external(format!("json_encode: {}", e))),
            }
        })?,
    )?;

    Ok(())
}

/// Convert a serde_json::Value to a Lua Value.
fn json_to_lua(lua: &Lua, val: &serde_json::Value) -> LuaResult<mlua::Value> {
    match val {
        serde_json::Value::Null => Ok(mlua::Value::Nil),
        serde_json::Value::Bool(b) => Ok(mlua::Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(mlua::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(mlua::Value::Number(f))
            } else {
                Ok(mlua::Value::Nil)
            }
        }
        serde_json::Value::String(s) => Ok(mlua::Value::String(lua.create_string(s)?)),
        serde_json::Value::Array(arr) => {
            let tbl = lua.create_table()?;
            for (i, item) in arr.iter().enumerate() {
                tbl.raw_set(i + 1, json_to_lua(lua, item)?)?;
            }
            Ok(mlua::Value::Table(tbl))
        }
        serde_json::Value::Object(obj) => {
            let tbl = lua.create_table()?;
            for (k, v) in obj {
                tbl.raw_set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Ok(mlua::Value::Table(tbl))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_i16() {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        register(&lua, &host).unwrap();

        let f: mlua::Function = host.get("decode_i16").unwrap();
        let result: f64 = f.call(65535u32).unwrap();
        assert_eq!(result, -1.0);

        let result: f64 = f.call(32768u32).unwrap();
        assert_eq!(result, -32768.0);
    }

    #[test]
    fn test_decode_u32() {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        register(&lua, &host).unwrap();

        let f: mlua::Function = host.get("decode_u32").unwrap();
        let result: f64 = f.call((1u32, 0u32)).unwrap();
        assert_eq!(result, 65536.0);
    }

    #[test]
    fn test_decode_f32() {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        register(&lua, &host).unwrap();

        let f: mlua::Function = host.get("decode_f32").unwrap();
        let result: f64 = f.call((0x3F80u32, 0x0000u32)).unwrap();
        assert!((result - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_scale() {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        register(&lua, &host).unwrap();

        let f: mlua::Function = host.get("scale").unwrap();
        let result: f64 = f.call((1500.0, -1.0)).unwrap();
        assert!((result - 150.0).abs() < 0.001);
    }

    #[test]
    fn test_json_roundtrip() {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        register(&lua, &host).unwrap();

        let decode: mlua::Function = host.get("json_decode").unwrap();
        let encode: mlua::Function = host.get("json_encode").unwrap();

        let original = r#"{"a":1,"b":"hello","c":[1,2,3]}"#;
        let (decoded, _): (mlua::Value, Option<String>) = decode.call(original.to_string()).unwrap();
        let encoded: String = encode.call(decoded).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], "hello");
        assert_eq!(parsed["c"][0], 1);
        assert_eq!(parsed["c"][2], 3);
    }
}
