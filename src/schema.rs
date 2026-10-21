//! Schema salvage for an already-parsed argument value.
//!
//! Valid JSON is not rewritten unless the schema asks for a rename, a
//! string coercion, or the drop of one unknown key.

use serde_json::{Map, Value};

use crate::{RepairError, Repaired, Rewrite, RewriteKind};

const PATH_KEYS: &[&str] = &["path", "file", "file_path", "filepath", "path_name"];

pub fn normalize(value: &Value, schema: &Value) -> Result<Repaired, RepairError> {
    let (next, rewrites) = normalize_child(value, schema)?;
    Ok(Repaired {
        value: next,
        rewrites,
    })
}

fn normalize_child(value: &Value, schema: &Value) -> Result<(Value, Vec<Rewrite>), RepairError> {
    let mut rewrites = Vec::new();
    let coerced = coerce(value, schema, &mut rewrites);
    if let Some(props) = object_properties(schema)
        && let Some(obj) = coerced.as_object()
    {
        let (next, more) = normalize_object(obj, schema, props)?;
        rewrites.extend(more);
        return Ok((next, rewrites));
    }
    if schema.get("type").and_then(Value::as_str) == Some("array")
        && let Some(items) = schema.get("items")
        && let Some(arr) = coerced.as_array()
    {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let (next, more) = normalize_child(item, items)?;
            rewrites.extend(more);
            out.push(next);
        }
        return Ok((Value::Array(out), rewrites));
    }
    Ok((coerced, rewrites))
}

fn normalize_object(
    obj: &Map<String, Value>,
    schema: &Value,
    props: &Map<String, Value>,
) -> Result<(Value, Vec<Rewrite>), RepairError> {
    let mut out = Map::new();
    let mut rewrites = Vec::new();
    let mut unknown = Vec::new();

    for (key, val) in obj {
        if let Some(prop_schema) = props.get(key) {
            let (next, more) = normalize_child(val, prop_schema)?;
            rewrites.extend(more);
            out.insert(key.clone(), next);
            continue;
        }
        match alias_target(key, props)? {
            Some(target) => {
                let existing = out.get(target).or_else(|| obj.get(target));
                if let Some(existing) = existing {
                    if existing != val {
                        return Err(RepairError::Ambiguous(format!(
                            "'{key}' and '{target}' are both set and disagree"
                        )));
                    }
                    rewrites.push(rewrite(
                        RewriteKind::Alias,
                        format!("dropped duplicate alias {key}"),
                    ));
                    continue;
                }
                let Some(prop_schema) = props.get(target) else {
                    unknown.push((key.clone(), val.clone()));
                    continue;
                };
                let (next, more) = normalize_child(val, prop_schema)?;
                rewrites.extend(more);
                rewrites.push(rewrite(
                    RewriteKind::Alias,
                    format!("renamed {key} to {target}"),
                ));
                out.insert(target.to_string(), next);
            }
            None => unknown.push((key.clone(), val.clone())),
        }
    }

    if denies_unknown(schema) {
        if unknown.len() > 1 {
            return Err(RepairError::Ambiguous(format!(
                "{} unknown keys and additionalProperties is false",
                unknown.len()
            )));
        }
        if let Some((key, val)) = unknown.pop() {
            if required_present(schema, &out) {
                rewrites.push(rewrite(
                    RewriteKind::DroppedUnknown,
                    format!("dropped unknown key {key}"),
                ));
            } else {
                out.insert(key, val);
            }
        }
    } else {
        for (key, val) in unknown {
            out.insert(key, val);
        }
    }

    Ok((Value::Object(out), rewrites))
}

fn alias_target<'a>(
    key: &str,
    props: &'a Map<String, Value>,
) -> Result<Option<&'a str>, RepairError> {
    if !PATH_KEYS.contains(&key) {
        return Ok(None);
    }
    let mut targets = Vec::new();
    for name in props.keys() {
        if PATH_KEYS.contains(&name.as_str()) {
            targets.push(name.as_str());
        }
    }
    match targets.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        _ => Err(RepairError::Ambiguous(format!(
            "'{key}' matches more than one path property"
        ))),
    }
}

fn object_properties(schema: &Value) -> Option<&Map<String, Value>> {
    let schema = if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    };
    schema.get("properties")?.as_object()
}

fn denies_unknown(schema: &Value) -> bool {
    let schema = if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    };
    matches!(schema.get("additionalProperties"), Some(Value::Bool(false)))
}

fn required_present(schema: &Value, out: &Map<String, Value>) -> bool {
    let schema = if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    };
    let Some(required) = schema.get("required").and_then(Value::as_array) else {
        return true;
    };
    required
        .iter()
        .filter_map(Value::as_str)
        .all(|name| out.contains_key(name))
}

fn coerce(value: &Value, schema: &Value, rewrites: &mut Vec<Rewrite>) -> Value {
    let Some(text) = value.as_str() else {
        return value.clone();
    };
    let Some(ty) = schema.get("type").and_then(Value::as_str) else {
        return value.clone();
    };
    match ty {
        "boolean" => {
            if text.eq_ignore_ascii_case("true") {
                rewrites.push(rewrite(
                    RewriteKind::BoolString,
                    "boolean string true".into(),
                ));
                Value::Bool(true)
            } else if text.eq_ignore_ascii_case("false") {
                rewrites.push(rewrite(
                    RewriteKind::BoolString,
                    "boolean string false".into(),
                ));
                Value::Bool(false)
            } else {
                value.clone()
            }
        }
        "integer" => integer_string(text).map_or_else(
            || value.clone(),
            |number| {
                rewrites.push(rewrite(
                    RewriteKind::NumberString,
                    format!("integer string {text}"),
                ));
                Value::Number(number)
            },
        ),
        "number" => number_string(text).map_or_else(
            || value.clone(),
            |number| {
                rewrites.push(rewrite(
                    RewriteKind::NumberString,
                    format!("number string {text}"),
                ));
                Value::Number(number)
            },
        ),
        _ => value.clone(),
    }
}

fn integer_string(text: &str) -> Option<serde_json::Number> {
    if text.is_empty() || !is_integer_token(text) {
        return None;
    }
    let parsed = text.parse::<i64>().ok()?;
    Some(serde_json::Number::from(parsed))
}

fn number_string(text: &str) -> Option<serde_json::Number> {
    if let Some(number) = integer_string(text) {
        return Some(number);
    }
    if !is_decimal_token(text) {
        return None;
    }
    let parsed = text.parse::<f64>().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    serde_json::Number::from_f64(parsed)
}

fn is_integer_token(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn is_decimal_token(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let mut parts = digits.split('.');
    let whole = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    parts.next().is_none()
        && !whole.is_empty()
        && !frac.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

fn rewrite(kind: RewriteKind, detail: String) -> Rewrite {
    Rewrite { kind, detail }
}
