//! Tool-argument repair.
//!
//! The crate does not know about Bline errors, chat requests, or the tool
//! catalog. Callers pass the raw argument string and, when they have it,
//! the tool's JSON schema.

mod salvage;
mod schema;
mod syntax;

use serde_json::Value;

pub use schema::normalize;

/// Result of [`repair_json`].
#[derive(Debug, Clone, PartialEq)]
pub enum RepairOutcome {
    /// The input parsed. Nothing was rewritten.
    Valid(Value),
    /// The input was rewritten and then parsed.
    Repaired { value: Value, repairs: Vec<Rewrite> },
    /// The input is not JSON this crate can salvage.
    Unfixable(String),
    /// More than one repair was possible, so nothing was chosen.
    Ambiguous(String),
}

/// One recorded rewrite. Valid JSON that needs no schema change has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    pub kind: RewriteKind,
    pub detail: String,
}

/// Why a rewrite was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteKind {
    Syntax,
    Fence,
    ClosedTruncation,
    XmlParameters,
    Alias,
    BoolString,
    NumberString,
    NullString,
    DroppedUnknown,
    Empty,
}

/// Arguments plus an optional JSON schema.
#[derive(Debug, Clone, Copy)]
pub struct RepairRequest<'a> {
    pub tool_name: &'a str,
    pub raw: &'a str,
    pub schema: Option<&'a Value>,
}

/// A value the host can execute, with every rewrite that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Repaired {
    pub value: Value,
    pub rewrites: Vec<Rewrite>,
}

/// Repair refused. [`RepairError::Ambiguous`] is not a guess.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepairError {
    #[error("{0}")]
    Unfixable(String),
    #[error("{0}")]
    Ambiguous(String),
}

/// Syntax repair only. Empty input and garbage stay [`RepairOutcome::Unfixable`].
///
/// A repeated XML parameter name is [`RepairOutcome::Ambiguous`].
pub fn repair_json(raw: &str) -> RepairOutcome {
    match salvage_value(raw) {
        Ok((value, repairs)) if repairs.is_empty() => RepairOutcome::Valid(value),
        Ok((value, repairs)) => RepairOutcome::Repaired { value, repairs },
        Err(RepairError::Unfixable(reason)) => RepairOutcome::Unfixable(reason),
        Err(RepairError::Ambiguous(reason)) => RepairOutcome::Ambiguous(reason),
    }
}

fn salvage_value(raw: &str) -> Result<(Value, Vec<Rewrite>), RepairError> {
    let (raw, mut prefix) = match without_edge_unicode_whitespace(raw) {
        Some(rest) => (
            rest,
            vec![note(RewriteKind::Syntax, "stripped surrounding whitespace")],
        ),
        None => (raw, Vec::new()),
    };
    let raw = match without_leading_bom(raw) {
        Some(rest) => {
            prefix.push(note(RewriteKind::Syntax, "stripped leading BOM"));
            rest
        }
        None => raw,
    };
    if let Ok(value) = serde_json::from_str(raw) {
        return with_unique_keys(raw, value, prefix);
    }

    let mut body = raw.to_string();
    if let Some(inner) = salvage::unwrap_fence(raw) {
        prefix.push(note(RewriteKind::Fence, "unwrapped markdown fence"));
        body = inner;
        if let Ok(value) = serde_json::from_str(&body) {
            return with_unique_keys(&body, value, prefix);
        }
    }

    if prefix.is_empty() {
        match syntax::repair_tool_call_json(raw) {
            RepairOutcome::Valid(value) => return Ok((value, prefix)),
            RepairOutcome::Repaired { value, repairs } => {
                prefix.extend(repairs);
                return Ok((value, prefix));
            }
            RepairOutcome::Ambiguous(reason) => return Err(RepairError::Ambiguous(reason)),
            RepairOutcome::Unfixable(_) => {}
        }
    }

    let staged = syntax::apply_stages(&body);
    let parse_error = match serde_json::from_str::<Value>(&staged.text) {
        Ok(value) => {
            prefix.extend(staged.repairs);
            return with_unique_keys(&staged.text, value, prefix);
        }
        Err(err) => err,
    };

    if let Some(closed) = salvage::close_truncated(&staged.text)
        && let Ok(value) = serde_json::from_str::<Value>(&closed)
    {
        prefix.extend(staged.repairs);
        prefix.push(note(RewriteKind::ClosedTruncation, "closed truncated json"));
        return with_unique_keys(&closed, value, prefix);
    }

    match salvage::xml_parameters(&body)? {
        Some((value, more)) => {
            prefix.extend(more);
            prefix.push(note(RewriteKind::XmlParameters, "read xml parameters"));
            Ok((value, prefix))
        }
        None => Err(RepairError::Unfixable(format!(
            "parse failed after all repairs: {parse_error}"
        ))),
    }
}

/// Non-ASCII whitespace around the document, such as U+00A0.
/// ASCII whitespace is left in place so a valid document stays Valid.
fn without_edge_unicode_whitespace(raw: &str) -> Option<&str> {
    let mut start = 0;
    let mut end = raw.len();
    let mut saw_non_ascii = false;
    for (idx, ch) in raw.char_indices() {
        if !ch.is_whitespace() {
            break;
        }
        if !ch.is_ascii_whitespace() {
            saw_non_ascii = true;
        }
        start = idx + ch.len_utf8();
    }
    for (idx, ch) in raw.char_indices().rev() {
        if idx < start || !ch.is_whitespace() {
            break;
        }
        if !ch.is_ascii_whitespace() {
            saw_non_ascii = true;
        }
        end = idx;
    }
    if !saw_non_ascii {
        return None;
    }
    Some(&raw[start..end])
}

/// BOM after ASCII whitespace, before the JSON text.
fn without_leading_bom(raw: &str) -> Option<&str> {
    let rest = raw.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
    rest.strip_prefix('\u{feff}')
}

fn with_unique_keys(
    text: &str,
    value: Value,
    mut repairs: Vec<Rewrite>,
) -> Result<(Value, Vec<Rewrite>), RepairError> {
    for detail in syntax::duplicate_key_notes(text)? {
        repairs.push(note(RewriteKind::Syntax, &detail));
    }
    Ok((value, repairs))
}

fn note(kind: RewriteKind, detail: &str) -> Rewrite {
    Rewrite {
        kind,
        detail: detail.to_string(),
    }
}

fn is_blank_input(raw: &str) -> bool {
    raw.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}')
        .is_empty()
}

fn schema_types(schema: &Value) -> Vec<&str> {
    fn names(type_value: &Value) -> Vec<&str> {
        if let Some(kind) = type_value.as_str() {
            return vec![kind];
        }
        type_value
            .as_array()
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }
    if let Some(type_value) = schema.get("type") {
        let found = names(type_value);
        if !found.is_empty() {
            return found;
        }
    }
    schema
        .get("parameters")
        .and_then(|params| params.get("type"))
        .map(names)
        .unwrap_or_default()
}

/// Blank tool arguments are an empty object or an empty array.
/// A string, number, or boolean is not invented. A union of object and
/// array is not guessed.
fn blank_value(schema: Option<&Value>) -> Result<Value, RepairError> {
    let Some(schema) = schema else {
        return Ok(Value::Object(serde_json::Map::new()));
    };
    let types = schema_types(schema);
    if types.is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let object = types.contains(&"object");
    let array = types.contains(&"array");
    match (object, array) {
        (true, false) => Ok(Value::Object(serde_json::Map::new())),
        (false, true) => Ok(Value::Array(Vec::new())),
        (true, true) => Err(RepairError::Ambiguous(
            "blank input matches both an object and an array".into(),
        )),
        (false, false) if types.iter().all(|ty| *ty == "null") => Ok(Value::Null),
        (false, false) => Err(RepairError::Unfixable(
            "blank input does not match the schema type".into(),
        )),
    }
}

/// Parse `raw`, then apply [`normalize`] when a schema is present.
///
/// Blank input is `{}`, `[]`, or null, matching the schema type.
pub fn repair_args(request: &RepairRequest<'_>) -> Result<Repaired, RepairError> {
    if is_blank_input(request.raw) {
        let value = blank_value(request.schema).map_err(|err| named(request.tool_name, err))?;
        let detail = match &value {
            Value::Array(_) => "empty input is []",
            Value::Null => "empty input is null",
            _ => "empty input is {}",
        };
        let mut repaired = Repaired {
            value,
            rewrites: vec![Rewrite {
                kind: RewriteKind::Empty,
                detail: detail.into(),
            }],
        };
        if let Some(schema) = request.schema {
            let normalized =
                normalize(&repaired.value, schema).map_err(|err| named(request.tool_name, err))?;
            repaired.value = normalized.value;
            repaired.rewrites.extend(normalized.rewrites);
        }
        return Ok(repaired);
    }

    let (value, mut rewrites) = match salvage_value(request.raw) {
        Ok(parsed) => parsed,
        Err(err) => return Err(named(request.tool_name, err)),
    };
    if let Some(schema) = request.schema {
        let normalized = normalize(&value, schema).map_err(|err| named(request.tool_name, err))?;
        rewrites.extend(normalized.rewrites);
        return Ok(Repaired {
            value: normalized.value,
            rewrites,
        });
    }
    Ok(Repaired { value, rewrites })
}

fn named(tool_name: &str, err: RepairError) -> RepairError {
    if tool_name.is_empty() {
        return err;
    }
    let reason = err.to_string();
    let text = format!("tool '{tool_name}': {reason}");
    match err {
        RepairError::Unfixable(_) => RepairError::Unfixable(text),
        RepairError::Ambiguous(_) => RepairError::Ambiguous(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unclosed_fence_unwraps_when_the_body_is_json() {
        let raw = "```json\n{file: \"a.rs\"}";
        let repaired = repair_args(&RepairRequest {
            tool_name: "edit_file",
            raw,
            schema: None,
        })
        .expect("unclosed fence");
        assert_eq!(repaired.value["file"], "a.rs");
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|item| item.kind == RewriteKind::Fence)
        );
        assert!(matches!(
            repair_json("```json\n{\"a\": 1} {\"b\": 2}"),
            RepairOutcome::Unfixable(_)
        ));
    }

    #[test]
    fn fence_with_trailing_comment_unwraps() {
        for raw in [
            "```json\n{\"a\": 1}\n``` # done",
            "```json\n{\"a\": 1}\n```\n// done",
        ] {
            match repair_json(raw) {
                RepairOutcome::Repaired { value, .. } => assert_eq!(value["a"], 1, "{raw:?}"),
                other => panic!("{raw:?}: {other:?}"),
            }
        }
        assert!(matches!(
            repair_json("```json\n{\"a\": 1}\n```\n{\"b\": 2}"),
            RepairOutcome::Unfixable(_)
        ));
    }

    #[test]
    fn fence_with_jsonc_tag_unwraps() {
        let raw = "```jsonc\n{file: \"a.rs\"}\n```";
        let repaired = repair_args(&RepairRequest {
            tool_name: "edit_file",
            raw,
            schema: None,
        })
        .expect("jsonc");
        assert_eq!(repaired.value["file"], "a.rs");
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|item| item.kind == RewriteKind::Fence)
        );
    }

    #[test]
    fn fence_then_bare_key() {
        let raw = "```json\n{file: \"a.rs\"}\n```";
        match repair_json(raw) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "a.rs");
                assert!(
                    repairs.iter().any(|item| item.kind == RewriteKind::Fence),
                    "{repairs:?}"
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn truncated_object_closes() {
        match repair_json(r#"{"file": "a.rs""#) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "a.rs");
                assert!(
                    repairs
                        .iter()
                        .any(|item| item.kind == RewriteKind::ClosedTruncation)
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn open_string_stays_unfixable() {
        assert!(matches!(
            repair_json("{\"path\": \"src/mai"),
            RepairOutcome::Unfixable(_)
        ));
    }

    #[test]
    fn missing_value_stays_unfixable() {
        assert!(matches!(
            repair_json(r#"{"file":"#),
            RepairOutcome::Unfixable(_)
        ));
    }

    #[test]
    fn xml_bare_scalars_stay_strings() {
        let truth = repair_json(r#"<parameter name="file">true</parameter>"#);
        let number = repair_json(r#"<parameter name="file">1</parameter>"#);
        match (truth, number) {
            (
                RepairOutcome::Repaired { value: left, .. },
                RepairOutcome::Repaired { value: right, .. },
            ) => {
                assert_eq!(left["file"], "true");
                assert_eq!(right["file"], "1");
            }
            other => panic!("expected both repaired, got {other:?}"),
        }
    }

    #[test]
    fn xml_containers_still_parse() {
        let raw = r#"<parameter name="meta">{"ok":true}</parameter><parameter name="items">[1]</parameter><parameter name="note">"quoted"</parameter>"#;
        match repair_json(raw) {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["meta"]["ok"], true);
                assert_eq!(value["items"][0], 1);
                assert_eq!(value["note"], "quoted");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn xml_boolean_string_coerces_when_the_schema_says_boolean() {
        let schema = json!({"type": "object", "properties": {"force": {"type": "boolean"}}});
        let repaired = repair_args(&RepairRequest {
            tool_name: "edit_file",
            raw: r#"<parameter name="force">true</parameter>"#,
            schema: Some(&schema),
        })
        .expect("schema coerce");
        assert_eq!(repaired.value["force"], true);
    }

    #[test]
    fn xml_parameter_forms() {
        let named = repair_json(r#"<parameter name="dirPath">src</parameter>"#);
        let equals = repair_json(r#"<parameter=dirPath>src</parameter>"#);
        match (named, equals) {
            (
                RepairOutcome::Repaired { value: left, .. },
                RepairOutcome::Repaired { value: right, .. },
            ) => {
                assert_eq!(left["dirPath"], "src");
                assert_eq!(right["dirPath"], "src");
            }
            other => panic!("expected both repaired, got {other:?}"),
        }
    }

    #[test]
    fn repeated_xml_parameter_is_ambiguous() {
        let raw = r#"<parameter name="file">a</parameter><parameter name="file">b</parameter>"#;
        let err = repair_args(&RepairRequest {
            tool_name: "edit_file",
            raw,
            schema: None,
        })
        .unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        assert!(matches!(repair_json(raw), RepairOutcome::Ambiguous(_)));
    }

    #[test]
    fn duplicate_json_key_with_different_values_is_ambiguous() {
        for raw in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":1,"b":{"a":9},"a":3}"#,
            r#"[{"a":1,"a":2}]"#,
            r#"{"ü":1,"ü":2}"#,
            "{\"a\\\"b\":1,\"a\\\"b\":2}",
            r#"{"a":1,"a":2"#,
        ] {
            assert!(
                matches!(repair_json(raw), RepairOutcome::Ambiguous(_)),
                "{raw:?} -> {:?}",
                repair_json(raw)
            );
        }
        let err = repair_args(&RepairRequest {
            tool_name: "edit_file",
            raw: r#"{"a":1,"a":2}"#,
            schema: None,
        })
        .unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        match repair_json(r#"{"a":{"a":1},"b":2}"#) {
            RepairOutcome::Valid(value) => {
                assert_eq!(value["a"]["a"], 1);
                assert_eq!(value["b"], 2);
            }
            other => panic!("expected Valid, got {other:?}"),
        }
        match repair_json(r#"{"msg":"{\"a\":1,\"a\":2}"}"#) {
            RepairOutcome::Valid(value) => assert_eq!(value["msg"], r#"{"a":1,"a":2}"#),
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_json_key_with_the_same_value_is_recorded() {
        match repair_json(r#"{"a":1,"a":1}"#) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["a"], 1);
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail == "dropped duplicate key a")
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        match repair_json(r#"{a: 1, a: 1}"#) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["a"], 1);
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail.contains("bare keys"))
                );
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail == "dropped duplicate key a")
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        match repair_json(r#"{"a":{"x":1,"y":2},"a":{"y":2,"x":1}}"#) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["a"]["x"], 1);
                assert_eq!(value["a"]["y"], 2);
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail == "dropped duplicate key a")
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        match repair_json("{\"a\":1,\"a\":1") {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["a"], 1);
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail.contains("closed truncated"))
                );
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail == "dropped duplicate key a")
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn trailing_text_after_xml_parameters_is_unfixable() {
        let raw = r#"<parameter name="file">a.rs</parameter> leftover"#;
        assert!(matches!(repair_json(raw), RepairOutcome::Unfixable(_)));
        let between = r#"<parameter name="a">1</parameter> junk <parameter name="b">2</parameter>"#;
        assert!(matches!(repair_json(between), RepairOutcome::Unfixable(_)));
    }

    #[test]
    fn whitespace_after_xml_parameters_is_kept() {
        let raw = "<parameter name=\"file\">a.rs</parameter>\n";
        match repair_json(raw) {
            RepairOutcome::Repaired { value, .. } => assert_eq!(value["file"], "a.rs"),
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn unfixable_json_keeps_the_serde_error() {
        match repair_json("garbage") {
            RepairOutcome::Unfixable(reason) => {
                assert!(
                    reason.starts_with("parse failed after all repairs:"),
                    "{reason}"
                );
                assert!(reason.contains("expected"), "{reason}");
            }
            other => panic!("expected Unfixable, got {other:?}"),
        }
    }

    #[test]
    fn parameter_tag_inside_prose_is_not_xml() {
        let raw = "not json <parameter name=\"file\">a</parameter>";
        assert!(matches!(repair_json(raw), RepairOutcome::Unfixable(_)));
    }

    #[test]
    fn path_alias_renames_to_schema_property() {
        let schema = json!({"type": "object", "properties": {"file": {"type": "string"}}});
        let repaired = repair_args(&RepairRequest {
            tool_name: "edit_file",
            raw: r#"{"path": "a.rs"}"#,
            schema: Some(&schema),
        })
        .expect("alias");
        assert_eq!(repaired.value["file"], "a.rs");
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|item| item.kind == RewriteKind::Alias)
        );
    }

    #[test]
    fn two_path_properties_are_ambiguous() {
        let schema = json!({
            "type": "object",
            "properties": {
                "file": {"type": "string"},
                "path": {"type": "string"}
            }
        });
        let err = normalize(&json!({"filepath": "a.rs"}), &schema).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)));
    }

    #[test]
    fn whitespace_around_a_typed_string_is_trimmed() {
        let schema = json!({
            "type": "object",
            "properties": {
                "ok": {"type": "boolean"},
                "n": {"type": "integer"}
            }
        });
        let repaired = normalize(&json!({"ok": " true ", "n": " 3\n"}), &schema).expect("trim");
        assert_eq!(repaired.value["ok"], true);
        assert_eq!(repaired.value["n"], 3);
        let repaired = normalize(&json!({"ok": "true story", "n": " 01 "}), &schema).expect("keep");
        assert_eq!(repaired.value["ok"], "true story");
        assert_eq!(repaired.value["n"], " 01 ");
    }

    #[test]
    fn boolean_and_integer_strings() {
        let schema = json!({
            "type": "object",
            "properties": {
                "force": {"type": "boolean"},
                "line": {"type": "integer"}
            }
        });
        let repaired = normalize(&json!({"force": "true", "line": "3"}), &schema).expect("coerce");
        assert_eq!(repaired.value["force"], true);
        assert_eq!(repaired.value["line"], 3);
    }

    #[test]
    fn type_union_coerces_when_string_is_absent() {
        let schema = json!({
            "type": "object",
            "properties": {
                "force": {"type": ["boolean", "null"]},
                "line": {"type": ["integer", "null"]},
                "ratio": {"type": ["number", "null"]}
            }
        });
        let repaired = normalize(
            &json!({"force": "false", "line": "4", "ratio": "1.5"}),
            &schema,
        )
        .expect("union");
        assert_eq!(repaired.value["force"], false);
        assert_eq!(repaired.value["line"], 4);
        assert_eq!(repaired.value["ratio"], 1.5);
    }

    #[test]
    fn type_union_with_string_keeps_the_text() {
        let schema = json!({
            "type": "object",
            "properties": {
                "force": {"type": ["string", "boolean"]}
            }
        });
        let repaired = normalize(&json!({"force": "true"}), &schema).expect("keep");
        assert_eq!(repaired.value["force"], "true");
        assert!(repaired.rewrites.is_empty());
    }

    #[test]
    fn two_unknown_keys_are_refused() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {"file": {"type": "string"}},
            "required": ["file"]
        });
        let err = normalize(&json!({"file": "a.rs", "a": 1, "b": 2}), &schema).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)));
    }

    #[test]
    fn unknown_key_stays_unfixed_when_required_is_missing() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {"file": {"type": "string"}},
            "required": ["file"]
        });
        let err = normalize(&json!({"note": "x"}), &schema).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
    }

    #[test]
    fn only_unknown_key_is_not_dropped() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {"file": {"type": "string"}}
        });
        let err = normalize(&json!({"note": "x"}), &schema).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
    }

    #[test]
    fn additional_property_schema_coerces_unknown_values() {
        let schema = json!({
            "additionalProperties": {"type": "integer"}
        });
        let repaired = normalize(&json!({"n": "1", "m": "2"}), &schema).expect("coerce");
        assert_eq!(repaired.value["n"], 1);
        assert_eq!(repaired.value["m"], 2);
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|rewrite| rewrite.detail.contains("integer"))
        );

        let mixed = json!({
            "properties": {"flag": {"type": "boolean"}},
            "additionalProperties": {"type": "integer"}
        });
        let repaired = normalize(&json!({"flag": "true", "n": "3"}), &mixed).expect("coerce");
        assert_eq!(repaired.value["flag"], true);
        assert_eq!(repaired.value["n"], 3);

        let nested = json!({
            "additionalProperties": {
                "type": "object",
                "properties": {"n": {"type": "integer"}}
            }
        });
        let repaired = normalize(&json!({"child": {"n": "4"}}), &nested).expect("coerce");
        assert_eq!(repaired.value["child"]["n"], 4);

        let kept = normalize(&json!({"n": "hello"}), &schema).expect("keep");
        assert_eq!(kept.value["n"], "hello");
        assert!(kept.rewrites.is_empty());

        let parameters = json!({
            "parameters": {"additionalProperties": {"type": "integer"}}
        });
        let repaired = normalize(&json!({"n": "8"}), &parameters).expect("parameters");
        assert_eq!(repaired.value["n"], 8);

        let open = json!({"additionalProperties": true});
        let repaired = normalize(&json!({"n": "1"}), &open).expect("open");
        assert_eq!(repaired.value["n"], "1");
        assert!(repaired.rewrites.is_empty());

        let patterned = json!({
            "additionalProperties": {"type": "integer"},
            "patternProperties": {"^x-": {"type": "string"}}
        });
        let err = normalize(&json!({"x-note": "1"}), &patterned).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
    }

    #[test]
    fn pattern_properties_are_not_dropped() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {"file": {"type": "string"}},
            "required": ["file"],
            "patternProperties": {"^x-": {"type": "string"}}
        });
        let err = normalize(&json!({"file": "a.rs", "x-note": "hi"}), &schema).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
    }

    #[test]
    fn one_unknown_key_is_dropped_when_required_is_present() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {"file": {"type": "string"}},
            "required": ["file"]
        });
        let repaired = normalize(&json!({"file": "a.rs", "note": "x"}), &schema).expect("drop");
        assert!(repaired.value.get("note").is_none());
        assert_eq!(repaired.value["file"], "a.rs");
    }

    #[test]
    fn valid_json_without_schema_is_not_rewritten() {
        let repaired = repair_args(&RepairRequest {
            tool_name: "edit_file",
            raw: r#"{"path": "a.rs"}"#,
            schema: None,
        })
        .expect("valid");
        assert!(repaired.rewrites.is_empty());
        assert_eq!(repaired.value["path"], "a.rs");
    }

    #[test]
    fn blank_input_follows_the_schema_type() {
        let array = json!({"type": "array", "items": {"type": "string"}});
        let repaired = repair_args(&RepairRequest {
            tool_name: "list",
            raw: " \n",
            schema: Some(&array),
        })
        .expect("array");
        assert_eq!(repaired.value, json!([]));
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|item| { item.kind == RewriteKind::Empty && item.detail.contains("[]") })
        );

        let text = json!({"type": "string"});
        let err = repair_args(&RepairRequest {
            tool_name: "echo",
            raw: "",
            schema: Some(&text),
        })
        .unwrap_err();
        assert!(
            matches!(err, RepairError::Unfixable(ref reason) if reason.contains("echo")),
            "{err:?}"
        );

        let number = json!({"type": "integer"});
        let err = repair_args(&RepairRequest {
            tool_name: "count",
            raw: "\u{feff}",
            schema: Some(&number),
        })
        .unwrap_err();
        assert!(matches!(err, RepairError::Unfixable(_)), "{err:?}");

        let either = json!({"type": ["object", "array"]});
        let err = repair_args(&RepairRequest {
            tool_name: "box",
            raw: "",
            schema: Some(&either),
        })
        .unwrap_err();
        assert!(
            matches!(err, RepairError::Ambiguous(ref reason) if reason.contains("box")),
            "{err:?}"
        );

        let array_or_null = json!({"type": ["array", "null"]});
        let repaired = repair_args(&RepairRequest {
            tool_name: "list",
            raw: "",
            schema: Some(&array_or_null),
        })
        .expect("array or null");
        assert_eq!(repaired.value, json!([]));
    }

    #[test]
    fn blank_input_is_empty_object() {
        for raw in ["", "   ", "\n\t", "\u{feff}"] {
            let repaired = repair_args(&RepairRequest {
                tool_name: "edit_file",
                raw,
                schema: None,
            })
            .unwrap_or_else(|err| panic!("{raw:?}: {err}"));
            assert_eq!(repaired.value, json!({}), "{raw:?}");
            assert!(
                repaired
                    .rewrites
                    .iter()
                    .any(|item| item.kind == RewriteKind::Empty),
                "{raw:?} {repaired:?}"
            );
        }
    }

    #[test]
    fn garbage_stays_unfixable() {
        assert!(matches!(
            repair_json("this is not json at all }{[]"),
            RepairOutcome::Unfixable(_)
        ));
        assert!(matches!(repair_json(""), RepairOutcome::Unfixable(_)));
    }

    #[test]
    fn inexact_decimal_string_stays_a_string() {
        let schema = json!({"type": "object", "properties": {"n": {"type": "number"}}});
        let repaired = normalize(&json!({"n": "9007199254740993.5"}), &schema).expect("keep");
        assert_eq!(repaired.value["n"], "9007199254740993.5");
        assert!(repaired.rewrites.is_empty(), "{:?}", repaired.rewrites);
    }

    #[test]
    fn scientific_number_string_coerces_when_exact() {
        let schema = json!({"type": "object", "properties": {"n": {"type": "number"}}});
        let repaired = normalize(&json!({"n": "1e2"}), &schema).expect("exact");
        assert_eq!(repaired.value["n"].as_f64(), Some(100.0));
        let repaired = normalize(&json!({"n": "1.5e1"}), &schema).expect("fifteen");
        assert_eq!(repaired.value["n"].as_f64(), Some(15.0));
        let repaired = normalize(&json!({"n": "1e309"}), &schema).expect("overflow");
        assert_eq!(repaired.value["n"], "1e309");
        assert!(repaired.rewrites.is_empty());

        let integers = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
        let repaired = normalize(&json!({"n": "1e2"}), &integers).expect("not an integer token");
        assert_eq!(repaired.value["n"], "1e2");
    }

    #[test]
    fn exact_decimal_string_is_still_coerced() {
        let schema = json!({
            "type": "object",
            "properties": {
                "tenth": {"type": "number"},
                "half": {"type": "number"}
            }
        });
        let repaired = normalize(&json!({"tenth": "0.1", "half": "1.5"}), &schema).expect("coerce");
        assert_eq!(repaired.value["tenth"], json!(0.1));
        assert_eq!(repaired.value["half"], json!(1.5));
    }

    #[test]
    fn unclosed_block_comment_does_not_drop_later_keys() {
        let raw = r#"{"file": "a.rs" /* , "cmd": "rm" }"#;
        assert!(matches!(repair_json(raw), RepairOutcome::Unfixable(_)));
        assert!(matches!(
            repair_json(r#"{"a": 1} /*"#),
            RepairOutcome::Unfixable(_)
        ));
    }

    #[test]
    fn bare_opener_is_not_an_empty_container() {
        assert!(matches!(repair_json("{"), RepairOutcome::Unfixable(_)));
        assert!(matches!(repair_json("["), RepairOutcome::Unfixable(_)));
        assert!(matches!(repair_json("[{"), RepairOutcome::Unfixable(_)));
        assert!(matches!(repair_json("[1,"), RepairOutcome::Unfixable(_)));
    }

    #[test]
    fn complete_array_still_closes() {
        match repair_json("[1, 2") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value, json!([1, 2]));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn xml_truncated_container_is_unfixable() {
        let raw = r#"<parameter name="meta">{</parameter>"#;
        assert!(matches!(repair_json(raw), RepairOutcome::Unfixable(_)));
    }

    #[test]
    fn xml_json_container_is_repaired_not_stringified() {
        let raw = r#"<parameter name="meta">{ "ok": true, }</parameter><parameter name="file">a.rs</parameter>"#;
        match repair_json(raw) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["meta"]["ok"], true);
                assert_eq!(value["file"], "a.rs");
                assert!(
                    repairs.iter().any(|item| item.kind == RewriteKind::Syntax),
                    "{repairs:?}"
                );
                assert!(
                    repairs
                        .iter()
                        .any(|item| item.kind == RewriteKind::XmlParameters)
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn single_quoted_apostrophe_and_newline() {
        match repair_json(r"{'a': 'it\'s // fine'}") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["a"], "it's // fine");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        match repair_json("{'msg': 'line1\nline2'}") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["msg"], "line1\nline2");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn python_literals_become_json_values() {
        match repair_json("{a: True, b: False, c: None, items: [True, None]}") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["a"], true);
                assert_eq!(value["b"], false);
                assert_eq!(value["c"], json!(null));
                assert_eq!(value["items"][0], true);
                assert_eq!(value["items"][1], json!(null));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        match repair_json("{True: 1}") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["True"], 1);
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        assert!(matches!(
            repair_json(r#"{"a": "True"}"#),
            RepairOutcome::Valid(_)
        ));
    }

    #[test]
    fn closed_object_schema_with_no_properties_rejects_keys() {
        let schema = json!({"type": "object", "additionalProperties": false});
        let err = normalize(&json!({"a": 1}), &schema).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let err = normalize(&json!({"a": 1, "b": 2}), &schema).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!({}), &schema).expect("empty");
        assert_eq!(repaired.value, json!({}));
        assert!(repaired.rewrites.is_empty());

        let nested = json!({
            "type": "object",
            "properties": {
                "child": {"type": "object", "additionalProperties": false}
            }
        });
        let err = normalize(&json!({"child": {"x": 1}}), &nested).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!({"child": {}}), &nested).expect("empty child");
        assert_eq!(repaired.value["child"], json!({}));
    }

    #[test]
    fn unicode_nbsp_around_the_document_is_stripped() {
        let raw = "\u{a0}{\"a\":1}\u{a0}";
        match repair_json(raw) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["a"], 1);
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail.contains("whitespace"))
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        match repair_json("\n{\"a\":1}") {
            RepairOutcome::Valid(value) => assert_eq!(value["a"], 1),
            other => panic!("expected Valid, got {other:?}"),
        }
        match repair_json("{\"a\":\"\u{a0}\"}") {
            RepairOutcome::Valid(value) => assert_eq!(value["a"], "\u{a0}"),
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn blank_unicode_whitespace_follows_the_schema_type() {
        let repaired = repair_args(&RepairRequest {
            tool_name: "edit_file",
            raw: "\u{a0}",
            schema: None,
        })
        .expect("object");
        assert_eq!(repaired.value, json!({}));
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|rewrite| rewrite.detail.contains("{}"))
        );

        let array = json!({"type": "array"});
        let repaired = repair_args(&RepairRequest {
            tool_name: "list",
            raw: "\u{a0}\n",
            schema: Some(&array),
        })
        .expect("array");
        assert_eq!(repaired.value, json!([]));
    }

    #[test]
    fn unicode_nbsp_around_a_typed_string_is_trimmed() {
        let schema = json!({
            "type": "object",
            "properties": {
                "ok": {"type": "boolean"},
                "n": {"type": "integer"}
            }
        });
        let repaired = normalize(
            &json!({"ok": "\u{a0}true\u{a0}", "n": "\u{a0}01\u{a0}"}),
            &schema,
        )
        .expect("nbsp");
        assert_eq!(repaired.value["ok"], true);
        assert_eq!(repaired.value["n"], "\u{a0}01\u{a0}");
    }

    #[test]
    fn leading_bom_is_stripped_and_recorded() {
        for raw in ["\u{feff}{\"a\": 1}", " \u{feff}{\"a\": 1}"] {
            match repair_json(raw) {
                RepairOutcome::Repaired { value, repairs } => {
                    assert_eq!(value["a"], 1);
                    assert!(
                        repairs.iter().any(|item| item.detail.contains("BOM")),
                        "{repairs:?}"
                    );
                }
                other => panic!("expected Repaired, got {other:?}"),
            }
        }
    }

    #[test]
    fn bom_inside_a_string_is_not_rewritten() {
        let raw = "{\"a\": \"\u{feff}\"}";
        match repair_json(raw) {
            RepairOutcome::Valid(value) => assert_eq!(value["a"], "\u{feff}"),
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn hash_comment_is_stripped_outside_strings() {
        match repair_json("{a: 1} # tail") {
            RepairOutcome::Repaired { value, .. } => assert_eq!(value["a"], 1),
            other => panic!("expected Repaired, got {other:?}"),
        }
        match repair_json("{a: \"hash # stay\",}") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["a"], "hash # stay");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        assert!(matches!(
            repair_json(r#"{"a": "hash # stay"}"#),
            RepairOutcome::Valid(_)
        ));
    }

    #[test]
    fn dotted_bare_key_is_quoted() {
        match repair_json(r#"{file.name: "a.rs", a.b.c: true}"#) {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["file.name"], "a.rs");
                assert_eq!(value["a.b.c"], true);
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        match repair_json("{n: 1.2}") {
            RepairOutcome::Repaired { value, .. } => assert_eq!(value["n"], 1.2),
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn numeric_underscores_are_removed_outside_strings() {
        match repair_json("{a: 1_000, b: 2.5_0}") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["a"], 1000);
                assert_eq!(value["b"], 2.5);
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        assert!(matches!(
            repair_json("{a: 1__0}"),
            RepairOutcome::Unfixable(_)
        ));
        assert!(matches!(
            repair_json(r#"{"a": "1_000"}"#),
            RepairOutcome::Valid(_)
        ));
        let schema = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
        let repaired = normalize(&json!({"n": "1_000"}), &schema).expect("coerce");
        assert_eq!(repaired.value["n"], 1000);
    }

    #[test]
    fn hyphenated_bare_key_is_quoted() {
        match repair_json(r#"{file-path: "a.rs"}"#) {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["file-path"], "a.rs");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        assert!(matches!(
            repair_json("{n: 1-2}"),
            RepairOutcome::Unfixable(_)
        ));
    }

    #[test]
    fn dollar_ref_key_is_quoted() {
        match repair_json(r##"{$ref: "#/defs/a", file: "a.rs"}"##) {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["$ref"], "#/defs/a");
                assert_eq!(value["file"], "a.rs");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        assert!(matches!(
            repair_json(r##"{"$ref": "#/defs/a"}"##),
            RepairOutcome::Valid(_)
        ));
        assert!(matches!(
            repair_json(r#"{file: $ref}"#),
            RepairOutcome::Unfixable(_)
        ));
    }

    #[test]
    fn single_quoted_url_is_not_cut_at_the_slashes() {
        match repair_json("{'url': 'http://example.com/a'}") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["url"], "http://example.com/a");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn leading_zero_integer_string_is_not_coerced() {
        let schema = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
        let repaired = normalize(&json!({"n": "01"}), &schema).expect("keep");
        assert_eq!(repaired.value["n"], "01");
        assert!(repaired.rewrites.is_empty());
        let repaired = normalize(&json!({"n": "+2"}), &schema).expect("plus");
        assert_eq!(repaired.value["n"], 2);
    }

    #[test]
    fn dependent_schema_applies_when_the_property_is_present() {
        let schema = json!({
            "dependentSchemas": {
                "flag": {"properties": {"n": {"type": "integer"}}}
            }
        });
        let repaired = normalize(&json!({"flag": true, "n": "1"}), &schema).expect("present");
        assert_eq!(repaired.value["n"], 1);
        assert_eq!(repaired.value["flag"], true);

        let kept = normalize(&json!({"n": "1"}), &schema).expect("absent");
        assert_eq!(kept.value["n"], "1");
        assert!(kept.rewrites.is_empty());

        let dependencies = json!({
            "dependencies": {
                "flag": {"properties": {"n": {"type": "integer"}}}
            }
        });
        let repaired =
            normalize(&json!({"flag": true, "n": "2"}), &dependencies).expect("dependencies");
        assert_eq!(repaired.value["n"], 2);

        let names = json!({
            "dependencies": {"flag": ["n"]}
        });
        let kept = normalize(&json!({"flag": true}), &names).expect("names");
        assert!(kept.value.get("n").is_none());
        assert!(kept.rewrites.is_empty());

        let clash = json!({
            "dependentSchemas": {
                "flag": {"properties": {"n": {"type": "integer"}}},
                "other": {"properties": {"n": {"type": "boolean"}}}
            }
        });
        let kept =
            normalize(&json!({"flag": true, "other": true, "n": "1"}), &clash).expect("clash");
        assert_eq!(kept.value["n"], "1");
        assert!(kept.rewrites.is_empty());
    }

    #[test]
    fn size_limits_reject_a_coercion_past_the_bound() {
        let props = json!({
            "properties": {
                "a": {"type": "integer"},
                "c": {"type": "integer"}
            },
            "maxProperties": 1
        });
        let err = normalize(&json!({"a": "1", "c": "2"}), &props).expect_err("properties");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let already = normalize(&json!({"a": 1, "c": 2}), &props).expect("already");
        assert_eq!(already.value["a"], 1);
        assert!(already.rewrites.is_empty());

        let dropped = json!({
            "properties": {"a": {"type": "integer"}},
            "additionalProperties": false,
            "maxProperties": 1
        });
        let repaired = normalize(&json!({"a": "1", "b": 2}), &dropped).expect("drop");
        assert_eq!(repaired.value, json!({"a": 1}));

        let items = json!({
            "items": {"type": "integer"},
            "maxItems": 1
        });
        let err = normalize(&json!(["1", "2"]), &items).expect_err("items");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!(["1"]), &items).expect("one");
        assert_eq!(repaired.value, json!([1]));

        let short = json!({
            "items": {"type": "integer"},
            "minItems": 2
        });
        let err = normalize(&json!(["1"]), &short).expect_err("short");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!(["1", "2"]), &short).expect("long enough");
        assert_eq!(repaired.value, json!([1, 2]));
    }

    #[test]
    fn nested_exact_float_matches_one_const_number() {
        let schema = json!({"const": {"a": 1}});
        let value: serde_json::Value = serde_json::from_str(r#"{"a": 1.0}"#).unwrap();
        let repaired = normalize(&value, &schema).expect("nested");
        assert_eq!(repaired.value["a"], 1);
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|rewrite| rewrite.detail.contains("const"))
        );

        let row = json!({"const": [1, 2]});
        let value: serde_json::Value = serde_json::from_str("[1.0, 2.0]").unwrap();
        let repaired = normalize(&value, &row).expect("row");
        assert_eq!(repaired.value, json!([1, 2]));

        let both = json!({"enum": [{"a": 1}, {"a": 1.0}]});
        let value: serde_json::Value = serde_json::from_str(r#"{"a": 1.0}"#).unwrap();
        let kept = normalize(&value, &both).expect("both");
        assert_eq!(kept.value["a"].as_f64(), Some(1.0));
        assert!(kept.rewrites.is_empty());

        let already = normalize(&json!({"a": 1}), &schema).expect("already");
        assert_eq!(already.value["a"], 1);
        assert!(already.rewrites.is_empty());
    }

    #[test]
    fn const_object_coerces_when_one_member_matches() {
        let schema = json!({"enum": [{"a": 1}, {"a": 2}]});
        let repaired = normalize(&json!({"a": "1"}), &schema).expect("one");
        assert_eq!(repaired.value, json!({"a": 1}));
        let kept = normalize(&json!({"a": "3"}), &schema).expect("none");
        assert_eq!(kept.value["a"], "3");
        assert!(kept.rewrites.is_empty());

        let both = json!({"enum": [{"a": 1}, {"a": "1"}]});
        let kept = normalize(&json!({"a": "1"}), &both).expect("both");
        assert_eq!(kept.value["a"], "1");
        assert!(kept.rewrites.is_empty());

        let row = json!({"const": [1, true]});
        let repaired = normalize(&json!(["1", "true"]), &row).expect("row");
        assert_eq!(repaired.value, json!([1, true]));

        let already = normalize(&json!({"a": 1}), &schema).expect("already");
        assert_eq!(already.value, json!({"a": 1}));
        assert!(already.rewrites.is_empty());

        let typed = json!({
            "type": "object",
            "properties": {"a": {"type": "integer"}},
            "enum": [{"a": 1}, {"a": 2}]
        });
        let err = normalize(&json!({"a": "3"}), &typed).expect_err("outside");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
    }

    #[test]
    fn contains_rejects_a_coercion_that_drops_the_match() {
        let dropped = json!({
            "type": "array",
            "items": {"type": "integer"},
            "contains": {"const": "1"}
        });
        let err = normalize(&json!(["1"]), &dropped).expect_err("dropped");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");

        let made = json!({
            "type": "array",
            "items": {"type": "integer"},
            "contains": {"const": 1}
        });
        let repaired = normalize(&json!(["1"]), &made).expect("made");
        assert_eq!(repaired.value, json!([1]));
        let already = normalize(&json!([1]), &made).expect("already");
        assert_eq!(already.value, json!([1]));
        assert!(already.rewrites.is_empty());

        let capped = json!({
            "type": "array",
            "items": {"type": "integer"},
            "contains": {"const": 1},
            "maxContains": 1
        });
        let err = normalize(&json!(["1", "1"]), &capped).expect_err("two");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!(["1", "2"]), &capped).expect("one");
        assert_eq!(repaired.value, json!([1, 2]));

        let at_least = json!({
            "type": "array",
            "items": {"type": "integer"},
            "contains": {"const": 1},
            "minContains": 2
        });
        let err = normalize(&json!(["1", "2"]), &at_least).expect_err("need two");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!(["1", "1"]), &at_least).expect("two ones");
        assert_eq!(repaired.value, json!([1, 1]));

        let optional = json!({
            "type": "array",
            "items": {"type": "integer"},
            "contains": {"const": 1},
            "minContains": 0
        });
        let repaired = normalize(&json!(["2"]), &optional).expect("optional");
        assert_eq!(repaired.value, json!([2]));

        let branches = json!({
            "allOf": [
                {"items": {"type": "integer"}},
                {"contains": {"const": "1"}}
            ]
        });
        let kept = normalize(&json!(["1"]), &branches).expect("allof");
        assert_eq!(kept.value, json!(["1"]));
        assert!(kept.rewrites.is_empty());
    }

    #[test]
    fn applicator_honors_a_constraint_on_every_branch() {
        let bounds = json!({
            "allOf": [{"type": "integer"}, {"minimum": 0}]
        });
        let kept = normalize(&json!("-1"), &bounds).expect("below");
        assert_eq!(kept.value, "-1");
        assert!(kept.rewrites.is_empty());
        let repaired = normalize(&json!("1"), &bounds).expect("inside");
        assert_eq!(repaired.value, 1);

        let excluded = json!({
            "allOf": [{"type": "integer"}, {"not": {"const": 1}}]
        });
        let kept = normalize(&json!("1"), &excluded).expect("excluded");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());
        let repaired = normalize(&json!("2"), &excluded).expect("other");
        assert_eq!(repaired.value, 2);

        let sides = json!({
            "oneOf": [
                {"type": "integer", "minimum": 0},
                {"type": "integer", "maximum": -1}
            ]
        });
        let repaired = normalize(&json!("1"), &sides).expect("positive");
        assert_eq!(repaired.value, 1);
        let repaired = normalize(&json!("-5"), &sides).expect("negative");
        assert_eq!(repaired.value, -5);

        let branch = json!({
            "if": {"type": "integer", "minimum": 0},
            "else": {"type": "integer", "maximum": -1}
        });
        let repaired = normalize(&json!("4"), &branch).expect("if");
        assert_eq!(repaired.value, 4);
        let repaired = normalize(&json!("-2"), &branch).expect("else");
        assert_eq!(repaired.value, -2);
    }

    #[test]
    fn unique_items_rejects_a_coercion_that_makes_duplicates() {
        let schema = json!({
            "type": "array",
            "items": {"type": "integer"},
            "uniqueItems": true
        });
        let err = normalize(&json!(["1", 1]), &schema).expect_err("duplicate");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!(["1", "2"]), &schema).expect("distinct");
        assert_eq!(repaired.value, json!([1, 2]));
        let kept = normalize(&json!([1, 1]), &schema).expect("already");
        assert_eq!(kept.value, json!([1, 1]));
        assert!(kept.rewrites.is_empty());
    }

    #[test]
    fn decimal_multiple_of_rejects_a_coercion_that_does_not_divide() {
        let schema = json!({"type": "number", "multipleOf": 0.5});
        let err = normalize(&json!("1.25"), &schema).expect_err("quarter");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!("1.5"), &schema).expect("half");
        assert_eq!(repaired.value.as_f64(), Some(1.5));
        let repaired = normalize(&json!("-1.5"), &schema).expect("negative");
        assert_eq!(repaired.value.as_f64(), Some(-1.5));

        let kept = normalize(&json!(1.25), &schema).expect("already");
        assert_eq!(kept.value.as_f64(), Some(1.25));
        assert!(kept.rewrites.is_empty());

        let property = json!({
            "properties": {"n": {"type": "number", "multipleOf": 0.25}}
        });
        let repaired = normalize(&json!({"n": "1.25"}), &property).expect("quarter fits");
        assert_eq!(repaired.value["n"].as_f64(), Some(1.25));
        let bad = json!({
            "properties": {"n": {"type": "number", "multipleOf": 0.5}}
        });
        let err = normalize(&json!({"n": "1.75"}), &bad).expect_err("not half");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
    }

    #[test]
    fn multiple_of_rejects_a_coercion_that_is_not_a_multiple() {
        let schema = json!({"type": "integer", "multipleOf": 2});
        let err = normalize(&json!("3"), &schema).expect_err("odd");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let err = normalize(&json!("-3"), &schema).expect_err("negative");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!("4"), &schema).expect("even");
        assert_eq!(repaired.value, 4);
        let repaired = normalize(&json!("-2"), &schema).expect("neg even");
        assert_eq!(repaired.value, -2);
        let repaired = normalize(&json!("0"), &schema).expect("zero");
        assert_eq!(repaired.value, 0);
        let kept = normalize(&json!(3), &schema).expect("already");
        assert_eq!(kept.value, 3);
        assert!(kept.rewrites.is_empty());

        let zero = json!({"type": "integer", "multipleOf": 0});
        let repaired = normalize(&json!("3"), &zero).expect("zero multiple");
        assert_eq!(repaired.value, 3);

        let property = json!({
            "properties": {"n": {"type": "integer", "multipleOf": 2}}
        });
        let err = normalize(&json!({"n": "5"}), &property).expect_err("property");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
    }

    #[test]
    fn numeric_bounds_reject_a_coercion_outside_the_range() {
        let schema = json!({"type": "integer", "minimum": 0, "maximum": 10});
        let err = normalize(&json!("-1"), &schema).expect_err("below");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let err = normalize(&json!("11"), &schema).expect_err("above");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!("0"), &schema).expect("zero");
        assert_eq!(repaired.value, 0);
        let repaired = normalize(&json!("10"), &schema).expect("ten");
        assert_eq!(repaired.value, 10);
        let kept = normalize(&json!(-1), &schema).expect("already");
        assert_eq!(kept.value, -1);
        assert!(kept.rewrites.is_empty());

        let exclusive = json!({
            "type": "integer",
            "exclusiveMinimum": 0,
            "exclusiveMaximum": 10
        });
        let err = normalize(&json!("0"), &exclusive).expect_err("exclusive min");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let err = normalize(&json!("10"), &exclusive).expect_err("exclusive max");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!("1"), &exclusive).expect("inside");
        assert_eq!(repaired.value, 1);

        let draft4 = json!({
            "type": "integer",
            "minimum": 0,
            "exclusiveMinimum": true
        });
        let err = normalize(&json!("0"), &draft4).expect_err("draft4");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!("1"), &draft4).expect("above zero");
        assert_eq!(repaired.value, 1);

        let property = json!({
            "properties": {"n": {"type": "number", "maximum": 1}}
        });
        let err = normalize(&json!({"n": "1.5"}), &property).expect_err("property");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");

        let text = json!({"type": "integer", "minimum": "0"});
        let repaired = normalize(&json!("-1"), &text).expect("string bound");
        assert_eq!(repaired.value, -1);
    }

    #[test]
    fn not_rejects_a_coercion_that_matches_the_excluded_schema() {
        let schema = json!({"type": "integer", "not": {"const": 1}});
        let err = normalize(&json!("1"), &schema).expect_err("excluded");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!("2"), &schema).expect("allowed");
        assert_eq!(repaired.value, 2);
        let kept = normalize(&json!(1), &schema).expect("already");
        assert_eq!(kept.value, 1);
        assert!(kept.rewrites.is_empty());

        let unknown = json!({"type": "integer", "not": {"maximum": 0}});
        let repaired = normalize(&json!("1"), &unknown).expect("unknown keyword");
        assert_eq!(repaired.value, 1);

        let object = json!({
            "properties": {"n": {"type": "integer"}},
            "not": {"properties": {"n": {"const": 1}}}
        });
        let err = normalize(&json!({"n": "1"}), &object).expect_err("property");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!({"n": "2"}), &object).expect("other");
        assert_eq!(repaired.value["n"], 2);

        let branch = json!({
            "if": {"const": 1},
            "then": {"enum": [1]},
            "not": {"const": 1}
        });
        let err = normalize(&json!("1"), &branch).expect_err("then");
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let kept = normalize(&json!("2"), &branch).expect("miss");
        assert_eq!(kept.value, "2");
        assert!(kept.rewrites.is_empty());
    }

    #[test]
    fn applicator_accepts_only_when_properties_and_required_hold() {
        let schema = json!({
            "if": {
                "properties": {"flag": {"const": true}},
                "required": ["flag"]
            },
            "then": {"properties": {"n": {"type": "integer"}}},
            "else": {"properties": {"n": {"type": "string"}}}
        });
        let repaired = normalize(&json!({"flag": true, "n": "1"}), &schema).expect("then");
        assert_eq!(repaired.value["n"], 1);
        assert_eq!(repaired.value["flag"], true);

        let kept = normalize(&json!({"flag": false, "n": "1"}), &schema).expect("wrong flag");
        assert_eq!(kept.value["n"], "1");
        assert!(kept.rewrites.is_empty());

        let kept = normalize(&json!({"n": "1"}), &schema).expect("missing flag");
        assert_eq!(kept.value["n"], "1");
        assert!(kept.rewrites.is_empty());

        let clash = json!({
            "allOf": [
                {"properties": {"n": {"type": "integer"}}},
                {"properties": {"n": {"type": "boolean"}}}
            ]
        });
        let kept = normalize(&json!({"n": "1"}), &clash).expect("allof");
        assert_eq!(kept.value["n"], "1");
        assert!(kept.rewrites.is_empty());

        let branches = json!({
            "oneOf": [
                {"properties": {"n": {"type": "integer"}}, "required": ["n"]},
                {"properties": {"s": {"type": "string"}}, "required": ["s"]}
            ]
        });
        let repaired = normalize(&json!({"n": "2"}), &branches).expect("one branch");
        assert_eq!(repaired.value["n"], 2);
        let kept = normalize(&json!({"n": "2", "s": "a"}), &branches).expect("two branches");
        assert_eq!(kept.value["n"], "2");
        assert_eq!(kept.value["s"], "a");
        assert!(kept.rewrites.is_empty());

        let rows = json!({
            "if": {"prefixItems": [{"const": "count"}]},
            "then": {"prefixItems": [{}, {"type": "integer"}]},
            "else": {"prefixItems": [{}, {"type": "string"}]}
        });
        let repaired = normalize(&json!(["count", "1"]), &rows).expect("count");
        assert_eq!(repaired.value[1], 1);
        let kept = normalize(&json!(["other", "1"]), &rows).expect("other");
        assert_eq!(kept.value[1], "1");
        assert!(kept.rewrites.is_empty());

        let numbers = json!({
            "if": {"items": {"const": 1}},
            "else": {"items": {"type": "integer"}}
        });
        let repaired = normalize(&json!(["2"]), &numbers).expect("else integer");
        assert_eq!(repaired.value[0], 2);
        let kept = normalize(&json!([1]), &numbers).expect("already");
        assert_eq!(kept.value[0], 1);
        assert!(kept.rewrites.is_empty());
    }

    #[test]
    fn if_then_else_follows_the_matching_branch() {
        let schema = json!({
            "if": {"type": "boolean"},
            "then": {"enum": [true]},
            "else": {"type": "integer"}
        });
        let repaired = normalize(&json!("true"), &schema).expect("then");
        assert_eq!(repaired.value, true);
        let repaired = normalize(&json!("1"), &schema).expect("else");
        assert_eq!(repaired.value, 1);
        let kept = normalize(&json!("hello"), &schema).expect("neither");
        assert_eq!(kept.value, "hello");
        assert!(kept.rewrites.is_empty());

        let only_if = json!({
            "if": {"const": 1},
            "else": {"type": "boolean"}
        });
        let repaired = normalize(&json!("1"), &only_if).expect("if const");
        assert_eq!(repaired.value, 1);
        let repaired = normalize(&json!("TRUE"), &only_if).expect("else bool");
        assert_eq!(repaired.value, true);

        let clash = json!({
            "if": {"type": "string"},
            "then": {"type": "integer"}
        });
        let kept = normalize(&json!("1"), &clash).expect("clash");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());

        let already = normalize(&json!(true), &schema).expect("already");
        assert_eq!(already.value, true);
        assert!(already.rewrites.is_empty());
    }

    #[test]
    fn all_of_applies_every_branch_when_they_agree() {
        let schema = json!({
            "allOf": [{"type": "integer"}, {"enum": [1, 2]}]
        });
        let repaired = normalize(&json!("1"), &schema).expect("agree");
        assert_eq!(repaired.value, 1);

        let conflict = json!({
            "allOf": [{"type": "integer"}, {"type": "boolean"}]
        });
        let kept = normalize(&json!("1"), &conflict).expect("conflict");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());

        let text = json!({
            "allOf": [{"type": "string"}, {"type": "integer"}]
        });
        let kept = normalize(&json!("1"), &text).expect("string and integer");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());

        let fields = json!({
            "allOf": [
                {"type": "object", "properties": {"n": {"type": "integer"}}},
                {"type": "object", "properties": {"flag": {"type": "boolean"}}}
            ]
        });
        let repaired = normalize(&json!({"n": "1", "flag": "true"}), &fields).expect("fields");
        assert_eq!(repaired.value["n"], 1);
        assert_eq!(repaired.value["flag"], true);

        let already = normalize(&json!(1), &schema).expect("already");
        assert_eq!(already.value, 1);
        assert!(already.rewrites.is_empty());
    }

    #[test]
    fn one_of_coerces_when_only_one_branch_accepts() {
        let schema = json!({
            "oneOf": [{"type": "integer"}, {"type": "boolean"}]
        });
        let repaired = normalize(&json!("1"), &schema).expect("integer");
        assert_eq!(repaired.value, 1);
        let repaired = normalize(&json!("true"), &schema).expect("bool");
        assert_eq!(repaired.value, true);
        let kept = normalize(&json!("hello"), &schema).expect("neither");
        assert_eq!(kept.value, "hello");
        assert!(kept.rewrites.is_empty());

        let either = json!({
            "oneOf": [{"type": "integer"}, {"type": "string"}]
        });
        let kept = normalize(&json!("1"), &either).expect("both");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());

        let consts = json!({
            "oneOf": [{"const": 1}, {"const": 2}]
        });
        let repaired = normalize(&json!("1"), &consts).expect("const");
        assert_eq!(repaired.value, 1);

        let any = json!({
            "anyOf": [{"type": "integer"}, {"type": "boolean"}]
        });
        let repaired = normalize(&json!("1"), &any).expect("any");
        assert_eq!(repaired.value, 1);

        let duplicate = json!({
            "oneOf": [{"type": "integer"}, {"type": "integer"}]
        });
        let kept = normalize(&json!("1"), &duplicate).expect("duplicate");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());

        let nested = json!({
            "type": "object",
            "properties": {
                "n": {"oneOf": [{"type": "integer"}, {"type": "boolean"}]}
            }
        });
        let repaired = normalize(&json!({"n": "2"}), &nested).expect("nested");
        assert_eq!(repaired.value["n"], 2);

        let already = normalize(&json!(1), &schema).expect("already");
        assert_eq!(already.value, 1);
        assert!(already.rewrites.is_empty());
    }

    #[test]
    fn exact_float_matches_one_const_or_enum_number() {
        let value: serde_json::Value = serde_json::from_str("1.0").unwrap();
        let repaired = normalize(&value, &json!({"const": 1})).expect("const");
        assert_eq!(repaired.value.as_i64(), Some(1));
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|rewrite| rewrite.detail.contains("const"))
        );

        let repaired = normalize(&value, &json!({"enum": [1, 2]})).expect("enum");
        assert_eq!(repaired.value.as_i64(), Some(1));

        let kept = normalize(&value, &json!({"const": 2})).expect("other");
        assert!(kept.value.as_i64().is_none());
        assert!(kept.rewrites.is_empty());

        let fraction: serde_json::Value = serde_json::from_str("1.5").unwrap();
        let kept = normalize(&fraction, &json!({"enum": [1, 2]})).expect("fraction");
        assert!(kept.value.as_i64().is_none());
        assert!(kept.rewrites.is_empty());

        let same: serde_json::Value = serde_json::from_str(r#"{"enum":[1.0,2]}"#).unwrap();
        let repaired = normalize(&value, &same).expect("already");
        assert!(repaired.value.as_i64().is_none());
        assert!(repaired.rewrites.is_empty());
    }

    #[test]
    fn const_and_enum_coerce_one_matching_string() {
        let repaired = normalize(&json!("1"), &json!({"enum": [1, 2]})).expect("enum");
        assert_eq!(repaired.value, 1);
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|rewrite| rewrite.detail.contains("enum"))
        );

        let repaired = normalize(&json!("TRUE"), &json!({"enum": [true, false]})).expect("bool");
        assert_eq!(repaired.value, true);

        let repaired = normalize(&json!(" null "), &json!({"const": null})).expect("null");
        assert!(repaired.value.is_null());

        let repaired = normalize(&json!("1e2"), &json!({"enum": [100, 200]})).expect("scientific");
        assert_eq!(repaired.value, 100);

        let repaired = normalize(&json!("1.0"), &json!({"const": 1})).expect("zero fraction");
        assert_eq!(repaired.value, 1);

        let kept = normalize(&json!("1"), &json!({"enum": ["1", 1]})).expect("both");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());

        let kept = normalize(&json!("1"), &json!({"const": 1, "enum": ["1"]})).expect("conflict");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());

        let kept = normalize(&json!("1"), &json!({"type": "string", "enum": [1, 2]}))
            .expect("string type");
        assert_eq!(kept.value, "1");
        assert!(kept.rewrites.is_empty());

        let kept = normalize(&json!("01"), &json!({"enum": [1]})).expect("leading zero");
        assert_eq!(kept.value, "01");
        assert!(kept.rewrites.is_empty());

        let kept = normalize(&json!("hello"), &json!({"enum": [1]})).expect("no match");
        assert_eq!(kept.value, "hello");
        assert!(kept.rewrites.is_empty());

        let kept = normalize(&json!("1.5"), &json!({"const": 1})).expect("fraction");
        assert_eq!(kept.value, "1.5");
        assert!(kept.rewrites.is_empty());
    }

    #[test]
    fn exact_float_is_an_integer_when_the_schema_says_integer() {
        let schema = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
        let value: serde_json::Value = serde_json::from_str(r#"{"n":1.0}"#).unwrap();
        let repaired = normalize(&value, &schema).expect("coerce");
        assert_eq!(repaired.value["n"].as_i64(), Some(1));
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|rewrite| rewrite.detail.contains("integer number"))
        );

        let negative: serde_json::Value = serde_json::from_str(r#"{"n":-2.0}"#).unwrap();
        let repaired = normalize(&negative, &schema).expect("negative");
        assert_eq!(repaired.value["n"].as_i64(), Some(-2));

        let hundred: serde_json::Value = serde_json::from_str(r#"{"n":1e2}"#).unwrap();
        let repaired = normalize(&hundred, &schema).expect("scientific");
        assert_eq!(repaired.value["n"].as_i64(), Some(100));

        let fraction: serde_json::Value = serde_json::from_str(r#"{"n":1.5}"#).unwrap();
        let repaired = normalize(&fraction, &schema).expect("keep fraction");
        assert!(repaired.value["n"].as_i64().is_none());
        assert!(repaired.rewrites.is_empty());

        let already: serde_json::Value = serde_json::from_str(r#"{"n":1}"#).unwrap();
        let repaired = normalize(&already, &schema).expect("already");
        assert_eq!(repaired.value["n"].as_i64(), Some(1));
        assert!(repaired.rewrites.is_empty());

        let wide = json!({
            "type": "object",
            "properties": {"n": {"type": ["integer", "number"]}}
        });
        let repaired = normalize(&value, &wide).expect("union");
        assert!(repaired.value["n"].as_i64().is_none());
        assert!(repaired.rewrites.is_empty());

        let huge: serde_json::Value = serde_json::from_str(r#"{"n":1e20}"#).unwrap();
        let repaired = normalize(&huge, &schema).expect("huge");
        assert!(repaired.value["n"].as_i64().is_none());
        assert!(repaired.rewrites.is_empty());
    }

    #[test]
    fn zero_fraction_integer_string_is_coerced() {
        let schema = json!({
            "type": "object",
            "properties": {
                "n": {"type": "integer"},
                "s": {"type": ["integer", "string"]}
            }
        });
        let repaired = normalize(&json!({"n": "3.0", "s": "4.0"}), &schema).expect("fraction");
        assert_eq!(repaired.value["n"], 3);
        assert_eq!(repaired.value["s"], "4.0");
        assert!(
            repaired
                .rewrites
                .iter()
                .any(|rewrite| rewrite.detail == "integer string 3.0")
        );
        let repaired = normalize(&json!({"n": "-2.000"}), &schema).expect("negative");
        assert_eq!(repaired.value["n"], -2);
        let repaired = normalize(&json!({"n": "+3.0"}), &schema).expect("plus");
        assert_eq!(repaired.value["n"], 3);
        let repaired = normalize(&json!({"n": " 0.0\n"}), &schema).expect("zero");
        assert_eq!(repaired.value["n"], 0);
        let repaired = normalize(&json!({"n": "1_000.00"}), &schema).expect("underscores");
        assert_eq!(repaired.value["n"], 1000);
        let repaired = normalize(&json!({"n": "3.10"}), &schema).expect("keep fraction");
        assert_eq!(repaired.value["n"], "3.10");
        let repaired = normalize(&json!({"n": "03.0"}), &schema).expect("keep leading zero");
        assert_eq!(repaired.value["n"], "03.0");
        let repaired = normalize(&json!({"n": "1e2"}), &schema).expect("keep scientific");
        assert_eq!(repaired.value["n"], "1e2");
    }

    #[test]
    fn tuple_items_coerce_by_position() {
        let schema = json!({
            "type": "array",
            "items": [{"type": "integer"}, {"type": "boolean"}]
        });
        let repaired = normalize(&json!(["1", " true "]), &schema).expect("tuple");
        assert_eq!(repaired.value[0], 1);
        assert_eq!(repaired.value[1], true);
        let repaired = normalize(&json!(["1", "true", "x"]), &schema).expect("extra");
        assert_eq!(repaired.value[2], "x");

        let closed = json!({
            "type": "array",
            "items": [{"type": "integer"}],
            "additionalItems": false
        });
        let err = normalize(&json!(["1", "no"]), &closed).unwrap_err();
        assert!(matches!(err, RepairError::Ambiguous(_)), "{err:?}");
        let repaired = normalize(&json!(["4"]), &closed).expect("exact");
        assert_eq!(repaired.value[0], 4);

        let prefix = json!({
            "type": "array",
            "prefixItems": [{"type": "integer"}],
            "items": {"type": "boolean"}
        });
        let repaired = normalize(&json!(["2", "false"]), &prefix).expect("prefix");
        assert_eq!(repaired.value[0], 2);
        assert_eq!(repaired.value[1], false);

        let every = json!({"type": "array", "items": {"type": "integer"}});
        let repaired = normalize(&json!(["2", "3"]), &every).expect("every");
        assert_eq!(repaired.value[0], 2);
        assert_eq!(repaired.value[1], 3);
    }

    #[test]
    fn array_items_apply_without_a_type_keyword() {
        let schema = json!({"items": {"type": "integer"}});
        let repaired = normalize(&json!(["1", "2"]), &schema).expect("items");
        assert_eq!(repaired.value[0], 1);
        assert_eq!(repaired.value[1], 2);
        let repaired = normalize(&json!({"a": "1"}), &schema).expect("object");
        assert_eq!(repaired.value["a"], "1");

        let tuple = json!({"items": [{"type": "integer"}, {"type": "boolean"}]});
        let repaired = normalize(&json!(["3", "false"]), &tuple).expect("tuple");
        assert_eq!(repaired.value[0], 3);
        assert_eq!(repaired.value[1], false);

        let prefix = json!({
            "prefixItems": [{"type": "integer"}],
            "items": {"type": "boolean"}
        });
        let repaired = normalize(&json!(["4", "true"]), &prefix).expect("prefix");
        assert_eq!(repaired.value[0], 4);
        assert_eq!(repaired.value[1], true);

        let object = json!({"type": "object", "items": {"type": "integer"}});
        let repaired = normalize(&json!(["1"]), &object).expect("keep");
        assert_eq!(repaired.value[0], "1");
    }

    #[test]
    fn empty_fraction_string_is_an_integer() {
        let schema = json!({
            "type": "object",
            "properties": {
                "n": {"type": "integer"},
                "s": {"type": ["integer", "string"]}
            }
        });
        let repaired = normalize(&json!({"n": "1.", "s": "2."}), &schema).expect("empty fraction");
        assert_eq!(repaired.value["n"], 1);
        assert_eq!(repaired.value["s"], "2.");
        let repaired = normalize(&json!({"n": "-2."}), &schema).expect("negative");
        assert_eq!(repaired.value["n"], -2);
        let repaired = normalize(&json!({"n": "01."}), &schema).expect("leading zero");
        assert_eq!(repaired.value["n"], "01.");
        let repaired = normalize(&json!({"n": "1.e2"}), &schema).expect("scientific");
        assert_eq!(repaired.value["n"], "1.e2");
        let number = json!({"type": "object", "properties": {"n": {"type": "number"}}});
        let repaired = normalize(&json!({"n": "1."}), &number).expect("number");
        assert_eq!(repaired.value["n"], 1);
        let repaired = normalize(&json!({"n": ".5"}), &number).expect("leading dot");
        assert_eq!(repaired.value["n"], ".5");
    }

    #[test]
    fn null_string_is_coerced_when_the_schema_is_null() {
        let schema = json!({
            "type": "object",
            "properties": {
                "z": {"type": "null"},
                "s": {"type": ["null", "string"]},
                "b": {"type": "boolean"}
            }
        });
        let repaired =
            normalize(&json!({"z": " null ", "s": "null", "b": "null"}), &schema).expect("null");
        assert!(repaired.value["z"].is_null());
        assert_eq!(repaired.value["s"], "null");
        assert_eq!(repaired.value["b"], "null");
        assert!(repaired.rewrites.iter().any(|rewrite| {
            rewrite.kind == RewriteKind::NullString && rewrite.detail == "null string null"
        }));
        let repaired = normalize(&json!({"z": "NULL"}), &schema).expect("upper");
        assert!(repaired.value["z"].is_null());
        let repaired = normalize(&json!({"z": "None"}), &schema).expect("none");
        assert_eq!(repaired.value["z"], "None");
        let repaired = normalize(&json!({"z": "null story"}), &schema).expect("story");
        assert_eq!(repaired.value["z"], "null story");
    }

    #[test]
    fn unary_plus_before_a_number_is_stripped() {
        match repair_json("{a: +1, b: + 1.5, c: -2}") {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["a"], 1);
                assert_eq!(value["b"], 1.5);
                assert_eq!(value["c"], -2);
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        assert!(matches!(
            repair_json("{a: 1+2}"),
            RepairOutcome::Unfixable(_)
        ));
        assert!(matches!(
            repair_json(r#"{"a": "+1"}"#),
            RepairOutcome::Valid(_)
        ));
    }

    #[test]
    fn unknown_fence_word_is_not_a_language_tag() {
        assert!(matches!(
            repair_json("```hello {a: 1}```"),
            RepairOutcome::Unfixable(_)
        ));
        match repair_json("```javascript\n{\"a\": 1}\n```") {
            RepairOutcome::Repaired { value, .. } | RepairOutcome::Valid(value) => {
                assert_eq!(value["a"], 1);
            }
            other => panic!("expected JSON, got {other:?}"),
        }
    }
}
