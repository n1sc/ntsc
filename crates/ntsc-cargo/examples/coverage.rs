//! Read-only API classification audit of already-cached Rust crate sources.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use ntsc_cargo::adaptor::discover_public_api;
use ntsc_cargo::{CargoError, CargoResult};
use serde::Serialize;

#[derive(Serialize)]
struct Audit {
    scope: &'static str,
    crates: Vec<CrateCoverage>,
}

#[derive(Serialize)]
struct CrateCoverage {
    name: String,
    version: String,
    feature_mode: &'static str,
    classified: usize,
    supported: usize,
    supported_percent: Option<f64>,
    statuses: BTreeMap<String, usize>,
    functions: Vec<String>,
    unsupported: Vec<UnsupportedItem>,
}

#[derive(Serialize)]
struct UnsupportedItem {
    path: String,
    status: &'static str,
    reason: String,
}

fn inspect(source: &Path) -> CargoResult<CrateCoverage> {
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(source.join("Cargo.toml"))?)
            .map_err(|error| CargoError(format!("{}: {error}", source.display())))?;
    let package_field = |field: &str| {
        manifest
            .get("package")
            .and_then(|package| package.get(field))
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CargoError(format!("{}: missing package.{field}", source.display())))
    };
    let discovered = discover_public_api(source);
    let supported = discovered.functions.len();
    let classified = supported + discovered.unsupported.len();
    let mut statuses = BTreeMap::from([("typed".to_owned(), supported)]);
    let mut functions: Vec<String> = discovered
        .functions
        .into_iter()
        .map(|function| function.call_path)
        .collect();
    functions.sort();
    let mut unsupported = Vec::new();
    for item in discovered.unsupported {
        let status = item.status.as_wire_str();
        *statuses.entry(status.to_owned()).or_default() += 1;
        unsupported.push(UnsupportedItem {
            path: item.path,
            status,
            reason: item.reason,
        });
    }
    unsupported.sort_by(|left, right| {
        (&left.path, left.status, &left.reason).cmp(&(&right.path, right.status, &right.reason))
    });
    Ok(CrateCoverage {
        name: package_field("name")?,
        version: package_field("version")?,
        feature_mode: "default features, host target",
        classified,
        supported,
        supported_percent: (classified != 0).then(|| 100.0 * supported as f64 / classified as f64),
        statuses,
        functions,
        unsupported,
    })
}

fn main() -> CargoResult<()> {
    let sources: Vec<_> = std::env::args_os().skip(1).collect();
    if sources.is_empty() {
        return Err(CargoError(
            "usage: cargo run -p ntsc-cargo --example coverage -- <crate-source-dir> ...".into(),
        ));
    }
    let audit = Audit {
        scope: "source-discovered APIs only; not compiler-expanded, compile-verified, or full upstream API coverage",
        crates: sources
            .iter()
            .map(|source| inspect(Path::new(source)))
            .collect::<CargoResult<_>>()?,
    };
    let mut output = io::stdout().lock();
    serde_json::to_writer_pretty(&mut output, &audit)
        .map_err(|error| CargoError(error.to_string()))?;
    writeln!(output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str, source: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "ntsc-coverage-example-{name}-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(
                root.join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\nversion = \"1.0.0\"\n"),
            )
            .unwrap();
            std::fs::write(root.join("src/lib.rs"), source).unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn audit_counts_rejections_without_rewriting_sources() {
        let source = "pub fn good() -> bool { true }\npub fn bad(value: *const u8) {}\n";
        let fixture = Fixture::new("classified", source);
        let first = inspect(&fixture.0).unwrap();
        let second = inspect(&fixture.0).unwrap();
        assert_eq!(first.classified, 2);
        assert_eq!(first.supported, 1);
        assert_eq!(first.supported_percent, Some(50.0));
        assert_eq!(first.statuses.get("unsupported"), Some(&1));
        assert_eq!(first.unsupported[0].path, "bad");
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("src/lib.rs")).unwrap(),
            source
        );
        assert!(!fixture.0.join(".ntsc").exists());
        assert!(!fixture.0.join("target").exists());
    }

    #[test]
    fn empty_discovery_is_not_reported_as_complete_coverage() {
        let fixture = Fixture::new("empty", "");
        let coverage = inspect(&fixture.0).unwrap();
        assert_eq!(coverage.classified, 0);
        assert_eq!(coverage.supported_percent, None);
        std::fs::write(
            fixture.0.join("Cargo.toml"),
            "[package]\nname = \"empty\"\n",
        )
        .unwrap();
        assert!(inspect(&fixture.0).is_err());
    }
}
