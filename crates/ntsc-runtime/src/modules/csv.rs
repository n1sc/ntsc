//! NTSC standard library: `csv` module.
//! CSV values are compact CSV strings; arguments are borrowed handles,
//! returned handles are owned by the caller.

use crate::registry;

fn csv_escape_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        let escaped = field.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        field.to_string()
    }
}

use crate::modules::unescape;

fn parse_csv_row(row: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(current);
            current = String::new();
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_csv_parse(s: i64) -> i64 {
    let input = registry::get_string(s).unwrap_or_default();
    let input = unescape(&input);
    let input = input.trim();
    if input.is_empty() {
        return registry::put_string("[]".to_string());
    }
    let mut lines: Vec<&str> = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
    }
    if lines.is_empty() {
        return registry::put_string("[]".to_string());
    }
    let headers = parse_csv_row(lines[0]);
    let mut rows = Vec::new();
    for line in &lines[1..] {
        let fields = parse_csv_row(line);
        let mut pairs = Vec::new();
        for (i, header) in headers.iter().enumerate() {
            let val = fields.get(i).map(|s| s.trim()).unwrap_or("");
            let val_json = if val.is_empty() {
                "\"\"".to_string()
            } else if val == "true" {
                "true".to_string()
            } else if val == "false" {
                "false".to_string()
            } else if val == "null" {
                "null".to_string()
            } else if let Ok(n) = val.parse::<i64>() {
                n.to_string()
            } else if let Ok(f) = val.parse::<f64>() {
                format!("{f}")
            } else {
                let escaped = val
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r");
                format!("\"{escaped}\"")
            };
            pairs.push(format!("\"{}\":{}", header, val_json));
        }
        rows.push(format!("{{{}}}", pairs.join(",")));
    }
    registry::put_string(format!("[{}]", rows.join(",")))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_csv_stringify(s: i64) -> i64 {
    let json = registry::get_string(s).unwrap_or_default();
    let json = json.trim();
    if json.is_empty() || json == "[]" {
        return registry::put_string(String::new());
    }
    if !json.starts_with('[') {
        return super::throw_str("csv.stringify: expected a JSON array of objects".to_string());
    }
    let inner = &json[1..json.len() - 1].trim();
    if inner.is_empty() {
        return registry::put_string(String::new());
    }
    let mut objects = Vec::new();
    let mut depth = 0;
    let mut in_str = false;
    let mut escape_next = false;
    let mut start = 0;
    for (i, c) in inner.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if c == '\\' && in_str {
            escape_next = true;
            continue;
        }
        if c == '"' {
            in_str = !in_str;
            continue;
        }
        if in_str {
            continue;
        }
        if c == '{' {
            if depth == 0 {
                start = i;
            }
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                objects.push(&inner[start..=i]);
            }
        }
    }
    if objects.is_empty() {
        return registry::put_string(String::new());
    }
    let first_obj = objects[0];
    let headers: Vec<String> = extract_object_keys(first_obj);
    let mut output = headers.join(",");
    for obj in &objects {
        let values = extract_object_values(obj, &headers);
        output.push('\n');
        output.push_str(
            &values
                .iter()
                .map(|v| csv_escape_field(v))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    output.push('\n');
    registry::put_string(output)
}

fn extract_object_keys(obj: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let inner = obj.trim();
    let inner = if inner.starts_with('{') && inner.ends_with('}') {
        &inner[1..inner.len() - 1]
    } else {
        inner
    };
    let bytes = inner.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'"' {
            i += 1;
            let mut key = String::new();
            while i < len && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 1;
                }
                key.push(bytes[i] as char);
                i += 1;
            }
            if i < len {
                i += 1;
            }
            while i < len && bytes[i] != b':' {
                i += 1;
            }
            if i < len {
                i += 1;
            }
            keys.push(key);
            skip_json_value(inner, &mut i);
        } else {
            i += 1;
        }
    }
    keys
}

fn skip_json_value(s: &str, i: &mut usize) {
    let bytes = s.as_bytes();
    let len = bytes.len();
    while *i < len && bytes[*i].is_ascii_whitespace() {
        *i += 1;
    }
    if *i >= len {
        return;
    }
    match bytes[*i] {
        b'"' => {
            *i += 1;
            let mut escaped = false;
            while *i < len {
                if escaped {
                    escaped = false;
                    *i += 1;
                    continue;
                }
                if bytes[*i] == b'\\' {
                    escaped = true;
                    *i += 1;
                    continue;
                }
                if bytes[*i] == b'"' {
                    *i += 1;
                    return;
                }
                *i += 1;
            }
        }
        b'{' | b'[' => {
            let open = bytes[*i];
            let close = if open == b'{' { b'}' } else { b']' };
            *i += 1;
            let mut depth = 1;
            let mut in_str = false;
            let mut esc = false;
            while *i < len && depth > 0 {
                if esc {
                    esc = false;
                    *i += 1;
                    continue;
                }
                if bytes[*i] == b'\\' && in_str {
                    esc = true;
                    *i += 1;
                    continue;
                }
                if bytes[*i] == b'"' {
                    in_str = !in_str;
                } else if !in_str {
                    if bytes[*i] == open {
                        depth += 1;
                    } else if bytes[*i] == close {
                        depth -= 1;
                    }
                }
                *i += 1;
            }
        }
        b't' => {
            let remaining = &s[*i..];
            if remaining.starts_with("true") {
                *i += 4;
            }
        }
        b'f' => {
            let remaining = &s[*i..];
            if remaining.starts_with("false") {
                *i += 5;
            }
        }
        b'n' => {
            let remaining = &s[*i..];
            if remaining.starts_with("null") {
                *i += 4;
            }
        }
        b'-' | b'0'..=b'9' => {
            while *i < len
                && (bytes[*i].is_ascii_digit()
                    || bytes[*i] == b'.'
                    || bytes[*i] == b'e'
                    || bytes[*i] == b'E'
                    || bytes[*i] == b'+'
                    || bytes[*i] == b'-')
            {
                *i += 1;
            }
        }
        _ => {}
    }
}

fn extract_object_values(obj: &str, keys: &[String]) -> Vec<String> {
    let inner = obj.trim();
    let inner = if inner.starts_with('{') && inner.ends_with('}') {
        &inner[1..inner.len() - 1]
    } else {
        inner
    };
    let mut values = Vec::new();
    for key in keys {
        let search = format!("\"{key}\":");
        if let Some(pos) = inner.find(&search) {
            let val_start = pos + search.len();
            let remaining = &inner[val_start..].trim_start();
            let val = extract_json_value(remaining);
            values.push(json_value_to_string(&val));
        } else {
            values.push(String::new());
        }
    }
    values
}

fn extract_json_value(s: &str) -> String {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('"') {
        let mut result = String::new();
        let mut chars = inner.chars();
        let mut escape_next = false;
        for c in chars.by_ref() {
            if escape_next {
                match c {
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    '\\' => result.push('\\'),
                    '"' => result.push('"'),
                    _ => {
                        result.push('\\');
                        result.push(c);
                    }
                }
                escape_next = false;
            } else if c == '\\' {
                escape_next = true;
            } else if c == '"' {
                return result;
            } else {
                result.push(c);
            }
        }
        result
    } else if s.starts_with('{') || s.starts_with('[') {
        let open = s.as_bytes()[0];
        let close = if open == b'{' { b'}' } else { b']' };
        let mut depth = 0;
        let mut in_str = false;
        let mut escape_next = false;
        for (i, c) in s.char_indices() {
            if escape_next {
                escape_next = false;
                continue;
            }
            if c == '\\' && in_str {
                escape_next = true;
                continue;
            }
            if c == '"' {
                in_str = !in_str;
                continue;
            }
            if in_str {
                continue;
            }
            if c as u8 == open {
                depth += 1;
            } else if c as u8 == close {
                depth -= 1;
                if depth == 0 {
                    return s[..=i].to_string();
                }
            }
        }
        s.to_string()
    } else {
        let end = s.find(|c| [',', '}', ']'].contains(&c)).unwrap_or(s.len());
        s[..end].trim().to_string()
    }
}

fn json_value_to_string(val: &str) -> String {
    if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
        let inner = &val[1..val.len() - 1];
        let mut result = String::new();
        let mut chars = inner.chars();
        let mut escape_next = false;
        for c in chars.by_ref() {
            if escape_next {
                match c {
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    '\\' => result.push('\\'),
                    '"' => result.push('"'),
                    _ => {
                        result.push('\\');
                        result.push(c);
                    }
                }
                escape_next = false;
            } else if c == '\\' {
                escape_next = true;
            } else {
                result.push(c);
            }
        }
        result
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
        let input = "name,age\nAlice,30\nBob,25";
        let r = ntsc_csv_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"name\""));
        assert!(s.contains("\"Alice\""));
        assert!(s.contains("\"age\":30"));
        assert!(s.contains("\"Bob\""));
        assert!(s.contains("\"age\":25"));
    }

    #[test]
    fn test_parse_empty() {
        let r = ntsc_csv_parse(put(""));
        assert_eq!(read(r), "[]");
    }

    #[test]
    fn test_parse_quoted_fields() {
        let input = "name,desc\n\"Alice\",\"has a, comma\"";
        let r = ntsc_csv_parse(put(input));
        let s = read(r);
        assert!(s.contains("\"has a, comma\""));
    }

    #[test]
    fn test_parse_unicode_fields() {
        let input = "name,city\nAlice,Москва\n\"Böb\",\"Zürich, Stadt\"";
        let r = ntsc_csv_parse(put(input));
        let s = read(r);
        assert!(s.contains("Москва"), "plain: {s}");
        assert!(s.contains("Böb"), "quoted: {s}");
        assert!(s.contains("Zürich, Stadt"), "quoted comma: {s}");
    }

    #[test]
    fn test_stringify() {
        let json = r#"[{"name":"Alice","age":30},{"name":"Bob","age":25}]"#;
        let r = ntsc_csv_stringify(put(json));
        let s = read(r);
        assert!(s.contains("name,age"));
        assert!(s.contains("Alice,30"));
        assert!(s.contains("Bob,25"));
    }

    #[test]
    fn test_stringify_empty() {
        let r = ntsc_csv_stringify(put("[]"));
        assert_eq!(read(r), "");
    }

    #[test]
    fn test_stringify_not_array() {
        let err = catch_throw(|| {
            let _ = ntsc_csv_stringify(put(r#"{"a":1}"#));
        });
        assert!(err.is_some());
        assert!(err.unwrap().contains("csv.stringify"));
    }

    #[test]
    fn test_parse_type_inference() {
        let input = "val\n42\n3.14\ntrue\nhello";
        let r = ntsc_csv_parse(put(input));
        let s = read(r);
        assert!(s.contains(":42"));
        assert!(s.contains(":3.14"));
        assert!(s.contains(":true"));
        assert!(s.contains("\"hello\""));
    }
}
