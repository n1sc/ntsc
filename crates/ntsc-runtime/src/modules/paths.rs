//! NTSC standard library: `paths` module.
//! Comprehensive path manipulation beyond the basics in `os`.

use std::path::{Path, PathBuf};

use crate::registry;

fn fail(fn_name: &str, msg: impl std::fmt::Display) -> i64 {
    super::throw_str(format!("paths.{fn_name}: {msg}"))
}

/// `paths.join(a, b)` — joins two path components; empty components
/// contribute nothing, matching the documented join behavior.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_join(a: i64, b: i64) -> i64 {
    let left = registry::get_string(a).unwrap_or_default();
    let right = registry::get_string(b).unwrap_or_default();
    let joined = if left.is_empty() {
        right
    } else if right.is_empty() {
        left
    } else {
        Path::new(&left).join(&right).to_string_lossy().to_string()
    };
    registry::put_string(joined)
}

/// `paths.parent(path)` — the parent directory, or "" at root.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_parent(path: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    let parent = Path::new(&p)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(parent)
}

/// `paths.file_name(path)` — the final component of the path.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_file_name(path: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    let name = Path::new(&p)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(name)
}

/// `paths.extension(path)` — the file extension (without the dot).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_extension(path: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    let ext = Path::new(&p)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(ext)
}

/// `paths.with_extension(path, ext)` — replace the extension.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_with_extension(path: i64, ext: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    let e = registry::get_string(ext).unwrap_or_default();
    let result = Path::new(&p)
        .with_extension(&e)
        .to_string_lossy()
        .to_string();
    registry::put_string(result)
}

/// `paths.stem(path)` — the filename without its extension.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_stem(path: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    let stem = Path::new(&p)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(stem)
}

/// `paths.absolute(path)` — absolutize a relative path against cwd.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_absolute(path: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    match std::path::absolute(&p) {
        Ok(a) => registry::put_string(a.to_string_lossy().to_string()),
        Err(e) => fail("absolute", format!("cannot absolutize '{p}': {e}")),
    }
}

/// `paths.relative(path, base)` — compute `path` relative to `base`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_relative(path: i64, base: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    let b = registry::get_string(base).unwrap_or_default();
    let path = Path::new(&p);
    let base = Path::new(&b);
    match path.strip_prefix(base) {
        Ok(rel) => registry::put_string(rel.to_string_lossy().to_string()),
        Err(_) => {
            let abs_path = match std::path::absolute(path) {
                Ok(a) => a,
                Err(e) => return fail("relative", format!("cannot absolutize '{p}': {e}")),
            };
            let abs_base = match std::path::absolute(base) {
                Ok(a) => a,
                Err(e) => return fail("relative", format!("cannot absolutize '{b}': {e}")),
            };
            // Walk up from abs_path until we find a common ancestor with abs_base.
            let mut result = PathBuf::new();
            let mut current = abs_path.as_path();
            loop {
                if let Ok(rel) = current.strip_prefix(&abs_base) {
                    // Add the remaining relative part.
                    if !rel.as_os_str().is_empty() {
                        result = result.join(rel);
                    }
                    break;
                }
                // Go up one directory.
                result = result.join("..");
                match current.parent() {
                    Some(parent) if parent != current => current = parent,
                    _ => break,
                }
            }
            registry::put_string(result.to_string_lossy().to_string())
        }
    }
}

/// `paths.is_absolute(path)` — whether the path is absolute.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_is_absolute(path: i64) -> i8 {
    let p = registry::get_string(path).unwrap_or_default();
    if Path::new(&p).is_absolute() { 1 } else { 0 }
}

/// `paths.components(path)` — newline-separated components of the path.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_components(path: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    let components: Vec<String> = Path::new(&p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    registry::put_string(components.join("\n"))
}

/// `paths.normalize(path)` — resolve `.` and `..` without touching the
/// filesystem.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_paths_normalize(path: i64) -> i64 {
    let p = registry::get_string(path).unwrap_or_default();
    let mut result = PathBuf::new();
    for component in Path::new(&p).components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            other => result.push(other),
        }
    }
    registry::put_string(result.to_string_lossy().to_string())
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
    fn test_join() {
        assert_eq!(
            read(ntsc_paths_join(put("a"), put("b"))),
            format!("a{}b", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(read(ntsc_paths_join(put("a"), put(""))), "a");
    }

    #[test]
    fn test_parent() {
        assert_eq!(read(ntsc_paths_parent(put("/a/b/c.txt"))), "/a/b");
        assert_eq!(read(ntsc_paths_parent(put("/a"))), "/");
    }

    #[test]
    fn test_file_name() {
        assert_eq!(read(ntsc_paths_file_name(put("/a/b/c.txt"))), "c.txt");
    }

    #[test]
    fn test_extension() {
        assert_eq!(read(ntsc_paths_extension(put("/a/b/c.txt"))), "txt");
    }

    #[test]
    fn test_with_extension() {
        assert_eq!(
            read(ntsc_paths_with_extension(put("/a/b/c.txt"), put("rs"))),
            "/a/b/c.rs"
        );
    }

    #[test]
    fn test_stem() {
        assert_eq!(read(ntsc_paths_stem(put("/a/b/c.txt"))), "c");
    }

    #[test]
    fn test_normalize() {
        let result = read(ntsc_paths_normalize(put("/a/b/../c/./d.txt")));
        let expected = Path::new("/a/c/d.txt");
        let got = Path::new(&result);
        assert_eq!(
            got.components().collect::<Vec<_>>(),
            expected.components().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_components() {
        let comps = read(ntsc_paths_components(put("/a/b/c.txt")));
        let parts: Vec<&str> = comps.split('\n').collect();
        assert!(parts.len() >= 3);
    }
}
