//! Shared Lua <-> JSON conversion utilities.

use mlua::{Lua, Result as LuaResult, Table, Value};

/// Convert a Lua table to a serde_json::Value.
pub fn lua_table_to_json(lua: &Lua, table: &Table) -> LuaResult<serde_json::Value> {
    use serde_json::{Map, Value as JsonValue};

    // Check if this is an array (sequential integer keys starting at 1)
    let mut is_array = true;
    let mut max_key = 0i64;
    for pair in table.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        match k {
            Value::Integer(i) if i >= 1 => {
                if i > max_key {
                    max_key = i;
                }
            }
            _ => {
                is_array = false;
                break;
            }
        }
    }

    if is_array && max_key > 0 {
        let mut arr = Vec::with_capacity(max_key as usize);
        for i in 1..=max_key {
            let val: Value = table.raw_get(i)?;
            arr.push(lua_value_to_json(lua, &val)?);
        }
        Ok(JsonValue::Array(arr))
    } else {
        let mut map = Map::new();
        for pair in table.clone().pairs::<Value, Value>() {
            let (k, v) = pair?;
            let key = match k {
                Value::String(s) => s.to_str()?.to_string(),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            map.insert(key, lua_value_to_json(lua, &v)?);
        }
        Ok(JsonValue::Object(map))
    }
}

/// Convert a single Lua value to a serde_json::Value.
pub fn lua_value_to_json(lua: &Lua, val: &Value) -> LuaResult<serde_json::Value> {
    use serde_json::Value as JsonValue;

    match val {
        Value::Nil => Ok(JsonValue::Null),
        Value::Boolean(b) => Ok(JsonValue::Bool(*b)),
        Value::Integer(i) => Ok(JsonValue::Number((*i).into())),
        Value::Number(n) => Ok(serde_json::Number::from_f64(*n)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null)),
        Value::String(s) => Ok(JsonValue::String(s.to_str()?.to_string())),
        Value::Table(t) => lua_table_to_json(lua, t),
        _ => Ok(JsonValue::Null),
    }
}
