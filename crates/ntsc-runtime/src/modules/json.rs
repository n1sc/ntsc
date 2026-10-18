//! NTSC standard library: `json` module.
//! JSON values are compact JSON strings; arguments are borrowed handles,
//! returned handles are owned by the caller.

use crate::registry;

struct JsonParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn at(input: &'a str, pos: usize) -> Self {
        Self { input, pos }
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_whitespace() {
            self.pos += 1;
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

    fn expect(&mut self, expected: u8) -> Result<(), String> {
        if self.advance() == Some(expected) {
            Ok(())
        } else {
            Err(format!("Expected '{}'", expected as char))
        }
    }

    fn parse_value(&mut self) -> Result<String, String> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'"') => self.parse_string(),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b't') | Some(b'f') => self.parse_bool(),
            Some(b'n') => self.parse_null(),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(c) => Err(format!("Unexpected character: {}", c as char)),
            None => Err("Unexpected end of JSON".to_string()),
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut result = String::new();
        loop {
            match self.advance_char() {
                Some('"') => return Ok(format!("\"{}\"", result)),
                Some('\\') => match self.advance_char() {
                    Some('"') => result.push('"'),
                    Some('\\') => result.push('\\'),
                    Some('/') => result.push('/'),
                    Some('b') => result.push('\u{0008}'),
                    Some('f') => result.push('\u{000C}'),
                    Some('n') => result.push('\n'),
                    Some('r') => result.push('\r'),
                    Some('t') => result.push('\t'),
                    Some('u') => {
                        // \uXXXX escapes are not decoded; each is replaced with '?'.
                        for _ in 0..4 {
                            let _ = self.advance_char();
                        }
                        result.push('?');
                    }
                    Some(c) => result.push(c),
                    None => return Err("Unexpected end in string escape".to_string()),
                },
                Some(c) => result.push(c),
                None => return Err("Unterminated string".to_string()),
            }
        }
    }

    fn parse_number(&mut self) -> Result<String, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.advance();
        }
        while let Some(c) = self.peek() {
            // Allow '+'/'-' only immediately after 'e'/'E'.
            if c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E' || c == b'+' || c == b'-' {
                if (c == b'+' || c == b'-')
                    && (self.pos == start
                        || (self.pos > 0
                            && self.input.as_bytes()[self.pos - 1] != b'e'
                            && self.input.as_bytes()[self.pos - 1] != b'E'))
                {
                    break;
                }
                self.advance();
            } else {
                break;
            }
        }
        Ok(self.input[start..self.pos].to_string())
    }

    fn parse_bool(&mut self) -> Result<String, String> {
        if self.input[self.pos..].starts_with("true") {
            self.pos += 4;
            Ok("true".to_string())
        } else if self.input[self.pos..].starts_with("false") {
            self.pos += 5;
            Ok("false".to_string())
        } else {
            Err("Expected boolean".to_string())
        }
    }

    fn parse_null(&mut self) -> Result<String, String> {
        if self.input[self.pos..].starts_with("null") {
            self.pos += 4;
            Ok("null".to_string())
        } else {
            Err("Expected null".to_string())
        }
    }

    fn parse_array(&mut self) -> Result<String, String> {
        self.expect(b'[')?;
        let mut elements: Vec<String> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.advance();
            return Ok("[]".to_string());
        }
        loop {
            let val = self.parse_value()?;
            elements.push(val);
            self.skip_whitespace();
            match self.peek() {
                Some(b']') => {
                    self.advance();
                    return Ok(format!("[{}]", elements.join(",")));
                }
                Some(b',') => {
                    self.advance();
                    self.skip_whitespace();
                }
                _ => return Err("Expected ',' or ']' in array".to_string()),
            }
        }
    }

    fn parse_object(&mut self) -> Result<String, String> {
        self.expect(b'{')?;
        let mut pairs: Vec<String> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.advance();
            return Ok("{}".to_string());
        }
        loop {
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let val = self.parse_value()?;
            pairs.push(format!("{}:{}", key, val));
            self.skip_whitespace();
            match self.peek() {
                Some(b'}') => {
                    self.advance();
                    return Ok(format!("{{{}}}", pairs.join(",")));
                }
                Some(b',') => {
                    self.advance();
                    self.skip_whitespace();
                }
                _ => return Err("Expected ',' or '}' in object".to_string()),
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_parse(s: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let mut parser = JsonParser::new(&s);
    match parser.parse_value() {
        Ok(result) => registry::put_string(result),
        Err(e) => super::throw_str(format!("json.parse: JSON parse error: {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_stringify(s: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    registry::put_string(s)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_is_valid(s: i64) -> i8 {
    let s = registry::get_string(s).unwrap_or_default();
    let mut parser = JsonParser::new(&s);
    i8::from(parser.parse_value().is_ok())
}

/// `json.get(json_str, key)` — the value for `key`, or the string "null"
/// when the key is absent (a real JSON `null` is indistinguishable).
/// Validate that `text` is one complete JSON value.
///
/// The only caller is the `http` module, which is feature-gated out of core
/// runtime builds; without the gate those builds report dead code for this.
#[cfg(feature = "http")]
pub(crate) fn json_validate(text: &str) -> Result<(), String> {
    let mut parser = JsonParser::new(text);
    parser.parse_value().map(|_| ())
}

/// Read one key from a JSON object, returning the raw value text (a quoted
/// string keeps its quotes, matching the `json` module's own values), or
/// `None` when the key is absent or malformed. Other modules that need to
/// read a nested value out of a JSON blob reuse this instead of going
/// through the string registry.
///
/// Only the `http` module (feature-gated out of core runtime builds) calls
/// this today.
#[cfg(feature = "http")]
pub(crate) fn json_get_str(json: &str, key: &str) -> Option<String> {
    let search = format!("\"{key}\":");
    let pos = json.find(&search)? + search.len();
    let mut parser = JsonParser::new(&json[pos..]);
    parser.parse_value().ok()
}

/// The value of an *immediate* member of a JSON object, or `None`.
///
/// Only top-level members are considered. Searching the raw text for
/// `"key":` would let a nested object shadow its parent, so
/// `{"artist":{"mbid":"a"},"mbid":"b"}` would answer `"a"` for `mbid`.
fn immediate_member(json: &str, key: &str) -> Option<String> {
    let bytes = json.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            // A nested value belongs to another member: skip it whole.
            b'{' | b'[' => i = skip_value(json, i),
            b',' | b'}' | b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'"' => {
                let mut parser = JsonParser::at(json, i);
                let Ok(name) = parser.parse_string() else {
                    return None;
                };
                let after = after_key_colon(json, parser.pos)?;
                if unquote_key(&name) == key {
                    let mut value_parser = JsonParser::at(json, after);
                    return Some(value_parser.parse_value().unwrap_or_else(|_| "null".into()));
                }
                i = skip_value(json, after);
            }
            _ => return None,
        }
    }
    None
}

/// The offset just past the `:` (and its whitespace) following a member key.
fn after_key_colon(json: &str, pos: usize) -> Option<usize> {
    let bytes = json.as_bytes();
    let mut i = pos;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    Some(i)
}

/// A parsed member key, which keeps its surrounding quotes.
fn unquote_key(name: &str) -> &str {
    let trimmed = name.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

/// Skip one JSON value starting at `pos`, returning the offset just past it.
fn skip_value(json: &str, pos: usize) -> usize {
    let bytes = json.as_bytes();
    let mut i = pos;
    if i >= bytes.len() {
        return i;
    }
    match bytes[i] {
        b'"' => {
            let mut escape = false;
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                if escape {
                    escape = false;
                } else if c == b'\\' {
                    escape = true;
                } else if c == b'"' {
                    return i + 1;
                }
                i += 1;
            }
            i
        }
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut in_str = false;
            let mut escape = false;
            while i < bytes.len() {
                let c = bytes[i];
                if escape {
                    escape = false;
                } else if in_str && c == b'\\' {
                    escape = true;
                } else if c == b'"' {
                    in_str = !in_str;
                } else if !in_str && (c == b'{' || c == b'[') {
                    depth += 1;
                } else if !in_str && (c == b'}' || c == b']') {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                i += 1;
            }
            i
        }
        _ => {
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b'}' {
                i += 1;
            }
            i
        }
    }
}

/// `json.get(json_str, key)` — the value at `key`, or `null`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_get(json_str: i64, key: i64) -> i64 {
    let json = registry::get_string(json_str).unwrap_or_default();
    let key = registry::get_string(key).unwrap_or_default();
    match immediate_member(&json, &key) {
        Some(value) => registry::put_string(value),
        None => registry::put_string("null".to_string()),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_has(json_str: i64, key: i64) -> i8 {
    let json = registry::get_string(json_str).unwrap_or_default();
    let key = registry::get_string(key).unwrap_or_default();
    let search = format!("\"{key}\":");
    i8::from(json.contains(&search))
}

/// The elements of an immediate JSON array, in order.
pub(crate) fn array_elements(json: &str) -> Vec<String> {
    let bytes = json.as_bytes();
    if bytes.first() != Some(&b'[') {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b']' | b',' | b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            _ => {
                let mut parser = JsonParser::at(json, i);
                match parser.parse_value() {
                    Ok(value) => {
                        // The parser leaves `pos` just past the value.
                        let end = parser.pos;
                        out.push(value);
                        i = end.max(i + 1);
                    }
                    Err(_) => i += 1,
                }
            }
        }
    }
    out
}

/// `json.length(arr)` — the number of elements in a JSON array; `0` for
/// anything else.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_length(json_str: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let json = registry::get_string(json_str).unwrap_or_default();
    i64::try_from(array_elements(&json).len()).unwrap_or(i64::MAX)
}

/// `json.at(arr, i)` — element `i` of a JSON array as JSON text, or
/// `"null"` when the index is out of range.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_at(json_str: i64, index: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let json = registry::get_string(json_str).unwrap_or_default();
    let elements = array_elements(&json);
    let Ok(index) = usize::try_from(index) else {
        return registry::put_string(String::from("null"));
    };
    match elements.get(index) {
        Some(value) => registry::put_string(value.clone()),
        None => registry::put_string(String::from("null")),
    }
}

/// `json.keys(json_str)` — comma-separated list of keys.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_keys(json_str: i64) -> i64 {
    let json = registry::get_string(json_str).unwrap_or_default();
    let mut keys: Vec<String> = Vec::new();
    let mut pos = 0;
    while let Some(start) = json[pos..].find('"') {
        let abs_start = pos + start + 1;
        if let Some(end) = json[abs_start..].find('"') {
            let key = &json[abs_start..abs_start + end];

            let after = abs_start + end + 1;
            if json.as_bytes().get(after) == Some(&b':') {
                keys.push(key.to_string());
                pos = after + 1;
            } else {
                pos = abs_start + end + 1;
            }
        } else {
            break;
        }
    }
    registry::put_string(keys.join(","))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_stringify_pretty(json_str: i64) -> i64 {
    let json = registry::get_string(json_str).unwrap_or_default();
    let mut result = String::new();
    let mut indent = 0;
    let mut in_string = false;
    for ch in json.chars() {
        if ch == '"' {
            in_string = !in_string;
            result.push(ch);
        } else if in_string {
            result.push(ch);
        } else {
            match ch {
                '{' | '[' => {
                    result.push(ch);
                    result.push('\n');
                    indent += 1;
                    for _ in 0..indent {
                        result.push_str("  ");
                    }
                }
                '}' | ']' => {
                    result.push('\n');
                    indent -= 1;
                    for _ in 0..indent {
                        result.push_str("  ");
                    }
                    result.push(ch);
                }
                ',' => {
                    result.push(ch);
                    result.push('\n');
                    for _ in 0..indent {
                        result.push_str("  ");
                    }
                }
                ':' => {
                    result.push_str(": ");
                }
                ' ' => {}
                _ => result.push(ch),
            }
        }
    }
    registry::put_string(result)
}

/// Escape a raw string as a JSON string literal (surrounding quotes and all),
/// for building object literals at runtime.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_json_escape_string(s: i64) -> i64 {
    let input = registry::get_string(s).unwrap_or_default();
    let mut escaped = String::from("\"");
    for ch in input.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{0008}' => escaped.push_str("\\b"),
            '\u{000C}' => escaped.push_str("\\f"),
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    registry::put_string(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(s: &str) -> i64 {
        registry::put_string(s.to_string())
    }

    fn read(id: i64) -> String {
        let s = registry::get_string(id).unwrap_or_default();
        let _ = registry::take_string(id);
        s
    }

    #[test]
    fn test_parse_valid() {
        let r = ntsc_json_parse(put(r#"{"a":1,"b":"hello"}"#));
        let s = read(r);
        assert!(s.contains("\"a\":1"));
        assert!(s.contains("\"b\":\"hello\""));
    }

    #[test]
    fn test_parse_array() {
        let r = ntsc_json_parse(put("[1,2,3]"));
        assert_eq!(read(r), "[1,2,3]");
    }

    #[test]
    fn test_is_valid() {
        assert_eq!(ntsc_json_is_valid(put(r#"{"a":1}"#)), 1);
        assert_eq!(ntsc_json_is_valid(put("not json")), 0);
    }

    #[test]
    fn test_get() {
        let r = ntsc_json_get(put(r#"{"name":"Alice","age":30}"#), put("name"));
        assert_eq!(read(r), "\"Alice\"");
    }

    /// A nested object must not shadow a member of the same name in its
    /// parent: the track's own `mbid` wins over the artist's.
    #[test]
    fn test_get_ignores_nested_members() {
        let json = r##"{"artist":{"mbid":"artist-id","#text":"B"},"mbid":"track-id"}"##;
        assert_eq!(read(ntsc_json_get(put(json), put("mbid"))), "\"track-id\"");
        // `#text` lives inside the nested artist, so it is not a member here.
        assert_eq!(read(ntsc_json_get(put(json), put("#text"))), "null");
    }

    /// Lookup reaches members that follow a nested value, and arrays are
    /// skipped whole.
    #[test]
    fn test_get_walks_past_nested_values() {
        let json = r#"{"artists":[{"id":"a"}],"name":"Song","tracks":[{"x":1}],"id":"t"}"#;
        assert_eq!(read(ntsc_json_get(put(json), put("name"))), "\"Song\"");
        assert_eq!(read(ntsc_json_get(put(json), put("id"))), "\"t\"");
        assert_eq!(read(ntsc_json_get(put(json), put("missing"))), "null");
    }

    #[test]
    fn test_get_on_non_object_is_null() {
        assert_eq!(read(ntsc_json_get(put("[1,2,3]"), put("a"))), "null");
        assert_eq!(read(ntsc_json_get(put("42"), put("a"))), "null");
    }

    #[test]
    fn test_parse_unicode_strings() {
        let r = ntsc_json_parse(put(r#"{"greeting":"Привет","日本語":"값"}"#));
        let s = read(r);
        assert!(s.contains("\"greeting\":\"Привет\""), "value: {s}");
        assert!(s.contains("\"日本語\":\"값\""), "key: {s}");
    }

    #[test]
    fn test_keys() {
        let r = ntsc_json_keys(put(r#"{"a":1,"b":2}"#));
        let s = read(r);
        assert!(s.contains("a"));
        assert!(s.contains("b"));
    }

    #[test]
    fn pretty_printed_members_with_spaces_around_colons() {
        let doc = "{\n  \"name\" : \"Moonride\",\n  \"count\" : 42,\n  \"tags\" : [\"a\"]\n}";
        assert_eq!(
            super::immediate_member(doc, "name").as_deref(),
            Some("\"Moonride\"")
        );
        assert_eq!(super::immediate_member(doc, "count").as_deref(), Some("42"));
        assert_eq!(super::immediate_member(doc, "missing"), None);
    }

    #[test]
    fn test_escape_string() {
        assert_eq!(
            read(ntsc_json_escape_string(put("a\"b\n"))),
            "\"a\\\"b\\n\""
        );
    }

    #[test]
    fn test_parse_invalid_throws() {
        use crate::modules::test_util::catch_throw;
        let input = put("not json");
        let err = catch_throw(|| {
            let _ = ntsc_json_parse(input);
        });
        let _ = registry::take_string(input);
        let msg = err.unwrap();
        assert!(msg.contains("json.parse"), "unexpected message: {msg}");
    }
}
