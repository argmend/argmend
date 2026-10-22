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
    let (raw, mut prefix) = match without_leading_bom(raw) {
        Some(rest) => (
            rest,
            vec![note(RewriteKind::Syntax, "stripped leading BOM")],
        ),
        None => (raw, Vec::new()),
    };
    if let Ok(value) = serde_json::from_str(raw) {
        return Ok((value, prefix));
    }

    let mut body = raw.to_string();
    if let Some(inner) = salvage::unwrap_fence(raw) {
        prefix.push(note(RewriteKind::Fence, "unwrapped markdown fence"));
        body = inner;
        if let Ok(value) = serde_json::from_str(&body) {
            return Ok((value, prefix));
        }
    }

    if prefix.is_empty() {
        match syntax::repair_tool_call_json(raw) {
            RepairOutcome::Valid(value) => return Ok((value, prefix)),
            RepairOutcome::Repaired { value, repairs } => {
                prefix.extend(repairs);
                return Ok((value, prefix));
            }
            RepairOutcome::Unfixable(_) | RepairOutcome::Ambiguous(_) => {}
        }
    }

    let staged = syntax::apply_stages(&body);
    let parse_error = match serde_json::from_str::<Value>(&staged.text) {
        Ok(value) => {
            prefix.extend(staged.repairs);
            return Ok((value, prefix));
        }
        Err(err) => err,
    };

    if let Some(closed) = salvage::close_truncated(&staged.text)
        && let Ok(value) = serde_json::from_str::<Value>(&closed)
    {
        prefix.extend(staged.repairs);
        prefix.push(note(RewriteKind::ClosedTruncation, "closed truncated json"));
        return Ok((value, prefix));
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

/// BOM after ASCII whitespace, before the JSON text.
fn without_leading_bom(raw: &str) -> Option<&str> {
    let rest = raw.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
    rest.strip_prefix('\u{feff}')
}

fn note(kind: RewriteKind, detail: &str) -> Rewrite {
    Rewrite {
        kind,
        detail: detail.to_string(),
    }
}

fn is_blank_input(raw: &str) -> bool {
    raw.trim_matches(|ch: char| ch.is_ascii_whitespace() || ch == '\u{feff}')
        .is_empty()
}

fn schema_type(schema: &Value) -> Option<&str> {
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        return Some(kind);
    }
    schema
        .get("parameters")
        .and_then(|params| params.get("type"))
        .and_then(Value::as_str)
}

/// Blank tool arguments are an empty object or an empty array.
/// A string, number, or boolean is not invented.
fn blank_value(schema: Option<&Value>) -> Result<Value, RepairError> {
    let Some(schema) = schema else {
        return Ok(Value::Object(serde_json::Map::new()));
    };
    match schema_type(schema) {
        None | Some("object") => Ok(Value::Object(serde_json::Map::new())),
        Some("array") => Ok(Value::Array(Vec::new())),
        _ => Err(RepairError::Unfixable(
            "blank input does not match the schema type".into(),
        )),
    }
}

/// Parse `raw`, then apply [`normalize`] when a schema is present.
///
/// Empty input is `{}`.
pub fn repair_args(request: &RepairRequest<'_>) -> Result<Repaired, RepairError> {
    if is_blank_input(request.raw) {
        let value = blank_value(request.schema).map_err(|err| named(request.tool_name, err))?;
        let mut repaired = Repaired {
            value,
            rewrites: vec![Rewrite {
                kind: RewriteKind::Empty,
                detail: "empty input is {}".into(),
            }],
        };
        if let Some(schema) = request.schema {
            let normalized = normalize(&repaired.value, schema)?;
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
                .any(|item| item.kind == RewriteKind::Empty)
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
}
