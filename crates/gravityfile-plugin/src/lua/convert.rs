//! Shared conversions between Lua values and the plugin [`Value`] type.
//!
//! Both the main [`LuaRuntime`](super::runtime::LuaRuntime) and the
//! [`LuaIsolatedContext`](super::isolate::LuaIsolatedContext) use these so the
//! two paths can't drift (an earlier copy in the isolate flattened Lua arrays
//! into objects, losing array structure).

use mlua::{Lua, Value as LuaValue};

use crate::types::Value;

/// Convert a Lua value into the plugin [`Value`] type.
///
/// Tables with consecutive positive-integer keys (`1..=n`) become [`Value::Array`];
/// all other tables become [`Value::Object`].
pub(super) fn lua_to_value(lua_val: LuaValue) -> Value {
    match lua_val {
        LuaValue::Nil => Value::Null,
        LuaValue::Boolean(b) => Value::Bool(b),
        LuaValue::Integer(i) => Value::Integer(i),
        LuaValue::Number(n) => Value::Float(n),
        LuaValue::String(s) => Value::String(s.to_string_lossy()),
        LuaValue::Table(t) => {
            // Detect array (positive contiguous integer keys) vs object.
            let mut is_array = true;
            let mut max_index = 0i64;

            for pair in t.clone().pairs::<i64, LuaValue>() {
                match pair {
                    Ok((k, _)) if k > 0 => max_index = max_index.max(k),
                    _ => {
                        is_array = false;
                        break;
                    }
                }
            }

            if is_array && max_index > 0 {
                let mut arr = Vec::new();
                for i in 1..=max_index {
                    if let Ok(v) = t.get::<LuaValue>(i) {
                        arr.push(lua_to_value(v));
                    }
                }
                Value::Array(arr)
            } else {
                let mut obj = std::collections::HashMap::new();
                for (k, v) in t.pairs::<String, LuaValue>().flatten() {
                    obj.insert(k, lua_to_value(v));
                }
                Value::Object(obj)
            }
        }
        _ => Value::Null,
    }
}

/// Convert a plugin [`Value`] into a Lua value.
pub(super) fn value_to_lua(lua: &Lua, val: &Value) -> mlua::Result<LuaValue> {
    match val {
        Value::Null => Ok(LuaValue::Nil),
        Value::Bool(b) => Ok(LuaValue::Boolean(*b)),
        Value::Integer(i) => Ok(LuaValue::Integer(*i)),
        Value::Float(f) => Ok(LuaValue::Number(*f)),
        Value::String(s) => Ok(LuaValue::String(lua.create_string(s)?)),
        Value::Array(arr) => {
            let table = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                table.set(i + 1, value_to_lua(lua, v)?)?;
            }
            Ok(LuaValue::Table(table))
        }
        Value::Object(obj) => {
            let table = lua.create_table()?;
            for (k, v) in obj {
                table.set(k.as_str(), value_to_lua(lua, v)?)?;
            }
            Ok(LuaValue::Table(table))
        }
        Value::Bytes(b) => Ok(LuaValue::String(lua.create_string(b)?)),
    }
}
