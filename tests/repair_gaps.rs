//! Public-entry properties and the schema edges that example tests skipped.
//!
//! The schema checker below is independent on purpose. It does not call
//! `branch_accepts` or `settled`, so a rewrite that lands outside a
//! checkable keyword fails here even when those helpers agree with it.
//! An unchanged subtree is not rechecked: a sibling string that is not a
//! number stays a string, and that value can still violate its schema.

use std::collections::BTreeSet;

use argmend::{RepairOutcome, RepairRequest, normalize, repair_args, repair_json};
use proptest::prelude::*;
use serde_json::{Map, Value, json};

fn type_names(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn type_ok(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => false,
    }
}

fn u64_keyword(schema: &Value, key: &str) -> Option<u64> {
    let value = schema.get(key)?;
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
}

fn i64_keyword(schema: &Value, key: &str) -> Option<i64> {
    schema.get(key).and_then(Value::as_i64)
}

fn length_violation(schema: &Value, value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let len = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
    if let Some(max) = u64_keyword(schema, "maxLength")
        && len > max
    {
        return Some(format!("maxLength {max}"));
    }
    if let Some(min) = u64_keyword(schema, "minLength")
        && len < min
    {
        return Some(format!("minLength {min}"));
    }
    None
}

fn bounds_violation(schema: &Value, value: &Value) -> Option<String> {
    let number = value.as_i64()?;
    if let Some(max) = i64_keyword(schema, "maximum")
        && number > max
    {
        return Some(format!("maximum {max}"));
    }
    if let Some(min) = i64_keyword(schema, "minimum")
        && number < min
    {
        return Some(format!("minimum {min}"));
    }
    if let Some(max) = i64_keyword(schema, "exclusiveMaximum")
        && number >= max
    {
        return Some(format!("exclusiveMaximum {max}"));
    }
    if let Some(min) = i64_keyword(schema, "exclusiveMinimum")
        && number <= min
    {
        return Some(format!("exclusiveMinimum {min}"));
    }
    None
}

fn multiple_violation(schema: &Value, value: &Value) -> Option<String> {
    let factor = i64_keyword(schema, "multipleOf")?;
    if factor <= 0 {
        return None;
    }
    let number = value.as_i64()?;
    if number.rem_euclid(factor) != 0 {
        return Some(format!("multipleOf {factor}"));
    }
    None
}

fn unique_violation(schema: &Value, value: &Value) -> Option<String> {
    if schema.get("uniqueItems") != Some(&Value::Bool(true)) {
        return None;
    }
    let items = value.as_array()?;
    let duplicated = items
        .iter()
        .enumerate()
        .any(|(index, item)| items[..index].contains(item));
    duplicated.then(|| "uniqueItems".to_string())
}

fn size_violation(schema: &Value, value: &Value) -> Option<String> {
    if let Some(items) = value.as_array() {
        let len = u64::try_from(items.len()).unwrap_or(u64::MAX);
        if let Some(max) = u64_keyword(schema, "maxItems")
            && len > max
        {
            return Some(format!("maxItems {max}"));
        }
        if let Some(min) = u64_keyword(schema, "minItems")
            && len < min
        {
            return Some(format!("minItems {min}"));
        }
    }
    if let Some(object) = value.as_object() {
        let len = u64::try_from(object.len()).unwrap_or(u64::MAX);
        if let Some(max) = u64_keyword(schema, "maxProperties")
            && len > max
        {
            return Some(format!("maxProperties {max}"));
        }
        if let Some(min) = u64_keyword(schema, "minProperties")
            && len < min
        {
            return Some(format!("minProperties {min}"));
        }
    }
    None
}

fn const_violation(schema: &Value, value: &Value) -> Option<String> {
    let expected = schema.get("const")?;
    (expected != value).then(|| "const".to_string())
}

fn enum_violation(schema: &Value, value: &Value) -> Option<String> {
    let items = schema.get("enum").and_then(Value::as_array)?;
    (!items.contains(value)).then(|| "enum".to_string())
}

fn type_violation(schema: &Value, value: &Value) -> Option<String> {
    let types = type_names(schema);
    if types.is_empty() || types.iter().any(|kind| type_ok(kind, value)) {
        return None;
    }
    Some(format!("type {types:?}"))
}

/// Keywords on this schema object. Applicators and nested values are separate.
fn keyword_violation(schema: &Value, value: &Value) -> Option<String> {
    type_violation(schema, value)
        .or_else(|| const_violation(schema, value))
        .or_else(|| enum_violation(schema, value))
        .or_else(|| bounds_violation(schema, value))
        .or_else(|| multiple_violation(schema, value))
        .or_else(|| length_violation(schema, value))
        .or_else(|| size_violation(schema, value))
        .or_else(|| unique_violation(schema, value))
        .or_else(|| not_violation(schema, value))
}

fn not_violation(schema: &Value, value: &Value) -> Option<String> {
    let excluded = schema.get("not")?;
    if !checkable(excluded) || !fully_accepts(excluded, value) {
        return None;
    }
    Some("not".to_string())
}

fn checkable(schema: &Value) -> bool {
    let Some(map) = schema.as_object() else {
        return schema.is_boolean();
    };
    map.keys().any(|key| {
        matches!(
            key.as_str(),
            "type"
                | "const"
                | "enum"
                | "minimum"
                | "maximum"
                | "exclusiveMinimum"
                | "exclusiveMaximum"
                | "multipleOf"
                | "minLength"
                | "maxLength"
                | "minItems"
                | "maxItems"
                | "minProperties"
                | "maxProperties"
                | "uniqueItems"
                | "properties"
                | "items"
                | "prefixItems"
                | "additionalProperties"
                | "required"
                | "allOf"
                | "anyOf"
                | "oneOf"
                | "if"
                | "not"
        )
    })
}

fn fully_accepts(schema: &Value, value: &Value) -> bool {
    match schema {
        Value::Bool(flag) => *flag,
        Value::Object(_) => {
            keyword_violation(schema, value).is_none()
                && nested_accepts(schema, value)
                && applicator_accepts(schema, value)
        }
        _ => true,
    }
}

fn nested_accepts(schema: &Value, value: &Value) -> bool {
    properties_accept(schema, value)
        && extras_accept(schema, value)
        && elements_accept(schema, value)
        && required_accepts(schema, value)
}

fn properties_accept(schema: &Value, value: &Value) -> bool {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return true;
    };
    let Some(object) = value.as_object() else {
        return true;
    };
    props.iter().all(|(key, prop_schema)| {
        object
            .get(key)
            .is_none_or(|child| fully_accepts(prop_schema, child))
    })
}

fn extras_accept(schema: &Value, value: &Value) -> bool {
    let Some(extra) = schema.get("additionalProperties") else {
        return true;
    };
    let Some(object) = value.as_object() else {
        return true;
    };
    let known = schema.get("properties").and_then(Value::as_object);
    if extra == &Value::Bool(false) {
        return object
            .keys()
            .all(|key| known.is_some_and(|props| props.contains_key(key)));
    }
    if extra.is_object() {
        return object.iter().all(|(key, child)| {
            known.is_some_and(|props| props.contains_key(key)) || fully_accepts(extra, child)
        });
    }
    true
}

fn elements_accept(schema: &Value, value: &Value) -> bool {
    let Some(items) = value.as_array() else {
        return true;
    };
    if let Some(prefix) = schema.get("prefixItems").and_then(Value::as_array) {
        let tail_ok = items
            .iter()
            .enumerate()
            .all(|(index, item)| match prefix.get(index) {
                Some(position) => fully_accepts(position, item),
                None => match schema.get("items") {
                    Some(Value::Bool(false)) => false,
                    Some(rest) if rest.is_object() => fully_accepts(rest, item),
                    _ => true,
                },
            });
        return tail_ok;
    }
    match schema.get("items") {
        Some(Value::Bool(false)) => items.is_empty(),
        Some(position) if position.is_object() => {
            items.iter().all(|item| fully_accepts(position, item))
        }
        Some(Value::Array(positions)) => items.iter().enumerate().all(|(index, item)| {
            positions
                .get(index)
                .is_none_or(|position| fully_accepts(position, item))
        }),
        _ => true,
    }
}

fn required_accepts(schema: &Value, value: &Value) -> bool {
    let Some(names) = schema.get("required").and_then(Value::as_array) else {
        return true;
    };
    let Some(object) = value.as_object() else {
        return names.is_empty();
    };
    names
        .iter()
        .filter_map(Value::as_str)
        .all(|name| object.contains_key(name))
}

fn applicator_accepts(schema: &Value, value: &Value) -> bool {
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array)
        && !branches.iter().all(|branch| fully_accepts(branch, value))
    {
        return false;
    }
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array)
        && (branches.is_empty() || !branches.iter().any(|branch| fully_accepts(branch, value)))
    {
        return false;
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        let matched = branches
            .iter()
            .filter(|branch| fully_accepts(branch, value))
            .count();
        if matched != 1 {
            return false;
        }
    }
    conditional_accepts(schema, value)
}

fn conditional_accepts(schema: &Value, value: &Value) -> bool {
    let Some(if_schema) = schema.get("if") else {
        return true;
    };
    if fully_accepts(if_schema, value) {
        schema
            .get("then")
            .is_none_or(|then_schema| fully_accepts(then_schema, value))
    } else {
        schema
            .get("else")
            .is_none_or(|else_schema| fully_accepts(else_schema, value))
    }
}

/// Why `after` leaves the schema when it differs from `before`.
/// `None` means the rewrite stayed inside the checkable keywords.
fn rewritten_violation(schema: &Value, before: &Value, after: &Value) -> Option<String> {
    if before == after {
        return None;
    }
    changed_violation(schema, before, after)
}

fn changed_violation(schema: &Value, before: &Value, after: &Value) -> Option<String> {
    if let (Some(before), Some(after)) = (before.as_object(), after.as_object()) {
        return object_violation(schema, before, after);
    }
    if let (Some(before), Some(after)) = (before.as_array(), after.as_array()) {
        return array_violation(schema, before, after);
    }
    if let Some(reason) = length_violation(schema, before) {
        return Some(reason);
    }
    if fully_accepts(schema, after) {
        None
    } else {
        Some(format!("leaf {after}"))
    }
}

fn object_violation(
    schema: &Value,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Option<String> {
    if let Some(reason) = dropped_allof_property(schema, before, after) {
        return Some(reason);
    }
    if let Some(reason) = keyword_violation(schema, &Value::Object(after.clone())) {
        return Some(reason);
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (key, prop_schema) in props {
            match (before.get(key), after.get(key)) {
                (Some(prev), Some(next)) if prev != next => {
                    if let Some(reason) = changed_violation(prop_schema, prev, next) {
                        return Some(format!("'{key}': {reason}"));
                    }
                }
                (None, Some(next)) if !fully_accepts(prop_schema, next) => {
                    return Some(format!("'{key}' was added outside its schema"));
                }
                _ => {}
            }
        }
    }
    if let Some(reason) = extra_violation(schema, before, after) {
        return Some(reason);
    }
    if fully_accepts_applicators(schema, &Value::Object(after.clone())) {
        None
    } else {
        Some("object applicators".to_string())
    }
}

/// A successful repair must not delete a key an `allOf` branch lists.
/// `unevaluatedProperties` evaluated that key. `additionalProperties`
/// would reject it, and deleting it is a different outcome.
fn dropped_allof_property(
    schema: &Value,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Option<String> {
    let unevaluated = schema.get("additionalProperties").is_none()
        && schema.get("unevaluatedProperties") == Some(&Value::Bool(false));
    let additional = schema.get("additionalProperties") == Some(&Value::Bool(false));
    if !unevaluated && !additional {
        return None;
    }
    let named = allof_property_names(schema);
    if let Some(dropped) = before
        .keys()
        .find(|key| !after.contains_key(*key) && named.contains(*key))
    {
        return Some(format!("dropped '{dropped}' named by allOf"));
    }
    if schema_has_unresolved_ref(schema) {
        let dropped = before.keys().find(|key| !after.contains_key(*key))?;
        return Some(format!("dropped '{dropped}' near an unresolved ref"));
    }
    None
}

/// `$ref` or `$dynamicRef` on this object or an applicator branch.
/// A ref that appears only inside unused `$defs` does not count.
fn schema_has_unresolved_ref(schema: &Value) -> bool {
    let Some(map) = schema.as_object() else {
        return false;
    };
    if map.contains_key("$ref") || map.contains_key("$dynamicRef") {
        return true;
    }
    for keyword in ["allOf", "anyOf", "oneOf", "if", "then", "else"] {
        match map.get(keyword) {
            Some(Value::Array(branches)) if branches.iter().any(schema_has_unresolved_ref) => {
                return true;
            }
            Some(child) if schema_has_unresolved_ref(child) => return true,
            _ => {}
        }
    }
    for keyword in ["dependentSchemas", "dependencies"] {
        let Some(deps) = map.get(keyword).and_then(Value::as_object) else {
            continue;
        };
        if deps
            .values()
            .any(|sub| sub.is_object() && schema_has_unresolved_ref(sub))
        {
            return true;
        }
    }
    false
}

fn allof_property_names(schema: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_allof_names(schema, &mut names, false);
    names
}

fn collect_allof_names(schema: &Value, names: &mut BTreeSet<String>, in_allof: bool) {
    let Some(map) = schema.as_object() else {
        return;
    };
    if in_allof && let Some(props) = map.get("properties").and_then(Value::as_object) {
        names.extend(props.keys().cloned());
    }
    if let Some(branches) = map.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            collect_allof_names(branch, names, true);
        }
    }
}

fn extra_violation(
    schema: &Value,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Option<String> {
    let known = schema.get("properties").and_then(Value::as_object);
    let extra = schema.get("additionalProperties")?;
    if extra == &Value::Bool(false) {
        let unknown = after
            .keys()
            .find(|key| known.is_none_or(|props| !props.contains_key(*key)));
        return unknown.map(|key| format!("unknown key '{key}'"));
    }
    let extra_schema = extra.as_object().map(|_| extra)?;
    for (key, next) in after {
        if known.is_some_and(|props| props.contains_key(key)) {
            continue;
        }
        if before.get(key) == Some(next) {
            continue;
        }
        if !fully_accepts(extra_schema, next) {
            return Some(format!("'{key}' additionalProperties"));
        }
    }
    None
}

fn array_violation(schema: &Value, before: &[Value], after: &[Value]) -> Option<String> {
    if let Some(reason) = keyword_violation(schema, &Value::Array(after.to_vec())) {
        return Some(reason);
    }
    let prefix = schema
        .get("prefixItems")
        .and_then(Value::as_array)
        .or_else(|| schema.get("items").and_then(Value::as_array));
    for (index, next) in after.iter().enumerate() {
        let Some(prev) = before.get(index) else {
            return Some(format!("[{index}] was added"));
        };
        if prev == next {
            continue;
        }
        let position = prefix.and_then(|items| items.get(index)).or_else(|| {
            schema
                .get("items")
                .filter(|items| items.is_object() && prefix.is_none())
        });
        if let Some(position) = position
            && let Some(reason) = changed_violation(position, prev, next)
        {
            return Some(format!("[{index}]: {reason}"));
        }
        if prefix.is_some()
            && prefix.is_some_and(|items| index >= items.len())
            && let Some(rest) = schema.get("items").filter(|item| item.is_object())
            && let Some(reason) = changed_violation(rest, prev, next)
        {
            return Some(format!("[{index}]: {reason}"));
        }
    }
    if fully_accepts_applicators(schema, &Value::Array(after.to_vec())) {
        None
    } else {
        Some("array applicators".to_string())
    }
}

fn fully_accepts_applicators(schema: &Value, value: &Value) -> bool {
    let Some(map) = schema.as_object() else {
        return true;
    };
    if map.contains_key("allOf")
        || map.contains_key("anyOf")
        || map.contains_key("oneOf")
        || map.contains_key("if")
    {
        return applicator_accepts(schema, value);
    }
    true
}

fn cases(count: u32) -> ProptestConfig {
    ProptestConfig {
        cases: count,
        ..ProptestConfig::default()
    }
}

fn arb_key() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("a".to_string()),
        Just("b".to_string()),
        Just("n".to_string()),
        Just("m".to_string()),
    ]
}

fn arb_leaf_string() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("1".to_string()),
        Just("0".to_string()),
        Just("2".to_string()),
        Just("11".to_string()),
        Just("-3".to_string()),
        Just("3.0".to_string()),
        Just("true".to_string()),
        Just("false".to_string()),
        Just("null".to_string()),
        Just("01".to_string()),
        Just("hello".to_string()),
        "[a-z]{1,4}",
    ]
}

fn arb_integer_schema() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(json!({"type": "integer"})),
        (-8i64..8, 0i64..12).prop_map(|(min, span)| {
            json!({"type": "integer", "minimum": min, "maximum": min + span})
        }),
        (1i64..6).prop_map(|factor| json!({"type": "integer", "multipleOf": factor})),
    ]
}

fn arb_schema() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        arb_integer_schema(),
        Just(json!({"type": "boolean"})),
        Just(json!({"type": "number"})),
        Just(json!({"type": "null"})),
        Just(json!({"type": "string"})),
        (0u64..6, 0u64..6).prop_map(|(min, extra)| {
            json!({
                "type": "string",
                "minLength": min,
                "maxLength": min + extra
            })
        }),
        (-5i64..6).prop_map(|number| json!({"const": number})),
        prop::collection::vec(-3i64..5, 1..4).prop_map(|numbers| json!({"enum": numbers})),
    ];
    leaf.prop_recursive(3, 8, 2, |inner| {
        prop_oneof![
            (arb_key(), inner.clone(), any::<bool>()).prop_map(|(key, child, closed)| {
                if closed {
                    json!({
                        "type": "object",
                        "properties": {key: child},
                        "additionalProperties": false
                    })
                } else {
                    json!({"type": "object", "properties": {key: child}})
                }
            }),
            (inner.clone(), prop::option::of(0u64..3)).prop_map(|(child, min_items)| {
                let mut schema = json!({"type": "array", "items": child});
                if let Some(min) = min_items {
                    schema["minItems"] = json!(min);
                    schema["maxItems"] = json!(min + 3);
                }
                schema
            }),
            (inner.clone(), inner.clone())
                .prop_map(|(left, right)| json!({"allOf": [left, right]})),
            (arb_key(), arb_key(), inner.clone(), any::<bool>()).prop_map(
                |(known, extra, child, additional)| {
                    let mut schema = json!({
                        "type": "object",
                        "properties": {known: {"type": "string"}},
                        "allOf": [{"properties": {extra: child}}]
                    });
                    if additional {
                        schema["additionalProperties"] = json!(false);
                    } else {
                        schema["unevaluatedProperties"] = json!(false);
                    }
                    schema
                },
            ),
            (inner.clone(), inner.clone())
                .prop_map(|(left, right)| json!({"anyOf": [left, right]})),
            (inner.clone(), inner.clone())
                .prop_map(|(left, right)| json!({"oneOf": [left, right]})),
            (inner.clone(), inner.clone(), inner.clone()).prop_map(
                |(cond, then_schema, else_schema)| {
                    json!({"if": cond, "then": then_schema, "else": else_schema})
                }
            ),
            (inner.clone(), inner.clone())
                .prop_map(|(first, second)| { json!({"prefixItems": [first, second]}) }),
            inner.prop_map(|child| json!({"not": child})),
        ]
    })
}

fn arb_biased_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-12i64..12).prop_map(|number| json!(number)),
        arb_leaf_string().prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 8, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            (arb_key(), inner.clone(), arb_key(), inner).prop_map(
                |(left, left_value, right, right_value)| {
                    let mut object = Map::new();
                    object.insert(left, left_value);
                    object.insert(right, right_value);
                    Value::Object(object)
                }
            ),
        ]
    })
}

fn arb_any_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-1000i64..1000).prop_map(|number| json!(number)),
        prop::string::string_regex("\\PC{0,8}")
            .expect("unicode string regex")
            .prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 16, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map("[a-z]{1,3}", inner, 0..3)
                .prop_map(|map| { Value::Object(map.into_iter().collect()) }),
        ]
    })
}

fn panic_schema(index: u8) -> Value {
    match index % 3 {
        0 => json!({"type": "object", "properties": {"n": {"type": "integer"}}}),
        1 => json!({"type": "array", "items": {"type": "integer", "minimum": 0}}),
        _ => json!({"type": "integer", "minimum": 0, "maximum": 10}),
    }
}

#[test]
fn boolean_schema_is_not_applied() {
    for schema in [json!(true), json!(false)] {
        let repaired = normalize(&json!("1"), &schema).expect("boolean schema");
        assert_eq!(repaired.value, json!("1"));
        assert!(repaired.rewrites.is_empty());
    }
}

#[test]
fn json_text_inside_a_string_stays_a_string() {
    let object = json!({"type": "object", "properties": {"a": {"type": "integer"}}});
    let repaired = normalize(&json!("{\"a\":1}"), &object).expect("object string");
    assert_eq!(repaired.value, json!("{\"a\":1}"));
    assert!(repaired.rewrites.is_empty());

    let array = json!({"type": "array", "items": {"type": "integer"}});
    let repaired = normalize(&json!("[1,2]"), &array).expect("array string");
    assert_eq!(repaired.value, json!("[1,2]"));
    assert!(repaired.rewrites.is_empty());
}

#[test]
fn sibling_ref_does_not_block_a_checkable_type() {
    let schema = json!({"type": "integer", "$ref": "#/$defs/small"});
    let repaired = normalize(&json!("9"), &schema).expect("ref");
    assert_eq!(repaired.value, json!(9));

    let bounded = json!({
        "type": "integer",
        "maximum": 3,
        "$ref": "#/$defs/small"
    });
    let err = normalize(&json!("9"), &bounded).expect_err("maximum");
    assert!(err.to_string().contains("bounds"), "{err}");
    let repaired = normalize(&json!("2"), &bounded).expect("inside");
    assert_eq!(repaired.value, json!(2));
}

#[test]
fn checker_flags_a_drop_beside_an_unresolved_ref() {
    let schema = json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "unevaluatedProperties": false,
        "allOf": [{"$ref": "#/$defs/x"}]
    });
    let before = json!({"path": "a", "line": 3});
    let after = json!({"path": "a"});
    assert_eq!(
        object_violation(
            &schema,
            before.as_object().expect("before"),
            after.as_object().expect("after"),
        )
        .as_deref(),
        Some("dropped 'line' near an unresolved ref")
    );

    let named = json!({
        "unevaluatedProperties": false,
        "allOf": [{
            "properties": {"line": {"type": "integer"}},
            "$ref": "#/$defs/x"
        }]
    });
    assert_eq!(
        object_violation(
            &named,
            before.as_object().expect("before"),
            after.as_object().expect("after"),
        )
        .as_deref(),
        Some("dropped 'line' named by allOf")
    );

    let defs_only = json!({
        "properties": {"path": {"type": "string"}},
        "unevaluatedProperties": false,
        "$defs": {"x": {"$ref": "#/$defs/y"}}
    });
    assert_eq!(
        object_violation(
            &defs_only,
            before.as_object().expect("before"),
            after.as_object().expect("after"),
        ),
        None
    );
}

#[test]
fn missing_required_on_an_object_stays() {
    let schema = json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "required": ["path"]
    });
    let repaired = normalize(&json!({}), &schema).expect("required");
    assert_eq!(repaired.value, json!({}));
    assert!(repaired.rewrites.is_empty());
}

#[test]
fn pattern_on_a_string_does_not_rewrite_it() {
    let schema = json!({"type": "string", "pattern": "^a+$"});
    let repaired = normalize(&json!("b"), &schema).expect("pattern");
    assert_eq!(repaired.value, json!("b"));
    assert!(repaired.rewrites.is_empty());
}

#[test]
fn repaired_bare_key_is_stable() {
    let RepairOutcome::Repaired { value, .. } = repair_json("{a: 1,}") else {
        panic!("bare key should repair");
    };
    assert_eq!(value, json!({"a": 1}));
    let text = serde_json::to_string(&value).expect("serialize");
    match repair_json(&text) {
        RepairOutcome::Valid(again) => assert_eq!(again, value),
        other => panic!("second pass {other:?}"),
    }
}

#[test]
fn unicode_padding_repairs_once() {
    let RepairOutcome::Repaired { value, .. } = repair_json("\u{00a0}{\"a\":1}") else {
        panic!("nbsp should repair");
    };
    assert_eq!(value, json!({"a": 1}));
    let text = serde_json::to_string(&value).expect("serialize");
    assert!(matches!(repair_json(&text), RepairOutcome::Valid(_)));
}

#[test]
fn checker_rejects_a_dropped_applicator_field() {
    let schema = json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "unevaluatedProperties": false,
        "allOf": [{"properties": {"line": {"type": "integer"}}}]
    });
    let before = json!({"path": "a.rs", "line": "3"});
    let stripped = json!({"path": "a.rs"});
    let reason = rewritten_violation(&schema, &before, &stripped);
    assert!(
        reason.as_ref().is_some_and(|text| text.contains("line")),
        "{reason:?}"
    );

    let repaired = normalize(&before, &schema).expect("keep");
    assert_eq!(repaired.value, json!({"path": "a.rs", "line": 3}));
    assert!(rewritten_violation(&schema, &before, &repaired.value).is_none());

    let additional = json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "additionalProperties": false,
        "allOf": [{"properties": {"line": {"type": "integer"}}}]
    });
    assert!(rewritten_violation(&additional, &before, &stripped).is_some());
    assert!(normalize(&before, &additional).is_err());
}

#[test]
fn checker_rejects_a_number_past_a_nested_maximum() {
    let schema = json!({
        "type": "integer",
        "oneOf": [{"maximum": 3}, {"minimum": 10}]
    });
    assert!(normalize(&json!("9"), &schema).is_err());
    assert!(rewritten_violation(&schema, &json!("9"), &json!(9)).is_some());
    let repaired = normalize(&json!("2"), &schema).expect("inside");
    assert_eq!(repaired.value, json!(2));
    assert!(rewritten_violation(&schema, &json!("2"), &repaired.value).is_none());
}

#[test]
fn checker_allows_an_unchanged_sibling_that_is_not_a_number() {
    let schema = json!({
        "type": "object",
        "properties": {
            "n": {"type": "integer"},
            "m": {"type": "integer"}
        }
    });
    let before = json!({"n": "hello", "m": "1"});
    let after = json!({"n": "hello", "m": 1});
    assert!(rewritten_violation(&schema, &before, &after).is_none());
}

#[test]
fn checker_rejects_a_value_that_matches_two_integer_branches() {
    let schema = json!({"oneOf": [{"type": "integer"}, {"type": "integer"}]});
    assert!(rewritten_violation(&schema, &json!("1"), &json!(1)).is_some());
}

#[test]
fn checker_rejects_a_short_string_that_was_coerced_away() {
    let schema = json!({"type": "integer", "minLength": 5});
    assert!(rewritten_violation(&schema, &json!("1"), &json!(1)).is_some());
    assert!(rewritten_violation(&schema, &json!("100000"), &json!(100000)).is_none());
}

proptest! {
    #![proptest_config(cases(256))]

    #[test]
    fn valid_json_round_trips(value in arb_any_json()) {
        let text = serde_json::to_string(&value).expect("serialize");
        for raw in [text.clone(), format!("\n{text}"), format!(" {text} ")] {
            match repair_json(&raw) {
                RepairOutcome::Valid(parsed) => prop_assert_eq!(parsed, value.clone()),
                other => prop_assert!(false, "{raw:?} -> {other:?}"),
            }
        }
    }

    #[test]
    fn repaired_value_is_stable(raw in prop::string::string_regex("\\PC{0,80}").unwrap()) {
        if let RepairOutcome::Repaired { value, .. } = repair_json(&raw) {
            let text = serde_json::to_string(&value).expect("serialize");
            match repair_json(&text) {
                RepairOutcome::Valid(again) => prop_assert_eq!(again, value),
                other => prop_assert!(false, "second pass {other:?} for {raw:?}"),
            }
        }
    }

    #[test]
    fn repair_entries_do_not_panic(
        raw in prop::string::string_regex("\\PC{0,80}").unwrap(),
        schema in arb_schema(),
        index in 0u8..3,
    ) {
        let _ = repair_json(&raw);
        let fixed = panic_schema(index);
        for chosen in [&schema, &fixed] {
            let _ = normalize(&json!(raw), chosen);
            if let RepairOutcome::Valid(value) | RepairOutcome::Repaired { value, .. } = repair_json(&raw) {
                let _ = normalize(&value, chosen);
            }
            let _ = repair_args(&RepairRequest {
                tool_name: "probe",
                raw: &raw,
                schema: Some(chosen),
            });
        }
        let _ = repair_args(&RepairRequest {
            tool_name: "",
            raw: &raw,
            schema: None,
        });
    }

    #[test]
    fn changed_schema_result_stays_inside_checkable_keywords(
        value in arb_biased_value(),
        schema in arb_schema(),
    ) {
        let Ok(repaired) = normalize(&value, &schema) else {
            return Ok(());
        };
        if repaired.value == value && repaired.rewrites.is_empty() {
            return Ok(());
        }
        let violation = rewritten_violation(&schema, &value, &repaired.value);
        prop_assert!(
            violation.is_none(),
            "schema={schema}\nbefore={value}\nafter={}\nreason={violation:?}",
            repaired.value
        );
    }
}
