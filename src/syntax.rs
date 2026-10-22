//! Syntax stages for tool-argument JSON. Valid JSON is not rewritten.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::{RepairError, RepairOutcome, Rewrite, RewriteKind};

/// Attempt to parse `raw` as JSON, applying progressive repairs if the
/// initial parse fails.
///
/// Repair stages (applied cumulatively):
/// 1. Strip control characters (except `\n`, `\t`, `\r`).
/// 2. Strip `//` and `/* */` comments (before trailing-comma removal,
///    since comments can mask trailing commas).
/// 3. Fix single quotes to double quotes (preserving apostrophes inside
///    double-quoted strings).
/// 4. Quote bare (unquoted) keys.
/// 5. Remove trailing commas before `]` or `}`.
pub(crate) struct Staged {
    pub text: String,
    pub repairs: Vec<Rewrite>,
}

fn syntax_note(detail: &str) -> Rewrite {
    Rewrite {
        kind: RewriteKind::Syntax,
        detail: detail.to_string(),
    }
}

pub(crate) fn apply_stages(raw: &str) -> Staged {
    let mut text = raw.to_string();
    let mut repairs = Vec::new();

    let cleaned = strip_control_chars(&text);
    if cleaned != text {
        repairs.push(syntax_note("stripped control characters"));
        text = cleaned;
    }

    // Comments before trailing-comma removal: `42, /* note */}` hides a comma.
    let cleaned = strip_json_comments(&text);
    if cleaned != text {
        repairs.push(syntax_note("stripped comments"));
        text = cleaned;
    }

    let cleaned = fix_single_quotes(&text);
    if cleaned != text {
        repairs.push(syntax_note("replaced single quotes with double quotes"));
        text = cleaned;
    }

    let cleaned = normalize_curly_quotes(&text);
    if cleaned != text {
        repairs.push(syntax_note("replaced curly quotes"));
        text = cleaned;
    }

    let cleaned = escape_controls_in_strings(&text);
    if cleaned != text {
        repairs.push(syntax_note("escaped controls inside strings"));
        text = cleaned;
    }

    let cleaned = strip_unary_plus(&text);
    if cleaned != text {
        repairs.push(syntax_note("stripped unary plus"));
        text = cleaned;
    }

    let cleaned = strip_numeric_underscores(&text);
    if cleaned != text {
        repairs.push(syntax_note("stripped underscores in numbers"));
        text = cleaned;
    }

    let cleaned = fix_triple_quotes(&text);
    if cleaned != text {
        repairs.push(syntax_note(
            "replaced Python triple-quotes with JSON strings",
        ));
        text = cleaned;
    }

    let cleaned = quote_bare_keys(&text);
    if cleaned != text {
        repairs.push(syntax_note("quoted bare keys"));
        text = cleaned;
    }

    let cleaned = replace_python_literals(&text);
    if cleaned != text {
        repairs.push(syntax_note("replaced Python literals"));
        text = cleaned;
    }

    let cleaned = remove_trailing_commas(&text);
    if cleaned != text {
        repairs.push(syntax_note("removed trailing commas"));
        text = cleaned;
    }

    Staged { text, repairs }
}

pub(crate) fn repair_tool_call_json(raw: &str) -> RepairOutcome {
    if let Ok(value) = serde_json::from_str(raw) {
        return outcome_with_unique_keys(raw, value, Vec::new());
    }
    let staged = apply_stages(raw);
    match serde_json::from_str(&staged.text) {
        Ok(value) => outcome_with_unique_keys(&staged.text, value, staged.repairs),
        Err(e) => RepairOutcome::Unfixable(format!("parse failed after all repairs: {e}")),
    }
}

fn outcome_with_unique_keys(text: &str, value: Value, mut repairs: Vec<Rewrite>) -> RepairOutcome {
    match duplicate_key_notes(text) {
        Ok(notes) => {
            for detail in notes {
                repairs.push(Rewrite {
                    kind: RewriteKind::Syntax,
                    detail,
                });
            }
            if repairs.is_empty() {
                RepairOutcome::Valid(value)
            } else {
                RepairOutcome::Repaired { value, repairs }
            }
        }
        Err(RepairError::Ambiguous(reason)) => RepairOutcome::Ambiguous(reason),
        Err(RepairError::Unfixable(reason)) => RepairOutcome::Unfixable(reason),
    }
}

/// Repeated object keys with the same value are collapsed and recorded.
/// Two different values are ambiguous, because a parser may keep either one.
pub(crate) fn duplicate_key_notes(text: &str) -> Result<Vec<String>, RepairError> {
    let mut scan = KeyScan {
        text,
        chars: text.char_indices().collect(),
        index: 0,
        dropped: Vec::new(),
    };
    scan.parse_value()?;
    Ok(scan.dropped)
}

struct KeyScan<'a> {
    text: &'a str,
    chars: Vec<(usize, char)>,
    index: usize,
    dropped: Vec<String>,
}

impl KeyScan<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).map(|(_, ch)| *ch)
    }

    fn bump(&mut self) {
        self.index += 1;
    }

    fn byte_at(&self) -> usize {
        self.chars
            .get(self.index)
            .map(|(byte, _)| *byte)
            .unwrap_or(self.text.len())
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.bump();
        }
    }

    fn parse_value(&mut self) -> Result<(), RepairError> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => {
                self.parse_string()?;
                Ok(())
            }
            Some('t') => {
                self.parse_literal("true");
                Ok(())
            }
            Some('f') => {
                self.parse_literal("false");
                Ok(())
            }
            Some('n') => {
                self.parse_literal("null");
                Ok(())
            }
            Some('-' | '0'..='9') => {
                self.parse_number();
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn parse_object(&mut self) -> Result<(), RepairError> {
        self.bump();
        let mut seen: HashMap<String, Value> = HashMap::new();
        let mut noted = HashSet::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.bump();
                    return Ok(());
                }
                Some('"') => {}
                _ => return Ok(()),
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() == Some(':') {
                self.bump();
            }
            self.skip_ws();
            let start = self.byte_at();
            self.parse_value()?;
            let end = self.byte_at();
            let slice = self.text[start..end].trim();
            let parsed = serde_json::from_str(slice).map_err(|_| {
                RepairError::Unfixable(format!("duplicate key scan could not read '{key}'"))
            })?;
            match seen.get(&key) {
                Some(prev) if prev != &parsed => {
                    return Err(RepairError::Ambiguous(format!("duplicate key '{key}'")));
                }
                Some(_) => {
                    if noted.insert(key.clone()) {
                        self.dropped.push(format!("dropped duplicate key {key}"));
                    }
                }
                None => {
                    seen.insert(key, parsed);
                }
            }
            self.skip_ws();
            match self.peek() {
                Some(',') => self.bump(),
                Some('}') => {
                    self.bump();
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
    }

    fn parse_array(&mut self) -> Result<(), RepairError> {
        self.bump();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(']') => {
                    self.bump();
                    return Ok(());
                }
                None => return Ok(()),
                _ => self.parse_value()?,
            }
            self.skip_ws();
            match self.peek() {
                Some(',') => self.bump(),
                Some(']') => {
                    self.bump();
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, RepairError> {
        let start = self.byte_at();
        if self.peek() != Some('"') {
            return Err(RepairError::Unfixable(
                "duplicate key scan expected a string".into(),
            ));
        }
        self.bump();
        while let Some(ch) = self.peek() {
            if ch == '\\' {
                self.bump();
                if self.peek().is_some() {
                    self.bump();
                }
                continue;
            }
            self.bump();
            if ch == '"' {
                break;
            }
        }
        let end = self.byte_at();
        serde_json::from_str(&self.text[start..end]).map_err(|_| {
            RepairError::Unfixable("duplicate key scan could not read a string".into())
        })
    }

    fn parse_literal(&mut self, literal: &str) {
        for expected in literal.chars() {
            if self.peek() != Some(expected) {
                return;
            }
            self.bump();
        }
    }

    fn parse_number(&mut self) {
        if self.peek() == Some('-') {
            self.bump();
        }
        self.parse_digits();
        if self.peek() == Some('.') {
            self.bump();
            self.parse_digits();
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            self.parse_digits();
        }
    }

    fn parse_digits(&mut self) {
        while matches!(self.peek(), Some('0'..='9')) {
            self.bump();
        }
    }
}

// Syntax helpers. Tests in this module call them directly.

/// Remove control characters (U+0000..U+001F) except newline, tab, and
/// carriage return. These occasionally appear when LLMs copy binary data
/// or terminal escape sequences into JSON strings.
pub(crate) fn strip_control_chars(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t' || *c == '\r')
        .collect()
}

/// Replace single-quoted strings with double-quoted strings.
///
/// This is deliberately conservative: we only replace single quotes that
/// appear in *key* or *value* positions (outside existing double-quoted
/// strings). Apostrophes that appear inside an already double-quoted
/// string are left untouched.
pub(crate) fn fix_single_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            // Skip over double-quoted strings verbatim.
            '"' => {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' {
                        out.push(chars[i]);
                        i += 1;
                        if i < len {
                            out.push(chars[i]);
                            i += 1;
                        }
                    } else {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < len {
                    out.push('"');
                    i += 1;
                }
            }
            // Convert single-quoted string to double-quoted string.
            '\'' => {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\\' {
                        i += 1;
                        if i < len {
                            // `\'` is an apostrophe. Copied through, it is
                            // not a legal JSON escape, so the repair failed.
                            if chars[i] == '\'' {
                                out.push('\'');
                            } else {
                                out.push('\\');
                                out.push(chars[i]);
                            }
                            i += 1;
                        }
                    } else {
                        match chars[i] {
                            '"' => out.push_str("\\\""),
                            '\n' => out.push_str("\\n"),
                            '\r' => out.push_str("\\r"),
                            '\t' => out.push_str("\\t"),
                            other => out.push(other),
                        }
                        i += 1;
                    }
                }
                out.push('"');
                if i < len {
                    i += 1; // skip closing '
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out
}

/// Wrap bare (unquoted) object keys in double quotes.
///
/// A bare key is a sequence of `[a-zA-Z_][a-zA-Z0-9_]*` immediately
/// followed by `:` (with optional whitespace) while not inside a string.
pub(crate) fn quote_bare_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            // Skip over double-quoted strings.
            '"' => {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' {
                        out.push(chars[i]);
                        i += 1;
                        if i < len {
                            out.push(chars[i]);
                            i += 1;
                        }
                    } else {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < len {
                    out.push('"');
                    i += 1;
                }
            }
            c if c == '$' || c.is_ascii_alphabetic() || c == '_' => {
                // Collect the identifier. `$` is only a leading character,
                // so `$ref` is one key and `a$b` is not.
                let start = i;
                i += 1;
                while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let mut hyphenated = i;
                loop {
                    if hyphenated + 1 < len
                        && (chars[hyphenated] == '-' || chars[hyphenated] == '.')
                        && (chars[hyphenated + 1].is_ascii_alphanumeric()
                            || chars[hyphenated + 1] == '_')
                    {
                        hyphenated += 1;
                        while hyphenated < len
                            && (chars[hyphenated].is_ascii_alphanumeric()
                                || chars[hyphenated] == '_')
                        {
                            hyphenated += 1;
                        }
                    } else {
                        break;
                    }
                }
                let mut j = hyphenated;
                while j < len && chars[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < len && chars[j] == ':' {
                    i = hyphenated;
                }
                let ident: String = chars[start..i].iter().collect();

                // Look ahead past optional whitespace for ':'.
                let mut j = i;
                while j < len && chars[j].is_ascii_whitespace() {
                    j += 1;
                }

                if j < len && chars[j] == ':' {
                    // It is a bare key -- quote it.
                    out.push('"');
                    out.push_str(&ident);
                    out.push('"');
                } else {
                    // Not a key; emit literal values (true, false, null) and
                    // other identifiers verbatim.
                    out.push_str(&ident);
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out
}

/// Replace Python triple-quoted strings (`"""..."""`) with proper
/// JSON-escaped double-quoted strings. Small models trained on Python
/// emit triple quotes inside JSON tool call arguments.
pub(crate) fn fix_triple_quotes(s: &str) -> String {
    if !s.contains("\"\"\"") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("\"\"\"") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 3..];
        if let Some(end) = after_open.find("\"\"\"") {
            // Extract the content between triple quotes and JSON-escape it.
            let content = &after_open[..end];
            out.push('"');
            for ch in content.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    _ => out.push(ch),
                }
            }
            out.push('"');
            rest = &after_open[end + 3..];
        } else {
            // No closing triple-quote found; emit as-is.
            out.push_str("\"\"\"");
            rest = after_open;
        }
    }
    out.push_str(rest);
    out
}

/// Replace bare Python `True` / `False` / `None` values with JSON.
///
/// A following `:` means the word is a key, so it is left alone.
pub(crate) fn replace_python_literals(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            '"' => {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' {
                        out.push(chars[i]);
                        i += 1;
                        if i < len {
                            out.push(chars[i]);
                            i += 1;
                        }
                    } else {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < len {
                    out.push('"');
                    i += 1;
                }
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                let mut j = i;
                while j < len && chars[j].is_ascii_whitespace() {
                    j += 1;
                }
                let is_key = j < len && chars[j] == ':';
                if !is_key {
                    match ident.as_str() {
                        "True" => {
                            out.push_str("true");
                            continue;
                        }
                        "False" => {
                            out.push_str("false");
                            continue;
                        }
                        "None" => {
                            out.push_str("null");
                            continue;
                        }
                        _ => {}
                    }
                }
                out.push_str(&ident);
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out
}

/// `“` and `”` are JSON quotes. A curly quote inside a straight-quoted
/// string stays. Curly single quotes are left alone.
pub(crate) fn normalize_curly_quotes(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut index = 0;
    while index < len {
        let ch = chars[index];
        if ch != '"' && ch != '\u{201c}' {
            out.push(ch);
            index += 1;
            continue;
        }
        let closer = if ch == '"' { '"' } else { '\u{201d}' };
        out.push('"');
        index += 1;
        while index < len && chars[index] != closer {
            if chars[index] == '\\' && closer == '"' {
                out.push('\\');
                index += 1;
                if index < len {
                    out.push(chars[index]);
                    index += 1;
                }
                continue;
            }
            out.push(chars[index]);
            index += 1;
        }
        if index < len && chars[index] == closer {
            out.push('"');
            index += 1;
        }
    }
    out
}

/// A raw newline, carriage return, or tab inside a double-quoted string
/// is content. Outside a string those bytes are whitespace and stay.
pub(crate) fn escape_controls_in_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut index = 0;
    while index < len {
        if chars[index] != '"' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        out.push('"');
        index += 1;
        while index < len && chars[index] != '"' {
            if chars[index] == '\\' {
                out.push('\\');
                index += 1;
                if index < len {
                    out.push(chars[index]);
                    index += 1;
                }
                continue;
            }
            match chars[index] {
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                other => out.push(other),
            }
            index += 1;
        }
        if index < len {
            out.push('"');
            index += 1;
        }
    }
    out
}

/// Drop `_` between digits outside strings. `1__0` is left alone.
pub(crate) fn strip_numeric_underscores(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        if chars[i] == '"' {
            out.push('"');
            i += 1;
            while i < len && chars[i] != '"' {
                if chars[i] == '\\' {
                    out.push(chars[i]);
                    i += 1;
                    if i < len {
                        out.push(chars[i]);
                        i += 1;
                    }
                } else {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            if i < len {
                out.push('"');
                i += 1;
            }
            continue;
        }
        if chars[i] == '_'
            && out
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_ascii_digit())
            && i + 1 < len
            && chars[i + 1].is_ascii_digit()
        {
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Drop a unary `+` before a number. `1+2` is left alone.
pub(crate) fn strip_unary_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut prev_sig = None;

    while i < len {
        match chars[i] {
            '"' => {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' {
                        out.push(chars[i]);
                        i += 1;
                        if i < len {
                            out.push(chars[i]);
                            i += 1;
                        }
                    } else {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < len {
                    out.push('"');
                    i += 1;
                }
                prev_sig = Some('"');
            }
            '+' if matches!(prev_sig, Some(':' | ',' | '[' | '{')) => {
                let mut j = i + 1;
                while j < len && chars[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < len && chars[j].is_ascii_digit() {
                    i = j;
                    continue;
                }
                out.push('+');
                prev_sig = Some('+');
                i += 1;
            }
            other => {
                out.push(other);
                if !other.is_ascii_whitespace() {
                    prev_sig = Some(other);
                }
                i += 1;
            }
        }
    }
    out
}

/// Remove trailing commas before `]` or `}`.
pub(crate) fn remove_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            // Skip over strings.
            '"' => {
                out.push('"');
                i += 1;
                while i < len && chars[i] != '"' {
                    if chars[i] == '\\' {
                        out.push(chars[i]);
                        i += 1;
                        if i < len {
                            out.push(chars[i]);
                            i += 1;
                        }
                    } else {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < len {
                    out.push('"');
                    i += 1;
                }
            }
            ',' => {
                // Look ahead past whitespace for ] or }.
                let mut j = i + 1;
                while j < len && chars[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < len && (chars[j] == ']' || chars[j] == '}') {
                    // Drop the comma; keep any whitespace that follows
                    // (it will be consumed normally).
                    i += 1;
                } else {
                    out.push(',');
                    i += 1;
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out
}

/// Strip `//` and `#` line comments and `/* ... */` block comments.
///
/// Single-quoted strings are skipped, same as double-quoted strings.
/// Quote repair runs after this stage, so `//` inside `'http://example.com'`
/// is part of the value. A `#` inside a string is also kept.
pub(crate) fn strip_json_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            '"' | '\'' => {
                let quote = chars[i];
                out.push(quote);
                i += 1;
                while i < len && chars[i] != quote {
                    if chars[i] == '\\' {
                        out.push(chars[i]);
                        i += 1;
                        if i < len {
                            out.push(chars[i]);
                            i += 1;
                        }
                    } else {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < len {
                    out.push(quote);
                    i += 1;
                }
            }
            '#' => {
                i += 1;
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < len => {
                if chars[i + 1] == '/' {
                    // Line comment: skip until newline.
                    i += 2;
                    while i < len && chars[i] != '\n' {
                        i += 1;
                    }
                } else if chars[i + 1] == '*' {
                    // A closed block comment is removed. An unclosed one is
                    // kept: dropping the tail and then inventing `}` would
                    // change the object.
                    let start = i;
                    i += 2;
                    let mut closed = false;
                    while i + 1 < len {
                        if chars[i] == '*' && chars[i + 1] == '/' {
                            i += 2;
                            closed = true;
                            break;
                        }
                        i += 1;
                    }
                    if !closed {
                        for ch in &chars[start..] {
                            out.push(*ch);
                        }
                        break;
                    }
                } else {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. Valid JSON passes through ──────────────────────────────────

    #[test]
    fn valid_json_passes_through() {
        let input = r#"{"file": "foo.rs", "line": 42}"#;
        let outcome = repair_tool_call_json(input);
        assert!(
            matches!(outcome, RepairOutcome::Valid(_)),
            "expected Valid, got {outcome:?}"
        );
    }

    // ── 2. Trailing comma ─────────────────────────────────────────────

    #[test]
    fn trailing_comma_repaired() {
        let input = r#"{"file": "foo.rs", "line": 42,}"#;
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "foo.rs");
                assert_eq!(value["line"], 42);
                assert!(repairs.iter().any(|r| r.detail.contains("trailing commas")));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    // ── 3. Unquoted keys ──────────────────────────────────────────────

    #[test]
    fn unquoted_keys_repaired() {
        let input = r#"{file: "foo.rs"}"#;
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "foo.rs");
                assert!(repairs.iter().any(|r| r.detail.contains("bare keys")));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    // ── 4. Single quotes ──────────────────────────────────────────────

    #[test]
    fn single_quotes_repaired() {
        let input = "{'file': 'foo.rs'}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "foo.rs");
                assert!(repairs.iter().any(|r| r.detail.contains("single quotes")));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn curly_double_quotes_are_json_quotes() {
        let input = "{\u{201c}a\u{201d}: 1}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["a"], 1);
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail.contains("curly"))
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        let input = "{\u{201c}a\u{201d}: \u{201c}hello\u{201d}}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, .. } => assert_eq!(value["a"], "hello"),
            other => panic!("expected Repaired, got {other:?}"),
        }
        let kept = "{\"a\": \"he said \u{201c}hi\u{201d}\"}";
        match repair_tool_call_json(kept) {
            RepairOutcome::Valid(value) => {
                assert_eq!(value["a"], "he said \u{201c}hi\u{201d}");
            }
            other => panic!("expected Valid, got {other:?}"),
        }
        let mixed = "{\"a\": \"he said \u{201c}hi\u{201d}\", b: 1}";
        match repair_tool_call_json(mixed) {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["a"], "he said \u{201c}hi\u{201d}");
                assert_eq!(value["b"], 1);
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        assert!(matches!(
            repair_tool_call_json("{\u{2018}a\u{2019}: 1}"),
            RepairOutcome::Unfixable(_)
        ));
    }

    #[test]
    fn control_characters_stripped() {
        let input = "{\"file\": \"foo\x07.rs\"}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "foo.rs");
                assert!(repairs.iter().any(|r| r.detail.contains("control")));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn raw_newline_inside_a_double_quoted_string_is_escaped() {
        let input = "{\"file\": \"foo\nbar\"}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "foo\nbar");
                assert!(
                    repairs
                        .iter()
                        .any(|rewrite| rewrite.detail.contains("escaped"))
                );
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
        let input = "{\"file\": \"foo\tbar\r\"}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, .. } => assert_eq!(value["file"], "foo\tbar\r"),
            other => panic!("expected Repaired, got {other:?}"),
        }
        assert!(matches!(
            repair_tool_call_json(r#"{"file":"foo\nbar"}"#),
            RepairOutcome::Valid(_)
        ));
        assert!(matches!(
            repair_tool_call_json("{\n\"file\": 1\n}"),
            RepairOutcome::Valid(_)
        ));
        assert!(matches!(
            repair_tool_call_json("{\"a\": \"hello\n\"b\": 1}"),
            RepairOutcome::Unfixable(_)
        ));
    }

    // ── 6. Block comments ─────────────────────────────────────────────

    #[test]
    fn block_comments_stripped() {
        let input = r#"{"file": "foo.rs" /* main */}"#;
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "foo.rs");
                assert!(repairs.iter().any(|r| r.detail.contains("comments")));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    // ── 7. Line comments ──────────────────────────────────────────────

    #[test]
    fn line_comments_stripped() {
        let input = "{\"file\": \"foo.rs\" // main\n}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "foo.rs");
                assert!(repairs.iter().any(|r| r.detail.contains("comments")));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    // ── 8. Multiple issues combined ───────────────────────────────────

    #[test]
    fn multiple_issues_combined() {
        // single quotes + bare key + trailing comma + block comment
        let input = "{file: 'foo.rs', line: 42, /* note */}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["file"], "foo.rs");
                assert_eq!(value["line"], 42);
                assert!(repairs.len() >= 2, "expected multiple repairs: {repairs:?}");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    // ── 9. Completely broken JSON ─────────────────────────────────────

    #[test]
    fn completely_broken_is_unfixable() {
        let input = "this is not json at all }{[]";
        assert!(
            matches!(repair_tool_call_json(input), RepairOutcome::Unfixable(_)),
            "expected Unfixable"
        );
    }

    // ── 10. Empty string ──────────────────────────────────────────────

    #[test]
    fn empty_string_is_unfixable() {
        assert!(
            matches!(repair_tool_call_json(""), RepairOutcome::Unfixable(_)),
            "expected Unfixable"
        );
    }

    // ── 11. Nested objects with issues ────────────────────────────────

    #[test]
    fn nested_objects_repaired() {
        let input = "{outer: {inner: 'value',}}";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                assert_eq!(value["outer"]["inner"], "value");
                assert!(!repairs.is_empty());
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    // ── 12. Arrays with trailing commas ───────────────────────────────

    #[test]
    fn array_trailing_comma_repaired() {
        let input = r#"{"items": [1, 2, 3,]}"#;
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, repairs } => {
                let items = value["items"].as_array().expect("items should be array");
                assert_eq!(items.len(), 3);
                assert!(repairs.iter().any(|r| r.detail.contains("trailing commas")));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    // ── Unit tests for individual functions ───────────────────────────

    #[test]
    fn strip_control_chars_preserves_whitespace() {
        let s = "hello\tworld\nfoo\rbar\x00baz\x1b";
        let out = strip_control_chars(s);
        assert_eq!(out, "hello\tworld\nfoo\rbarbaz");
    }

    #[test]
    fn fix_single_quotes_preserves_apostrophe_in_double_quoted() {
        let s = r#"{"msg": "it's fine"}"#;
        let out = fix_single_quotes(s);
        // The apostrophe inside a double-quoted string is untouched.
        assert_eq!(out, r#"{"msg": "it's fine"}"#);
    }

    #[test]
    fn fix_single_quotes_handles_embedded_double_quote() {
        // Single-quoted string containing a double quote.
        let s = r#"{'key': 'say "hi"'}"#;
        let out = fix_single_quotes(s);
        assert_eq!(out, r#"{"key": "say \"hi\""}"#);
    }

    #[test]
    fn quote_bare_keys_leaves_values_alone() {
        let input = r#"{enabled: true, count: null}"#;
        let out = quote_bare_keys(input);
        assert_eq!(out, r#"{"enabled": true, "count": null}"#);
    }

    #[test]
    fn remove_trailing_commas_inside_string() {
        // A comma-brace inside a string must NOT be modified.
        let input = r#"{"a": "x,}"}"#;
        let out = remove_trailing_commas(input);
        assert_eq!(out, r#"{"a": "x,}"}"#);
    }

    #[test]
    fn strip_json_comments_inside_string() {
        // "// not a comment" inside a string must be preserved.
        let input = r#"{"url": "https://x.com"}"#;
        let out = strip_json_comments(input);
        assert_eq!(out, input);
    }

    #[test]
    fn unclosed_block_comment_is_left_in_place() {
        let input = r#"{"file": "a.rs" /* , "cmd": "rm" }"#;
        assert_eq!(strip_json_comments(input), input);
    }

    #[test]
    fn strip_json_comments_inside_single_quoted_string() {
        let input = "{'url': 'http://example.com/a', 'note': 'keep /* this */'}";
        assert_eq!(strip_json_comments(input), input);
    }

    #[test]
    fn single_quoted_url_keeps_the_path_and_drops_the_trailing_comment() {
        let input = "{'url': 'http://example.com/a', 'note': 'keep /* this */'} // tail";
        match repair_tool_call_json(input) {
            RepairOutcome::Repaired { value, .. } => {
                assert_eq!(value["url"], "http://example.com/a");
                assert_eq!(value["note"], "keep /* this */");
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn fix_triple_quotes_basic() {
        let input = r#"{"content": """hello\nworld"""}"#;
        let out = fix_triple_quotes(input);
        assert_eq!(out, r#"{"content": "hello\\nworld"}"#);
    }

    #[test]
    fn fix_triple_quotes_with_embedded_double_quotes() {
        let input = r#"{"content": """say "hi" please"""}"#;
        let out = fix_triple_quotes(input);
        assert_eq!(out, r#"{"content": "say \"hi\" please"}"#);
    }

    #[test]
    fn fix_triple_quotes_multiline() {
        let input = "{\n  \"content\": \"\"\"\nline1\nline2\n\"\"\"\n}";
        let out = fix_triple_quotes(input);
        assert_eq!(out, "{\n  \"content\": \"\\nline1\\nline2\\n\"\n}");
    }

    #[test]
    fn fix_triple_quotes_no_change_when_absent() {
        let input = r#"{"content": "normal string"}"#;
        let out = fix_triple_quotes(input);
        assert_eq!(out, input);
    }

    #[test]
    fn quote_bare_keys_multiple() {
        let input = r#"{foo: 1, bar_baz: "hello"}"#;
        let out = quote_bare_keys(input);
        assert_eq!(out, r#"{"foo": 1, "bar_baz": "hello"}"#);
    }

    #[test]
    fn valid_array_passes_through() {
        let input = r#"[1, 2, 3]"#;
        assert!(matches!(
            repair_tool_call_json(input),
            RepairOutcome::Valid(_)
        ));
    }

    // ── Property-based tests ──────────────────────────────────────────

    #[test]
    fn repair_outcome_unfixable_is_not_silently_consumed() {
        // Verify the low-level API correctly reports Unfixable (not Valid/Repaired)
        let garbage = "<<<binary garbage>>>";
        let outcome = repair_tool_call_json(garbage);
        assert!(
            matches!(outcome, RepairOutcome::Unfixable(_)),
            "expected Unfixable, got {outcome:?}"
        );
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Any valid JSON survives repair unchanged (idempotence).
            #[test]
            fn valid_json_passes_through(s in "[a-zA-Z0-9 _]{0,20}") {
                let json = serde_json::to_string(&s).unwrap();
                match repair_tool_call_json(&json) {
                    RepairOutcome::Valid(v) => {
                        assert_eq!(v.as_str().unwrap(), s);
                    }
                    other => panic!("expected Valid, got {other:?}"),
                }
            }

            /// Repair never panics on arbitrary input.
            #[test]
            fn repair_never_panics(s in "\\PC{0,200}") {
                let _ = repair_tool_call_json(&s);
            }

            /// strip_control_chars is idempotent.
            #[test]
            fn strip_control_chars_idempotent(s in "\\PC{0,100}") {
                let once = strip_control_chars(&s);
                let twice = strip_control_chars(&once);
                assert_eq!(once, twice);
            }

            /// remove_trailing_commas is idempotent.
            #[test]
            fn remove_trailing_commas_idempotent(s in "\\PC{0,100}") {
                let once = remove_trailing_commas(&s);
                let twice = remove_trailing_commas(&once);
                assert_eq!(once, twice);
            }

            /// Repair of a JSON object with trailing comma produces
            /// semantically identical JSON.
            #[test]
            fn trailing_comma_repair_preserves_value(
                key in "[a-z]{1,10}",
                val in 0i64..1000,
            ) {
                let with_comma = format!("{{\"{key}\": {val},}}");
                match repair_tool_call_json(&with_comma) {
                    RepairOutcome::Repaired { value, .. } => {
                        assert_eq!(value[&key], val);
                    }
                    RepairOutcome::Valid(value) => {
                        assert_eq!(value[&key], val);
                    }
                    other => panic!("expected Repaired, got {other:?}"),
                }
            }

            /// Single-quoted JSON strings repair to valid double-quoted JSON.
            #[test]
            fn single_quote_repair_produces_valid_json(
                key in "[a-z]{1,10}",
                val in "[a-zA-Z0-9 ]{0,20}",
            ) {
                let input = format!("{{'{key}': '{val}'}}");
                match repair_tool_call_json(&input) {
                    RepairOutcome::Valid(_) | RepairOutcome::Repaired { .. } => {}
                    RepairOutcome::Unfixable(e) | RepairOutcome::Ambiguous(e) => {
                        panic!("single-quote repair failed: {e}");
                    }
                }
            }

            /// Bare-key repair produces valid JSON.
            #[test]
            fn bare_key_repair_produces_valid_json(
                key in "[a-z]{1,10}",
                val in 0i64..1000,
            ) {
                let input = format!("{{{key}: {val}}}");
                match repair_tool_call_json(&input) {
                    RepairOutcome::Valid(_) | RepairOutcome::Repaired { .. } => {}
                    RepairOutcome::Unfixable(e) | RepairOutcome::Ambiguous(e) => {
                        panic!("bare-key repair failed: {e}");
                    }
                }
            }
        }
    }
}
