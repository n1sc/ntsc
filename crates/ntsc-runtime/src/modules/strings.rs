//! NTSC standard library: `strings` module.
//! String arguments are borrowed handles; returned handles are owned by the
//! caller. `split` returns items joined by newlines.

use crate::registry;

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_split(s: i64, delim: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let delim = registry::get_string(delim).unwrap_or_default();
    let parts: Vec<&str> = if delim.is_empty() {
        s.split("").filter(|c| !c.is_empty()).collect()
    } else {
        s.split(&delim).collect()
    };
    registry::put_string(parts.join("\n"))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_join(a: i64, b: i64, delim: i64) -> i64 {
    let a = registry::get_string(a).unwrap_or_default();
    let b = registry::get_string(b).unwrap_or_default();
    let delim = registry::get_string(delim).unwrap_or_default();
    registry::put_string(format!("{a}{delim}{b}"))
}

/// `strings.split_array(s, delim)` — like `split`, but returns a real
/// `array[string]` handle instead of a newline-joined string, so callers can
/// iterate with `arrays.length` / `arrays.at` without re-tokenising.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_split_array(s: i64, delim: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let delim = registry::get_string(delim).unwrap_or_default();
    let parts: Vec<&str> = if delim.is_empty() {
        s.split("").filter(|c| !c.is_empty()).collect()
    } else {
        s.split(&delim).collect()
    };
    let items = registry::array_new(registry::PTR_SIZE as i64, 0, 1);
    for part in parts {
        let temp = registry::put_string(part.to_string());
        if !registry::array_push(items, temp) {
            crate::ntsc_string_drop(temp);
            break;
        }
        // `array_push` deep-copies string elements into handles the array
        // owns, so the original temp must be reclaimed here or every part
        // leaks.
        crate::ntsc_string_drop(temp);
    }
    items
}

/// `strings.join_all(arr, sep)` — the elements of a string array joined by
/// `sep`, the inverse of `strings.split`. Non-string elements read as their
/// decimal form, so a mixed array still joins instead of failing.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_join_all(arr: i64, sep: i64) -> i64 {
    let sep = registry::get_string(sep).unwrap_or_default();
    let len = registry::array_len(arr);
    let mut out = String::new();
    for index in 0..len {
        if index > 0 {
            out.push_str(&sep);
        }
        if let Some(handle) = registry::array_get(arr, index) {
            out.push_str(&registry::get_string(handle).unwrap_or_default());
        }
    }
    registry::put_string(out)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_trim(s: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    registry::put_string(s.trim().to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_trim_left(s: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    registry::put_string(s.trim_start().to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_trim_right(s: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    registry::put_string(s.trim_end().to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_upper(s: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    registry::put_string(s.to_uppercase())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_lower(s: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    registry::put_string(s.to_lowercase())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_replace(s: i64, from: i64, to: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let from = registry::get_string(from).unwrap_or_default();
    let to = registry::get_string(to).unwrap_or_default();
    registry::put_string(s.replace(&from, &to))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_replace_first(s: i64, from: i64, to: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let from = registry::get_string(from).unwrap_or_default();
    let to = registry::get_string(to).unwrap_or_default();
    let result = match s.find(&from) {
        Some(pos) => format!("{}{}{}", &s[..pos], to, &s[pos + from.len()..]),
        None => s,
    };
    registry::put_string(result)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_contains(s: i64, sub: i64) -> i8 {
    let s = registry::get_string(s).unwrap_or_default();
    let sub = registry::get_string(sub).unwrap_or_default();
    i8::from(s.contains(&sub))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_starts_with(s: i64, prefix: i64) -> i8 {
    let s = registry::get_string(s).unwrap_or_default();
    let prefix = registry::get_string(prefix).unwrap_or_default();
    i8::from(s.starts_with(&prefix))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_ends_with(s: i64, suffix: i64) -> i8 {
    let s = registry::get_string(s).unwrap_or_default();
    let suffix = registry::get_string(suffix).unwrap_or_default();
    i8::from(s.ends_with(&suffix))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_index_of(s: i64, sub: i64, start: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let sub = registry::get_string(sub).unwrap_or_default();
    if sub.is_empty() {
        return -1;
    }
    let start = if start < 0 {
        0
    } else {
        (start as usize).min(s.len())
    };

    // Snap to a UTF-8 boundary so a byte index mid-codepoint cannot panic.
    let start = s.floor_char_boundary(start);
    s[start..]
        .find(&sub)
        .map(|pos| (pos + start) as i64)
        .unwrap_or(-1)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_last_index_of(s: i64, sub: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let sub = registry::get_string(sub).unwrap_or_default();
    s.rfind(&sub).map(|pos| pos as i64).unwrap_or(-1)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_substring(s: i64, start: i64, end: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let len = s.len() as i64;
    let s_start = s.floor_char_boundary(start.max(0).min(len) as usize);
    let s_end = if end < 0 {
        len as usize
    } else {
        s.floor_char_boundary(end.max(0).min(len) as usize)
    };
    let text = if s_start >= s_end {
        String::new()
    } else {
        s[s_start..s_end].to_string()
    };
    registry::put_string(text)
}

/// `strings.repeat(str, n)` — output is clamped so a hostile repeat count
/// cannot overflow `String::repeat`'s capacity and panic.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_repeat(s: i64, n: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let text = if n <= 0 {
        String::new()
    } else {
        let n = (n as usize).min(MAX_REPEAT_OUTPUT / s.len().max(1));
        s.repeat(n)
    };
    registry::put_string(text)
}

/// Upper bound for `strings.repeat` and `fmt.pad_*` output, in bytes.
const MAX_REPEAT_OUTPUT: usize = 1 << 24;

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_reverse(s: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    registry::put_string(s.chars().rev().collect())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_length(s: i64) -> i64 {
    registry::with_string(s, str::len).unwrap_or(0) as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_count(s: i64, sub: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let sub = registry::get_string(sub).unwrap_or_default();
    if sub.is_empty() {
        return 0;
    }
    s.matches(&sub).count() as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_char_at(s: i64, index: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();

    // Snap to a UTF-8 boundary so a byte index mid-codepoint cannot panic.
    let idx = s.floor_char_boundary((index as usize).min(s.len()));
    let text = if idx >= s.len() {
        String::new()
    } else {
        s[idx..]
            .chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_default()
    };
    registry::put_string(text)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_char_code(s: i64, index: i64) -> i64 {
    let s = registry::get_string(s).unwrap_or_default();
    let idx = index as usize;
    if idx >= s.len() {
        return -1;
    }
    s.as_bytes().get(idx).copied().unwrap_or(0) as i64
}

// Only ASCII (0-127) is representable; other codes yield "".
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_from_char_code(code: i64) -> i64 {
    let text = if (0..=127).contains(&code) {
        String::from(code as u8 as char)
    } else {
        String::new()
    };
    registry::put_string(text)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_is_empty(s: i64) -> i8 {
    i8::from(registry::with_string(s, str::is_empty).unwrap_or(true))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_is_alpha(s: i64) -> i8 {
    let s = registry::get_string(s).unwrap_or_default();
    i8::from(!s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic()))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_is_digit(s: i64) -> i8 {
    let s = registry::get_string(s).unwrap_or_default();
    i8::from(!s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_strings_is_alnum(s: i64) -> i8 {
    let s = registry::get_string(s).unwrap_or_default();
    i8::from(!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()))
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
    fn split_array_returns_an_iterable_string_array() {
        let arr = ntsc_strings_split_array(put("a,b,,c"), put(","));
        assert_eq!(registry::array_len(arr), 4);
        let first = registry::array_get(arr, 0).expect("first element");
        assert_eq!(read(first), "a");
        let empty = registry::array_get(arr, 2).expect("empty element");
        assert_eq!(read(empty), "");
    }

    #[test]
    fn split_array_on_empty_delimiter_splits_characters() {
        let arr = ntsc_strings_split_array(put("abc"), put(""));
        assert_eq!(registry::array_len(arr), 3);
    }

    #[test]
    fn test_trim() {
        assert_eq!(read(ntsc_strings_trim(put("  hello  "))), "hello");
    }

    #[test]
    fn test_upper_lower() {
        assert_eq!(read(ntsc_strings_upper(put("hello"))), "HELLO");
        assert_eq!(read(ntsc_strings_lower(put("HELLO"))), "hello");
    }

    #[test]
    fn test_contains() {
        assert_eq!(ntsc_strings_contains(put("hello world"), put("world")), 1);
        assert_eq!(ntsc_strings_contains(put("hello world"), put("xyz")), 0);
    }

    #[test]
    fn test_length() {
        assert_eq!(ntsc_strings_length(put("hello")), 5);
    }

    #[test]
    fn substring_straddling_utf8_boundary_does_not_panic() {
        assert_eq!(read(ntsc_strings_substring(put("abçd"), 3, 4)), "ç");

        assert_eq!(read(ntsc_strings_substring(put("abçd"), 4, 3)), "");
    }

    #[test]
    fn char_at_straddling_utf8_boundary_does_not_panic() {
        assert_eq!(read(ntsc_strings_char_at(put("abçd"), 3)), "ç");
    }
}
