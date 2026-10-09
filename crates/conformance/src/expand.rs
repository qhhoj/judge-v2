//! Placeholders in scenario messages: a string `{{name}}` becomes the bound value
//! (a submission id, or `session:<judge>` for a session id); an object
//! `{"$repeat": [text, count]}` becomes `text` repeated `count` times, and
//! `{"$cases": [count, status]}` becomes `count` Case objects at positions 1..=count.
use std::collections::HashMap;

use serde_json::Value;

pub type Bindings = HashMap<String, String>;

pub fn expand(value: &Value, bindings: &Bindings) -> Result<Value, String> {
    match value {
        Value::String(text) => expand_text(text, bindings),
        Value::Array(items) => items.iter().map(|item| expand(item, bindings)).collect(),
        Value::Object(fields) if fields.contains_key("$repeat") => generator(fields, "$repeat", repeat),
        Value::Object(fields) if fields.contains_key("$cases") => generator(fields, "$cases", cases),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, item)| Ok((key.clone(), expand(item, bindings)?)))
            .collect::<Result<_, String>>()
            .map(Value::Object),
        other => Ok(other.clone()),
    }
}

fn expand_text(text: &str, bindings: &Bindings) -> Result<Value, String> {
    match text.strip_prefix("{{").and_then(|rest| rest.strip_suffix("}}")) {
        Some(name) => bindings
            .get(name)
            .map(|bound| Value::String(bound.clone()))
            .ok_or_else(|| format!("unbound placeholder {{{{{name}}}}}")),
        None => Ok(Value::String(text.to_string())),
    }
}

/// A generator object holds nothing but its own key.
fn generator(
    fields: &serde_json::Map<String, Value>,
    key: &str,
    build: fn(&Value) -> Result<Value, String>,
) -> Result<Value, String> {
    if fields.len() != 1 {
        return Err(format!("{key} must be the only key of its object"));
    }
    build(&fields[key])
}

fn repeat(arguments: &Value) -> Result<Value, String> {
    match arguments.as_array().map(Vec::as_slice) {
        Some([Value::String(text), Value::Number(count)]) => {
            let count = count.as_u64().ok_or("$repeat count must be a whole number")?;
            Ok(Value::String(text.repeat(count as usize)))
        }
        _ => Err("$repeat takes [text, count]".to_string()),
    }
}

fn cases(arguments: &Value) -> Result<Value, String> {
    match arguments.as_array().map(Vec::as_slice) {
        Some([Value::Number(count), Value::String(status)]) => {
            let count = count.as_u64().ok_or("$cases count must be a whole number")?;
            let case = |position: u64| serde_json::json!({ "position": position, "status": status });
            Ok(Value::Array((1..=count).map(case).collect()))
        }
        _ => Err("$cases takes [count, status]".to_string()),
    }
}
