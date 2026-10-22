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
    if let Some(obj) = coerced.as_object() {
        // No `properties` map and `additionalProperties: false` means every
        // key is forbidden. An empty map is that case, not "skip the object".
        let props = object_properties(schema);
        if props.is_some() || denies_unknown(schema) {
            let empty = Map::new();
            let props = props.unwrap_or(&empty);
            let (next, more) = normalize_object(obj, schema, props)?;
            rewrites.extend(more);
            return Ok((next, rewrites));
        }
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

fn schema_type_names(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn coerce(value: &Value, schema: &Value, rewrites: &mut Vec<Rewrite>) -> Value {
    let Some(text) = value.as_str() else {
        return value.clone();
    };
    let text = text.trim_matches(char::is_whitespace);
    if text.is_empty() {
        return value.clone();
    }
    let types = schema_type_names(schema);
    // A union that includes string can keep the text. Coercing it would guess.
    if types.is_empty() || types.contains(&"string") {
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
