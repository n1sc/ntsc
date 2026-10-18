//! NTSC standard library: module registry.
//! Stdlib modules are compiled into `libntsc_runtime.a`; `ntsc-codegen`
//! forward-declares their extern "C" functions and the linker resolves them.

#[cfg(feature = "archive")]
pub mod archive;
pub mod arrays;
pub mod collections;
#[cfg(feature = "crypto")]
pub mod crypto;
pub mod csv;
#[cfg(feature = "crypto")]
pub mod encoding;
pub mod fmt;
#[cfg(feature = "regex")]
pub mod glob;
#[cfg(feature = "crypto")]
pub mod hash;
#[cfg(feature = "http")]
pub mod http;
pub mod io;
pub mod json;
pub mod math;
pub mod memory;
pub mod net;
pub mod os;
pub mod paths;
pub mod process;
pub mod random;
#[cfg(feature = "regex")]
pub mod regex;
pub mod slices;
pub mod sort;
pub mod strings;
pub mod sys;
pub mod term;
pub mod testing;
pub mod time;
pub mod toml;
pub mod yaml;

use crate::registry;

/// Register `msg` as the pending exception and return 0, the failure
/// sentinel checked by generated callers.
pub(crate) fn throw_str(msg: String) -> i64 {
    crate::ntsc_throw(registry::put_string(msg))
}

/// Convert NTSC escape sequences stored as literal characters in a
/// runtime string into their real equivalents.  Processes `\n`, `\r`,
/// `\t`, `\"`, and `\\` in a single left-to-right pass.
pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod test_util {
    use crate::registry;

    pub fn catch_throw(f: impl FnOnce()) -> Option<String> {
        f();
        if crate::ntsc_exception_pending() != 0 {
            let message = crate::ntsc_exception_take_message();
            let text = registry::get_string(message).unwrap_or_default();
            let _ = registry::take_string(message);
            Some(text)
        } else {
            None
        }
    }
}
