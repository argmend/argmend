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
    let (next, rewrites) = normalize_known(value, schema)?;
    if next != *value || !rewrites.is_empty() {
        if violates_not(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value is excluded by not".into(),
            ));
        }
        if violates_bounds(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value is outside the schema bounds".into(),
            ));
        }
    }
    Ok((next, rewrites))
}

fn normalize_known(value: &Value, schema: &Value) -> Result<(Value, Vec<Rewrite>), RepairError> {
    if let Some((next, rewrites)) = if_then_else(value, schema) {
        return Ok((next, rewrites));
    }
    if let Some((next, rewrites)) = all_of(value, schema) {
        return Ok((next, rewrites));
    }
    if let Some((next, rewrites)) = exclusive_branch(value, schema) {
        return Ok((next, rewrites));
    }
    let mut rewrites = Vec::new();
    let coerced = coerce(value, schema, &mut rewrites);
    if let Some(obj) = coerced.as_object() {
        // No `properties` map and `additionalProperties: false` means every
        // key is forbidden. An empty map is that case, not "skip the object".
        let props = object_properties(schema);
        let mut next = coerced.clone();
        let mut saw_object_schema = false;
        if props.is_some()
            || denies_unknown(schema)
            || additional_properties_schema(schema).is_some()
        {
            let empty = Map::new();
            let props = props.unwrap_or(&empty);
            let (normalized, more) = normalize_object(obj, schema, props)?;
            rewrites.extend(more);
            next = normalized;
            saw_object_schema = true;
        }
        if let Some((updated, extra)) = next
            .as_object()
            .and_then(|map| apply_dependent_schemas(map, schema))
        {
            rewrites.extend(extra);
            return Ok((updated, rewrites));
        }
        if saw_object_schema {
            return Ok((next, rewrites));
        }
    }
    if describes_array(schema)
        && let Some(arr) = coerced.as_array()
    {
        let (next, more) = normalize_array(arr, schema)?;
        rewrites.extend(more);
        return Ok((next, rewrites));
    }
    Ok((coerced, rewrites))
}

#[derive(Clone, Copy)]
enum ArrayTail<'a> {
    Keep,
    Forbid,
    Schema(&'a Value),
}

/// A present property applies its `dependentSchemas` or schema-form
/// `dependencies` entry. An array of required names is not invented.
fn apply_dependent_schemas(
    obj: &Map<String, Value>,
    schema: &Value,
) -> Option<(Value, Vec<Rewrite>)> {
    let active = dependent_object_schemas(schema)
        .into_iter()
        .filter(|(name, _)| obj.contains_key(*name))
        .map(|(_, sub)| sub)
        .collect::<Vec<_>>();
    if active.is_empty() {
        return None;
    }
    let mut current = Value::Object(obj.clone());
    let mut rewrites = Vec::new();
    for branch in &active {
        let (next, more) = normalize_child(&current, branch).ok()?;
        if !branch_accepts(branch, &next) {
            return None;
        }
        current = next;
        rewrites.extend(more);
    }
    for branch in &active {
        let (next, _) = normalize_child(&current, branch).ok()?;
        if next != current || !branch_accepts(branch, &next) {
            return None;
        }
    }
    if current == Value::Object(obj.clone()) && rewrites.is_empty() {
        return None;
    }
    Some((current, rewrites))
}

fn dependent_object_schemas(schema: &Value) -> Vec<(&str, &Value)> {
    let schema = if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    };
    let mut out = Vec::new();
    for keyword in ["dependentSchemas", "dependencies"] {
        let Some(map) = schema.get(keyword).and_then(Value::as_object) else {
            continue;
        };
        for (name, sub) in map {
            if sub.is_object() {
                out.push((name.as_str(), sub));
            }
        }
    }
    out
}

/// When `if` matches, apply `then`. Otherwise apply `else`.
/// The `then` result must still match `if`.
/// Property schemas and `required` names are part of that match.
fn if_then_else(value: &Value, schema: &Value) -> Option<(Value, Vec<Rewrite>)> {
    if schema.get("allOf").is_some()
        || schema.get("oneOf").is_some()
        || schema.get("anyOf").is_some()
    {
        return None;
    }
    let if_schema = schema.get("if")?;
    let then_schema = schema.get("then");
    let else_schema = schema.get("else");
    if then_schema.is_none() && else_schema.is_none() {
        return None;
    }
    let (probed, probe_rewrites) = normalize_child(value, if_schema).ok()?;
    if branch_accepts(if_schema, &probed) {
        let Some(then_schema) = then_schema else {
            if probed == *value && probe_rewrites.is_empty() {
                return None;
            }
            return Some((probed, probe_rewrites));
        };
        let (next, more) = normalize_child(&probed, then_schema).ok()?;
        if !branch_accepts(if_schema, &next) || !branch_accepts(then_schema, &next) {
            return None;
        }
        let mut rewrites = probe_rewrites;
        rewrites.extend(more);
        if next == *value && rewrites.is_empty() {
            return None;
        }
        return Some((next, rewrites));
    }
    let else_schema = else_schema?;
    let (next, rewrites) = normalize_child(value, else_schema).ok()?;
    if !branch_accepts(else_schema, &next) {
        return None;
    }
    if next == *value && rewrites.is_empty() {
        return None;
    }
    Some((next, rewrites))
}

/// Apply every `allOf` branch. The result must still satisfy each branch.
fn all_of(value: &Value, schema: &Value) -> Option<(Value, Vec<Rewrite>)> {
    let branches = single_applicator(schema, "allOf")?;
    if branches.is_empty() {
        return None;
    }
    let mut current = value.clone();
    let mut rewrites = Vec::new();
    for branch in branches {
        let (next, more) = normalize_child(&current, branch).ok()?;
        if !branch_accepts(branch, &next) {
            return None;
        }
        current = next;
        rewrites.extend(more);
    }
    for branch in branches {
        let (next, _) = normalize_child(&current, branch).ok()?;
        if next != current || !branch_accepts(branch, &next) {
            return None;
        }
    }
    if current == *value && rewrites.is_empty() {
        return None;
    }
    Some((current, rewrites))
}

fn single_applicator<'a>(schema: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    let present = ["allOf", "oneOf", "anyOf"]
        .into_iter()
        .filter(|name| schema.get(*name).is_some())
        .count();
    if present != 1 {
        return None;
    }
    schema.get(key).and_then(Value::as_array)
}

/// `oneOf` and `anyOf` coerce only when one branch accepts the value.
/// Two accepting branches are left unchanged.
fn exclusive_branch(value: &Value, schema: &Value) -> Option<(Value, Vec<Rewrite>)> {
    let branches =
        single_applicator(schema, "oneOf").or_else(|| single_applicator(schema, "anyOf"))?;
    let mut matched = Vec::new();
    for branch in branches {
        let (next, rewrites) = normalize_child(value, branch).ok()?;
        if branch_accepts(branch, &next) && branch_accepts(schema, &next) {
            matched.push((next, rewrites));
        }
    }
    if matched.len() == 1 {
        matched.pop()
    } else {
        None
    }
}

/// A rewrite that satisfies `not` is refused.
/// `not` with only an unrecognized keyword is not a match.
fn violates_not(schema: &Value, value: &Value) -> bool {
    let Some(not_schema) = schema.get("not") else {
        return false;
    };
    known_constraint(not_schema) && branch_accepts(not_schema, value)
}

/// Inclusive `minimum` / `maximum`, numeric exclusive bounds, and the
/// draft-04 boolean form of `exclusiveMinimum` / `exclusiveMaximum`.
/// A bound that is not an exact finite number is ignored.
fn violates_bounds(schema: &Value, value: &Value) -> bool {
    let Some(number) = finite_bound(value) else {
        return false;
    };
    below_minimum(schema, number) || above_maximum(schema, number)
}

fn below_minimum(schema: &Value, number: f64) -> bool {
    if let Some(limit) = schema.get("exclusiveMinimum").and_then(finite_bound)
        && number <= limit
    {
        return true;
    }
    let Some(limit) = schema.get("minimum").and_then(finite_bound) else {
        return false;
    };
    if matches!(schema.get("exclusiveMinimum"), Some(Value::Bool(true))) {
        number <= limit
    } else {
        number < limit
    }
}

fn above_maximum(schema: &Value, number: f64) -> bool {
    if let Some(limit) = schema.get("exclusiveMaximum").and_then(finite_bound)
        && number >= limit
    {
        return true;
    }
    let Some(limit) = schema.get("maximum").and_then(finite_bound) else {
        return false;
    };
    if matches!(schema.get("exclusiveMaximum"), Some(Value::Bool(true))) {
        number >= limit
    } else {
        number > limit
    }
}

fn finite_bound(value: &Value) -> Option<f64> {
    let number = value.as_f64()?;
    if !number.is_finite() || !exact_json_number(value) {
        return None;
    }
    Some(number)
}

fn exact_json_number(value: &Value) -> bool {
    let Some(number) = value.as_f64() else {
        return false;
    };
    if let Some(int) = value.as_i64() {
        return int as f64 == number && number as i64 == int;
    }
    if let Some(int) = value.as_u64() {
        return int as f64 == number && number as u64 == int;
    }
    true
}

fn known_constraint(schema: &Value) -> bool {
    schema.get("type").is_some()
        || schema.get("const").is_some()
        || schema.get("enum").is_some()
        || schema.get("properties").is_some()
        || schema.get("required").is_some()
        || schema.get("items").is_some()
        || schema.get("prefixItems").is_some()
}

/// Type, `const`, `enum`, properties, array items, and `required` names.
fn branch_accepts(schema: &Value, value: &Value) -> bool {
    constraints_hold(schema, value)
        && properties_match(schema, value)
        && elements_match(schema, value)
        && required_holds(schema, value)
}

fn elements_match(schema: &Value, value: &Value) -> bool {
    let Some(arr) = value.as_array() else {
        return true;
    };
    if let Some((positions, tail)) = tuple_positions(schema) {
        if matches!(tail, ArrayTail::Forbid) && arr.len() > positions.len() {
            return false;
        }
        return arr
            .iter()
            .enumerate()
            .all(|(index, item)| match positions.get(index) {
                Some(position) => branch_accepts(position, item),
                None => match tail {
                    ArrayTail::Schema(rest) => branch_accepts(rest, item),
                    ArrayTail::Keep | ArrayTail::Forbid => true,
                },
            });
    }
    let Some(items) = schema.get("items").filter(|items| items.is_object()) else {
        return true;
    };
    arr.iter().all(|item| branch_accepts(items, item))
}

fn properties_match(schema: &Value, value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return true;
    };
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return true;
    };
    props.iter().all(|(key, prop_schema)| {
        obj.get(key)
            .is_none_or(|child| branch_accepts(prop_schema, child))
    })
}

fn constraints_hold(schema: &Value, value: &Value) -> bool {
    let types = schema_type_names(schema);
    if !types.is_empty() && !type_matches(&types, value) {
        return false;
    }
    if let Some(const_value) = schema.get("const")
        && const_value != value
        && !values_equivalent(const_value, value)
    {
        return false;
    }
    if let Some(items) = schema.get("enum").and_then(Value::as_array)
        && !items
            .iter()
            .any(|item| item == value || values_equivalent(item, value))
    {
        return false;
    }
    true
}

fn type_matches(types: &[&str], value: &Value) -> bool {
    types.iter().any(|kind| match *kind {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.as_number().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => false,
    })
}

fn tuple_positions(schema: &Value) -> Option<(&Vec<Value>, ArrayTail<'_>)> {
    if let Some(Value::Array(prefix)) = schema.get("prefixItems") {
        let tail = match schema.get("items") {
            Some(Value::Bool(false)) => ArrayTail::Forbid,
            Some(rest) if rest.is_object() => ArrayTail::Schema(rest),
            _ => ArrayTail::Keep,
        };
        return Some((prefix, tail));
    }
    if let Some(Value::Array(items)) = schema.get("items") {
        let tail = match schema.get("additionalItems") {
            Some(Value::Bool(false)) => ArrayTail::Forbid,
            Some(rest) if rest.is_object() => ArrayTail::Schema(rest),
            _ => ArrayTail::Keep,
        };
        return Some((items, tail));
    }
    None
}

fn normalize_array(arr: &[Value], schema: &Value) -> Result<(Value, Vec<Rewrite>), RepairError> {
    if let Some((positions, tail)) = tuple_positions(schema) {
        if matches!(tail, ArrayTail::Forbid) && arr.len() > positions.len() {
            return Err(RepairError::Ambiguous(
                "array item is past the tuple schema".into(),
            ));
        }
        let mut out = Vec::with_capacity(arr.len());
        let mut rewrites = Vec::new();
        for (index, item) in arr.iter().enumerate() {
            if let Some(position) = positions.get(index) {
                let (next, more) = normalize_child(item, position)?;
                rewrites.extend(more);
                out.push(next);
            } else if let ArrayTail::Schema(rest) = tail {
                let (next, more) = normalize_child(item, rest)?;
                rewrites.extend(more);
                out.push(next);
            } else {
                out.push(item.clone());
            }
        }
        return Ok((Value::Array(out), rewrites));
    }
    let Some(items) = schema.get("items").filter(|items| items.is_object()) else {
        return Ok((Value::Array(arr.to_vec()), Vec::new()));
    };
    let mut out = Vec::with_capacity(arr.len());
    let mut rewrites = Vec::new();
    for item in arr {
        let (next, more) = normalize_child(item, items)?;
        rewrites.extend(more);
        out.push(next);
    }
    Ok((Value::Array(out), rewrites))
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
        if !unknown.is_empty() && has_pattern_properties(schema) {
            let key = &unknown[0].0;
            return Err(RepairError::Ambiguous(format!(
                "unknown key '{key}' may be allowed by patternProperties"
            )));
        }
        if unknown.len() > 1 {
            return Err(RepairError::Ambiguous(format!(
                "{} unknown keys and additionalProperties is false",
                unknown.len()
            )));
        }
        if let Some((key, _)) = unknown.pop() {
            let ready = required_present(schema, &out);
            if ready && !out.is_empty() {
                rewrites.push(rewrite(
                    RewriteKind::DroppedUnknown,
                    format!("dropped unknown key {key}"),
                ));
            } else if ready {
                return Err(RepairError::Ambiguous(format!(
                    "refusing to drop the only key '{key}'"
                )));
            } else {
                return Err(RepairError::Ambiguous(format!(
                    "unknown key '{key}' and a required field is missing"
                )));
            }
        }
    } else if let Some(extra) = additional_properties_schema(schema) {
        if !unknown.is_empty() && has_pattern_properties(schema) {
            let key = &unknown[0].0;
            return Err(RepairError::Ambiguous(format!(
                "unknown key '{key}' may be allowed by patternProperties"
            )));
        }
        for (key, val) in unknown {
            let (next, more) = normalize_child(&val, extra)?;
            rewrites.extend(more);
            out.insert(key, next);
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

fn has_pattern_properties(schema: &Value) -> bool {
    fn nonempty(value: &Value) -> bool {
        value
            .get("patternProperties")
            .and_then(Value::as_object)
            .is_some_and(|patterns| !patterns.is_empty())
    }
    nonempty(schema) || schema.get("parameters").is_some_and(nonempty)
}

fn additional_properties_schema(schema: &Value) -> Option<&Value> {
    let schema = if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    };
    schema
        .get("additionalProperties")
        .filter(|value| value.is_object())
}

fn denies_unknown(schema: &Value) -> bool {
    let schema = if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    };
    matches!(schema.get("additionalProperties"), Some(Value::Bool(false)))
}

fn required_holds(schema: &Value, value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        let schema = if schema.get("properties").is_none() {
            schema.get("parameters").unwrap_or(schema)
        } else {
            schema
        };
        return schema
            .get("required")
            .and_then(Value::as_array)
            .is_none_or(|items| items.iter().all(|item| item.as_str().is_none()));
    };
    required_present(schema, obj)
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

/// An array value uses `items` or `prefixItems` even when `type` is omitted.
/// An explicit non-array type does not.
fn describes_array(schema: &Value) -> bool {
    let types = schema_type_names(schema);
    if types.contains(&"array") {
        return true;
    }
    if !types.is_empty() {
        return false;
    }
    schema.get("items").is_some() || schema.get("prefixItems").is_some()
}

fn schema_type_names(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn coerce(value: &Value, schema: &Value, rewrites: &mut Vec<Rewrite>) -> Value {
    if let Some(number) = integer_from_exact_float(value, schema) {
        rewrites.push(rewrite(
            RewriteKind::NumberString,
            format!("integer number {number}"),
        ));
        return Value::Number(number);
    }
    if let Some(matched) = float_const_or_enum(value, schema) {
        let word = if schema.get("const").is_some() {
            "const"
        } else {
            "enum"
        };
        let shown = value.as_f64().unwrap_or(0.0);
        rewrites.push(rewrite(
            RewriteKind::NumberString,
            format!("{word} number {shown:?}"),
        ));
        return matched;
    }
    let Some(text) = value.as_str() else {
        return value.clone();
    };
    let text = text.trim_matches(char::is_whitespace);
    if text.is_empty() {
        return value.clone();
    }
    let types = schema_type_names(schema);
    // A union that includes string can keep the text. Coercing it would guess.
    if types.contains(&"string") {
        return value.clone();
    }
    if types.is_empty() {
        if let Some(matched) = const_or_enum_value(value, text, schema) {
            record_constrained(text, &matched, schema, rewrites);
            return matched;
        }
        return value.clone();
    }
    if types.contains(&"null") && text.eq_ignore_ascii_case("null") {
        rewrites.push(rewrite(
            RewriteKind::NullString,
            format!("null string {text}"),
        ));
        return Value::Null;
    }
    if types.contains(&"boolean") {
        if text.eq_ignore_ascii_case("true") {
            rewrites.push(rewrite(
                RewriteKind::BoolString,
                "boolean string true".into(),
            ));
            return Value::Bool(true);
        }
        if text.eq_ignore_ascii_case("false") {
            rewrites.push(rewrite(
                RewriteKind::BoolString,
                "boolean string false".into(),
            ));
            return Value::Bool(false);
        }
    }
    if types.contains(&"integer")
        && let Some(number) = integer_string(text).or_else(|| integer_from_zero_fraction(text))
    {
        rewrites.push(rewrite(
            RewriteKind::NumberString,
            format!("integer string {text}"),
        ));
        return Value::Number(number);
    }
    if types.contains(&"number")
        && let Some(number) = number_string(text)
    {
        rewrites.push(rewrite(
            RewriteKind::NumberString,
            format!("number string {text}"),
        ));
        return Value::Number(number);
    }
    value.clone()
}

/// `1.0` is an integer. `1.5`, a value past 2^53, and a schema that also
/// allows `number` are left unchanged.
fn integer_from_exact_float(value: &Value, schema: &Value) -> Option<serde_json::Number> {
    let types = schema_type_names(schema);
    if types.is_empty()
        || types.contains(&"number")
        || types.contains(&"string")
        || !types.contains(&"integer")
    {
        return None;
    }
    if value.as_i64().is_some() || value.as_u64().is_some() {
        return None;
    }
    let number = value.as_f64()?;
    if !number.is_finite() || number.fract() != 0.0 || number.abs() > (1u64 << 53) as f64 {
        return None;
    }
    let as_int = number as i64;
    if as_int as f64 != number {
        return None;
    }
    Some(serde_json::Number::from(as_int))
}

fn float_const_or_enum(value: &Value, schema: &Value) -> Option<Value> {
    if value.as_i64().is_some() || value.as_u64().is_some() {
        return None;
    }
    let number = value.as_f64()?;
    if !number.is_finite() || number.fract() != 0.0 || number.abs() > (1u64 << 53) as f64 {
        return None;
    }
    let targets = constrained_values(schema)?;
    if targets.contains(&value) {
        return None;
    }
    let as_int = serde_json::Number::from(number as i64);
    let mut chosen: Option<&Value> = None;
    for target in &targets {
        let Some(target_number) = target.as_number() else {
            continue;
        };
        if !numbers_equal(&as_int, target_number) {
            continue;
        }
        match chosen {
            Some(existing) if values_equivalent(existing, target) => {
                if target.as_i64().is_some() || target.as_u64().is_some() {
                    chosen = Some(target);
                }
            }
            Some(_) => return None,
            None => chosen = Some(target),
        }
    }
    chosen.cloned()
}

fn const_or_enum_value(original: &Value, text: &str, schema: &Value) -> Option<Value> {
    let targets = constrained_values(schema)?;
    if targets.contains(&original) {
        return None;
    }
    let mut chosen: Option<&Value> = None;
    for target in &targets {
        if !string_matches_target(text, target) {
            continue;
        }
        match chosen {
            Some(existing) if values_equivalent(existing, target) => {
                if target.as_i64().is_some() || target.as_u64().is_some() {
                    chosen = Some(target);
                }
            }
            Some(_) => return None,
            None => chosen = Some(target),
        }
    }
    chosen.cloned()
}

fn constrained_values(schema: &Value) -> Option<Vec<&Value>> {
    let const_value = schema.get("const");
    let enum_values = schema.get("enum").and_then(Value::as_array);
    match (const_value, enum_values) {
        (None, None) => None,
        (Some(value), None) => Some(vec![value]),
        (None, Some(items)) => Some(items.iter().collect()),
        (Some(value), Some(items)) => Some(items.iter().filter(|item| *item == value).collect()),
    }
}

fn string_matches_target(text: &str, target: &Value) -> bool {
    match target {
        Value::Bool(flag) => text.eq_ignore_ascii_case(if *flag { "true" } else { "false" }),
        Value::Null => text.eq_ignore_ascii_case("null"),
        Value::Number(number) => number_matches(text, number),
        _ => false,
    }
}

fn number_matches(text: &str, target: &serde_json::Number) -> bool {
    if let Some(number) = integer_string(text).or_else(|| integer_from_zero_fraction(text))
        && numbers_equal(&number, target)
    {
        return true;
    }
    number_string(text).is_some_and(|number| numbers_equal(&number, target))
}

fn numbers_equal(left: &serde_json::Number, right: &serde_json::Number) -> bool {
    if left == right {
        return true;
    }
    match (left.as_f64(), right.as_f64()) {
        (Some(lhs), Some(rhs)) => {
            let limit = (1u64 << 53) as f64;
            lhs == rhs && lhs.abs() <= limit && rhs.abs() <= limit
        }
        _ => false,
    }
}

fn values_equivalent(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    match (left.as_number(), right.as_number()) {
        (Some(lhs), Some(rhs)) => numbers_equal(lhs, rhs),
        _ => false,
    }
}

fn record_constrained(text: &str, matched: &Value, schema: &Value, rewrites: &mut Vec<Rewrite>) {
    let word = if schema.get("const").is_some() {
        "const"
    } else {
        "enum"
    };
    match matched {
        Value::Bool(true) => rewrites.push(rewrite(
            RewriteKind::BoolString,
            format!("{word} boolean {text}"),
        )),
        Value::Bool(false) => rewrites.push(rewrite(
            RewriteKind::BoolString,
            format!("{word} boolean {text}"),
        )),
        Value::Null => rewrites.push(rewrite(
            RewriteKind::NullString,
            format!("{word} null {text}"),
        )),
        Value::Number(_) => rewrites.push(rewrite(
            RewriteKind::NumberString,
            format!("{word} number {text}"),
        )),
        _ => {}
    }
}

fn integer_string(text: &str) -> Option<serde_json::Number> {
    let text = without_digit_underscores(text)?;
    let text = text.strip_prefix('+').unwrap_or(&text);
    if text.is_empty() || !is_integer_token(text) || has_leading_zero(text) {
        return None;
    }
    let parsed = text.parse::<i64>().ok()?;
    Some(serde_json::Number::from(parsed))
}

/// `"3.0"`, `"-2.000"`, and `"1."` are integers. `"3.10"` and `"03.0"` are not.
fn integer_from_zero_fraction(text: &str) -> Option<serde_json::Number> {
    let text = without_digit_underscores(text)?;
    let text = text.strip_prefix('+').unwrap_or(&text);
    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, frac) = rest.split_once('.')?;
    if whole.is_empty()
        || !frac.bytes().all(|byte| byte == b'0')
        || !is_integer_token(whole)
        || has_leading_zero(whole)
    {
        return None;
    }
    let mut digits = String::with_capacity(whole.len() + 1);
    if negative && whole.bytes().any(|byte| byte != b'0') {
        digits.push('-');
    }
    digits.push_str(whole);
    let parsed = digits.parse::<i64>().ok()?;
    Some(serde_json::Number::from(parsed))
}

fn number_string(text: &str) -> Option<serde_json::Number> {
    let text = without_digit_underscores(text)?;
    let text = text.strip_prefix('+').unwrap_or(&text);
    if let Some(number) = integer_string(text) {
        return Some(number);
    }
    if let Some(number) = integer_from_zero_fraction(text) {
        return Some(number);
    }
    if (!is_decimal_token(text) && !is_scientific_token(text)) || has_leading_zero(text) {
        return None;
    }
    let parsed = text.parse::<f64>().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    let number = serde_json::Number::from_f64(parsed)?;
    // f64 turns 9007199254740993.5 into 9007199254740994.0. Keep the
    // original string unless the JSON number is the same value.
    if !same_decimal(text, &number.to_string()) {
        return None;
    }
    Some(number)
}

/// `coef * 10^exp10`, with factors of ten folded into `exp10`.
#[derive(PartialEq, Eq)]
struct Decimal {
    neg: bool,
    coef: u128,
    exp10: i32,
}

fn same_decimal(left: &str, right: &str) -> bool {
    match (parse_decimal(left), parse_decimal(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn parse_decimal(text: &str) -> Option<Decimal> {
    let neg = text.starts_with('-');
    let text = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exp_text) = text
        .split_once(['e', 'E'])
        .map_or((text, None), |(mantissa, exp)| (mantissa, Some(exp)));
    let exp: i32 = match exp_text {
        Some(exp) => exp.parse().ok()?,
        None => 0,
    };
    let (whole, frac) = mantissa
        .split_once('.')
        .map_or((mantissa, ""), |(whole, frac)| (whole, frac));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let frac = frac.trim_end_matches('0');
    let mut digits = String::new();
    digits.push_str(whole.trim_start_matches('0'));
    digits.push_str(frac);
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(Decimal {
            neg: false,
            coef: 0,
            exp10: 0,
        });
    }
    if digits.len() > 38 {
        return None;
    }
    let mut coef: u128 = digits.parse().ok()?;
    let mut exp10 = exp.checked_sub(i32::try_from(frac.len()).ok()?)?;
    while coef.is_multiple_of(10) {
        coef /= 10;
        exp10 = exp10.checked_add(1)?;
    }
    Some(Decimal {
        neg: neg && coef != 0,
        coef,
        exp10,
    })
}

fn is_scientific_token(text: &str) -> bool {
    let Some((mantissa, exp)) = text.split_once(['e', 'E']) else {
        return false;
    };
    let exp = exp.strip_prefix('+').unwrap_or(exp);
    let exp_digits = exp.strip_prefix('-').unwrap_or(exp);
    if exp_digits.is_empty() || !exp_digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    is_integer_token(mantissa) || is_decimal_token(mantissa)
}

fn without_digit_underscores(text: &str) -> Option<String> {
    if !text.contains('_') {
        return Some(text.to_string());
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch == '_' {
            let before_digit = index > 0 && chars[index - 1].is_ascii_digit();
            let after_digit = chars.get(index + 1).is_some_and(char::is_ascii_digit);
            if before_digit && after_digit {
                continue;
            }
            return None;
        }
        out.push(ch);
    }
    Some(out)
}

fn has_leading_zero(text: &str) -> bool {
    let mantissa = text.split(['e', 'E']).next().unwrap_or(text);
    let digits = mantissa.strip_prefix('-').unwrap_or(mantissa);
    let whole = digits.split('.').next().unwrap_or(digits);
    whole.len() > 1 && whole.starts_with('0')
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
