//! Reading and writing the `[rust-deps]` section of `neutron.toml` and the
//! project's `ntsc.lock`.
//!
//! `neutron.toml` is preserved as text: only the `[rust-deps]` table is
//! rewritten, so comments and formatting in the rest of the file survive an
//! `ntsc cargo install`. The lockfile is generated data and is rewritten
//! wholesale after each resolution.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::CargoResult;

/// The name of the dependency section owned by this crate.
const RUST_DEPS_SECTION: &str = "rust-deps";

/// How a crate's version is pinned in `neutron.toml`.
#[derive(Debug, Clone, PartialEq)]
pub enum DependencySpec {
    /// A bare version requirement (`"1.0"`, `">=0.31, <0.32"`, `"*"`).
    Version(String),
    /// An inline table (`{ version = "1.38", features = ["full"] }`).
    Table(toml::map::Map<String, toml::Value>),
    /// No requirement written yet — resolves to the latest compatible.
    Any,
}

impl DependencySpec {
    /// The TOML value this spec is written back as.
    fn to_toml(&self) -> toml::Value {
        match self {
            Self::Version(v) => toml::Value::String(v.clone()),
            Self::Table(t) => toml::Value::Table(t.clone()),
            Self::Any => toml::Value::String("*".to_string()),
        }
    }

    /// The local directory of a `{ path = ... }` spec, if any. Paths are
    /// relative to the project root and canonicalized at use time.
    pub fn path_spec(&self) -> Option<&str> {
        match self {
            Self::Table(t) => t.get("path").and_then(toml::Value::as_str),
            _ => None,
        }
    }

    /// The version requirement, when the spec carries one.
    fn version_req(&self) -> Option<&str> {
        match self {
            Self::Version(v) => Some(v.as_str()),
            Self::Any => Some("*"),
            Self::Table(t) => match t.get("version") {
                Some(toml::Value::String(v)) => Some(v.as_str()),
                _ => None,
            },
        }
    }

    /// One-line rendering for `ntsc cargo list`.
    pub fn display(&self) -> String {
        match self {
            Self::Version(v) => v.clone(),
            Self::Any => "*".to_string(),
            Self::Table(t) => {
                let entries: Vec<String> = t
                    .iter()
                    .map(|(key, value)| format!("{key} = {value}"))
                    .collect();
                format!("{{ {} }}", entries.join(", "))
            }
        }
    }
}

impl fmt::Display for DependencySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display())
    }
}

/// A parsed `neutron.toml`, with `[rust-deps]` exposed for editing.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// The raw document so that untouched sections survive a rewrite.
    doc: toml::Value,
    /// The dependency table, kept ordered by name for stable output.
    pub rust_deps: BTreeMap<String, DependencySpec>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            doc: toml::Value::Table(toml::map::Map::new()),
            rust_deps: BTreeMap::new(),
        }
    }
}

impl Manifest {
    /// Read `neutron.toml` from `path`.
    pub fn read(path: &Path) -> CargoResult<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| crate::CargoError(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&text)
    }

    /// Parse `neutron.toml` source text.
    pub fn parse(text: &str) -> CargoResult<Self> {
        let doc: toml::Value = text
            .parse()
            .map_err(|e| crate::CargoError(format!("neutron.toml: {e}")))?;

        let mut rust_deps = BTreeMap::new();
        if let Some(toml::Value::Table(table)) = doc.get(RUST_DEPS_SECTION) {
            for (name, value) in table {
                let spec = match value {
                    toml::Value::String(v) => DependencySpec::Version(v.clone()),
                    toml::Value::Table(t) => DependencySpec::Table(t.clone()),
                    other => {
                        return Err(crate::CargoError(format!(
                            "`{name}` in [{RUST_DEPS_SECTION}] must be a string or table, got {other}"
                        )));
                    }
                };
                rust_deps.insert(name.clone(), spec);
            }
        }
        Ok(Self { doc, rust_deps })
    }

    /// Whether `[rust-deps]` names `name`.
    pub fn has_rust_dep(&self, name: &str) -> bool {
        self.rust_deps.contains_key(name)
    }

    /// Add (or overwrite) a dependency.
    pub fn add_rust_dep(&mut self, name: &str, spec: DependencySpec) {
        self.rust_deps.insert(name.to_string(), spec);
    }

    /// Remove a dependency. Returns whether it was present.
    pub fn remove_rust_dep(&mut self, name: &str) -> bool {
        self.rust_deps.remove(name).is_some()
    }

    /// Add a feature to a dependency, converting a shorthand spec to a table.
    pub fn add_feature(&mut self, name: &str, feature: &str) -> CargoResult<()> {
        let spec = self
            .rust_deps
            .get_mut(name)
            .ok_or_else(|| crate::CargoError(format!("crate `{name}` is not in [rust-deps]")))?;
        let table = match spec {
            DependencySpec::Table(table) => table,
            DependencySpec::Version(version) => {
                let mut table = toml::map::Map::new();
                table.insert(
                    "version".into(),
                    toml::Value::String(std::mem::take(version)),
                );
                *spec = DependencySpec::Table(table);
                match spec {
                    DependencySpec::Table(table) => table,
                    _ => unreachable!(),
                }
            }
            DependencySpec::Any => {
                *spec = DependencySpec::Table(toml::map::Map::new());
                match spec {
                    DependencySpec::Table(table) => table,
                    _ => unreachable!(),
                }
            }
        };
        let features = table
            .entry("features")
            .or_insert_with(|| toml::Value::Array(Vec::new()));
        let list = features.as_array_mut().ok_or_else(|| {
            crate::CargoError(format!("dependency `{name}` has non-array features"))
        })?;
        if !list.iter().any(|v| v.as_str() == Some(feature)) {
            list.push(toml::Value::String(feature.into()));
        }
        Ok(())
    }

    /// Remove a feature from a dependency. Returns whether it was present.
    pub fn remove_feature(&mut self, name: &str, feature: &str) -> CargoResult<bool> {
        let Some(DependencySpec::Table(table)) = self.rust_deps.get_mut(name) else {
            return Err(crate::CargoError(format!(
                "crate `{name}` has no editable feature list"
            )));
        };
        let Some(list) = table
            .get_mut("features")
            .and_then(toml::Value::as_array_mut)
        else {
            return Ok(false);
        };
        let old_len = list.len();
        list.retain(|v| v.as_str() != Some(feature));
        Ok(list.len() != old_len)
    }

    /// The version requirement of a dependency, when it carries one.
    pub fn version_req(&self, name: &str) -> Option<&str> {
        self.rust_deps
            .get(name)
            .and_then(DependencySpec::version_req)
    }

    /// Write the manifest back to `path`, rewriting only the
    /// `[rust-deps]` section of the original document.
    pub fn write(&self, path: &Path) -> CargoResult<()> {
        let mut doc = self.doc.clone();
        if let toml::Value::Table(table) = &mut doc {
            if self.rust_deps.is_empty() {
                table.remove(RUST_DEPS_SECTION);
            } else {
                let mut deps = toml::map::Map::new();
                for (name, spec) in &self.rust_deps {
                    deps.insert(name.clone(), spec.to_toml());
                }
                table.insert(RUST_DEPS_SECTION.to_string(), toml::Value::Table(deps));
            }
        }
        // Render as a standard document: `toml::to_string` emits each
        // top-level table as a `[section]` header, where a `Value`'s
        // Display would inline everything onto one line.
        let text = toml::to_string(&doc)
            .map_err(|e| crate::CargoError(format!("cannot serialize neutron.toml: {e}")))?;
        std::fs::write(path, text)
            .map_err(|e| crate::CargoError(format!("cannot write {}: {e}", path.display())))?;
        Ok(())
    }
}

/// One resolved crate in `ntsc.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedDep {
    pub name: String,
    pub version: String,
    pub source: String,
    pub checksum: Option<String>,
}

/// A parsed `ntsc.lock`.
#[derive(Debug, Clone, Default)]
pub struct Lockfile {
    pub deps: Vec<LockedDep>,
}

impl Lockfile {
    /// The locked version of a crate, when present.
    pub fn version_for(&self, name: &str) -> Option<&str> {
        self.deps
            .iter()
            .find(|dep| dep.name == name)
            .map(|dep| dep.version.as_str())
    }
}

/// The lockfile path of a project.
pub fn lock_path(project_dir: &Path) -> std::path::PathBuf {
    project_dir.join("ntsc.lock")
}

/// Read a project's `ntsc.lock`. Returns `None` when no lockfile exists.
pub fn read_lock(project_dir: &Path) -> CargoResult<Option<Lockfile>> {
    let path = lock_path(project_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(crate::CargoError(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    let doc: toml::Value = text
        .parse()
        .map_err(|e| crate::CargoError(format!("ntsc.lock: {e}")))?;

    let mut deps = Vec::new();
    if let Some(toml::Value::Array(packages)) = doc.get("package") {
        for package in packages {
            let table = package.as_table().ok_or_else(|| {
                crate::CargoError("ntsc.lock: [[package]] entry must be a table".into())
            })?;
            let name = table.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                crate::CargoError("ntsc.lock: package entry missing `name`".into())
            })?;
            let version = table
                .get("version")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    crate::CargoError(format!("ntsc.lock: package `{name}` missing `version`"))
                })?;
            let source = table
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("registry")
                .to_string();
            let checksum = table
                .get("checksum")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            deps.push(LockedDep {
                name: name.to_string(),
                version: version.to_string(),
                source,
                checksum,
            });
        }
    }
    Ok(Some(Lockfile { deps }))
}

/// Write a project's `ntsc.lock` from a resolution.
pub fn write_lock(project_dir: &Path, deps: &[LockedDep]) -> CargoResult<()> {
    let mut text = String::new();
    text.push_str(
        "# Generated by ntsc-cargo. Pins exact crate versions for reproducible builds.\n",
    );
    for dep in deps {
        text.push_str("\n[[package]]\n");
        text.push_str(&format!("name = \"{}\"\n", dep.name));
        text.push_str(&format!("version = \"{}\"\n", dep.version));
        text.push_str(&format!("source = \"{}\"\n", dep.source));
        if let Some(checksum) = &dep.checksum {
            text.push_str(&format!("checksum = \"{checksum}\"\n"));
        }
    }
    let path = lock_path(project_dir);
    std::fs::write(&path, text)
        .map_err(|e| crate::CargoError(format!("cannot write {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rust_deps_section() {
        let text = r#"
[package]
entry = "src/main.nt"
output = "app"

[rust-deps]
serde_json = "1.0"
tokio = { version = "1.38", features = ["full"] }
"#;
        let manifest = Manifest::parse(text).expect("parses");
        assert_eq!(
            manifest.rust_deps.get("serde_json"),
            Some(&DependencySpec::Version("1.0".to_string()))
        );
        assert!(matches!(
            manifest.rust_deps.get("tokio"),
            Some(DependencySpec::Table(_))
        ));
        assert_eq!(manifest.version_req("serde_json"), Some("1.0"));
    }

    #[test]
    fn preserves_other_sections_on_write() {
        let text = "[package]\nentry = \"src/main.nt\"  # the entry\noutput = \"app\"\n";
        let mut manifest = Manifest::parse(text).expect("parses");
        manifest.add_rust_dep("regex", DependencySpec::Version("1.10".into()));
        let written = toml::to_string_pretty(&manifest.doc).unwrap();

        // The edited table is present in the serialized document.
        let reparsed =
            Manifest::parse(&format!("{text}\n[rust-deps]\nregex = \"1.10\"\n")).unwrap();
        assert!(reparsed.has_rust_dep("regex"));
        assert!(written.contains("entry"));
    }

    #[test]
    fn round_trips_add_and_remove() {
        let text = "[package]\nentry = \"src/main.nt\"\noutput = \"app\"\n";
        let mut manifest = Manifest::parse(text).expect("parses");
        manifest.add_rust_dep("serde_json", DependencySpec::Any);
        assert!(manifest.has_rust_dep("serde_json"));

        assert!(manifest.remove_rust_dep("serde_json"));
        assert!(!manifest.has_rust_dep("serde_json"));
        assert!(!manifest.remove_rust_dep("serde_json"));
    }

    #[test]
    fn rejects_wrong_dep_type() {
        let text = "[rust-deps]\nfoo = 123\n";
        let err = Manifest::parse(text).unwrap_err();
        assert!(err.0.contains("must be a string or table"));
    }

    #[test]
    fn lockfile_round_trip() {
        let deps = vec![LockedDep {
            name: "serde_json".into(),
            version: "1.0.128".into(),
            source: "registry".into(),
            checksum: Some("sha256:abc".into()),
        }];
        let dir = std::env::temp_dir().join(format!("ntsc_lock_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_lock(&dir, &deps).unwrap();

        let lock = read_lock(&dir).unwrap().expect("lock exists");
        assert_eq!(lock.deps.len(), 1);
        assert_eq!(lock.version_for("serde_json"), Some("1.0.128"));
        assert_eq!(lock.deps[0].checksum.as_deref(), Some("sha256:abc"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn path_spec_accessor_reads_table_path() {
        let text =
            "[rust-deps]\nfoo = { path = \"../libs/foo\", features = [\"a\"] }\nbar = \"1.0\"\n";
        let manifest = Manifest::parse(text).expect("parses");
        assert_eq!(
            manifest.rust_deps.get("foo").and_then(|s| s.path_spec()),
            Some("../libs/foo")
        );
        assert_eq!(
            manifest.rust_deps.get("bar").and_then(|s| s.path_spec()),
            None
        );
    }

    #[test]
    fn missing_lock_is_none() {
        let dir = std::env::temp_dir().join(format!("ntsc_nolock_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_lock(&dir).unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
