//! Fence unwrap, unique truncation, and XML parameter objects.

use serde_json::Value;

use crate::{RepairError, RepairOutcome, Rewrite};

pub(crate) fn unwrap_fence(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix("```")?;
    let rest = strip_fence_info(rest);
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    if let Some(end) = rest.rfind("```") {
        let tail = &rest[end + 3..];
        if tail.trim().is_empty() || tail_is_comment(tail) {
            return Some(rest[..end].trim().to_string());
        }
        return None;
    }
    let body = rest.trim();
    if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    }
}

/// A comment after the closing fence is not another JSON value.
fn tail_is_comment(tail: &str) -> bool {
    let tail = tail.trim();
    if let Some(rest) = tail.strip_prefix("/*") {
        let Some(end) = rest.find("*/") else {
            return false;
        };
        return rest[end + 2..].trim().is_empty();
    }
    let Some(rest) = tail.strip_prefix("//").or_else(|| tail.strip_prefix('#')) else {
        return false;
    };
    match rest.split_once('\n') {
        None => true,
        Some((_, after)) => after.trim().is_empty(),
    }
}

/// Drop a fence info string such as `json`, `jsonc`, or `json5` when it is a tag.
///
/// The tag may contain a digit. An unknown word that only starts with a
/// known tag, such as `json5foo`, stays in the body.
fn strip_fence_info(rest: &str) -> &str {
    let end = rest
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_alphanumeric())
        .map(|(idx, _)| idx)
        .unwrap_or(rest.len());
    if end == 0 {
        return rest;
    }
    let tag = &rest[..end];
    let allowed = matches!(
        tag,
        "json" | "JSON" | "jsonc" | "JSONC" | "json5" | "JSON5" | "javascript" | "js" | "JS"
    );
    if !allowed {
        return rest;
    }
    let after = &rest[end..];
    if after.starts_with(['{', '[', ' ', '\t', '\n', '\r']) || after.is_empty() {
        after
    } else {
        rest
    }
}

/// Close an open `{` / `[` stack when every string is already finished.
///
/// An open string is left alone. Closing it would invent the end of a path
/// or a key. A trailing `:` or `,` is also left alone, because the value is
/// missing. A `{` or `[` that has not started a value is left alone too.
/// Closing it would invent an empty container.
pub(crate) fn close_truncated(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    let mut last_sig = None;
    for ch in &chars {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            if *ch == '\\' {
                escape = true;
                continue;
            }
            if *ch == '"' {
                in_string = false;
                last_sig = Some('"');
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            other if other.is_whitespace() => {}
            other => last_sig = Some(*other),
        }
    }
    if in_string || stack.is_empty() {
        return None;
    }
    if last_sig.is_none() || matches!(last_sig, Some(':') | Some(',')) {
        return None;
    }
    // A final digit may be the start of a longer number that was cut off.
    if last_sig.is_some_and(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let mut out = trimmed.to_string();
    while let Some(closer) = stack.pop() {
        out.push(closer);
    }
    Some(out)
}

pub(crate) fn xml_parameters(raw: &str) -> Result<Option<(Value, Vec<Rewrite>)>, RepairError> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("<parameter") {
        return Ok(None);
    }
    let mut object = serde_json::Map::new();
    let mut rewrites = Vec::new();
    let mut rest = trimmed;
    let mut found = false;
    while let Some(start) = rest.find("<parameter") {
        if !rest[..start].trim().is_empty() {
            return Err(RepairError::Unfixable(
                "trailing text after xml parameters".into(),
            ));
        }
        found = true;
        let after = &rest[start + "<parameter".len()..];
        let (name, after_name) = parameter_name(after)?;
        let Some(content_end) = after_name.find("</parameter>") else {
            return Err(RepairError::Unfixable(
                "xml parameter tag is not closed".into(),
            ));
        };
        let content = after_name[..content_end].trim();
        if object.contains_key(&name) {
            return Err(RepairError::Ambiguous(format!(
                "xml parameter '{name}' is repeated"
            )));
        }
        let (value, more) = parameter_value(content)?;
        rewrites.extend(more);
        object.insert(name, value);
        rest = &after_name[content_end + "</parameter>".len()..];
    }
    if !rest.trim().is_empty() {
        return Err(RepairError::Unfixable(
            "trailing text after xml parameters".into(),
        ));
    }
    if !found || object.is_empty() {
        return Err(RepairError::Unfixable(
            "xml parameter tag could not be read".into(),
        ));
    }
    Ok(Some((Value::Object(object), rewrites)))
}

fn parameter_name(after: &str) -> Result<(String, &str), RepairError> {
    let after = after.trim_start();
    if let Some(rest) = after.strip_prefix('=') {
        let rest = rest.trim_start();
        let end = rest
            .find('>')
            .ok_or_else(|| RepairError::Unfixable("xml parameter name is not closed".into()))?;
        let name = rest[..end].trim();
        if name.is_empty() {
            return Err(RepairError::Unfixable("xml parameter name is empty".into()));
        }
        return Ok((name.to_string(), &rest[end + 1..]));
    }
    let Some(rest) = after.strip_prefix("name=") else {
        return Err(RepairError::Unfixable("xml parameter has no name".into()));
    };
    let quote = rest
        .chars()
        .next()
        .filter(|ch| *ch == '"' || *ch == '\'')
        .ok_or_else(|| RepairError::Unfixable("xml parameter name is not quoted".into()))?;
    let rest = &rest[1..];
    let end = rest
        .find(quote)
        .ok_or_else(|| RepairError::Unfixable("xml parameter name quote is not closed".into()))?;
    let name = &rest[..end];
    if name.is_empty() {
        return Err(RepairError::Unfixable("xml parameter name is empty".into()));
    }
    let after_name = &rest[end + 1..];
    let gt = after_name
        .find('>')
        .ok_or_else(|| RepairError::Unfixable("xml parameter tag is not closed".into()))?;
    Ok((name.to_string(), &after_name[gt + 1..]))
}

fn parameter_value(content: &str) -> Result<(Value, Vec<Rewrite>), RepairError> {
    let trimmed = content.trim();
    // `{` and `[` with no body are cut-off containers. A repaired string is
    // file text: `"use strict";` must keep the quotes and the semicolon.
    // A string that was already JSON is the parsed text.
    if trimmed == "{" || trimmed == "[" {
        return Err(RepairError::Unfixable(
            "xml parameter container is truncated".into(),
        ));
    }
    match crate::repair_json(trimmed) {
        RepairOutcome::Valid(value)
            if value.is_object() || value.is_array() || value.is_string() =>
        {
            Ok((value, Vec::new()))
        }
        RepairOutcome::Ambiguous(reason) => Err(RepairError::Ambiguous(reason)),
        RepairOutcome::Repaired { value, repairs }
            if value.is_object() || (value.is_array() && !bare_bracket_word(trimmed)) =>
        {
            Ok((value, repairs))
        }
        _ => Ok((Value::String(content.to_string()), Vec::new())),
    }
}

/// `[package]` and `[package,]` are file text. Quote repair would turn the
/// word into a one-element array. A real array already has a JSON value.
fn bare_bracket_word(trimmed: &str) -> bool {
    let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return false;
    };
    let inner = inner.trim().trim_end_matches(',').trim();
    if inner.is_empty() || matches!(inner, "true" | "false" | "null" | "True" | "False" | "None") {
        return false;
    }
    inner
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
        && inner.chars().any(|ch| ch.is_ascii_alphabetic())
}
