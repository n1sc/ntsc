//! NTSC standard library: `toml` module.
//! TOML values are compact TOML strings; arguments are borrowed handles,
//! returned handles are owned by the caller.

use crate::registry;

use crate::modules::unescape;

struct TomlParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> TomlParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            while self.pos < self.input.len()
                && self.input.as_bytes()[self.pos].is_ascii_whitespace()
            {
                self.pos += 1;
            }
            if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'#' {
                while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let c = self.input.as_bytes().get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn advance_char(&mut self) -> Option<char> {
        let c = self.input[self.pos..].chars().next()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn parse_key(&mut self) -> Result<String, String> {
        self.skip_whitespace_and_comments();
        if self.peek() == Some(b'"') {
            self.parse_bare_string()
        } else if self.peek() == Some(b'\'') {
            self.parse_literal_key()
        } else {
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos == start {
                return Err("expected key".to_string());
            }
            Ok(self.input[start..self.pos].to_string())
        }
    }

    fn parse_value(&mut self) -> Result<String, String> {
        self.skip_whitespace_and_comments();
        match self.peek() {
            Some(b'"') => self.parse_quoted_string(),
            Some(b'\'') => self.parse_literal_string(),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_inline_table(),
            Some(b't') | Some(b'f') => self.parse_bool(),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            _ => Err(format!(
                "unexpected character: {:?}",
                self.peek().map(|c| c as char)
            )),
        }
    }

    fn parse_quoted_string(&mut self) -> Result<String, String> {
        self.advance();
        let mut result = String::new();
        loop {
            match self.advance_char() {
                Some('"') => {
                    let escaped = result.replace('\\', "\\\\").replace('"', "\\\"");
                    return Ok(format!("\"{escaped}\""));
                }
                Some('\\') => match self.advance_char() {
                    Some('"') => result.push('"'),
                    Some('\\') => result.push('\\'),
                    Some('n') => result.push('\n'),
                    Some('r') => result.push('\r'),
                    Some('t') => result.push('\t'),
                    Some(c) => result.push(c),
                    None => return Err("unexpected end in string escape".to_string()),
                },
                Some(c) => result.push(c),
                None => return Err("unterminated string".to_string()),
            }
        }
    }

    fn parse_bare_string(&mut self) -> Result<String, String> {
        self.parse_quoted_string()
    }

    fn parse_literal_key(&mut self) -> Result<String, String> {
        self.advance();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == b'\'' {
                let s = self.input[start..self.pos].to_string();
                self.advance();
                // Re-quote so downstream treats the key as one atomic
                // segment (literal keys never split on dots).
                return Ok(format!("\"{s}\""));
            }
            self.pos += 1;
        }
        Err("unterminated literal key".to_string())
    }

    fn parse_literal_string(&mut self) -> Result<String, String> {
        self.advance();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == b'\'' {
                let s = self.input[start..self.pos].to_string();
                self.advance();
                return Ok(format!("\"{s}\""));
            }
            self.pos += 1;
        }
        Err("unterminated literal string".to_string())
    }

    fn parse_number(&mut self) -> Result<String, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.advance();
        }
        // TOML radix integers (`0x`, `0o`, `0b`): underscores separate
        // digits and the value is emitted decimal.
        if self.input[self.pos..].starts_with("0x")
            || self.input[self.pos..].starts_with("0o")
            || self.input[self.pos..].starts_with("0b")
        {
            let radix = match self.input.as_bytes()[self.pos + 1] {
                b'x' => 16,
                b'o' => 8,
                _ => 2,
            };
            self.pos += 2;
            let digits = self.pos;
            while let Some(c) = self.peek() {
                if c == b'_' || char::from(c).is_digit(radix) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let text: String = self.input[digits..self.pos]
                .chars()
                .filter(|&c| c != '_')
                .collect();
            if text.is_empty() {
                return Err("invalid radix integer".to_string());
            }
            let negative = self.input[start..self.pos].starts_with('-');
            let val = i64::from_str_radix(&text, radix).map_err(|e| format!("invalid int: {e}"))?;
            return Ok(if negative {
                (-val).to_string()
            } else {
                val.to_string()
            });
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit()
                || c == b'.'
                || c == b'e'
                || c == b'E'
                || c == b'+'
                || c == b'-'
                || c == b'_'
            {
                if (c == b'+' || c == b'-')
                    && (self.pos == start
                        || (self.pos > 0
                            && self.input.as_bytes()[self.pos - 1] != b'e'
                            && self.input.as_bytes()[self.pos - 1] != b'E'))
                {
                    break;
                }
                self.pos += 1;
            } else {
                break;
            }
        }
        let text: String = self.input[start..self.pos]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        if text.contains('.') || text.contains('e') || text.contains('E') {
            let val: f64 = text.parse().map_err(|e| format!("invalid float: {e}"))?;
            Ok(format!("{val}"))
        } else {
            let val: i64 = text.parse().map_err(|e| format!("invalid int: {e}"))?;
            Ok(val.to_string())
        }
    }

    fn parse_bool(&mut self) -> Result<String, String> {
        if self.input[self.pos..].starts_with("true") {
            self.pos += 4;
            Ok("true".to_string())
        } else if self.input[self.pos..].starts_with("false") {
            self.pos += 5;
            Ok("false".to_string())
        } else {
            Err("expected boolean".to_string())
        }
    }

    fn parse_array(&mut self) -> Result<String, String> {
        self.advance();
        let mut elements = Vec::new();
        self.skip_whitespace_and_comments();
        if self.peek() == Some(b']') {
            self.advance();
            return Ok("[]".to_string());
        }
        loop {
            self.skip_whitespace_and_comments();
            if self.peek() == Some(b'#') {
                self.skip_whitespace_and_comments();
            }
            let val = self.parse_value()?;
            elements.push(val);
            self.skip_whitespace_and_comments();
            match self.peek() {
                Some(b']') => {
                    self.advance();
                    return Ok(format!("[{}]", elements.join(",")));
                }
                Some(b',') => {
                    self.advance();
                }
                _ => return Err("expected ',' or ']' in array".to_string()),
            }
        }
    }

    fn parse_inline_table(&mut self) -> Result<String, String> {
        self.advance();
        let mut pairs = Vec::new();
        self.skip_whitespace_and_comments();
        if self.peek() == Some(b'}') {
            self.advance();
            return Ok("{}".to_string());
        }
        loop {
            let key = self.parse_key()?;
            self.skip_whitespace_and_comments();
            if self.advance() != Some(b'=') {
                return Err("expected '=' in inline table".to_string());
            }
            let val = self.parse_value()?;
            pairs.push(format!("\"{key}\":{val}"));
            self.skip_whitespace_and_comments();
            match self.peek() {
                Some(b'}') => {
                    self.advance();
                    return Ok(format!("{{{}}}", pairs.join(",")));
                }
                Some(b',') => {
                    self.advance();
                }
                _ => return Err("expected ',' or '}}' in inline table".to_string()),
            }
        }
    }

    fn parse_toml(&mut self) -> Result<String, String> {
        let mut result = String::from("{");
        let mut first = true;
        let mut current_table: Vec<String> = Vec::new();

        while self.pos < self.input.len() {
            self.skip_whitespace_and_comments();
            if self.pos >= self.input.len() {
                break;
            }
            match self.peek() {
                Some(b'[') => {
                    self.advance();
                    if self.peek() == Some(b'[') {
                        self.advance();
                        let key = self.parse_array_of_tables_header()?;
                        self.skip_whitespace_and_comments();
                        if self.peek() == Some(b']') {
                            self.advance();
                        }
                        if self.peek() == Some(b']') {
                            self.advance();
                        }
                        current_table = key;
                    } else {
                        let key = self.parse_section_header()?;
                        self.skip_whitespace_and_comments();
                        if self.peek() == Some(b']') {
                            self.advance();
                        }
                        current_table = key;
                    }
                    continue;
                }
                Some(_) => {}
                None => break,
            }
            let key = self.parse_key()?;
            self.skip_whitespace_and_comments();
            if self.advance() != Some(b'=') {
                return Err("expected '='".to_string());
            }
            let val = self.parse_value()?;
            let full_key = if current_table.is_empty() {
                quote_dotted_key(&key)
            } else {
                let prefix: Vec<String> =
                    current_table.iter().map(|k| quote_dotted_key(k)).collect();
                format!("{}.{}", prefix.join("."), quote_dotted_key(&key))
            };
            if !first {
                result.push(',');
            }
            result.push_str(&format!("{full_key}:{val}"));
            first = false;
            self.skip_whitespace_and_comments();
        }

        build_nested_json(&result)
    }

    fn parse_section_header(&mut self) -> Result<Vec<String>, String> {
        self.skip_whitespace_and_comments();
        let mut keys = Vec::new();
        loop {
            let key = self.parse_key()?;
            keys.push(key);
            self.skip_whitespace_and_comments();
            if self.peek() == Some(b'.') {
                self.advance();
            } else {
                break;
            }
        }
        Ok(keys)
    }

    fn parse_array_of_tables_header(&mut self) -> Result<Vec<String>, String> {
        self.parse_section_header()
    }
}

/// Quote one raw key for the flat intermediate format: a key that arrived
/// quoted in source (`"b.c"`) stays one atomic quoted piece, while a bare
/// dotted key (`a.b`) splits into individually quoted segments. Every
/// piece the regrouper sees is therefore exactly one path segment.
fn quote_dotted_key(key: &str) -> String {
    if key.starts_with('"') {
        return key.to_string();
    }
    key.split('.')
        .map(|piece| format!("\"{}\"", piece.trim()))
        .collect::<Vec<_>>()
        .join(".")
}

/// A node of the regrouped document: either a verbatim JSON value or an
/// ordered object. Ordering follows first appearance, matching the source.
enum NestedNode {
    Value(String),
    Section(Vec<(String, NestedNode)>),
}

/// Regroup flat `key:val` entries (keys may carry dotted, quoted section
/// prefixes like `"a"."b"."x"`) into properly nested JSON objects, so
/// `[a.b]` merges into `[a]` instead of sitting beside it as a sibling.
///
/// The input carries the leading `{` the top-level loop opens with;
/// without stripping it the depth tracker never splits anything and the
/// whole document collapses into a single entry.
fn build_nested_json(flat: &str) -> Result<String, String> {
    let flat = flat.trim();
    let flat = flat.strip_prefix('{').unwrap_or(flat).trim();
    if flat.is_empty() {
        return Ok(String::from("{}"));
    }
    let entries = split_json_entries(flat)?;
    let mut root: Vec<(String, NestedNode)> = Vec::new();
    for entry in &entries {
        let Some(colon_pos) = find_json_key_colon(entry) else {
            continue;
        };
        let path = split_key_path(entry[..colon_pos].trim())?;
        let value = entry[colon_pos + 1..].trim().to_string();
        insert_nested(&mut root, &path, value);
    }
    let mut out = String::from("{");
    write_nested(&mut out, &root);
    out.push('}');
    Ok(out)
}

/// Split a dotted key path on dots outside double quotes (`"a"."b:c"."x"`
/// yields three segments). A segment that was never quoted is split
/// further on dots (bare `[a.b]` headers arrive as one dotted piece);
/// quoted segments stay atomic. Each segment is then canonicalized by
/// stripping surrounding quotes until none remain and unescaping
/// `\"`/`\\`.
fn split_key_path(path: &str) -> Result<Vec<String>, String> {
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut in_str = false;
    let mut escape_next = false;
    for c in path.chars() {
        if escape_next {
            current.push(c);
            escape_next = false;
        } else if in_str && c == '\\' {
            current.push(c);
            escape_next = true;
        } else if c == '"' {
            in_str = !in_str;
            quoted = true;
            current.push(c);
        } else if c == '.' && !in_str {
            parts.push((current.clone(), quoted));
            current.clear();
            quoted = false;
        } else {
            current.push(c);
        }
    }
    if in_str {
        return Err("unterminated quoted key".to_string());
    }
    parts.push((current, quoted));
    let mut out = Vec::new();
    for (part, was_quoted) in parts {
        // A quoted piece is one atomic key even when it holds dots
        // (`[a."b.c"]`); a bare piece (`[a.b]` arrives pre-wrapped and
        // unquoted inside) splits further on dots.
        let trimmed = part.trim();
        let atomic =
            was_quoted && trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2;
        if atomic {
            out.push(canonical_key_segment(trimmed));
        } else {
            for piece in trimmed.split('.') {
                out.push(canonical_key_segment(piece));
            }
        }
    }
    Ok(out)
}

/// Strip surrounding quotes until none remain and unescape `\"`/`\\`.
fn canonical_key_segment(part: &str) -> String {
    let mut part = part.trim().to_string();
    loop {
        let stripped = part
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .map(str::to_string);
        match stripped {
            Some(inner) => part = inner,
            None => break,
        }
    }
    part.replace("\\\"", "\"").replace("\\\\", "\\")
}

/// Insert `value` at `path`, creating intermediate sections. A repeated
/// full path overwrites (later `[[table]]` blocks win over earlier ones).
fn insert_nested(nodes: &mut Vec<(String, NestedNode)>, path: &[String], value: String) {
    let Some((head, rest)) = path.split_first() else {
        return;
    };
    let slot = match nodes.iter_mut().find(|(k, _)| k == head) {
        Some((_, node)) => node,
        None => {
            nodes.push((head.clone(), NestedNode::Section(Vec::new())));
            &mut nodes.last_mut().expect("just pushed").1
        }
    };
    if rest.is_empty() {
        *slot = NestedNode::Value(value);
        return;
    }
    match slot {
        NestedNode::Section(children) => insert_nested(children, rest, value),
        // A scalar already sits where a section wants to grow (a key that
        // is both a value and a table prefix): the section wins, matching
        // how repeated tables behave elsewhere here.
        taken => {
            let mut children = Vec::new();
            insert_nested(&mut children, rest, value);
            *taken = NestedNode::Section(children);
        }
    }
}

/// Escape one object key for JSON output.
fn escape_json_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 2);
    out.push('"');
    for c in key.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_nested(out: &mut String, nodes: &[(String, NestedNode)]) {
    let mut first = true;
    for (key, node) in nodes {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&escape_json_key(key));
        out.push(':');
        match node {
            NestedNode::Value(value) => out.push_str(value),
            NestedNode::Section(children) => {
                out.push('{');
                write_nested(out, children);
                out.push('}');
            }
        }
    }
}

fn find_json_key_colon(entry: &str) -> Option<usize> {
    let bytes = entry.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'"' {
            i += 1;
            while i < len && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < len {
                i += 1;
            }
            while i < len && bytes[i] != b':' {
                i += 1;
            }
            if i < len {
                return Some(i);
            }
            return None;
        }
        i += 1;
    }
    None
}

fn split_json_entries(s: &str) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut in_str = false;
    let mut escape_next = false;
    for c in s.chars() {
        if escape_next {
            current.push(c);
            escape_next = false;
            continue;
        }
        if c == '\\' && in_str {
            current.push(c);
            escape_next = true;
            continue;
        }
        if c == '"' {
            in_str = !in_str;
            current.push(c);
            continue;
        }
        if in_str {
            current.push(c);
            continue;
        }
        if c == '{' || c == '[' {
            depth += 1;
            current.push(c);
        } else if c == '}' || c == ']' {
            depth -= 1;
            current.push(c);
        } else if c == ',' && depth == 0 {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                entries.push(trimmed);
            }
            current.clear();
        } else {
            current.push(c);
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        entries.push(trimmed);
    }
    Ok(entries)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_toml_parse(s: i64) -> i64 {
    let input = registry::get_string(s).unwrap_or_default();
    let input = unescape(&input);
    let mut parser = TomlParser::new(&input);
    match parser.parse_toml() {
        Ok(result) => registry::put_string(result),
        Err(e) => super::throw_str(format!("toml.parse: TOML parse error: {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_toml_stringify(s: i64) -> i64 {
    let json = registry::get_string(s).unwrap_or_default();
    let json = json.trim();
    if json.is_empty() || json == "{}" {
        return registry::put_string(String::new());
    }
    if !json.starts_with('{') {
        return super::throw_str("toml.stringify: expected a JSON object".to_string());
    }
    let mut result = String::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let inner = &json[1..json.len() - 1].trim();
    if inner.is_empty() {
        return registry::put_string(String::new());
    }
    let entries = match split_json_entries(inner) {
        Ok(e) => e,
        Err(_) => {
            return super::throw_str("toml.stringify: malformed JSON object".to_string());
        }
    };
    for entry in &entries {
        if let Some(colon_pos) = find_json_key_colon(entry) {
            let key_part = &entry[..colon_pos].trim().trim_matches('"');
            let val_part = entry[colon_pos + 1..].trim();
            if val_part.starts_with('{') {
                sections.push((key_part.to_string(), val_part.to_string()));
            } else {
                result.push_str(&format!("{key_part} = {}\n", json_to_toml_value(val_part)));
            }
        }
    }
    for (key, val) in &sections {
        write_toml_section(&mut result, std::slice::from_ref(key), val);
    }
    registry::put_string(result)
}

/// Emit one `[dotted.path]` section: scalar fields first, then recurse
/// into sub-objects so arbitrarily nested JSON round-trips.
fn write_toml_section(result: &mut String, path: &[String], val: &str) {
    result.push_str(&format!("\n[{}]\n", path.join(".")));
    // Strip the object's own braces first: with them present the depth
    // tracker never splits and the whole section collapses into one
    // malformed entry.
    let inner_val = val
        .trim()
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(val)
        .trim();
    let Ok(nested_entries) = split_json_entries(inner_val) else {
        return;
    };
    let mut subs = Vec::new();
    for entry in &nested_entries {
        let Some(colon_pos) = find_json_key_colon(entry) else {
            continue;
        };
        let k = entry[..colon_pos].trim().trim_matches('"');
        let v = entry[colon_pos + 1..].trim();
        if v.starts_with('{') {
            subs.push((k.to_string(), v.to_string()));
        } else {
            result.push_str(&format!("{k} = {}\n", json_to_toml_value(v)));
        }
    }
    for (k, v) in subs {
        let mut child = path.to_vec();
        child.push(k);
        write_toml_section(result, &child, &v);
    }
}

fn json_to_toml_value(val: &str) -> String {
    if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
        || val == "true"
        || val == "false"
    {
        val.to_string()
    } else if val == "null" {
        "null".to_string()
    } else if val.starts_with('[') && val.ends_with(']') {
        let inner = &val[1..val.len() - 1].trim();
        if inner.is_empty() {
            "[]".to_string()
        } else {
            let items: Vec<&str> = inner.split(',').map(|s| s.trim()).collect();
            let formatted: Vec<String> = items.iter().map(|s| json_to_toml_value(s)).collect();
            format!("[{}]", formatted.join(", "))
        }
    } else {
        val.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::test_util::catch_throw;

    fn put(s: &str) -> i64 {
        registry::put_string(s.to_string())
    }

    fn read(id: i64) -> String {
        let s = registry::get_string(id).unwrap_or_default();
        let _ = registry::take_string(id);
        s
    }

    #[test]
    fn test_parse_simple() {
        let input = "name = \"Alice\"\nage = 30\nactive = true";
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"name\":\"Alice\""));
        assert!(s.contains("\"age\":30"));
        assert!(s.contains("\"active\":true"));
    }

    #[test]
    fn test_parse_section() {
        let input = "[server]\nhost = \"localhost\"\nport = 8080";
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"server\""));
        assert!(s.contains("\"host\":\"localhost\""));
        assert!(s.contains("\"port\":8080"));
    }

    #[test]
    fn test_parse_empty() {
        let r = ntsc_toml_parse(put(""));
        assert_eq!(read(r), "{}");
    }

    #[test]
    fn test_parse_comment() {
        let input = "# comment\nname = \"test\"\n# another\nvalue = 42";
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"name\":\"test\""));
        assert!(s.contains("\"value\":42"));
    }

    #[test]
    fn test_stringify() {
        let json = r#"{"name":"Alice","age":30}"#;
        let r = ntsc_toml_stringify(put(json));
        let s = read(r);
        assert!(s.contains("name = \"Alice\""));
        assert!(s.contains("age = 30"));
    }

    #[test]
    fn test_stringify_nests_sections() {
        let json = r#"{"name":"Alice","view":{"timestamp":true}}"#;
        let r = ntsc_toml_stringify(put(json));
        let s = read(r);
        assert!(s.contains("name = \"Alice\""), "scalar: {s}");
        assert!(s.contains("[view]"), "section: {s}");
        assert!(s.contains("timestamp = true"), "nested: {s}");
        assert!(!s.contains('{'), "no raw JSON braces: {s}");
    }

    #[test]
    fn test_parse_stringify_roundtrip() {
        let input = "[a]\nx = 1\n[a.b]\ny = 2";
        let parsed = read(ntsc_toml_parse(put(input)));
        let back = read(ntsc_toml_stringify(put(&parsed)));
        assert!(back.contains("[a]"), "section a: {back}");
        assert!(back.contains("x = 1"), "field: {back}");
        let reparsed = read(ntsc_toml_parse(put(&back)));
        assert!(reparsed.contains("\"y\":2"), "nested survives: {reparsed}");
    }

    #[test]
    fn test_stringify_empty() {
        let r = ntsc_toml_stringify(put("{}"));
        assert_eq!(read(r), "");
    }

    #[test]
    fn test_stringify_not_object() {
        let err = catch_throw(|| {
            let _ = ntsc_toml_stringify(put("[1,2,3]"));
        });
        assert!(err.is_some());
        assert!(err.unwrap().contains("toml.stringify"));
    }

    #[test]
    fn test_parse_inline_table() {
        let input = "server = {host = \"localhost\", port = 8080}";
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"server\""));
        assert!(s.contains("\"host\":\"localhost\""));
        assert!(s.contains("\"port\":8080"));
    }

    #[test]
    fn test_parse_nested_section_nests() {
        let input = "[a]\nx = 1\n[a.b]\ny = 2";
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert_eq!(s, "{\"a\":{\"x\":1,\"b\":{\"y\":2}}}");
    }

    #[test]
    fn test_parse_dotted_keys_share_one_section() {
        let input = "[a]\nx = 1\nz = 2";
        let r = ntsc_toml_parse(put(input));
        assert_eq!(read(r), "{\"a\":{\"x\":1,\"z\":2}}");
    }

    #[test]
    fn test_parse_repeated_table_merges() {
        let input = "[[p]]\na = 1\n[[p]]\nb = 2";
        let r = ntsc_toml_parse(put(input));
        // Array-of-tables collapse to one merged object, later keys winning.
        assert_eq!(read(r), "{\"p\":{\"a\":1,\"b\":2}}");
    }

    #[test]
    fn test_parse_radix_integers() {
        let input = "a = 0x0900\nb = 0o17\nc = 0b101\nd = 1_000";
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"a\":2304"), "hex: {s}");
        assert!(s.contains("\"b\":15"), "octal: {s}");
        assert!(s.contains("\"c\":5"), "binary: {s}");
        assert!(s.contains("\"d\":1000"), "underscores: {s}");
    }

    #[test]
    fn test_parse_unicode_strings() {
        let input = "greeting = \"Привет\"\n[\"日本語\"]\n\"한글\" = \"값\"";
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"greeting\":\"Привет\""), "value: {s}");
        assert!(s.contains("\"日本語\""), "section: {s}");
        assert!(s.contains("\"한글\":\"값\""), "key/value: {s}");
    }

    #[test]
    fn test_parse_literal_keys() {
        let input = "[sec]\n'plain' = 1\n'х' = 'h'";
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"plain\":1"), "literal key: {s}");
        assert!(s.contains("\"х\":\"h\""), "unicode literal key: {s}");
    }

    #[test]
    fn test_parse_string_with_quotes() {
        let input = r#"msg = "say \\\"hello\\\"""#;
        let r = ntsc_toml_parse(put(input));
        let s = read(r);
        assert!(s.contains("say \\\"hello\\\""), "unexpected output: {s}");
    }
}
