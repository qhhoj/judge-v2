//! Partial matching of an observed proto3-JSON message against a scenario's expectation.
//! The same rules live in backend-v2 `libs/judge-proto/src/conformance/matcher.ts`;
//! `proto/conformance/matcher-cases.json` pins both.
use serde_json::Value;

/// Ok when every field `expected` names matches `actual`; a field missing from
/// `actual` holds its proto3 default, except that an empty object `{}` needs the
/// field present (a oneof member, an empty message). `"*"` matches any present value.
pub fn matches(expected: &Value, actual: &Value) -> Result<(), String> {
    // the top-level keys name the oneof member of a message: it must be the one sent,
    // however empty the expectation for its fields is
    if let Value::Object(members) = expected
        && let Some(missing) = members.keys().find(|key| actual.get(key.as_str()).is_none_or(Value::is_null))
    {
        return Err(format!("$.{missing}: expected this message, got {actual}"));
    }
    match_at("$", expected, actual)
}

fn match_at(path: &str, expected: &Value, actual: &Value) -> Result<(), String> {
    match (expected, actual) {
        (Value::String(star), Value::Null) if star == "*" => {
            Err(format!("{path}: expected a value, got none"))
        }
        (Value::String(star), _) if star == "*" => Ok(()),
        // `{}` names a oneof member or an empty message: it must be there
        (Value::Object(fields), Value::Null) if fields.is_empty() => {
            Err(format!("{path}: expected a value, got none"))
        }
        (Value::Object(fields), Value::Object(_) | Value::Null) => {
            fields.iter().try_for_each(|(key, value)| {
                match_at(&format!("{path}.{key}"), value, actual.get(key).unwrap_or(&Value::Null))
            })
        }
        (Value::Array(items), Value::Array(_) | Value::Null) => match_items(path, items, actual),
        (_, Value::Null) if is_default(expected) => Ok(()),
        _ if scalars_equal(expected, actual) => Ok(()),
        _ => Err(format!("{path}: expected {expected}, got {actual}")),
    }
}

fn match_items(path: &str, items: &[Value], actual: &Value) -> Result<(), String> {
    let empty = Vec::new();
    let observed = actual.as_array().unwrap_or(&empty);
    if items.len() != observed.len() {
        return Err(format!("{path}: expected {} items, got {}", items.len(), observed.len()));
    }
    items
        .iter()
        .zip(observed)
        .enumerate()
        .try_for_each(|(index, (item, seen))| match_at(&format!("{path}[{index}]"), item, seen))
}

/// The proto3 default a missing field stands for.
fn is_default(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => !flag,
        Value::Number(number) => number.as_f64() == Some(0.0),
        Value::String(text) => text.is_empty() || text == "0" || text.ends_with("_UNSPECIFIED"),
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.values().all(is_default),
        Value::Null => true,
    }
}

/// 64-bit integers print as strings in proto3 JSON and as numbers in some
/// implementations, so `"7"` equals `7`; integers compare exactly, others as f64.
fn scalars_equal(expected: &Value, actual: &Value) -> bool {
    if expected == actual {
        return true;
    }
    let (Some(left), Some(right)) = (number_text(expected), number_text(actual)) else {
        return false;
    };
    match (left.parse::<i128>(), right.parse::<i128>()) {
        (Ok(left), Ok(right)) => left == right,
        _ => matches!((left.parse::<f64>(), right.parse::<f64>()), (Ok(left), Ok(right)) if left == right),
    }
}

fn number_text(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}
