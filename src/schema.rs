//! Schema salvage for an already-parsed argument value.
//!
//! Valid JSON is not rewritten unless the schema asks for a rename, a
//! string coercion, or the drop of one unknown key. [`normalize`] does
//! not accept or reject the original value. See that function for the
//! keywords this crate leaves unused.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::{RepairError, Repaired, Rewrite, RewriteKind};

const PATH_KEYS: &[&str] = &["path", "file", "file_path", "filepath", "path_name"];

/// Apply a tool schema to an already-parsed value.
///
/// `Ok` means this crate produced a value. It does not mean the schema
/// accepts that value. An unchanged value can still miss `required`, fail
/// `pattern` or `format`, or depend on an unresolved `$ref`.
///
/// A JSON boolean schema is unused. `true` and `false` both return the
/// value with no rewrites. A string whose text is JSON stays a string when
/// the schema type is an object or an array. `$ref` and `$dynamicRef` are
/// not resolved. They do not veto a sibling `type`, and they do not hide a
/// sibling bound such as `maximum`.
///
/// `unevaluatedProperties: false` keeps a property that `allOf`, `anyOf`,
/// `oneOf`, `if` / `then` / `else`, or a dependent schema evaluated.
/// `additionalProperties: false` does not look inside those branches. A key
/// one of them names is ambiguous instead of dropped. A path alias does not
/// rename a key one of those branches lists under `properties`. The same
/// refusal applies when the object is open. An unresolved `$ref` or
/// `$dynamicRef` on this object, or on one of those branches, blocks
/// dropping a key. The ref is not resolved.
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
        if let Some(reason) = applicator_field_failure(schema, value, &next) {
            return Err(RepairError::Ambiguous(reason));
        }
        if let Some(reason) = applicator_selection_failure(schema, &next) {
            return Err(RepairError::Ambiguous(reason));
        }
        if value.as_str().is_some() && violates_length(schema, value) {
            return Err(RepairError::Ambiguous(
                "coerced string is outside the schema length".into(),
            ));
        }
        if value.as_str().is_some() && unchecked_string_keyword(schema) {
            return Err(RepairError::Ambiguous(
                "coerced string is outside an unchecked string keyword".into(),
            ));
        }
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
        if violates_multiple(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value is not a multiple of the schema".into(),
            ));
        }
        if violates_unique(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value repeats an array item".into(),
            ));
        }
        if violates_contains(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value misses contains".into(),
            ));
        }
        if outside_const_or_enum(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value is not a const or enum member".into(),
            ));
        }
        if violates_size(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value is outside the schema size".into(),
            ));
        }
        if violates_dependent(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value misses a dependent property".into(),
            ));
        }
        if violates_length(schema, &next) {
            return Err(RepairError::Ambiguous(
                "coerced value is outside the schema length".into(),
            ));
        }
        if let Some(reason) = property_names_refusal(schema, &next) {
            return Err(RepairError::Ambiguous(reason));
        }
    }
    Ok((next, rewrites))
}

fn normalize_known(value: &Value, schema: &Value) -> Result<(Value, Vec<Rewrite>), RepairError> {
    if let Some((next, rewrites)) = if_then_else(value, schema)? {
        return enforce_outer_object(value, next, schema, rewrites);
    }
    if let Some((next, rewrites)) = all_of(value, schema)? {
        return enforce_outer_object(value, next, schema, rewrites);
    }
    if let Some((next, rewrites)) = exclusive_branch(value, schema)? {
        return enforce_outer_object(value, next, schema, rewrites);
    }
    if let Some((next, rewrites)) = deep_constrained(value, schema) {
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
        if let Some(map) = next.as_object()
            && let Some((updated, extra)) = apply_dependent_schemas(map, schema)?
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
/// A [`RepairError`] from a branch is not discarded.
fn apply_dependent_schemas(
    obj: &Map<String, Value>,
    schema: &Value,
) -> Result<Option<(Value, Vec<Rewrite>)>, RepairError> {
    let active = dependent_object_schemas(schema)
        .into_iter()
        .filter(|(name, _)| obj.contains_key(*name))
        .map(|(_, sub)| sub)
        .collect::<Vec<_>>();
    if active.is_empty() {
        return Ok(None);
    }
    let mut current = Value::Object(obj.clone());
    let mut rewrites = Vec::new();
    for branch in &active {
        let (next, more) = normalize_child(&current, branch)?;
        if !branch_accepts(branch, &next) {
            return Ok(None);
        }
        current = next;
        rewrites.extend(more);
    }
    for branch in &active {
        let (next, _) = normalize_child(&current, branch)?;
        if next != current || !branch_accepts(branch, &next) {
            return Ok(None);
        }
    }
    if current == Value::Object(obj.clone()) && rewrites.is_empty() {
        return Ok(None);
    }
    Ok(Some((current, rewrites)))
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
/// A refusing `if` probe does not match, so `else` still runs.
/// An error from the matched branch is kept when that side still applies.
/// It is dropped only when a keyword outside `if`, `then`, and `else`
/// rewrites the value onto the other side.
fn if_then_else(
    value: &Value,
    schema: &Value,
) -> Result<Option<(Value, Vec<Rewrite>)>, RepairError> {
    if schema.get("allOf").is_some()
        || schema.get("oneOf").is_some()
        || schema.get("anyOf").is_some()
    {
        return Ok(None);
    }
    let Some(if_schema) = schema.get("if") else {
        return Ok(None);
    };
    let then_schema = schema.get("then");
    let else_schema = schema.get("else");
    if then_schema.is_none() && else_schema.is_none() {
        return Ok(None);
    }
    if value.is_string()
        && (unchecked_string_keyword(if_schema)
            || then_schema.is_some_and(unchecked_string_keyword)
            || else_schema.is_some_and(unchecked_string_keyword))
    {
        return Ok(None);
    }
    let if_hit = match normalize_child(value, if_schema) {
        Ok((probed, probe_rewrites)) if branch_accepts(if_schema, &probed) => {
            Some((probed, probe_rewrites))
        }
        _ => None,
    };
    if let Some((probed, probe_rewrites)) = if_hit {
        let Some(then_schema) = then_schema else {
            if probed == *value && probe_rewrites.is_empty() {
                return Ok(None);
            }
            return Ok(Some((probed, probe_rewrites)));
        };
        let (next, more) = match normalize_child(&probed, then_schema) {
            Ok(pair) => pair,
            Err(err) => return discard_conditional_error(value, schema, true, err),
        };
        let coerced = probed != *value || !probe_rewrites.is_empty();
        if !branch_accepts(then_schema, &next) && coerced {
            // The `if` probe already coerced, so `then` did not see the original value.
            if let Err(err) = normalize_child(value, then_schema) {
                return discard_conditional_error(value, schema, true, err);
            }
        }
        if !branch_accepts(if_schema, &next) || !branch_accepts(then_schema, &next) {
            return Ok(None);
        }
        let mut rewrites = probe_rewrites;
        rewrites.extend(more);
        if next == *value && rewrites.is_empty() {
            return Ok(None);
        }
        return Ok(Some((next, rewrites)));
    }
    let Some(else_schema) = else_schema else {
        return Ok(None);
    };
    let (next, rewrites) = match normalize_child(value, else_schema) {
        Ok(pair) => pair,
        Err(err) => return discard_conditional_error(value, schema, false, err),
    };
    if !branch_accepts(else_schema, &next) {
        return Ok(None);
    }
    if next == *value && rewrites.is_empty() {
        return Ok(None);
    }
    Ok(Some((next, rewrites)))
}

/// Keep a `then` or `else` error when `if` still selects that side.
/// Drop it only after a keyword outside `if`, `then`, and `else` rewrites
/// the value onto the other side. `if` can be the schema that coerces.
fn discard_conditional_error(
    value: &Value,
    schema: &Value,
    then_side: bool,
    err: RepairError,
) -> Result<Option<(Value, Vec<Rewrite>)>, RepairError> {
    let Some(if_schema) = schema.get("if") else {
        return Err(err);
    };
    let mut stripped = schema.clone();
    if let Some(map) = stripped.as_object_mut() {
        map.remove("if");
        map.remove("then");
        map.remove("else");
    }
    let (coerced, rewrites) = normalize_child(value, &stripped)?;
    if coerced == *value && rewrites.is_empty() {
        return Err(err);
    }
    let keep = match settled(if_schema, &coerced) {
        Settled::Yes => then_side,
        Settled::No(_) => !then_side,
        Settled::Inconclusive => false,
    };
    if keep { Err(err) } else { Ok(None) }
}

/// Apply every `allOf` branch. The result must still satisfy each branch.
/// A [`RepairError`] from a branch is not discarded.
/// A coercion that a later branch rejects is [`RepairError::Ambiguous`].
/// It is not reported as the original value.
fn all_of(value: &Value, schema: &Value) -> Result<Option<(Value, Vec<Rewrite>)>, RepairError> {
    let Some(branches) = single_applicator(schema, "allOf") else {
        return Ok(None);
    };
    if branches.is_empty() {
        return Ok(None);
    }
    if value.is_string() && branches.iter().any(unchecked_string_keyword) {
        return Ok(None);
    }
    let mut current = value.clone();
    let mut rewrites = Vec::new();
    let mut coerced = false;
    for branch in branches {
        let (next, more) = normalize_child(&current, branch)?;
        let changed = next != current || !more.is_empty();
        if !branch_accepts(branch, &next) {
            if coerced || changed {
                return Err(RepairError::Ambiguous(schema_rejection(branch, &next)));
            }
            return Ok(None);
        }
        coerced |= changed;
        current = next;
        rewrites.extend(more);
    }
    for branch in branches {
        let (next, _) = normalize_child(&current, branch)?;
        if next != current || !branch_accepts(branch, &next) {
            if coerced || next != current {
                return Err(RepairError::Ambiguous(schema_rejection(branch, &next)));
            }
            return Ok(None);
        }
    }
    if current == *value && rewrites.is_empty() {
        return Ok(None);
    }
    Ok(Some((current, rewrites)))
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
/// A branch error is not a match. If no branch accepts and one rejected a
/// coercion, that error is returned. A value that matches nothing stays.
fn exclusive_branch(
    value: &Value,
    schema: &Value,
) -> Result<Option<(Value, Vec<Rewrite>)>, RepairError> {
    let Some(branches) =
        single_applicator(schema, "oneOf").or_else(|| single_applicator(schema, "anyOf"))
    else {
        return Ok(None);
    };
    if value.is_string() && branches.iter().any(unchecked_string_keyword) {
        return Ok(None);
    }
    let mut matched = Vec::new();
    let mut branch_error = None;
    for branch in branches {
        match normalize_child(value, branch) {
            Ok((next, rewrites)) => {
                if branch_accepts(branch, &next) && branch_accepts(schema, &next) {
                    matched.push((next, rewrites));
                }
            }
            Err(err) => {
                if branch_error.is_none() {
                    branch_error = Some(err);
                }
            }
        }
    }
    if matched.len() == 1 {
        return Ok(matched.pop());
    }
    // A branch that rejected a coercion is not ignored just because another
    // branch returned the original text without accepting it. A value that
    // simply matches nothing, such as "hello", stays unchanged.
    if matched.is_empty()
        && let Some(err) = branch_error
    {
        return Err(err);
    }
    Ok(None)
}

/// A rewrite that satisfies `not` is refused.
/// `not` with only an unrecognized keyword is not a match.
/// A numeric bound, `multipleOf`, `uniqueItems`, `contains`, or a size
/// limit counts once this crate can evaluate it.
fn violates_not(schema: &Value, value: &Value) -> bool {
    let Some(not_schema) = schema.get("not") else {
        return false;
    };
    known_constraint(not_schema) && branch_accepts(not_schema, value)
}

/// Inclusive `minimum` / `maximum`, numeric exclusive bounds, and the
/// draft-04 boolean form of `exclusiveMinimum` / `exclusiveMaximum`.
/// A bound that is not an exact finite number is ignored.
fn violates_unique(schema: &Value, value: &Value) -> bool {
    if schema.get("uniqueItems") != Some(&Value::Bool(true)) {
        return false;
    }
    let Some(items) = value.as_array() else {
        return false;
    };
    items
        .iter()
        .enumerate()
        .any(|(index, item)| items[..index].contains(item))
}

/// A positive `multipleOf`. Zero, negatives, and values that do not parse
/// as exact decimals are not checked.
fn violates_multiple(schema: &Value, value: &Value) -> bool {
    let Some(multiple) = schema.get("multipleOf").and_then(Value::as_number) else {
        return false;
    };
    let Some(factor) = multiple.as_f64() else {
        return false;
    };
    if !factor.is_finite() || factor <= 0.0 {
        return false;
    }
    let Some(number) = value.as_number() else {
        return false;
    };
    let (Some(left), Some(right)) = (
        parse_decimal(&number.to_string()),
        parse_decimal(&multiple.to_string()),
    ) else {
        return false;
    };
    decimal_divides(&left, &right) == Some(false)
}

fn decimal_divides(value: &Decimal, multiple: &Decimal) -> Option<bool> {
    if multiple.coef == 0 {
        return Some(false);
    }
    if value.coef == 0 {
        return Some(true);
    }
    let exp = value.exp10.checked_sub(multiple.exp10)?;
    if exp >= 0 {
        let factor = pow10(exp)?;
        let scaled = value.coef.checked_mul(factor)?;
        Some(scaled.is_multiple_of(multiple.coef))
    } else {
        let factor = pow10(-exp)?;
        let divisor = multiple.coef.checked_mul(factor)?;
        if divisor == 0 {
            return Some(false);
        }
        Some(value.coef.is_multiple_of(divisor))
    }
}

fn pow10(exp: i32) -> Option<u128> {
    if !(0..=38).contains(&exp) {
        return None;
    }
    let mut factor: u128 = 1;
    for _ in 0..exp {
        factor = factor.checked_mul(10)?;
    }
    Some(factor)
}

fn enforce_outer_object(
    original: &Value,
    next: Value,
    schema: &Value,
    mut rewrites: Vec<Rewrite>,
) -> Result<(Value, Vec<Rewrite>), RepairError> {
    if let Some(message) = outer_property_rejected(original, &next, schema) {
        return Err(RepairError::Ambiguous(message.to_string()));
    }
    let Some(obj) = next.as_object() else {
        return Ok((next, rewrites));
    };
    let props = object_properties(schema);
    if props.is_none() && !denies_unknown(schema) && additional_properties_schema(schema).is_none()
    {
        return Ok((next, rewrites));
    }
    let empty = Map::new();
    let (normalized, more) = normalize_object(obj, schema, props.unwrap_or(&empty))?;
    rewrites.extend(more);
    Ok((normalized, rewrites))
}

fn outer_property_rejected(
    original: &Value,
    produced: &Value,
    schema: &Value,
) -> Option<&'static str> {
    let props = object_properties(schema)?;
    let produced_obj = produced.as_object()?;
    let original_obj = original.as_object()?;
    for (key, prop_schema) in props {
        let Some(next) = produced_obj.get(key) else {
            continue;
        };
        let Some(prev) = original_obj.get(key) else {
            continue;
        };
        if prev == next {
            continue;
        }
        if violates_bounds(prop_schema, next) {
            return Some("coerced value is outside the schema bounds");
        }
        if prev.is_string()
            && !next.is_string()
            && schema_type_names(prop_schema).contains(&"string")
        {
            return Some("coerced value does not match the outer schema");
        }
    }
    None
}

fn violates_bounds(schema: &Value, value: &Value) -> bool {
    // An i64 past 2^53, or a u64 past i64::MAX, is not an exact f64, so the
    // float path would skip the bound. Compare it with each bound as an
    // integer when the bound's integer part is exact, including a fraction
    // such as 10.5.
    if let Some(int) = value.as_i64()
        && let Some(outside) = i64_against_schema_bounds(schema, int)
    {
        return outside;
    }
    if value.as_i64().is_none()
        && let Some(int) = value.as_u64()
        && let Some(outside) = u64_against_schema_bounds(schema, int)
    {
        return outside;
    }
    let Some(number) = finite_bound(value) else {
        return false;
    };
    below_minimum(schema, number) || above_maximum(schema, number)
}

fn i64_against_schema_bounds(schema: &Value, int: i64) -> Option<bool> {
    for key in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
        let Some(bound) = schema.get(key) else {
            continue;
        };
        if bound.is_boolean() {
            continue;
        }
        if schema_i64_limit(schema, key).is_none()
            && bound.as_u64().is_none()
            && !finite_above_every_u64(bound)
            && comparable_f64_bound(bound).is_none()
        {
            return None;
        }
    }
    Some(i64_outside_bounds(schema, int))
}

fn comparable_f64_bound(bound: &Value) -> Option<f64> {
    let number = bound.as_f64()?;
    if !number.is_finite() || number.trunc().abs() > (1i64 << 53) as f64 {
        return None;
    }
    Some(number)
}

fn i64_outside_bounds(schema: &Value, int: i64) -> bool {
    if bound_fails(schema, int, "exclusiveMinimum", true, true) {
        return true;
    }
    let minimum_exclusive = matches!(schema.get("exclusiveMinimum"), Some(Value::Bool(true)));
    if bound_fails(schema, int, "minimum", minimum_exclusive, true) {
        return true;
    }
    if bound_fails(schema, int, "exclusiveMaximum", true, false) {
        return true;
    }
    let maximum_exclusive = matches!(schema.get("exclusiveMaximum"), Some(Value::Bool(true)));
    bound_fails(schema, int, "maximum", maximum_exclusive, false)
}

fn bound_fails(schema: &Value, int: i64, key: &str, exclusive: bool, low: bool) -> bool {
    let Some(bound) = schema.get(key) else {
        return false;
    };
    if bound.is_boolean() {
        return false;
    }
    let order = if let Some(limit) = schema_i64_limit(schema, key) {
        int.cmp(&limit)
    } else if let Some(limit) = bound.as_u64() {
        cmp_i64_u64(int, limit)
    } else if finite_above_every_u64(bound) {
        Ordering::Less
    } else if let Some(limit) = comparable_f64_bound(bound) {
        cmp_i64_f64(int, limit)
    } else {
        return false;
    };
    if low {
        if exclusive {
            order.is_le()
        } else {
            order.is_lt()
        }
    } else if exclusive {
        order.is_ge()
    } else {
        order.is_gt()
    }
}

fn cmp_i64_u64(int: i64, limit: u64) -> Ordering {
    u64::try_from(int).map_or(Ordering::Less, |int| int.cmp(&limit))
}

fn finite_above_every_u64(bound: &Value) -> bool {
    bound
        .as_f64()
        .is_some_and(|number| number.is_finite() && number >= u64::MAX as f64)
}

fn cmp_i64_f64(int: i64, limit: f64) -> Ordering {
    let trunc = limit.trunc() as i64;
    match int.cmp(&trunc) {
        Ordering::Equal if limit == limit.trunc() => Ordering::Equal,
        Ordering::Equal if limit > trunc as f64 => Ordering::Less,
        Ordering::Equal => Ordering::Greater,
        other => other,
    }
}

fn u64_against_schema_bounds(schema: &Value, int: u64) -> Option<bool> {
    for key in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
        let Some(bound) = schema.get(key) else {
            continue;
        };
        if bound.is_boolean() {
            continue;
        }
        if bound.as_i64().is_none()
            && bound.as_u64().is_none()
            && !finite_above_every_u64(bound)
            && comparable_f64_bound(bound).is_none()
        {
            return None;
        }
    }
    Some(u64_outside_bounds(schema, int))
}

fn u64_outside_bounds(schema: &Value, int: u64) -> bool {
    if u64_bound_fails(schema, int, "exclusiveMinimum", true, true) {
        return true;
    }
    let minimum_exclusive = matches!(schema.get("exclusiveMinimum"), Some(Value::Bool(true)));
    if u64_bound_fails(schema, int, "minimum", minimum_exclusive, true) {
        return true;
    }
    if u64_bound_fails(schema, int, "exclusiveMaximum", true, false) {
        return true;
    }
    let maximum_exclusive = matches!(schema.get("exclusiveMaximum"), Some(Value::Bool(true)));
    u64_bound_fails(schema, int, "maximum", maximum_exclusive, false)
}

fn u64_bound_fails(schema: &Value, int: u64, key: &str, exclusive: bool, low: bool) -> bool {
    let Some(bound) = schema.get(key) else {
        return false;
    };
    if bound.is_boolean() {
        return false;
    }
    let Some(order) = cmp_u64_bound(int, bound) else {
        return false;
    };
    if low {
        if exclusive {
            order.is_le()
        } else {
            order.is_lt()
        }
    } else if exclusive {
        order.is_ge()
    } else {
        order.is_gt()
    }
}

fn cmp_u64_bound(int: u64, bound: &Value) -> Option<Ordering> {
    if let Some(limit) = bound.as_i64() {
        return Some(match u64::try_from(limit) {
            Ok(limit) => int.cmp(&limit),
            Err(_) => Ordering::Greater,
        });
    }
    if let Some(limit) = bound.as_u64() {
        return Some(int.cmp(&limit));
    }
    if finite_above_every_u64(bound) {
        return Some(Ordering::Less);
    }
    let limit = comparable_f64_bound(bound)?;
    Some(cmp_u64_f64(int, limit))
}

fn cmp_u64_f64(int: u64, limit: f64) -> Ordering {
    if limit < 0.0 {
        return Ordering::Greater;
    }
    let trunc = limit.trunc() as u64;
    match int.cmp(&trunc) {
        Ordering::Equal if limit == limit.trunc() => Ordering::Equal,
        Ordering::Equal if limit > trunc as f64 => Ordering::Less,
        Ordering::Equal => Ordering::Greater,
        other => other,
    }
}

fn schema_i64_limit(schema: &Value, key: &str) -> Option<i64> {
    let value = schema.get(key)?;
    if let Some(int) = value.as_i64() {
        return Some(int);
    }
    let number = value.as_f64()?;
    // `i64::MAX as f64` is 2^63. Casting that f64 to i64 saturates at i64::MAX.
    if !number.is_finite() || number >= i64::MAX as f64 {
        return None;
    }
    let int = number as i64;
    if int as f64 == number {
        Some(int)
    } else {
        None
    }
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
        || schema.get("dependentRequired").is_some()
        || schema.get("minLength").is_some()
        || schema.get("maxLength").is_some()
        || schema.get("propertyNames").is_some()
        || schema.get("items").is_some()
        || schema.get("prefixItems").is_some()
        || checkable_bound(schema)
        || checkable_multiple(schema)
        || schema.get("uniqueItems") == Some(&Value::Bool(true))
        || checkable_contains(schema)
        || checkable_size(schema)
}

fn checkable_bound(schema: &Value) -> bool {
    ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"]
        .into_iter()
        .any(|key| match schema.get(key) {
            Some(bound) if bound.is_boolean() => {
                let pair = if key.ends_with("Minimum") {
                    "minimum"
                } else {
                    "maximum"
                };
                schema.get(pair).is_some_and(checkable_number)
            }
            Some(bound) => checkable_number(bound),
            None => false,
        })
}

fn checkable_number(bound: &Value) -> bool {
    bound.as_i64().is_some()
        || bound.as_u64().is_some()
        || comparable_f64_bound(bound).is_some()
        || finite_above_every_u64(bound)
}

fn checkable_multiple(schema: &Value) -> bool {
    let Some(multiple) = schema.get("multipleOf").and_then(Value::as_number) else {
        return false;
    };
    let Some(factor) = multiple.as_f64() else {
        return false;
    };
    factor.is_finite() && factor > 0.0 && parse_decimal(&multiple.to_string()).is_some()
}

fn checkable_contains(schema: &Value) -> bool {
    schema.get("contains").is_some_and(Value::is_object) && contain_limits(schema).is_some()
}

fn checkable_size(schema: &Value) -> bool {
    ["minItems", "maxItems", "minProperties", "maxProperties"]
        .into_iter()
        .any(|key| integer_limit(schema, key).is_some())
}

/// An `allOf` property or item bound is checked after the outer schema
/// coerces that value. `if` / `then` / `else` is chosen later, on the
/// coerced value: the original string can satisfy `if` when the number
/// does not.
fn applicator_field_failure(schema: &Value, before: &Value, after: &Value) -> Option<String> {
    let mut branches = Vec::new();
    if let Some(items) = single_applicator(schema, "allOf") {
        // A pattern in this branch makes its bounds unevaluable. A sibling
        // branch with no string keyword is still a miss.
        branches.extend(
            items
                .iter()
                .filter(|branch| !unevaluable_string_keyword(branch)),
        );
    }
    branches.into_iter().find_map(|branch| {
        changed_field_failure(branch, before, after)
            .or_else(|| changed_item_failure(branch, before, after))
    })
}

/// Applicator bounds are judged after the outer schema coerces.
///
/// `oneOf`, `anyOf`, `allOf`, and `if` sit on this schema. `contains` and a
/// dependent schema can hide the same kind of bound, and the branch walker
/// does not enter them. An inconclusive keyword is not a miss.
fn applicator_selection_failure(schema: &Value, value: &Value) -> Option<String> {
    match selection_settled(schema, value) {
        Settled::No(reason) => return Some(reason),
        Settled::Yes | Settled::Inconclusive => {}
    }
    match contains_settled(schema, value) {
        Settled::No(reason) => return Some(reason),
        Settled::Yes | Settled::Inconclusive => {}
    }
    match dependent_settled(schema, value) {
        Settled::No(reason) => Some(reason),
        Settled::Yes | Settled::Inconclusive => None,
    }
}

enum Settled {
    Yes,
    No(String),
    /// An unchecked string keyword is in this schema. It is not a match
    /// and not a miss.
    Inconclusive,
}

fn selection_settled(schema: &Value, value: &Value) -> Settled {
    let mut inconclusive = false;
    for (key, exactly_one) in [("oneOf", true), ("anyOf", false)] {
        match counted_branch_failure(schema, value, key, exactly_one) {
            Settled::Yes => {}
            Settled::Inconclusive => inconclusive = true,
            Settled::No(reason) => return Settled::No(reason),
        }
    }
    match all_of_settled(schema, value) {
        Settled::Yes => {}
        Settled::Inconclusive => inconclusive = true,
        Settled::No(reason) => return Settled::No(reason),
    }
    match conditional_settled(schema, value) {
        Settled::Yes => {}
        Settled::Inconclusive => inconclusive = true,
        Settled::No(reason) => return Settled::No(reason),
    }
    if inconclusive {
        Settled::Inconclusive
    } else {
        Settled::Yes
    }
}

fn counted_branch_failure(schema: &Value, value: &Value, key: &str, exactly_one: bool) -> Settled {
    let Some(branches) = schema.get(key).and_then(Value::as_array) else {
        return Settled::Yes;
    };
    let mut yes = 0;
    let mut inconclusive = false;
    let mut first_miss = None;
    for branch in branches {
        match settled(branch, value) {
            Settled::Yes => yes += 1,
            Settled::Inconclusive => inconclusive = true,
            Settled::No(reason) => {
                if first_miss.is_none() {
                    first_miss = Some(reason);
                }
            }
        }
    }
    if !exactly_one && yes > 0 {
        return Settled::Yes;
    }
    if exactly_one && yes > 1 {
        return Settled::No("coerced value matches more than one schema branch".to_string());
    }
    if inconclusive {
        return Settled::Inconclusive;
    }
    if exactly_one && yes == 1 {
        return Settled::Yes;
    }
    Settled::No(
        first_miss.unwrap_or_else(|| "coerced value was rejected by the schema branch".to_string()),
    )
}

fn all_of_settled(schema: &Value, value: &Value) -> Settled {
    let Some(branches) = schema.get("allOf").and_then(Value::as_array) else {
        return Settled::Yes;
    };
    let mut inconclusive = false;
    for branch in branches {
        match settled(branch, value) {
            Settled::Yes => {}
            Settled::Inconclusive => inconclusive = true,
            Settled::No(reason) => return Settled::No(reason),
        }
    }
    if inconclusive {
        Settled::Inconclusive
    } else {
        Settled::Yes
    }
}

fn conditional_settled(schema: &Value, value: &Value) -> Settled {
    let Some(if_schema) = schema.get("if") else {
        return Settled::Yes;
    };
    let then_schema = schema.get("then");
    let else_schema = schema.get("else");
    if then_schema.is_none() && else_schema.is_none() {
        return Settled::Yes;
    }
    match settled(if_schema, value) {
        Settled::Yes => match then_schema {
            Some(side) => settled(side, value),
            None => Settled::Yes,
        },
        Settled::No(_) => match else_schema {
            Some(side) => settled(side, value),
            None => Settled::Yes,
        },
        // Which side applies is unknown. A value both sides reject is still out.
        Settled::Inconclusive => {
            let (Some(then_schema), Some(else_schema)) = (then_schema, else_schema) else {
                return Settled::Inconclusive;
            };
            match (settled(then_schema, value), settled(else_schema, value)) {
                (Settled::No(reason), Settled::No(_)) => Settled::No(reason),
                _ => Settled::Inconclusive,
            }
        }
    }
}

/// Leaf constraints, nested applicators, and nested properties or items.
/// This does not coerce. A string keyword on this object stays inconclusive
/// so a `pattern` beside a bound is not a miss. A keyword that sits only
/// under `allOf`, `oneOf`, `anyOf`, `if`, `then`, or `else` does not hide
/// a sibling applicator: a conclusive miss still wins. A keyword in one
/// array slot or dependent entry does not hide a later slot or entry.
fn settled(schema: &Value, value: &Value) -> Settled {
    if unevaluable_outside_applicators(schema) {
        return settle_past_unevaluable(schema, value);
    }
    if !branch_accepts(schema, value) {
        return Settled::No(schema_rejection(schema, value));
    }
    let mut inconclusive = false;
    match selection_settled(schema, value) {
        Settled::Yes => {}
        Settled::Inconclusive => inconclusive = true,
        Settled::No(reason) => return Settled::No(reason),
    }
    match contains_settled(schema, value) {
        Settled::Yes => {}
        Settled::Inconclusive => inconclusive = true,
        Settled::No(reason) => return Settled::No(reason),
    }
    match nested_value_failure(schema, value) {
        Settled::Yes => {}
        Settled::Inconclusive => inconclusive = true,
        Settled::No(reason) => return Settled::No(reason),
    }
    if inconclusive {
        Settled::Inconclusive
    } else {
        Settled::Yes
    }
}

/// A string keyword or unresolved `$ref` outside the applicators is not a
/// match or a miss. Selection still runs first. `contains`, dependent
/// entries, array slots, and size limits still run, so a keyword in one
/// slot does not hide a later conclusive miss. This path does not call
/// [`branch_accepts`]: a `pattern` beside a bound in one property would
/// become a false miss.
fn settle_past_unevaluable(schema: &Value, value: &Value) -> Settled {
    if let Settled::No(reason) = selection_settled(schema, value) {
        return Settled::No(reason);
    }
    if let Settled::No(reason) = contains_settled(schema, value) {
        return Settled::No(reason);
    }
    if let Settled::No(reason) = dependent_settled(schema, value) {
        return Settled::No(reason);
    }
    if let Some(items) = value.as_array()
        && let Settled::No(reason) = nested_items(schema, items)
    {
        return Settled::No(reason);
    }
    if violates_size(schema, value) {
        return Settled::No("coerced value is outside the schema size".to_string());
    }
    if forbidden_extra_item(schema, value) {
        return Settled::No("coerced value was rejected by the schema branch".to_string());
    }
    Settled::Inconclusive
}

fn forbidden_extra_item(schema: &Value, value: &Value) -> bool {
    let Some(items) = value.as_array() else {
        return false;
    };
    let Some((positions, tail)) = tuple_positions(schema) else {
        return false;
    };
    matches!(tail, ArrayTail::Forbid) && items.len() > positions.len()
}

/// `contains` counted with [`settled`], so an applicator under it can miss.
/// An unevaluable item does not erase a count that is already inside
/// `minContains` and `maxContains`, and it is not itself a miss.
fn contains_settled(schema: &Value, value: &Value) -> Settled {
    let Some(contains) = schema.get("contains").filter(|schema| schema.is_object()) else {
        return Settled::Yes;
    };
    let Some(items) = value.as_array() else {
        return Settled::Yes;
    };
    let Some((min, max)) = contain_limits(schema) else {
        return Settled::Yes;
    };
    let mut yes = 0u64;
    let mut inconclusive = 0u64;
    for item in items {
        match settled(contains, item) {
            Settled::Yes => yes += 1,
            Settled::No(_) => {}
            Settled::Inconclusive => inconclusive += 1,
        }
    }
    if max.is_some_and(|limit| yes > limit) {
        return Settled::No("coerced value misses contains".to_string());
    }
    let within_max = match max {
        None => true,
        Some(limit) => yes.saturating_add(inconclusive) <= limit,
    };
    if yes >= min && within_max {
        return Settled::Yes;
    }
    if inconclusive > 0 {
        return Settled::Inconclusive;
    }
    Settled::No("coerced value misses contains".to_string())
}

/// Active `dependentSchemas` or schema-form `dependencies`, judged against
/// the whole object. An array of required names is not a subschema.
/// An inconclusive entry does not hide a later conclusive miss.
fn dependent_settled(schema: &Value, value: &Value) -> Settled {
    let Some(obj) = value.as_object() else {
        return Settled::Yes;
    };
    let mut inconclusive = false;
    for (name, sub) in dependent_object_schemas(schema) {
        if !obj.contains_key(name) {
            continue;
        }
        match settled(sub, value) {
            Settled::Yes => {}
            Settled::Inconclusive => inconclusive = true,
            Settled::No(reason) => return Settled::No(reason),
        }
    }
    if inconclusive {
        Settled::Inconclusive
    } else {
        Settled::Yes
    }
}

fn nested_value_failure(schema: &Value, value: &Value) -> Settled {
    if let Some(obj) = value.as_object() {
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (key, prop_schema) in props {
                let Some(child) = obj.get(key) else {
                    continue;
                };
                if let Some(failure) = nested_child(settled(prop_schema, child), key) {
                    return failure;
                }
            }
        }
        if let Some(extra) = additional_properties_schema(schema) {
            let known = schema.get("properties").and_then(Value::as_object);
            for (key, child) in obj {
                if known.is_some_and(|props| props.contains_key(key)) {
                    continue;
                }
                if let Some(failure) = nested_child(settled(extra, child), key) {
                    return failure;
                }
            }
        }
        match dependent_settled(schema, value) {
            Settled::Yes => {}
            other => return other,
        }
    }
    let Some(items) = value.as_array() else {
        return Settled::Yes;
    };
    nested_items(schema, items)
}

fn nested_child(settled: Settled, key: &str) -> Option<Settled> {
    match settled {
        Settled::Yes => None,
        Settled::No(reason) => Some(Settled::No(format!("'{key}': {reason}"))),
        Settled::Inconclusive => Some(Settled::Inconclusive),
    }
}

fn nested_items(schema: &Value, items: &[Value]) -> Settled {
    if let Some((positions, tail)) = tuple_positions(schema) {
        return settle_elements(items, |index| match positions.get(index) {
            Some(position) => Some(position),
            None => match tail {
                ArrayTail::Schema(rest) => Some(rest),
                ArrayTail::Keep | ArrayTail::Forbid => None,
            },
        });
    }
    let Some(item_schema) = schema.get("items").filter(|item| item.is_object()) else {
        return Settled::Yes;
    };
    settle_elements(items, |_| Some(item_schema))
}

/// An inconclusive element does not hide a later element this crate can reject.
fn settle_elements<'a>(
    items: &[Value],
    mut schema_at: impl FnMut(usize) -> Option<&'a Value>,
) -> Settled {
    let mut inconclusive = false;
    for (index, child) in items.iter().enumerate() {
        let Some(position) = schema_at(index) else {
            continue;
        };
        match settled(position, child) {
            Settled::Yes => {}
            Settled::Inconclusive => inconclusive = true,
            Settled::No(reason) => return Settled::No(format!("[{index}]: {reason}")),
        }
    }
    if inconclusive {
        Settled::Inconclusive
    } else {
        Settled::Yes
    }
}

fn changed_field_failure(schema: &Value, before: &Value, after: &Value) -> Option<String> {
    let (Some(before_obj), Some(after_obj)) = (before.as_object(), after.as_object()) else {
        return None;
    };
    let props = schema.get("properties").and_then(Value::as_object)?;
    for (key, prop_schema) in props {
        let (Some(prev), Some(next)) = (before_obj.get(key), after_obj.get(key)) else {
            continue;
        };
        if prev == next || branch_accepts(prop_schema, next) {
            continue;
        }
        return Some(format!("'{key}': {}", schema_rejection(prop_schema, next)));
    }
    None
}

fn changed_item_failure(schema: &Value, before: &Value, after: &Value) -> Option<String> {
    let (Some(before_items), Some(after_items)) = (before.as_array(), after.as_array()) else {
        return None;
    };
    if let Some(item_schema) = schema.get("items").filter(|item| item.is_object()) {
        for (index, (prev, next)) in before_items.iter().zip(after_items.iter()).enumerate() {
            if prev == next || branch_accepts(item_schema, next) {
                continue;
            }
            return Some(format!(
                "[{index}]: {}",
                schema_rejection(item_schema, next)
            ));
        }
    }
    let positions = schema
        .get("prefixItems")
        .and_then(Value::as_array)
        .or_else(|| schema.get("items").and_then(Value::as_array))?;
    for (index, position) in positions.iter().enumerate() {
        let (Some(prev), Some(next)) = (before_items.get(index), after_items.get(index)) else {
            continue;
        };
        if prev == next || branch_accepts(position, next) {
            continue;
        }
        return Some(format!("[{index}]: {}", schema_rejection(position, next)));
    }
    None
}

fn item_rejection(schema: &Value, items: &[Value]) -> Option<String> {
    if let Some((positions, tail)) = tuple_positions(schema) {
        for (index, child) in items.iter().enumerate() {
            let position = match positions.get(index) {
                Some(position) => position,
                None => match tail {
                    ArrayTail::Schema(rest) => rest,
                    ArrayTail::Keep | ArrayTail::Forbid => continue,
                },
            };
            if !branch_accepts(position, child) {
                return Some(format!("[{index}]: {}", schema_rejection(position, child)));
            }
        }
        return None;
    }
    let item_schema = schema.get("items").filter(|item| item.is_object())?;
    for (index, child) in items.iter().enumerate() {
        if !branch_accepts(item_schema, child) {
            return Some(format!(
                "[{index}]: {}",
                schema_rejection(item_schema, child)
            ));
        }
    }
    None
}

fn schema_rejection(schema: &Value, value: &Value) -> String {
    if let (Some(obj), Some(props)) = (
        value.as_object(),
        schema.get("properties").and_then(Value::as_object),
    ) {
        for (key, prop_schema) in props {
            if let Some(child) = obj.get(key)
                && !branch_accepts(prop_schema, child)
            {
                return format!("'{key}': {}", schema_rejection(prop_schema, child));
            }
        }
    }
    if let Some(items) = value.as_array()
        && let Some(reason) = item_rejection(schema, items)
    {
        return reason;
    }
    constraint_failure(schema, value)
        .unwrap_or("coerced value was rejected by the schema branch")
        .to_string()
}

fn constraint_failure(schema: &Value, value: &Value) -> Option<&'static str> {
    if violates_not(schema, value) {
        return Some("coerced value is excluded by not");
    }
    if violates_bounds(schema, value) {
        return Some("coerced value is outside the schema bounds");
    }
    if violates_multiple(schema, value) {
        return Some("coerced value is not a multiple of the schema");
    }
    if violates_unique(schema, value) {
        return Some("coerced value repeats an array item");
    }
    if violates_contains(schema, value) {
        return Some("coerced value misses contains");
    }
    if violates_size(schema, value) {
        return Some("coerced value is outside the schema size");
    }
    if violates_length(schema, value) {
        return Some("coerced value is outside the schema length");
    }
    if violates_dependent(schema, value) {
        return Some("coerced value misses a dependent property");
    }
    if property_names_refusal(schema, value).is_some() {
        return Some("coerced value has a property name outside the schema");
    }
    let types = schema_type_names(schema);
    if !types.is_empty() && !type_matches(&types, value) {
        return Some("coerced value does not match the schema type");
    }
    if (schema.get("const").is_some() || schema.get("enum").is_some())
        && !constraints_hold(schema, value)
    {
        return Some("coerced value is not a const or enum member");
    }
    if !required_holds(schema, value) {
        return Some("coerced value misses a required property");
    }
    if value.is_string() && unchecked_string_keyword(schema) {
        return Some("coerced string is outside an unchecked string keyword");
    }
    None
}

/// Type, `const`, `enum`, properties, array items, `required` names,
/// dependent names, string length, property names, `not`, numeric bounds,
/// `multipleOf`, `uniqueItems`, `contains`, and size limits.
fn branch_accepts(schema: &Value, value: &Value) -> bool {
    if value.is_string() && unchecked_string_keyword(schema) {
        return false;
    }
    constraints_hold(schema, value)
        && properties_match(schema, value)
        && elements_match(schema, value)
        && required_holds(schema, value)
        && !violates_not(schema, value)
        && !violates_bounds(schema, value)
        && !violates_multiple(schema, value)
        && !violates_unique(schema, value)
        && !violates_contains(schema, value)
        && !violates_size(schema, value)
        && !violates_dependent(schema, value)
        && !violates_length(schema, value)
        && !violates_property_names(schema, value)
}

/// `pattern`, `format`, and the content keywords apply to strings and
/// this crate cannot evaluate them. A nested copy under `not` counts too.
fn unchecked_string_keyword(schema: &Value) -> bool {
    let Some(map) = schema.as_object() else {
        return false;
    };
    map.iter().any(|(key, child)| {
        matches!(
            key.as_str(),
            "pattern" | "format" | "contentEncoding" | "contentMediaType" | "contentSchema"
        ) || (key == "not" && unchecked_string_keyword(child))
    })
}

/// Same keywords as [`unchecked_string_keyword`], anywhere a later
/// [`settled`] check would look. Property names are not keywords.
/// `$defs` is skipped. `$ref` is not resolved, so it is inconclusive.
/// `const`, `enum`, and `example` are instance data, same as `default`
/// and `examples`.
fn unevaluable_string_keyword(schema: &Value) -> bool {
    unevaluable_keyword(schema, true)
}

/// Like [`unevaluable_string_keyword`], but a keyword that appears only
/// under an applicator is left for [`selection_settled`].
fn unevaluable_outside_applicators(schema: &Value) -> bool {
    unevaluable_keyword(schema, false)
}

fn unevaluable_keyword(schema: &Value, enter_applicators: bool) -> bool {
    match schema {
        Value::Array(items) => items
            .iter()
            .any(|item| unevaluable_keyword(item, enter_applicators)),
        Value::Object(map) => map.iter().any(|(key, child)| {
            if matches!(
                key.as_str(),
                "pattern" | "format" | "contentEncoding" | "contentMediaType" | "contentSchema"
            ) {
                return true;
            }
            if matches!(key.as_str(), "$ref" | "$dynamicRef") {
                return true;
            }
            if is_schema_annotation(key) {
                return false;
            }
            if !enter_applicators
                && matches!(
                    key.as_str(),
                    "allOf" | "oneOf" | "anyOf" | "if" | "then" | "else"
                )
            {
                return false;
            }
            if matches!(
                key.as_str(),
                "properties" | "patternProperties" | "dependentSchemas" | "dependencies"
            ) {
                return child.as_object().is_some_and(|props| {
                    props
                        .values()
                        .any(|value| unevaluable_keyword(value, enter_applicators))
                });
            }
            unevaluable_keyword(child, enter_applicators)
        }),
        _ => false,
    }
}

/// `minLength` and `maxLength` count characters. A limit that is not an
/// integer is ignored. A non-string value is not checked.
fn violates_length(schema: &Value, value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let len = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
    if let Some(max) = integer_limit(schema, "maxLength")
        && len > max
    {
        return true;
    }
    if let Some(min) = integer_limit(schema, "minLength")
        && len < min
    {
        return true;
    }
    false
}

fn violates_property_names(schema: &Value, value: &Value) -> bool {
    property_names_refusal(schema, value).is_some()
}

/// Each object key must satisfy `propertyNames`.
/// A keyword this crate cannot evaluate is named. A length check that
/// failed keeps the outside-schema sentence.
fn property_names_refusal(schema: &Value, value: &Value) -> Option<String> {
    let obj = value.as_object()?;
    let schema = if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    };
    let names = schema
        .get("propertyNames")
        .filter(|schema| schema.is_object())?;
    if obj.is_empty() {
        return None;
    }
    if let Some(keyword) = unevaluable_name_keyword(names) {
        return Some(format!("propertyNames {keyword} was not evaluated"));
    }
    let outside = obj
        .keys()
        .any(|key| !branch_accepts(names, &Value::String(key.clone())));
    if outside {
        Some("coerced value has a property name outside the schema".to_string())
    } else {
        None
    }
}

fn unevaluable_name_keyword(schema: &Value) -> Option<&str> {
    let map = schema.as_object()?;
    for (key, child) in map {
        let checked = matches!(
            key.as_str(),
            "type"
                | "const"
                | "enum"
                | "minLength"
                | "maxLength"
                | "not"
                | "minimum"
                | "maximum"
                | "exclusiveMinimum"
                | "exclusiveMaximum"
                | "multipleOf"
        );
        if !checked {
            return Some(key.as_str());
        }
        if key == "not"
            && let Some(inner) = unevaluable_name_keyword(child)
        {
            return Some(inner);
        }
        if key == "not" && !child.is_object() {
            return Some("not");
        }
    }
    None
}

/// `dependentRequired`, and the array form of `dependencies`.
/// A present property must bring those names with it. A schema-form
/// dependency is not a name list. A non-object value is not checked.
fn violates_dependent(schema: &Value, value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let schema = if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    };
    dependent_names_missing(schema, "dependentRequired", obj)
        || dependent_names_missing(schema, "dependencies", obj)
}

fn dependent_names_missing(schema: &Value, keyword: &str, obj: &Map<String, Value>) -> bool {
    let Some(map) = schema.get(keyword).and_then(Value::as_object) else {
        return false;
    };
    map.iter().any(|(name, need)| {
        obj.contains_key(name)
            && need.as_array().is_some_and(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|needed| !obj.contains_key(needed))
            })
    })
}

/// `minProperties`, `maxProperties`, `minItems`, and `maxItems`.
/// A limit that is not an integer is ignored.
fn violates_size(schema: &Value, value: &Value) -> bool {
    let count = if let Some(obj) = value.as_object() {
        let count = u64::try_from(obj.len()).unwrap_or(u64::MAX);
        if let Some(max) = integer_limit(schema, "maxProperties")
            && count > max
        {
            return true;
        }
        if let Some(min) = integer_limit(schema, "minProperties")
            && count < min
        {
            return true;
        }
        return false;
    } else if let Some(items) = value.as_array() {
        u64::try_from(items.len()).unwrap_or(u64::MAX)
    } else {
        return false;
    };
    if let Some(max) = integer_limit(schema, "maxItems")
        && count > max
    {
        return true;
    }
    if let Some(min) = integer_limit(schema, "minItems")
        && count < min
    {
        return true;
    }
    false
}

fn integer_limit(schema: &Value, key: &str) -> Option<u64> {
    schema.get(key).and_then(Value::as_u64)
}

/// `contains` fails when too few or too many items match.
/// `minContains` defaults to 1. A limit that is not an integer is ignored.
fn violates_contains(schema: &Value, value: &Value) -> bool {
    let Some(contains) = schema.get("contains").filter(|schema| schema.is_object()) else {
        return false;
    };
    let Some(items) = value.as_array() else {
        return false;
    };
    let Some((min, max)) = contain_limits(schema) else {
        return false;
    };
    // An applicator this walker cannot enter is not a hit.
    // [`contains_settled`] counts those items.
    if contains_count_needs_settled(contains) {
        return false;
    }
    let count = u64::try_from(
        items
            .iter()
            .filter(|item| branch_accepts(contains, item))
            .count(),
    )
    .unwrap_or(u64::MAX);
    count < min || max.is_some_and(|limit| count > limit)
}

/// `branch_accepts` treats an applicator it does not enter as a match,
/// including one nested under a property or an item. The count has to
/// go through [`contains_settled`] instead.
fn contains_count_needs_settled(contains: &Value) -> bool {
    hidden_applicator(contains) || unevaluable_string_keyword(contains)
}

/// `oneOf`, `anyOf`, `allOf`, and `if` are invisible to [`branch_accepts`].
/// A property name is not one of those keywords. Annotation values are not schemas.
fn hidden_applicator(schema: &Value) -> bool {
    match schema {
        Value::Array(items) => items.iter().any(hidden_applicator),
        Value::Object(map) => map.iter().any(|(key, child)| {
            if matches!(key.as_str(), "oneOf" | "anyOf" | "allOf" | "if") {
                return true;
            }
            if is_schema_annotation(key) {
                return false;
            }
            if matches!(
                key.as_str(),
                "properties" | "patternProperties" | "dependentSchemas" | "dependencies"
            ) {
                return child
                    .as_object()
                    .is_some_and(|props| props.values().any(hidden_applicator));
            }
            hidden_applicator(child)
        }),
        _ => false,
    }
}

fn is_schema_annotation(key: &str) -> bool {
    matches!(
        key,
        "$defs"
            | "definitions"
            | "$id"
            | "$schema"
            | "$anchor"
            | "$dynamicAnchor"
            | "$comment"
            | "description"
            | "title"
            | "examples"
            | "example"
            | "default"
            | "const"
            | "enum"
            | "deprecated"
    )
}

fn contain_limits(schema: &Value) -> Option<(u64, Option<u64>)> {
    let min = match schema.get("minContains") {
        None => 1,
        Some(Value::Number(number)) => number.as_u64()?,
        Some(_) => return None,
    };
    let max = match schema.get("maxContains") {
        None => None,
        Some(Value::Number(number)) => Some(number.as_u64()?),
        Some(_) => return None,
    };
    Some((min, max))
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
        let tail = forbid_unevaluated_items(schema, tail);
        if matches!(tail, ArrayTail::Forbid) && arr.len() > positions.len() {
            return Err(RepairError::Ambiguous(
                "array item is past the tuple schema".into(),
            ));
        }
        let mut out = Vec::with_capacity(arr.len());
        let mut rewrites = Vec::new();
        for (index, item) in arr.iter().enumerate() {
            if let Some(position) = positions.get(index) {
                let (next, more) = normalize_child(item, position)
                    .map_err(|err| prefix_index_error(index, err))?;
                rewrites.extend(more);
                out.push(next);
            } else if let ArrayTail::Schema(rest) = tail {
                let (next, more) =
                    normalize_child(item, rest).map_err(|err| prefix_index_error(index, err))?;
                rewrites.extend(more);
                out.push(next);
            } else {
                out.push(item.clone());
            }
        }
        return Ok((Value::Array(out), rewrites));
    }
    if forbids_every_item(schema) {
        if arr.is_empty() {
            return Ok((Value::Array(Vec::new()), Vec::new()));
        }
        return Err(RepairError::Ambiguous(
            "array item is not allowed by the schema".into(),
        ));
    }
    let Some(items) = schema.get("items").filter(|items| items.is_object()) else {
        return Ok((Value::Array(arr.to_vec()), Vec::new()));
    };
    let mut out = Vec::with_capacity(arr.len());
    let mut rewrites = Vec::new();
    for (index, item) in arr.iter().enumerate() {
        let (next, more) =
            normalize_child(item, items).map_err(|err| prefix_index_error(index, err))?;
        rewrites.extend(more);
        out.push(next);
    }
    Ok((Value::Array(out), rewrites))
}

/// Name the property on an ambiguous child error. Other variants stay as they are.
fn prefix_property_error(key: &str, err: RepairError) -> RepairError {
    prefix_ambiguous(&format!("'{key}'"), err)
}

/// Name the array index on an ambiguous child error. Other variants stay as they are.
fn prefix_index_error(index: usize, err: RepairError) -> RepairError {
    prefix_ambiguous(&format!("[{index}]"), err)
}

fn prefix_ambiguous(location: &str, err: RepairError) -> RepairError {
    match err {
        RepairError::Ambiguous(reason) => RepairError::Ambiguous(format!("{location}: {reason}")),
        other => other,
    }
}

fn normalize_object(
    obj: &Map<String, Value>,
    schema: &Value,
    props: &Map<String, Value>,
) -> Result<(Value, Vec<Rewrite>), RepairError> {
    let mut out = Map::new();
    let mut rewrites = Vec::new();
    let mut unknown = Vec::new();
    let coverage = if denies_unknown(schema) {
        applicator_coverage(schema, &Value::Object(obj.clone()))?
    } else {
        ApplicatorCoverage::default()
    };
    let keep_evaluated = unevaluated_properties_closed(schema);

    for (key, val) in obj {
        if let Some(prop_schema) = props.get(key) {
            let (next, more) =
                normalize_child(val, prop_schema).map_err(|err| prefix_property_error(key, err))?;
            rewrites.extend(more);
            out.insert(key.clone(), next);
            continue;
        }
        match alias_target(key, props)? {
            Some(target) => {
                if (has_pattern_properties(schema) || coverage.open_pattern)
                    && (denies_unknown(schema) || additional_properties_schema(schema).is_some())
                {
                    let reason = if has_pattern_properties(schema) {
                        format!("unknown key '{key}' may be allowed by patternProperties")
                    } else {
                        format!("unknown key '{key}' may be evaluated by patternProperties")
                    };
                    return Err(RepairError::Ambiguous(reason));
                }
                if PATH_KEYS.contains(&key.as_str())
                    && PATH_KEYS.contains(&target)
                    && applicator_names_property(schema, key)
                {
                    return Err(RepairError::Ambiguous(format!(
                        "'{key}' is named by a schema branch and is also a path alias for '{target}'"
                    )));
                }
                // The canonical key may already have been coerced into `out`.
                // Compare both sides after the property schema, so `"1"` and
                // `1` agree, and key order does not change the result.
                if let Some(canonical) = normalized_alias_side(obj, &out, target, props)? {
                    let alias = match props.get(target) {
                        Some(prop_schema) => {
                            normalize_child(val, prop_schema)
                                .map_err(|err| prefix_property_error(key, err))?
                                .0
                        }
                        None => val.clone(),
                    };
                    if !values_equivalent(&alias, &canonical) {
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
                let (next, more) = normalize_child(val, prop_schema)
                    .map_err(|err| prefix_property_error(key, err))?;
                rewrites.extend(more);
                rewrites.push(rewrite(
                    RewriteKind::Alias,
                    format!("renamed {key} to {target}"),
                ));
                out.insert(target.to_string(), next);
            }
            None if keep_evaluated && coverage.evaluated.contains(key) => {
                out.insert(key.clone(), val.clone());
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
        refuse_covered_unknown(schema, &unknown, &coverage)?;
        if unknown.len() > 1 {
            let reason = unknown_denial(schema).unwrap_or("unknown keys are forbidden");
            return Err(RepairError::Ambiguous(format!(
                "{} unknown keys and {reason}",
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
            let (next, more) =
                normalize_child(&val, extra).map_err(|err| prefix_property_error(&key, err))?;
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

/// The other side of an alias, after the property schema. `None` means the
/// canonical name is not set yet, so this alias is a rename.
fn normalized_alias_side(
    obj: &Map<String, Value>,
    out: &Map<String, Value>,
    target: &str,
    props: &Map<String, Value>,
) -> Result<Option<Value>, RepairError> {
    if let Some(raw) = obj.get(target) {
        let Some(prop_schema) = props.get(target) else {
            return Ok(Some(raw.clone()));
        };
        let (value, _) =
            normalize_child(raw, prop_schema).map_err(|err| prefix_property_error(target, err))?;
        return Ok(Some(value));
    }
    Ok(out.get(target).cloned())
}

/// Parent property this key renames to.
///
/// An `x-aliases` entry wins. A path key otherwise renames to the one parent
/// property that is also a path key (`path`, `file`, `file_path`, `filepath`,
/// `path_name`). [`normalize_object`] refuses that rename when an applicator
/// `properties` map lists the incoming key. Parent `properties` are not that
/// conflict. A nonempty `patternProperties` map blocks the rename when this
/// object would already refuse an unknown key for that reason.
fn alias_target<'a>(
    key: &str,
    props: &'a Map<String, Value>,
) -> Result<Option<&'a str>, RepairError> {
    let mut targets: Vec<&str> = props
        .iter()
        .filter(|(_, prop)| property_lists_alias(prop, key))
        .map(|(name, _)| name.as_str())
        .collect();
    if targets.is_empty() && PATH_KEYS.contains(&key) {
        for name in props.keys() {
            if PATH_KEYS.contains(&name.as_str()) {
                targets.push(name.as_str());
            }
        }
    }
    match targets.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        _ => Err(RepairError::Ambiguous(format!(
            "'{key}' matches more than one property"
        ))),
    }
}

/// `true` when an applicator branch lists `key` under `properties`.
/// This object's own `properties` map does not count.
fn applicator_names_property(schema: &Value, key: &str) -> bool {
    let mut mentioned = BTreeSet::new();
    let schema = schema_view(schema);
    let Some(map) = schema.as_object() else {
        return false;
    };
    for keyword in ["allOf", "anyOf", "oneOf", "if", "then", "else"] {
        match map.get(keyword) {
            Some(Value::Array(branches)) => {
                for branch in branches {
                    mention_applicator(branch, &mut mentioned);
                }
            }
            Some(child) => mention_applicator(child, &mut mentioned),
            None => {}
        }
    }
    for (_, sub) in dependent_object_schemas(schema) {
        mention_applicator(sub, &mut mentioned);
    }
    mentioned.contains(key)
}

fn property_lists_alias(prop: &Value, key: &str) -> bool {
    prop.get("x-aliases")
        .and_then(Value::as_array)
        .is_some_and(|aliases| aliases.iter().any(|alias| alias.as_str() == Some(key)))
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
    unknown_denial(schema).is_some()
}

/// `additionalProperties: false`, or `unevaluatedProperties: false` when
/// `additionalProperties` is absent. Applicator keys are not visible here.
/// [`normalize_object`] keeps a key `unevaluatedProperties` evaluated and
/// refuses to drop a key `additionalProperties` would only reject.
fn unknown_denial(schema: &Value) -> Option<&'static str> {
    let schema = schema_view(schema);
    if matches!(schema.get("additionalProperties"), Some(Value::Bool(false))) {
        return Some("additionalProperties is false");
    }
    if schema.get("additionalProperties").is_none()
        && schema.get("unevaluatedProperties") == Some(&Value::Bool(false))
    {
        return Some("unevaluatedProperties is false");
    }
    None
}

fn schema_view(schema: &Value) -> &Value {
    if schema.get("properties").is_none() {
        schema.get("parameters").unwrap_or(schema)
    } else {
        schema
    }
}

fn unevaluated_properties_closed(schema: &Value) -> bool {
    let schema = schema_view(schema);
    schema.get("additionalProperties").is_none()
        && schema.get("unevaluatedProperties") == Some(&Value::Bool(false))
}

fn additional_properties_closed(schema: &Value) -> bool {
    schema_view(schema).get("additionalProperties") == Some(&Value::Bool(false))
}

#[derive(Default)]
struct ApplicatorCoverage {
    /// Keys an applied branch listed under `properties`.
    evaluated: BTreeSet<String>,
    /// Keys an `allOf` branch or an active dependent schema lists, including
    /// a branch that rejects the instance. Dropping one hides that judgment.
    guarded: BTreeSet<String>,
    /// Keys any applicator `properties` map lists. `additionalProperties`
    /// does not treat those as known.
    mentioned: BTreeSet<String>,
    /// An applied branch has `patternProperties`. The pattern is not matched
    /// here, so a remaining key may already have been evaluated.
    open_pattern: bool,
    /// This object or an applicator branch contains `$ref` or `$dynamicRef`.
    /// The ref is not resolved, so a remaining key may be named by it.
    open_ref: bool,
}

fn applicator_coverage(
    schema: &Value,
    instance: &Value,
) -> Result<ApplicatorCoverage, RepairError> {
    let mut coverage = ApplicatorCoverage::default();
    collect_applicators(schema_view(schema), instance, &mut coverage)?;
    Ok(coverage)
}

/// Records `$ref` or `$dynamicRef` on this object or an applicator branch.
/// Acceptance is not required. The ref is not resolved. `properties`,
/// `$defs`, and `not` are not walked.
fn note_unresolved_ref(schema: &Value, coverage: &mut ApplicatorCoverage) {
    if coverage.open_ref {
        return;
    }
    let Some(map) = schema.as_object() else {
        return;
    };
    if map.contains_key("$ref") || map.contains_key("$dynamicRef") {
        coverage.open_ref = true;
        return;
    }
    for keyword in ["allOf", "anyOf", "oneOf", "if", "then", "else"] {
        match map.get(keyword) {
            Some(Value::Array(branches)) => {
                for branch in branches {
                    note_unresolved_ref(branch, coverage);
                    if coverage.open_ref {
                        return;
                    }
                }
            }
            Some(child) => {
                note_unresolved_ref(child, coverage);
                if coverage.open_ref {
                    return;
                }
            }
            None => {}
        }
    }
    for (_, sub) in dependent_object_schemas(schema) {
        note_unresolved_ref(sub, coverage);
        if coverage.open_ref {
            return;
        }
    }
}

fn collect_applicators(
    schema: &Value,
    instance: &Value,
    coverage: &mut ApplicatorCoverage,
) -> Result<(), RepairError> {
    note_unresolved_ref(schema, coverage);
    let Some(map) = schema.as_object() else {
        return Ok(());
    };
    if let Some(branches) = map.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            guard_allof_properties(branch, coverage);
            mention_applicator(branch, &mut coverage.mentioned);
            match branch_result(branch, instance) {
                Ok(true) => evaluate_branch(branch, instance, coverage)?,
                Ok(false) => {}
                Err(err) => return Err(err),
            }
        }
    }
    if let Some(branches) = map.get("anyOf").and_then(Value::as_array) {
        for branch in branches {
            mention_applicator(branch, &mut coverage.mentioned);
            if branch_accepted(branch, instance) {
                evaluate_branch(branch, instance, coverage)?;
            }
        }
    }
    if let Some(branches) = map.get("oneOf").and_then(Value::as_array) {
        let mut matched = Vec::new();
        for branch in branches {
            mention_applicator(branch, &mut coverage.mentioned);
            if branch_accepted(branch, instance) {
                matched.push(branch);
            }
        }
        if matched.len() == 1 {
            evaluate_branch(matched[0], instance, coverage)?;
        }
    }
    if let Some(if_schema) = map.get("if") {
        mention_applicator(if_schema, &mut coverage.mentioned);
        if let Some(then_schema) = map.get("then") {
            mention_applicator(then_schema, &mut coverage.mentioned);
        }
        if let Some(else_schema) = map.get("else") {
            mention_applicator(else_schema, &mut coverage.mentioned);
        }
        if branch_accepted(if_schema, instance) {
            // A successful `if` evaluates its own properties, not only `then`.
            evaluate_branch(if_schema, instance, coverage)?;
            if let Some(then_schema) = map.get("then")
                && branch_accepted(then_schema, instance)
            {
                evaluate_branch(then_schema, instance, coverage)?;
            }
        } else if !unchecked_string_keyword(if_schema)
            && let Some(else_schema) = map.get("else")
            && branch_accepted(else_schema, instance)
        {
            evaluate_branch(else_schema, instance, coverage)?;
        }
    }
    for (trigger, sub) in dependent_object_schemas(schema) {
        let present = instance
            .as_object()
            .is_some_and(|obj| obj.contains_key(trigger));
        if !present {
            continue;
        }
        guard_allof_properties(sub, coverage);
        mention_applicator(sub, &mut coverage.mentioned);
        match branch_result(sub, instance) {
            Ok(true) => evaluate_branch(sub, instance, coverage)?,
            Ok(false) => {
                if has_pattern_properties(sub) {
                    coverage.open_pattern = true;
                }
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn evaluate_branch(
    schema: &Value,
    instance: &Value,
    coverage: &mut ApplicatorCoverage,
) -> Result<(), RepairError> {
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        coverage.evaluated.extend(props.keys().cloned());
    }
    if has_pattern_properties(schema) {
        coverage.open_pattern = true;
    }
    collect_applicators(schema, instance, coverage)
}

fn guard_allof_properties(schema: &Value, coverage: &mut ApplicatorCoverage) {
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for key in props.keys() {
            coverage.guarded.insert(key.clone());
            coverage.mentioned.insert(key.clone());
        }
    }
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            guard_allof_properties(branch, coverage);
        }
    }
}

fn mention_applicator(schema: &Value, mentioned: &mut BTreeSet<String>) {
    let Some(map) = schema.as_object() else {
        return;
    };
    if let Some(props) = map.get("properties").and_then(Value::as_object) {
        mentioned.extend(props.keys().cloned());
    }
    for key in ["allOf", "anyOf", "oneOf", "if", "then", "else"] {
        match map.get(key) {
            Some(Value::Array(branches)) => {
                for branch in branches {
                    mention_applicator(branch, mentioned);
                }
            }
            Some(child) => mention_applicator(child, mentioned),
            None => {}
        }
    }
    for (_, sub) in dependent_object_schemas(schema) {
        mention_applicator(sub, mentioned);
    }
}

/// Whether this branch accepts `instance` after its own coercion.
/// The caller's object is left as it is. Callers decide which errors count.
fn branch_result(branch: &Value, instance: &Value) -> Result<bool, RepairError> {
    match normalize_child(instance, branch) {
        Ok((next, _)) => Ok(branch_accepts(branch, &next)),
        Err(err) => Err(err),
    }
}

fn branch_accepted(branch: &Value, instance: &Value) -> bool {
    matches!(branch_result(branch, instance), Ok(true))
}

fn refuse_covered_unknown(
    schema: &Value,
    unknown: &[(String, Value)],
    coverage: &ApplicatorCoverage,
) -> Result<(), RepairError> {
    if unknown.is_empty() {
        return Ok(());
    }
    let additional = additional_properties_closed(schema);
    let unevaluated = unevaluated_properties_closed(schema);
    if let Some((key, _)) = unknown.iter().find(|(key, _)| {
        (additional && coverage.mentioned.contains(key))
            || (unevaluated && coverage.guarded.contains(key) && !coverage.evaluated.contains(key))
    }) {
        if additional && coverage.mentioned.contains(key) {
            return Err(RepairError::Ambiguous(format!(
                "'{key}' is named by a schema branch and additionalProperties is false"
            )));
        }
        return Err(RepairError::Ambiguous(format!(
            "'{key}' is named by a schema branch that does not accept it"
        )));
    }
    if coverage.open_pattern && (additional || unevaluated) {
        let key = &unknown[0].0;
        return Err(RepairError::Ambiguous(format!(
            "unknown key '{key}' may be evaluated by patternProperties"
        )));
    }
    if coverage.open_ref && (additional || unevaluated) {
        let key = &unknown[0].0;
        return Err(RepairError::Ambiguous(format!(
            "unknown key '{key}' may be evaluated by an unresolved ref"
        )));
    }
    Ok(())
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

/// An array value uses `items`, `prefixItems`, or `unevaluatedItems` even
/// when `type` is omitted. An explicit non-array type does not.
fn describes_array(schema: &Value) -> bool {
    let types = schema_type_names(schema);
    if types.contains(&"array") {
        return true;
    }
    if !types.is_empty() {
        return false;
    }
    schema.get("items").is_some()
        || schema.get("prefixItems").is_some()
        || schema.get("unevaluatedItems").is_some()
}

/// `unevaluatedItems: false` closes a tuple whose tail keyword is absent.
/// `items: true` and `additionalItems: true` already evaluated that tail.
fn forbid_unevaluated_items<'a>(schema: &'a Value, tail: ArrayTail<'a>) -> ArrayTail<'a> {
    if matches!(tail, ArrayTail::Keep)
        && unevaluated_tuple_tail(schema)
        && schema.get("unevaluatedItems") == Some(&Value::Bool(false))
    {
        ArrayTail::Forbid
    } else {
        tail
    }
}

fn unevaluated_tuple_tail(schema: &Value) -> bool {
    if schema.get("prefixItems").is_some() {
        return schema.get("items").is_none();
    }
    schema.get("items").and_then(Value::as_array).is_some()
        && schema.get("additionalItems").is_none()
}

/// `items: false` allows no element. `unevaluatedItems: false` with no
/// item keyword does the same, because nothing evaluated an element.
fn forbids_every_item(schema: &Value) -> bool {
    if schema.get("prefixItems").is_some() {
        return false;
    }
    if schema.get("items") == Some(&Value::Bool(false)) {
        return true;
    }
    schema.get("items").is_none() && schema.get("unevaluatedItems") == Some(&Value::Bool(false))
}

fn schema_type_names(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// An object or array becomes the one `const` or `enum` member it can reach.
/// Two reachable members are left unchanged.
fn deep_constrained(value: &Value, schema: &Value) -> Option<(Value, Vec<Rewrite>)> {
    if !value.is_object() && !value.is_array() {
        return None;
    }
    let targets = constrained_values(schema)?;
    let mut matched = Vec::new();
    for target in targets {
        if let Some((next, rewrites)) = coerce_toward(value, target, schema)
            && &next == target
        {
            matched.push((next, rewrites));
        }
    }
    if matched.len() != 1 {
        return None;
    }
    let (next, rewrites) = matched.pop()?;
    if &next == value && rewrites.is_empty() {
        return None;
    }
    Some((next, rewrites))
}

fn coerce_toward(value: &Value, target: &Value, schema: &Value) -> Option<(Value, Vec<Rewrite>)> {
    if value == target {
        return Some((value.clone(), Vec::new()));
    }
    if let Some(pair) = exact_float_toward(value, target, schema) {
        return Some(pair);
    }
    match (value, target) {
        (Value::Array(items), Value::Array(targets)) => {
            if items.len() != targets.len() {
                return None;
            }
            let mut out = Vec::with_capacity(items.len());
            let mut rewrites = Vec::new();
            for (item, target_item) in items.iter().zip(targets) {
                let (next, more) = coerce_toward(item, target_item, schema)?;
                rewrites.extend(more);
                out.push(next);
            }
            Some((Value::Array(out), rewrites))
        }
        (Value::Object(obj), Value::Object(target_obj)) => {
            if obj.len() != target_obj.len() {
                return None;
            }
            let mut out = Map::new();
            let mut rewrites = Vec::new();
            for (key, target_child) in target_obj {
                let child = obj.get(key)?;
                let (next, more) = coerce_toward(child, target_child, schema)?;
                rewrites.extend(more);
                out.insert(key.clone(), next);
            }
            Some((Value::Object(out), rewrites))
        }
        (Value::String(text), _) => {
            let text = text.trim_matches(char::is_whitespace);
            if text.is_empty() || !string_matches_target(text, target) {
                return None;
            }
            let mut rewrites = Vec::new();
            record_constrained(text, target, schema, &mut rewrites);
            Some((target.clone(), rewrites))
        }
        _ => None,
    }
}

/// `1.0` reaches integer `1` inside an object or array member.
/// A float that is already one of the members is left unchanged.
fn exact_float_toward(
    value: &Value,
    target: &Value,
    schema: &Value,
) -> Option<(Value, Vec<Rewrite>)> {
    let left = value.as_number()?;
    let right = target.as_number()?;
    if !numbers_equal(left, right) {
        return None;
    }
    let shown = value.as_f64()?;
    if !shown.is_finite() || shown.fract() != 0.0 || shown.abs() > (1u64 << 53) as f64 {
        return None;
    }
    let word = if schema.get("const").is_some() {
        "const"
    } else {
        "enum"
    };
    let mut rewrites = Vec::new();
    rewrites.push(rewrite(
        RewriteKind::NumberString,
        format!("{word} number {shown:?}"),
    ));
    Some((target.clone(), rewrites))
}

fn outside_const_or_enum(schema: &Value, value: &Value) -> bool {
    if let Some(const_value) = schema.get("const")
        && const_value != value
        && !values_equivalent(const_value, value)
    {
        return true;
    }
    if let Some(items) = schema.get("enum").and_then(Value::as_array) {
        return !items
            .iter()
            .any(|item| item == value || values_equivalent(item, value));
    }
    false
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

fn integer_does_not_fit_f64(number: &serde_json::Number) -> bool {
    if let Some(int) = number.as_i64() {
        return int as f64 as i64 != int;
    }
    if let Some(int) = number.as_u64() {
        return int as f64 as u64 != int;
    }
    false
}

fn numbers_equal(left: &serde_json::Number, right: &serde_json::Number) -> bool {
    if left == right {
        return true;
    }
    if integer_does_not_fit_f64(left) || integer_does_not_fit_f64(right) {
        return false;
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

/// One leading `+` is unary plus. A second sign (`+-5`) is not a number
/// token. An integer that fits in `u64` but not `i64` is still that integer.
/// `const` and `enum` use this parser and follow the same rule.
fn integer_string(text: &str) -> Option<serde_json::Number> {
    let text = without_digit_underscores(text)?;
    let text = without_unary_plus(&text);
    if text.is_empty() || !is_integer_token(text) || has_leading_zero(text) {
        return None;
    }
    if let Ok(parsed) = text.parse::<i64>() {
        return Some(serde_json::Number::from(parsed));
    }
    if text.starts_with('-') {
        return None;
    }
    text.parse::<u64>().ok().map(serde_json::Number::from)
}

/// `"3.0"`, `"-2.000"`, and `"1."` are integers. `"3.10"` and `"03.0"` are not.
/// One leading `+` is unary plus. A second sign (`+-2.0`) is not a number
/// token. An integer that fits in `u64` but not `i64` is still that integer.
/// `const` and `enum` use this parser and follow the same rule.
fn integer_from_zero_fraction(text: &str) -> Option<serde_json::Number> {
    let text = without_digit_underscores(text)?;
    let text = without_unary_plus(&text);
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
    if let Ok(parsed) = digits.parse::<i64>() {
        return Some(serde_json::Number::from(parsed));
    }
    if negative {
        return None;
    }
    digits.parse::<u64>().ok().map(serde_json::Number::from)
}

/// One leading `+` is unary plus. A second sign (`+-1.5`, `+-1e2`) is not a
/// number token. An integer that fits in `u64` but not `i64` is still that
/// integer. `const` and `enum` use this parser and follow the same rule.
fn number_string(text: &str) -> Option<serde_json::Number> {
    let text = without_digit_underscores(text)?;
    let text = without_unary_plus(&text);
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

/// `+5` loses the plus. `+-5`, `++5`, and a lone `+` stay as written.
fn without_unary_plus(text: &str) -> &str {
    match text.strip_prefix('+') {
        Some(rest) if !rest.is_empty() && !rest.starts_with(['+', '-']) => rest,
        _ => text,
    }
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
