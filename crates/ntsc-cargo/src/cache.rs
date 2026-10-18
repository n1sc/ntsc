//! The global Rust-crate source cache at `~/.ntsc/cargo/`.
//!
//! The layout mirrors cargo's own cache model so its behaviour is familiar:
//! a compressed `.crate` archive per version, the extracted source beside
//! it, and compiled adaptor artifacts keyed by target and features. The
//! project's `.ntsc/` holds only the tiny generated adaptor source;
//! everything heavy lives here and is shared by every project on the
//! machine.
//!
//! ```text
//! ~/.ntsc/cargo/
//!   registry/cache/
//!     foo-1.2.3.crate            ← compressed source archive
//!   registry/src/
//!     foo-1.2.3/                 ← extracted source when needed
//!   build/
//!     foo-1.2.3/<target>-<features-hash>/  ← compiled adaptor artifacts
//! ```
//!
//! [`CacheRoot`] carries the root explicitly so callers decide where it
//! comes from: `$NTSC_CARGO_HOME` when set, `~/.ntsc/cargo` otherwise.
//! Tests construct one around a temp directory instead of mutating the
//! process environment.

use std::path::{Path, PathBuf};

use crate::CargoResult;

/// The resolved root of the global cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheRoot(PathBuf);

impl CacheRoot {
    /// The cache root from the environment: `$NTSC_CARGO_HOME` when set,
    /// `~/.ntsc/cargo` otherwise.
    pub fn from_env() -> Self {
        if let Some(home) = std::env::var_os("NTSC_CARGO_HOME") {
            return Self(PathBuf::from(home));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self(home.join(".ntsc").join("cargo"))
    }

    /// A cache root at an explicit path (tests, overrides).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// The root directory itself.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Directory holding the compressed `.crate` archives.
    pub fn registry_cache_dir(&self) -> PathBuf {
        self.0.join("registry").join("cache")
    }

    /// Directory holding extracted crate sources.
    pub fn registry_src_dir(&self) -> PathBuf {
        self.0.join("registry").join("src")
    }

    /// Directory holding compiled adaptor artifacts.
    pub fn build_dir(&self) -> PathBuf {
        self.0.join("build")
    }

    /// The archive filename cargo uses for one crate version.
    pub fn crate_archive_name(name: &str, version: &str) -> String {
        format!("{name}-{version}.crate")
    }

    /// Path of the compressed archive for one crate version.
    pub fn crate_archive_path(&self, name: &str, version: &str) -> PathBuf {
        self.registry_cache_dir()
            .join(Self::crate_archive_name(name, version))
    }

    /// Path of the extracted source for one crate version.
    pub fn crate_src_path(&self, name: &str, version: &str) -> PathBuf {
        self.registry_src_dir().join(format!("{name}-{version}"))
    }

    /// The scratch-project directory used for cargo-based resolution.
    pub fn resolve_dir(&self) -> PathBuf {
        self.0.join("resolve")
    }

    /// Create the cache directory skeleton if it does not exist yet.
    pub fn ensure_layout(&self) -> CargoResult<()> {
        for dir in [
            self.registry_cache_dir(),
            self.registry_src_dir(),
            self.build_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// Per-project directory holding generated adaptor crates.
pub fn project_deps_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".ntsc")
}

/// Remove cache entries older than the selected age.
pub fn touch_last_use(path: &Path) -> CargoResult<()> {
    std::fs::create_dir_all(path)?;
    let now = filetime_now();
    std::fs::write(path.join(".last-use"), now.to_string())?;
    Ok(())
}

fn filetime_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Remove cache entries older than the selected age. `force_rebuild` always
/// removes compiled artifacts, while registry entries still obey max-age.
pub fn gc(
    root: &CacheRoot,
    dry_run: bool,
    max_age_days: Option<u64>,
    force_rebuild: bool,
) -> CargoResult<()> {
    root.ensure_layout()?;
    let age =
        std::time::Duration::from_secs(max_age_days.unwrap_or(90).saturating_mul(24 * 60 * 60));
    let cutoff = std::time::SystemTime::now()
        .checked_sub(age)
        .unwrap_or(std::time::UNIX_EPOCH);
    for dir in [
        root.registry_cache_dir(),
        root.registry_src_dir(),
        root.build_dir(),
    ] {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let last_use = path.join(".last-use");
            let modified = std::fs::metadata(&last_use)
                .and_then(|metadata| metadata.modified())
                .or_else(|_| entry.metadata().and_then(|metadata| metadata.modified()))
                .unwrap_or(std::time::SystemTime::now());
            if (force_rebuild && dir == root.build_dir()) || modified < cutoff {
                if dry_run {
                    println!("would remove {}", path.display());
                } else if path.is_dir() {
                    std::fs::remove_dir_all(path)?;
                } else {
                    std::fs::remove_file(path)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_nested_under_the_root() {
        let root = CacheRoot::at("/tmp/ntsc-cargo-test-home");
        assert_eq!(
            root.crate_archive_path("foo", "1.2.3"),
            PathBuf::from("/tmp/ntsc-cargo-test-home/registry/cache/foo-1.2.3.crate")
        );
        assert_eq!(
            root.crate_src_path("foo", "1.2.3"),
            PathBuf::from("/tmp/ntsc-cargo-test-home/registry/src/foo-1.2.3")
        );
        assert_eq!(
            root.build_dir(),
            PathBuf::from("/tmp/ntsc-cargo-test-home/build")
        );
        assert_eq!(
            root.resolve_dir(),
            PathBuf::from("/tmp/ntsc-cargo-test-home/resolve")
        );
    }

    #[test]
    fn project_deps_dir_sits_beside_neutron_toml() {
        let dir = project_deps_dir(Path::new("/tmp/my-proj"));
        assert_eq!(dir, PathBuf::from("/tmp/my-proj/.ntsc"));
    }

    #[test]
    fn last_use_marker_is_written() {
        let root = CacheRoot::at(
            std::env::temp_dir().join(format!("ntsc_cache_use_{}", std::process::id())),
        );
        let artifact = root.build_dir().join("foo");
        touch_last_use(&artifact).unwrap();
        assert!(artifact.join(".last-use").is_file());
        let _ = std::fs::remove_dir_all(root.path());
    }

    #[test]
    fn ensure_layout_creates_skeleton() {
        let root = CacheRoot::at(
            std::env::temp_dir().join(format!("ntsc_cache_layout_{}", std::process::id())),
        );
        root.ensure_layout().unwrap();
        assert!(root.registry_cache_dir().is_dir());
        assert!(root.registry_src_dir().is_dir());
        assert!(root.build_dir().is_dir());
        let _ = std::fs::remove_dir_all(root.path());
    }
}
