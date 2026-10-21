//! Fence unwrap, unique truncation, and XML parameter objects.

use serde_json::Value;

use crate::RepairError;

pub(crate) fn unwrap_fence(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix("```")?;
    let rest = rest
        .strip_prefix("json")
        .or_else(|| rest.strip_prefix("JSON"))
        .unwrap_or(rest);
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.rfind("```")?;
    if rest[end + 3..].trim().is_empty() {
        Some(rest[..end].trim().to_string())
    } else {
        None
    }
}

/// Close an open `{` / `[` stack when every string is already finished.
///
/// An open string is left alone. Closing it would invent the end of a path
/// or a key. A trailing `:` or `,` is also left alone, because the value is
/// missing.
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
    if matches!(last_sig, Some(':') | Some(',')) {
        return None;
    }
    let mut out = trimmed.to_string();
    while let Some(closer) = stack.pop() {
        out.push(closer);
    }
    Some(out)
}

pub(crate) fn xml_parameters(raw: &str) -> Result<Option<Value>, RepairError> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("<parameter") {
        return Ok(None);
    }
    let mut object = serde_json::Map::new();
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
        object.insert(name, parameter_value(content));
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
    Ok(Some(Value::Object(object)))
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

fn parameter_value(content: &str) -> Value {
    if looks_like_json_container(content)
        && let Ok(value) = serde_json::from_str::<Value>(content)
    {
        return value;
    }
    Value::String(content.to_string())
}

fn looks_like_json_container(content: &str) -> bool {
    matches!(content.as_bytes().first(), Some(b'{' | b'[' | b'"'))
}
