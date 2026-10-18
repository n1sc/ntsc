//! Obtaining exact crate source into the global cache.
//!
//! Resolution runs `cargo fetch` in a scratch project, which leaves the
//! compressed `.crate` archives in cargo's own registry cache and the
//! extracted sources under `~/.cargo/registry/src/<host>/`. This module
//! mirrors the extracted source into the ntsc cache so the rest of
//! ntsc-cargo has stable paths to work against, independent of cargo's
//! internal layout.

use std::path::{Path, PathBuf};

use crate::CargoResult;
use crate::cache::CacheRoot;

/// Mirror the extracted source of one crate version into the ntsc cache.
///
/// Returns the local source path. When the ntsc cache already holds the
/// version, the copy is skipped (a version is immutable, so an existing
/// copy never needs refreshing).
pub fn fetch_source(
    cache: &CacheRoot,
    name: &str,
    version: &str,
    cargo_registry_dir: &Path,
) -> CargoResult<PathBuf> {
    let dest = cache.crate_src_path(name, version);
    if dest.exists() {
        crate::cache::touch_last_use(&dest)?;
        return Ok(dest);
    }

    let source = locate_crate_source(name, version, cargo_registry_dir)?;
    copy_dir_recursive(&source, &dest)?;
    crate::cache::touch_last_use(&dest)?;
    Ok(dest)
}

/// Find `<name>-<version>` under cargo's `registry/src/<host>/` layers.
fn locate_crate_source(
    name: &str,
    version: &str,
    cargo_registry_dir: &Path,
) -> CargoResult<PathBuf> {
    let wanted = format!("{name}-{version}");
    let mut stack = vec![cargo_registry_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if path.is_dir() {
                if file_name == wanted {
                    return Ok(path);
                }
                // Cargo nests one `<registry-host>` level under `src/`.
                if path
                    .parent()
                    .is_some_and(|parent| parent == cargo_registry_dir)
                {
                    stack.push(path);
                }
            }
        }
    }
    Err(crate::CargoError(format!(
        "cannot find source for `{name} {version}` under {} — run `ntsc cargo install {name}` first",
        cargo_registry_dir.display()
    )))
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(source: &Path, dest: &Path) -> CargoResult<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ntsc_fetch_test_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn locates_nested_crate_source() {
        let cargo_reg = TempDir::new("nested");
        let registry_src = cargo_reg.0.join("registry").join("src");
        let crate_dir = registry_src
            .join("index.crates.io-6f17d22bba15001f")
            .join("foo-1.2.3");
        std::fs::create_dir_all(&crate_dir).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "").unwrap();

        let found = locate_crate_source("foo", "1.2.3", &registry_src).unwrap();
        assert!(found.ends_with("foo-1.2.3"));
    }

    #[test]
    fn missing_source_is_an_error() {
        let cargo_reg = TempDir::new("missing");
        let registry_src = cargo_reg.0.join("registry").join("src");
        std::fs::create_dir_all(&registry_src).unwrap();
        let err = locate_crate_source("nope", "9.9.9", &registry_src).unwrap_err();
        assert!(err.0.contains("cannot find source"));
    }

    #[test]
    fn fetch_copies_tree_and_skips_existing() {
        let cargo_reg = TempDir::new("copy");
        let registry_src = cargo_reg.0.join("registry").join("src");
        let src_dir = registry_src.join("foo-1.0.0");
        std::fs::create_dir_all(src_dir.join("src")).unwrap();
        std::fs::write(src_dir.join("Cargo.toml"), "name = \"foo\"").unwrap();
        std::fs::write(src_dir.join("src").join("lib.rs"), "pub fn f() {}").unwrap();

        let cache_root = CacheRoot::at(TempDir::new("cache").0.clone());

        let dest = fetch_source(&cache_root, "foo", "1.0.0", &registry_src).unwrap();
        assert!(dest.starts_with(cache_root.path()));
        assert!(dest.join("src").join("lib.rs").is_file());

        // A second fetch is a no-op that still succeeds.
        let again = fetch_source(&cache_root, "foo", "1.0.0", &registry_src).unwrap();
        assert_eq!(again, dest);
    }
}
