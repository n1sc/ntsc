//! Emit adaptors for compile audits without copying cached crate sources.

use std::io::{self, Write};
use std::path::Path;

use ntsc_cargo::adaptor::{AdaptorRequest, emit_adaptor_from_source, feature_set_for_spec};
use ntsc_cargo::cache::CacheRoot;
use ntsc_cargo::manifest::DependencySpec;
use ntsc_cargo::{CargoError, CargoResult};

fn main() -> CargoResult<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let [project, sources @ ..] = arguments.as_slice() else {
        return Err(CargoError(
            "usage: emit_cached_adaptors <audit-directory> <crate-source-dir> ...".into(),
        ));
    };
    if sources.is_empty() {
        return Err(CargoError("at least one cached source is required".into()));
    }
    let project = Path::new(project);
    std::fs::create_dir_all(project)?;
    let project = project.canonicalize()?;
    let cache = CacheRoot::at(project.join("cache"));
    let mut output = io::stdout().lock();
    for source in sources {
        let source = Path::new(source).canonicalize()?;
        let manifest: toml::Value =
            toml::from_str(&std::fs::read_to_string(source.join("Cargo.toml"))?)
                .map_err(|error| CargoError(format!("{}: {error}", source.display())))?;
        let package_field = |field: &str| {
            manifest
                .get("package")
                .and_then(|package| package.get(field))
                .and_then(toml::Value::as_str)
                .ok_or_else(|| CargoError(format!("{}: missing package.{field}", source.display())))
        };
        let registry = source
            .parent()
            .ok_or_else(|| CargoError(format!("{}: missing source parent", source.display())))?;
        let dependency_spec = match std::env::var("FEATURES") {
            Ok(features) => {
                let mut table = toml::Table::new();
                table.insert("version".into(), package_field("version")?.into());
                table.insert(
                    "features".into(),
                    toml::Value::Array(
                        features
                            .split(',')
                            .filter(|feature| !feature.is_empty())
                            .map(|feature| toml::Value::String(feature.into()))
                            .collect(),
                    ),
                );
                Some(DependencySpec::Table(table))
            }
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(CargoError(format!("FEATURES: {error}"))),
        };
        let features = dependency_spec
            .as_ref()
            .map(|spec| feature_set_for_spec(&source, spec));
        let directory = emit_adaptor_from_source(
            &cache,
            &project,
            registry,
            &AdaptorRequest {
                name: package_field("name")?,
                version: package_field("version")?,
                source: &source,
                requested_features: features.as_ref(),
                dependency_spec: dependency_spec.as_ref(),
            },
        )?;
        writeln!(output, "{}", directory.join("Cargo.toml").display())?;
    }
    Ok(())
}
