//! Dependency resolution, delegated to the real `cargo` binary.
//!
//! We do not reimplement a semver solver. Instead a throwaway cargo
//! project is materialized inside the cache whose `[dependencies]` mirror
//! the manifest's `[rust-deps]` (specs pass through verbatim), then
//! `cargo update` resolves it and `cargo fetch` downloads the exact
//! sources. Cargo's own `Cargo.lock` in that scratch project is then read
//! for the pinned versions and checksums, which flow into the project's
//! `ntsc.lock`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::CargoResult;
use crate::cache::CacheRoot;
use crate::manifest::{DependencySpec, LockedDep, Manifest};

/// A resolved dependency set: the direct deps the manifest asked for, with
/// exact versions, plus the directory cargo extracted sources into.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub deps: Vec<LockedDep>,
    /// Cargo's `registry/src` directory the sources were fetched into.
    pub registry_dir: PathBuf,
}

/// Resolve the manifest's `[rust-deps]` through cargo: write a scratch
/// project, run `cargo fetch`, and read back the resolution.
///
/// `{ path = ... }` specs point at local directories (relative to
/// `project_dir`): their identity and version come from the local
/// `Cargo.toml`, and the scratch manifest pins them by absolute path so
/// cargo still resolves their transitive registry dependencies.
pub fn resolve(
    cache: &CacheRoot,
    project_dir: &Path,
    manifest: &Manifest,
) -> CargoResult<Resolution> {
    cache.ensure_layout()?;

    // The scratch project is keyed by the project path so different
    // projects keep separate resolutions; a change in the dep set rewrites
    // it wholesale.
    let scratch = scratch_project_dir(cache, manifest)?;
    std::fs::create_dir_all(scratch.join("src"))?;
    std::fs::write(scratch.join("src").join("lib.rs"), "")?;

    let mut cargo_toml = String::from(
        "[package]\nname = \"ntsc-resolve\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    for (name, spec) in &manifest.rust_deps {
        if spec.path_spec().is_some() {
            let dir = path_dep_dir(project_dir, name, spec)?;
            let mut table = match spec {
                DependencySpec::Table(t) => t.clone(),
                _ => toml::map::Map::new(),
            };
            table.insert(
                "path".into(),
                toml::Value::String(dir.display().to_string()),
            );
            // Rendered manually like `spec_dep_toml`: `Value`'s Display
            // would emit `[table]` sections, which are illegal inline here.
            let entries: Vec<String> = table
                .iter()
                .map(|(key, value)| format!("{key} = {value}"))
                .collect();
            cargo_toml.push_str(&format!("{name} = {{ {} }}\n", entries.join(", ")));
        } else {
            cargo_toml.push_str(&format!("{name} = {}\n", spec_dep_toml(spec)));
        }
    }
    std::fs::write(scratch.join("Cargo.toml"), cargo_toml)?;

    // Validate path deps up front so a missing directory or a
    // package-name mismatch fails with a ntsc error, not cargo's.
    for (name, spec) in &manifest.rust_deps {
        if spec.path_spec().is_some() {
            resolve_path_dep(project_dir, name, spec)?;
        }
    }

    let manifest_path = scratch.join("Cargo.toml").display().to_string();
    run_cargo(&scratch, &["fetch", "--manifest-path", &manifest_path])
        // An offline environment (no registry access) still works when
        // every crate is already in cargo's local cache.
        .or_else(|_| {
            run_cargo(
                &scratch,
                &["fetch", "--offline", "--manifest-path", &manifest_path],
            )
        })?;

    let deps = read_cargo_lock(&scratch.join("Cargo.lock"), manifest)?;
    Ok(Resolution {
        deps,
        registry_dir: cargo_registry_src(),
    })
}

/// Resolve and fetch in one step, then persist `ntsc.lock` and emit
/// adaptors. This is the shared body of `ntsc cargo install` and the
/// `ntsc build` dependency step.
pub fn resolve_and_fetch(
    cache: &CacheRoot,
    project_dir: &Path,
    manifest: &mut Manifest,
    manifest_path: &Path,
) -> CargoResult<Vec<LockedDep>> {
    let resolution = resolve(cache, project_dir, manifest)?;
    manifest.write(manifest_path)?;

    for dep in &resolution.deps {
        materialize_dep(cache, project_dir, manifest, dep, &resolution.registry_dir)?;
    }

    crate::manifest::write_lock(project_dir, &resolution.deps)?;
    for dep in &resolution.deps {
        println!("Installed {} {}", dep.name, dep.version);
    }
    Ok(resolution.deps)
}

/// Locate one resolved dep's source and (re-)emit its adaptor: registry
/// deps go through the cargo cache, `{ path = ... }` deps use their
/// canonicalized local directory directly. Shared by `resolve_and_fetch`
/// and `cmd_build` so both materialization paths stay in sync.
pub fn materialize_dep(
    cache: &CacheRoot,
    project_dir: &Path,
    manifest: &Manifest,
    dep: &LockedDep,
    registry_dir: &Path,
) -> CargoResult<()> {
    let spec = manifest.rust_deps.get(&dep.name);
    let dep_src = match spec.and_then(|spec| spec.path_spec()) {
        Some(_) => path_dep_dir(project_dir, &dep.name, spec.expect("path spec present"))?,
        None => crate::fetch::fetch_source(cache, &dep.name, &dep.version, registry_dir)?,
    };
    let features = spec.map(|spec| crate::adaptor::feature_set_for_spec(&dep_src, spec));
    crate::adaptor::emit_adaptor_from_source(
        cache,
        project_dir,
        registry_dir,
        &crate::adaptor::AdaptorRequest {
            name: &dep.name,
            version: &dep.version,
            source: &dep_src,
            requested_features: features.as_ref(),
            dependency_spec: spec,
        },
    )?;
    Ok(())
}

/// Resolve from the lockfile when it is up to date with the manifest,
/// falling back to a full resolve when it is stale or absent.
///
/// Up to date means: every `[rust-deps]` entry appears in the lock with a
/// version satisfying its requirement, and no locked crate has disappeared
/// from the manifest.
pub fn resolve_cached(
    cache: &CacheRoot,
    project_dir: &Path,
    manifest: &Manifest,
) -> CargoResult<Resolution> {
    if let Some(lock) = crate::manifest::read_lock(project_dir)?
        && lock_is_fresh(&lock, manifest, project_dir)
    {
        return Ok(Resolution {
            deps: lock.deps,
            registry_dir: cargo_registry_src(),
        });
    }
    let mut owned = manifest.clone();
    resolve_and_fetch(
        cache,
        project_dir,
        &mut owned,
        &project_dir.join("neutron.toml"),
    )?;
    resolve(cache, project_dir, &owned)
}

/// The canonicalized local directory of a `{ path = ... }` dependency,
/// resolved against the project root. Errors name the dep and the path
/// that failed so a typo in `neutron.toml` is actionable.
fn path_dep_dir(project_dir: &Path, name: &str, spec: &DependencySpec) -> CargoResult<PathBuf> {
    let rel = spec
        .path_spec()
        .ok_or_else(|| crate::CargoError(format!("crate `{name}` is not a path dependency")))?;
    let dir = project_dir.join(rel);
    std::fs::canonicalize(&dir).map_err(|e| {
        crate::CargoError(format!(
            "cannot resolve path dependency `{name}` at {}: {e}",
            dir.display()
        ))
    })
}

/// Resolve a `{ path = ... }` dependency without cargo: identity and
/// version come straight from the local `Cargo.toml`. The dep key must
/// match the package name so `use @<name>` and the adaptor directory
/// agree.
fn resolve_path_dep(
    project_dir: &Path,
    name: &str,
    spec: &DependencySpec,
) -> CargoResult<crate::manifest::LockedDep> {
    let dir = path_dep_dir(project_dir, name, spec)?;
    let (package, version) = local_package_id(&dir)?;
    if package != name {
        return Err(crate::CargoError(format!(
            "path dependency `{name}` at {} names its package `{package}`; the [rust-deps] key must match the package name",
            dir.display()
        )));
    }
    Ok(crate::manifest::LockedDep {
        name: name.to_string(),
        version,
        source: "path".to_string(),
        checksum: None,
    })
}

/// The `[package] name` and `version` of the crate at `dir`.
fn local_package_id(dir: &Path) -> CargoResult<(String, String)> {
    let path = dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| crate::CargoError(format!("cannot read {}: {e}", path.display())))?;
    let doc: toml::Value = text
        .parse()
        .map_err(|e| crate::CargoError(format!("{}: {e}", path.display())))?;
    let package = doc.get("package").and_then(toml::Value::as_table);
    let name = package
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| crate::CargoError(format!("{} has no [package] name", path.display())))?;
    let version = package
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| crate::CargoError(format!("{} has no [package] version", path.display())))?;
    Ok((name.to_string(), version.to_string()))
}

fn lock_is_fresh(
    lock: &crate::manifest::Lockfile,
    manifest: &Manifest,
    project_dir: &Path,
) -> bool {
    for (name, spec) in &manifest.rust_deps {
        let Some(locked) = lock.deps.iter().find(|dep| &dep.name == name) else {
            return false;
        };
        // A path dependency is fresh only when locked as one and its
        // local `Cargo.toml` still reports the locked version, so edits
        // to the local crate re-resolve instead of tagging stale builds.
        if spec.path_spec().is_some() {
            if locked.source != "path" {
                return false;
            }
            let Ok(dir) = path_dep_dir(project_dir, name, spec) else {
                return false;
            };
            let Ok((_, version)) = local_package_id(&dir) else {
                return false;
            };
            if version != locked.version {
                return false;
            }
            continue;
        }
        match spec {
            DependencySpec::Version(req)
                if req != "*" && !version_satisfies(&locked.version, req) =>
            {
                return false;
            }
            _ => {}
        }
    }
    // A locked crate that is no longer a direct dep makes the lock stale.
    for dep in &lock.deps {
        if !manifest.rust_deps.contains_key(&dep.name) {
            return false;
        }
    }
    true
}

/// Minimal semver `satisfies` check for the common `MAJOR.MINOR` /
/// `MAJOR.MINOR.PATCH` requirement forms. Anything more exotic
/// (`>=`, `~`, git specs) falls back to "unknown, treat as satisfied" so
/// the lock is trusted rather than re-resolved on every build.
fn version_satisfies(version: &str, req: &str) -> bool {
    let req = req.trim();
    if req.is_empty() || req == "*" {
        return true;
    }
    // Anything beyond a plain `x.y[.z]` / `^x.y[.z]` requirement is exotic;
    // trust the lock rather than half-parse it.
    let plain = req.trim_start_matches('^');
    if !plain
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '*')
    {
        return true;
    }
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| match p {
                "*" => 0,
                digits => digits.parse::<u64>().unwrap_or(0),
            })
            .collect()
    };
    let v = parse(version);
    let r = parse(plain);
    if r.is_empty() || v.is_empty() {
        return true;
    }
    // A requirement pins the fields it names; unnamed fields are free.
    for (i, want) in r.iter().enumerate() {
        match v.get(i) {
            Some(got) if got == want => {}
            _ => return false,
        }
    }
    true
}

/// The scratch project directory for one manifest's dep set, inside the
/// global cache.
fn scratch_project_dir(cache: &CacheRoot, manifest: &Manifest) -> CargoResult<PathBuf> {
    // The dep set is the identity: two projects with identical deps share
    // one scratch resolution, and any dep change rewrites it.
    let mut key_input = String::new();
    for (name, spec) in &manifest.rust_deps {
        key_input.push_str(name);
        key_input.push('=');
        key_input.push_str(&spec.display());
        key_input.push('\n');
    }
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key_input.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    };
    Ok(cache.resolve_dir().join(key))
}

/// Serialize one dependency spec as the right-hand side of a cargo
/// `[dependencies]` line.
fn spec_dep_toml(spec: &DependencySpec) -> String {
    match spec {
        DependencySpec::Version(v) => format!("\"{v}\""),
        DependencySpec::Any => "\"*\"".to_string(),
        DependencySpec::Table(t) => {
            let entries: Vec<String> = t
                .iter()
                .map(|(key, value)| format!("{key} = {value}"))
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
    }
}

/// Run the host's `cargo` in `dir`, failing with its stderr on error.
fn run_cargo(dir: &Path, args: &[&str]) -> CargoResult<()> {
    let output = std::process::Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| crate::CargoError(format!("failed to run cargo: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::CargoError(format!(
            "cargo {} failed:\n{stderr}",
            args.first().copied().unwrap_or_default()
        )));
    }
    Ok(())
}

/// Cargo's own lockfile keys are a superset of ours: the scratch package
/// itself (no `source`) and transitive deps (also `source = "registry"`)
/// appear alongside the direct deps. Registry-sourced packages named in
/// the manifest are kept as-is; `{ path = ... }` manifest entries match
/// the sourceless lock entries cargo writes for path dependencies and
/// lock with `source = "path"`.
fn read_cargo_lock(path: &Path, manifest: &Manifest) -> CargoResult<Vec<LockedDep>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| crate::CargoError(format!("cannot read {}: {e}", path.display())))?;
    let doc: toml::Value = text
        .parse()
        .map_err(|e| crate::CargoError(format!("{}: {e}", path.display())))?;

    let mut by_name: BTreeMap<String, LockedDep> = BTreeMap::new();
    let mut path_versions: BTreeMap<String, String> = BTreeMap::new();
    if let Some(toml::Value::Array(packages)) = doc.get("package") {
        for package in packages {
            let Some(table) = package.as_table() else {
                continue;
            };
            let Some(name) = table.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(version) = table.get("version").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(source) = table.get("source").and_then(|v| v.as_str()) else {
                // No source: the scratch package itself, or a path
                // dependency (a git dependency stays out of scope).
                path_versions.insert(name.to_string(), version.to_string());
                continue;
            };
            let checksum = table
                .get("checksum")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            by_name.insert(
                name.to_string(),
                LockedDep {
                    name: name.to_string(),
                    version: version.to_string(),
                    source: source.to_string(),
                    checksum,
                },
            );
        }
    }

    // Keep only the manifest's direct deps; transitive deps resolve with
    // their parent through the adaptor's own cargo build.
    let mut direct: Vec<LockedDep> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for (name, spec) in &manifest.rust_deps {
        if spec.path_spec().is_some() {
            match path_versions.get(name) {
                Some(version) => direct.push(LockedDep {
                    name: name.clone(),
                    version: version.clone(),
                    source: "path".to_string(),
                    checksum: None,
                }),
                None => missing.push(name.clone()),
            }
        } else if let Some(dep) = by_name.get(name) {
            direct.push(dep.clone());
        } else {
            missing.push(name.clone());
        }
    }
    if !missing.is_empty() {
        return Err(crate::CargoError(format!(
            "cargo did not resolve: {}",
            missing.join(", ")
        )));
    }
    Ok(direct)
}

/// Where the host cargo extracts registry sources (`~/.cargo/registry/src`).
fn cargo_registry_src() -> PathBuf {
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return PathBuf::from(home).join("registry").join("src");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cargo").join("registry").join("src")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_satisfies_common_forms() {
        assert!(version_satisfies("1.0.128", "1.0"));
        assert!(version_satisfies("1.0.128", "1"));
        assert!(version_satisfies("1.0.128", "*"));
        assert!(version_satisfies("1.0.128", "^1.0"));
        assert!(!version_satisfies("2.0.0", "1.0"));
        assert!(!version_satisfies("1.2.0", "1.1"));
        // Exotic requirements are trusted rather than re-resolved.
        assert!(version_satisfies("0.31.0", ">=0.31, <0.32"));
    }

    #[test]
    fn spec_dep_toml_forms() {
        assert_eq!(
            spec_dep_toml(&DependencySpec::Version("1.0".into())),
            "\"1.0\""
        );
        assert_eq!(spec_dep_toml(&DependencySpec::Any), "\"*\"");
        assert_eq!(
            spec_dep_toml(&DependencySpec::Table(toml::map::Map::from_iter([(
                "version".to_string(),
                toml::Value::String("1.38".into())
            )]))),
            "{ version = \"1.38\" }"
        );
    }

    #[test]
    fn dir_hash_is_stable_and_distinct() {
        let cache = CacheRoot::at("/tmp/ntsc-cache");
        let mut a = Manifest::default();
        a.add_rust_dep("foo", DependencySpec::Version("1.0".into()));
        let mut b = Manifest::default();
        b.add_rust_dep("bar", DependencySpec::Version("2.0".into()));

        let key_a = scratch_project_dir(&cache, &a).unwrap();
        let key_b = scratch_project_dir(&cache, &b).unwrap();
        assert_eq!(key_a, scratch_project_dir(&cache, &a).unwrap());
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn lock_is_fresh_checks_both_directions() {
        let mut manifest = Manifest::default();
        manifest.add_rust_dep("serde_json", DependencySpec::Version("1.0".into()));

        let fresh = crate::manifest::Lockfile {
            deps: vec![LockedDep {
                name: "serde_json".into(),
                version: "1.0.128".into(),
                source: "registry".into(),
                checksum: None,
            }],
        };
        assert!(lock_is_fresh(&fresh, &manifest, Path::new("/tmp")));

        let stale_version = crate::manifest::Lockfile {
            deps: vec![LockedDep {
                name: "serde_json".into(),
                version: "2.0.0".into(),
                source: "registry".into(),
                checksum: None,
            }],
        };
        assert!(!lock_is_fresh(&stale_version, &manifest, Path::new("/tmp")));

        let stale_missing = crate::manifest::Lockfile::default();
        assert!(!lock_is_fresh(&stale_missing, &manifest, Path::new("/tmp")));

        // A removed dep leaves the lock stale.
        let mut manifest2 = manifest.clone();
        assert!(manifest2.remove_rust_dep("serde_json"));
        assert!(!lock_is_fresh(&fresh, &manifest2, Path::new("/tmp")));
    }

    #[test]
    fn path_spec_reads_local_package_identity() {
        let root = std::env::temp_dir().join(format!("ntsc_pathdep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let local = root.join("libs").join("tiny");
        std::fs::create_dir_all(local.join("src")).unwrap();
        std::fs::write(
            local.join("Cargo.toml"),
            "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let project = root.join("proj");
        std::fs::create_dir_all(&project).unwrap();

        let spec = DependencySpec::Table(toml::map::Map::from_iter([(
            "path".to_string(),
            toml::Value::String("../libs/tiny".into()),
        )]));
        assert_eq!(spec.path_spec(), Some("../libs/tiny"));
        let dep = resolve_path_dep(&project, "tiny", &spec).unwrap();
        assert_eq!(dep.version, "0.1.0");
        assert_eq!(dep.source, "path");

        let wrong = resolve_path_dep(&project, "other", &spec);
        assert!(wrong.is_err());

        let missing_spec = DependencySpec::Table(toml::map::Map::from_iter([(
            "path".to_string(),
            toml::Value::String("../libs/nope".into()),
        )]));
        assert!(resolve_path_dep(&project, "nope", &missing_spec).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn read_cargo_lock_matches_sourceless_path_packages() {
        let root = std::env::temp_dir().join(format!("ntsc_pathlock_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("Cargo.lock"),
            "[[package]]\nname = \"ntsc-resolve\"\nversion = \"0.0.0\"\n\n[[package]]\nname = \"tiny\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let mut manifest = Manifest::default();
        manifest.add_rust_dep(
            "tiny",
            DependencySpec::Table(toml::map::Map::from_iter([(
                "path".to_string(),
                toml::Value::String("../libs/tiny".into()),
            )])),
        );
        let deps = read_cargo_lock(&root.join("Cargo.lock"), &manifest).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].version, "0.1.0");
        assert_eq!(deps[0].source, "path");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn path_dependency_resolves_and_emits_offline() {
        let root = std::env::temp_dir().join(format!("ntsc_pathe2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let local = root.join("libs").join("tiny");
        std::fs::create_dir_all(local.join("src")).unwrap();
        std::fs::write(
            local.join("Cargo.toml"),
            "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            local.join("src").join("lib.rs"),
            "pub fn answer() -> u64 { 42 }\n",
        )
        .unwrap();
        let project = root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let manifest_path = project.join("neutron.toml");
        std::fs::write(
            &manifest_path,
            "[package]\nentry = \"src/main.nt\"\noutput = \"app\"\n\n[rust-deps]\ntiny = { path = \"../libs/tiny\" }\n",
        )
        .unwrap();

        let mut manifest = Manifest::read(&manifest_path).unwrap();
        let cache = CacheRoot::at(root.join("cache"));
        let deps = resolve_and_fetch(&cache, &project, &mut manifest, &manifest_path).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "tiny");
        assert_eq!(deps[0].source, "path");
        assert!(
            project
                .join(".ntsc")
                .join("tiny")
                .join("tiny.export.json")
                .is_file()
        );

        let lock = crate::manifest::read_lock(&project).unwrap().unwrap();
        assert!(lock_is_fresh(&lock, &manifest, &project));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
