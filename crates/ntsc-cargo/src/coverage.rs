//! `ntsc cargo coverage` — summarize export manifest entries by status.

use std::collections::BTreeMap;
use std::path::Path;

use crate::manifest::Manifest;
use crate::{CargoError, CargoResult, cache};

#[derive(Debug, Default, PartialEq, Eq)]
struct CoverageCounts {
    total: usize,
    statuses: BTreeMap<String, usize>,
}

pub fn coverage(project_dir: &Path, raw_name: &str) -> CargoResult<()> {
    let name = raw_name.trim_start_matches('@').trim();
    if name.is_empty() {
        return Err(CargoError("usage: ntsc cargo coverage [<crate>]".into()));
    }
    coverage_one(project_dir, name)
}

pub fn coverage_all(project_dir: &Path) -> CargoResult<()> {
    let manifest = Manifest::read(&project_dir.join("neutron.toml"))?;
    if manifest.rust_deps.is_empty() {
        println!("no [rust-deps] in neutron.toml");
        return Ok(());
    }
    for name in manifest.rust_deps.keys() {
        coverage_one(project_dir, name)?;
    }
    Ok(())
}

fn coverage_one(project_dir: &Path, name: &str) -> CargoResult<()> {
    let path = cache::project_deps_dir(project_dir)
        .join(name)
        .join(format!("{name}.export.json"));
    if !path.is_file() {
        println!("no export manifest for `{name}` at {}", path.display());
        return Ok(());
    }

    let counts = read_counts(&path)?;
    println!("crate {name}");
    println!("  total: {}", counts.total);
    for (status, count) in counts.statuses {
        println!("  {status}: {count}");
    }
    Ok(())
}

fn read_counts(path: &Path) -> CargoResult<CoverageCounts> {
    let content = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|error| CargoError(format!("cannot parse {}: {error}", path.display())))?;
    let mut counts = CoverageCounts::default();
    for kind in ["functions", "types", "enums"] {
        let Some(entries) = value.get(kind).and_then(serde_json::Value::as_array) else {
            continue;
        };
        for entry in entries {
            let status = entry
                .get("status")
                .and_then(|status| {
                    status
                        .as_str()
                        .or_else(|| status.get("status").and_then(serde_json::Value::as_str))
                })
                .unwrap_or("typed");
            *counts.statuses.entry(status.to_owned()).or_default() += 1;
            counts.total += 1;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn counts_statuses_and_defaults_missing_status() {
        let root = tempfile_dir("coverage-status");
        let deps = root.join(".ntsc/foo");
        fs::create_dir_all(&deps).unwrap();
        fs::write(
            deps.join("foo.export.json"),
            r#"{"functions":[{"status":"typed"},{"status":"opaque"},{"name":"old"},{"status":"typed"}],"types":[{"status":"conditional"}],"enums":[]}"#,
        )
        .unwrap();
        let counts = read_counts(&deps.join("foo.export.json")).unwrap();
        assert_eq!(counts.total, 5);
        assert_eq!(counts.statuses.get("typed"), Some(&3));
        assert_eq!(counts.statuses.get("opaque"), Some(&1));
        assert_eq!(counts.statuses.get("conditional"), Some(&1));
        assert_eq!(counts.statuses.get("exported"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn coverage_all_uses_every_configured_rust_dependency() {
        let root = tempfile_dir("coverage-all");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("neutron.toml"),
            "[rust-deps]\nalpha = \"1\"\nbeta = \"1\"\n",
        )
        .unwrap();
        for name in ["alpha", "beta", "stale"] {
            let directory = root.join(".ntsc").join(name);
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join(format!("{name}.export.json")),
                r#"{"functions":[{"status":"typed"}]}"#,
            )
            .unwrap();
        }

        coverage_all(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_manifest_is_not_an_error() {
        let root = tempfile_dir("coverage-missing");
        assert!(coverage(&root, "foo").is_ok());
        let _ = fs::remove_dir_all(root);
    }

    fn tempfile_dir(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ntsc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }
}
