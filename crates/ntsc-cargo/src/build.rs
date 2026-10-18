//! Building the adaptor crates into one archive for linking.
//!
//! Every emitted adaptor under `<project>/.ntsc/` is compiled by one
//! `cargo build` invocation into a static archive (each adaptor is a
//! `crate-type = ["staticlib"]` crate), mirroring how `ntsc-runtime`
//! already produces its staticlib. The resulting archive is handed to the
//! existing `link_binary` path, so the linker consumes it exactly like the
//! runtime archive.

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};

use crate::cache::CacheRoot;
use crate::metadata::API_METADATA_SCHEMA_VERSION;
use crate::{CargoError, CargoResult};

/// Compile every adaptor under `<project>/.ntsc/` and return the
/// static archives to link, one per adaptor crate.
///
/// Archives are cached in the global cache keyed by
/// `(crate, version, profile)` so unchanged dependencies are not rebuilt.
pub fn build_adaptors(
    cache: &CacheRoot,
    project_dir: &Path,
    release: bool,
) -> CargoResult<Vec<PathBuf>> {
    Ok(build_adaptors_with_native_links(cache, project_dir, release)?.archives)
}

/// Build adaptor archives and collect native linker arguments emitted by
/// their Cargo build scripts. Cargo writes those directives to the build
/// script `output` files, which lets the final Neutron link preserve native
/// dependencies without knowing anything about individual crates.
pub fn build_adaptors_with_native_links(
    cache: &CacheRoot,
    project_dir: &Path,
    release: bool,
) -> CargoResult<AdaptorArtifacts> {
    let deps_dir = crate::cache::project_deps_dir(project_dir);
    if !deps_dir.is_dir() {
        return Ok(AdaptorArtifacts::default());
    }

    let mut archives = Vec::new();
    let mut native_link_args = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&deps_dir)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("Cargo.toml").is_file())
        .collect();
    entries.sort();

    if entries.len() > 1 {
        return build_adaptor_bundle(cache, &deps_dir, &entries, release);
    }

    for adaptor in entries {
        let name = adaptor
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let version = read_adaptor_version(&adaptor)?;
        let profile = if release { "release" } else { "debug" };

        let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
        let manifest_text = std::fs::read_to_string(adaptor.join("Cargo.toml")).unwrap_or_default();
        let features = adaptor_cache_features(&manifest_text, &name);
        let features_hash = stable_cache_key(&name, &version, &target, profile, &features);
        let out_dir = cache
            .build_dir()
            .join(format!("{name}-{version}"))
            .join(format!("{target}-{features_hash}"));
        // Cargo nests the archive one `<profile>/` level under the target
        // dir.
        let archive = out_dir.join(profile).join(staticlib_name(&name));
        let needs_rebuild = adaptor_needs_rebuild(&adaptor, &name, &archive);
        if !needs_rebuild && archive.is_file() {
            crate::cache::touch_last_use(&out_dir)?;
            archives.push(archive);
            collect_native_link_args(&out_dir, profile, &mut native_link_args);
            continue;
        }
        // If we are rebuilding, remove the old archive first so `cargo build`
        // does not see a stale `target` dir.
        if archive.is_file() {
            let _ = std::fs::remove_file(&archive);
        }

        run_cargo_build(&adaptor, &out_dir, release)?;
        if !archive.is_file() {
            return Err(crate::CargoError(format!(
                "cargo built `{name}` but no archive appeared at {}",
                archive.display()
            )));
        }
        crate::cache::touch_last_use(&out_dir)?;
        archives.push(archive);
        collect_native_link_args(&out_dir, profile, &mut native_link_args);
    }
    native_link_args.sort();
    native_link_args.dedup();
    Ok(AdaptorArtifacts {
        archives,
        native_link_args,
    })
}

// Rust layouts and trait objects are only interchangeable within the same
// Cargo graph. Separate staticlibs can enable different transitive features
// on a shared dependency even when its version and public type tag agree.
fn build_adaptor_bundle(
    cache: &CacheRoot,
    deps_dir: &Path,
    adaptors: &[PathBuf],
    release: bool,
) -> CargoResult<AdaptorArtifacts> {
    let bundle = deps_dir.join(".bundle");
    std::fs::create_dir_all(bundle.join("src"))?;
    let mut dependencies = toml::Table::new();
    let mut patches = toml::Table::new();
    let mut source = String::from("#![deny(unsafe_code)]\n");
    let mut inputs = Vec::new();
    for adaptor in adaptors {
        let text = std::fs::read_to_string(adaptor.join("Cargo.toml"))?;
        let manifest: toml::Table = text
            .parse()
            .map_err(|error| CargoError(format!("{}: {error}", adaptor.display())))?;
        let name = adaptor
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| CargoError(format!("invalid adaptor path: {}", adaptor.display())))?;
        if let Some(deps) = manifest.get("dependencies").and_then(toml::Value::as_table) {
            for (key, value) in deps {
                merge_bundle_dependency(&mut dependencies, key, value)?;
            }
        }
        if let Some(entries) = manifest
            .get("patch")
            .and_then(|value| value.get("crates-io"))
            .and_then(toml::Value::as_table)
        {
            for (key, value) in entries {
                merge_bundle_dependency(&mut patches, key, value)?;
            }
        }
        source.push_str(&format!(
            "#[path = \"../../{name}/src/lib.rs\"]\nmod ntsc_bridge_{};\n",
            name.replace('-', "_")
        ));
    }
    // Every directly adapted package must also satisfy transitive edges from
    // crates.io; otherwise Cargo still creates distinct copies of its types.
    for (key, value) in &dependencies {
        if let Some(path) = value.get("path").and_then(toml::Value::as_str) {
            let package = value
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            patches.insert(
                package.into(),
                toml::Value::Table(toml::Table::from_iter([(
                    "path".into(),
                    toml::Value::String(path.into()),
                )])),
            );
        }
    }
    let manifest = toml::Table::from_iter([
        (
            "package".into(),
            toml::Value::Table(toml::Table::from_iter([
                (
                    "name".into(),
                    toml::Value::String("ntsc-adaptor-bundle".into()),
                ),
                ("version".into(), toml::Value::String("0.0.0".into())),
                ("edition".into(), toml::Value::String("2021".into())),
            ])),
        ),
        ("workspace".into(), toml::Value::Table(toml::Table::new())),
        (
            "lib".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "crate-type".into(),
                toml::Value::Array(vec![toml::Value::String("staticlib".into())]),
            )])),
        ),
        ("dependencies".into(), toml::Value::Table(dependencies)),
        (
            "patch".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "crates-io".into(),
                toml::Value::Table(patches),
            )])),
        ),
    ]);
    let manifest_text =
        toml::to_string(&manifest).map_err(|error| CargoError(error.to_string()))?;
    write_bundle_file(&bundle.join("Cargo.toml"), &manifest_text)?;
    write_bundle_file(&bundle.join("src/lib.rs"), &source)?;
    inputs.push(manifest_text);
    let compiler = std::process::Command::new("rustc").arg("-vV").output()?;
    if !compiler.status.success() {
        return Err(CargoError(
            "rustc -vV failed while identifying the adaptor compiler".into(),
        ));
    }
    inputs.push(String::from_utf8_lossy(&compiler.stdout).into_owned());
    let profile = if release { "release" } else { "debug" };
    let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let key = stable_cache_key("bundle", "1", &target, profile, &inputs);
    let out_dir = cache
        .build_dir()
        .join("bundle")
        .join(format!("{target}-{key}"));
    run_cargo_build(&bundle, &out_dir, release)?;
    let archive = out_dir.join(profile).join(staticlib_name("bundle"));
    if !archive.is_file() {
        return Err(CargoError(format!(
            "cargo built the adaptor bundle but {} is missing",
            archive.display()
        )));
    }
    crate::cache::touch_last_use(&out_dir)?;
    let mut native_link_args = Vec::new();
    collect_native_link_args(&out_dir, profile, &mut native_link_args);
    native_link_args.sort();
    native_link_args.dedup();
    Ok(AdaptorArtifacts {
        archives: vec![archive],
        native_link_args,
    })
}

fn write_bundle_file(path: &Path, content: &str) -> CargoResult<()> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == content => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn merge_bundle_dependency(
    dependencies: &mut toml::Table,
    key: &str,
    value: &toml::Value,
) -> CargoResult<()> {
    let incoming = match value {
        toml::Value::String(version) => {
            toml::Table::from_iter([("version".into(), toml::Value::String(version.clone()))])
        }
        toml::Value::Table(table) => table.clone(),
        _ => return Err(CargoError(format!("invalid adaptor dependency `{key}`"))),
    };
    let Some(existing) = dependencies.get_mut(key) else {
        dependencies.insert(key.into(), toml::Value::Table(incoming));
        return Ok(());
    };
    let existing = existing
        .as_table_mut()
        .ok_or_else(|| CargoError(format!("invalid adaptor dependency `{key}`")))?;
    for field in ["path", "git", "package"] {
        if existing.get(field) != incoming.get(field) {
            return Err(CargoError(format!(
                "adaptors disagree on `{key}.{field}`; shared Rust types require one dependency source"
            )));
        }
    }
    // A shared path fixes the version; one edge may spell it as a range and
    // another as an exact pin. Registry-only edges must agree explicitly.
    if !existing.contains_key("path") && existing.get("version") != incoming.get("version") {
        return Err(CargoError(format!(
            "adaptors require incompatible versions of `{key}`"
        )));
    }
    let defaults = existing
        .get("default-features")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
        || incoming
            .get("default-features")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
    existing.insert("default-features".into(), toml::Value::Boolean(defaults));
    let mut features: Vec<String> = existing
        .get("features")
        .into_iter()
        .chain(incoming.get("features"))
        .filter_map(toml::Value::as_array)
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(String::from)
        .collect();
    features.sort();
    features.dedup();
    existing.insert(
        "features".into(),
        toml::Value::Array(features.into_iter().map(toml::Value::String).collect()),
    );
    Ok(())
}

#[derive(Debug, Default)]
pub struct AdaptorArtifacts {
    pub archives: Vec<PathBuf>,
    pub native_link_args: Vec<String>,
}

fn collect_native_link_args(out_dir: &Path, profile: &str, args: &mut Vec<String>) {
    let build_dir = out_dir.join(profile).join("build");
    let Ok(entries) = std::fs::read_dir(build_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let output = entry.path().join("output");
        let Ok(text) = std::fs::read_to_string(output) else {
            continue;
        };
        for line in text.lines() {
            if let Some(path) = line.strip_prefix("cargo:rustc-link-search=native=") {
                args.push(format!("-L{path}"));
            } else if let Some(lib) = line.strip_prefix("cargo:rustc-link-lib=") {
                let mut parts = lib.splitn(2, '=');
                let kind = parts.next().unwrap_or_default();
                let name = parts.next().unwrap_or(kind);
                if parts.next().is_some() {
                    continue;
                }
                if kind == "static" {
                    args.push(format!("-l:lib{name}.a"));
                } else {
                    args.push(format!("-l{name}"));
                }
            }
        }
    }
}

/// The static archive name for an adaptor crate (host-flavoured, with
/// cargo's `-`→`_` normalization of the crate name in the lib name).
fn staticlib_name(adaptor_dir_name: &str) -> String {
    // The directory name is the crate name (`serde_json`); the package
    // name in Cargo.toml is `ntsc-adaptor-<name>`, whose lib artifact
    // normalizes hyphens to underscores.
    let lib_name = format!("ntsc_adaptor_{}", adaptor_dir_name.replace('-', "_"));
    if cfg!(target_os = "windows") {
        format!("{lib_name}.lib")
    } else {
        format!("lib{lib_name}.a")
    }
}

/// The stable cache key for one adaptor build: FNV-1a over the exact
/// inputs that change the compiled archive (crate, version, target,
/// profile, sorted dependency features).
///
/// This must be deterministic across processes and machines — the
/// previous `DefaultHasher`-over-`Cargo.toml`-text key changed with the
/// absolute `path = "..."` line, so the same adaptor never shared a
/// cache entry between checkouts.
fn stable_cache_key(
    name: &str,
    version: &str,
    target: &str,
    profile: &str,
    features: &[String],
) -> String {
    let mut sorted = features.to_vec();
    sorted.sort();
    let mut hash: u64 = 0xcbf29ce484222325;
    for part in std::iter::once(name)
        .chain(std::iter::once(version))
        .chain(std::iter::once(target))
        .chain(std::iter::once(profile))
        .chain(sorted.iter().map(String::as_str))
    {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// The dependency features that participate in an adaptor build, read
/// back from its generated `Cargo.toml`: the dependency's own
/// `features` list plus a `tokio` marker when async bridges pulled the
/// Tokio runtime in. The machine-specific `path` line is deliberately
/// excluded so the key is portable.
fn adaptor_cache_features(manifest_text: &str, name: &str) -> Vec<String> {
    let mut features = Vec::new();
    let doc: toml::Value = match manifest_text.parse() {
        Ok(doc) => doc,
        Err(_) => return features,
    };
    let dep = doc.get("dependencies").and_then(|deps| deps.get(name));
    if let Some(list) = dep
        .and_then(|dep| dep.get("features"))
        .and_then(toml::Value::as_array)
    {
        features.extend(
            list.iter()
                .filter_map(toml::Value::as_str)
                .map(String::from),
        );
    }
    let has_tokio = doc
        .get("dependencies")
        .and_then(|deps| deps.get("tokio"))
        .is_some();
    if has_tokio {
        features.push("tokio".into());
    }
    features.sort();
    features
}

/// Whether the cached archive for one adaptor is stale: a missing
/// archive, an export manifest from an older schema, or any generated
/// file (`src/lib.rs`, `Cargo.toml`, `<name>.export.json`) newer than
/// the archive all require a rebuild.
fn adaptor_needs_rebuild(adaptor: &Path, name: &str, archive: &Path) -> bool {
    if !archive.is_file() {
        return true;
    }
    if read_export_schema(adaptor, name) != Some(API_METADATA_SCHEMA_VERSION) {
        return true;
    }
    let archive_mtime = std::fs::metadata(archive)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    [
        adaptor.join("src").join("lib.rs"),
        adaptor.join("Cargo.toml"),
        adaptor.join(format!("{name}.export.json")),
    ]
    .iter()
    .any(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified > archive_mtime)
            .unwrap_or(false)
    })
}

/// The `schema_version` recorded in one adaptor's export manifest, or
/// `None` when the manifest is missing or malformed.
fn read_export_schema(adaptor: &Path, name: &str) -> Option<u32> {
    let content = std::fs::read_to_string(adaptor.join(format!("{name}.export.json"))).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value.get("schema_version")?.as_u64()?.try_into().ok()
}

/// The crate version an adaptor was generated against, from its
/// `[package.metadata.ntsc]` table.
fn read_adaptor_version(adaptor_dir: &Path) -> CargoResult<String> {
    let text = std::fs::read_to_string(adaptor_dir.join("Cargo.toml"))
        .map_err(|e| crate::CargoError(format!("cannot read {}: {e}", adaptor_dir.display())))?;
    let doc: toml::Value = text
        .parse()
        .map_err(|e| crate::CargoError(format!("{}: {e}", adaptor_dir.display())))?;
    let version = doc
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("ntsc"))
        .and_then(|n| n.get("crate-version"))
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    Ok(version)
}

/// Run `cargo build` for one adaptor crate into `out_dir`.
///
/// On an interactive terminal, Cargo's per-crate `Compiling ...` flood is
/// collapsed into a single in-place progress bar (one line that updates as
/// each dependency finishes); on a pipe or in CI every line is streamed
/// verbatim so logs stay complete. The output is always buffered: on failure
/// the full log is returned in the error.
fn run_cargo_build(adaptor_dir: &Path, out_dir: &Path, release: bool) -> CargoResult<()> {
    let name = adaptor_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    // Collapse cargo's `Compiling` flood into an in-place bar only on a real
    // terminal: a pipe or CI must keep every line, and `TERM=dumb` cannot.
    let progress = std::io::stderr().is_terminal()
        && std::env::var("TERM")
            .map(|term| term != "dumb")
            .unwrap_or(true);
    let run = |offline: bool| -> CargoResult<(bool, String)> {
        let mut command = std::process::Command::new("cargo");
        command
            .arg("build")
            .arg("--manifest-path")
            .arg(adaptor_dir.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(out_dir)
            // The adaptor and its cached dependency sources are generated /
            // third-party code: cargo caps lints for registry dependencies,
            // but path dependencies do not get that treatment, so cap them
            // here to keep the build output quiet. Errors still surface.
            .env("RUSTFLAGS", "--cap-lints allow -C debuginfo=0")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if offline {
            command.arg("--offline");
        }
        if release {
            command.arg("--release");
        }
        let mut child = command
            .spawn()
            .map_err(|e| crate::CargoError(format!("failed to run cargo: {e}")))?;
        // Pump stderr: buffer every line for the failure report, and render
        // cargo's per-crate progress either as a live in-place bar (TTY) or
        // as plain streamed lines (pipe / CI).
        let stderr = child.stderr.take().expect("piped stderr");
        let label = name.clone();
        let pump = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            let mut buffered = String::new();
            let mut compiled = 0usize;
            let mut drawn = false;
            use std::io::BufRead as _;
            for line in reader.lines() {
                let line = line.unwrap_or_default();
                buffered.push_str(&line);
                buffered.push('\n');
                if let Some(krate) = compiling_crate(&line) {
                    compiled += 1;
                    if progress {
                        render_progress(compiled, &krate);
                        drawn = true;
                        continue;
                    }
                }
                if drawn {
                    clear_progress();
                    drawn = false;
                }
                eprintln!("{line}");
            }
            if drawn {
                finish_progress(&label, compiled);
            }
            buffered
        });
        let output = child
            .wait_with_output()
            .map_err(|e| crate::CargoError(format!("failed to run cargo: {e}")))?;
        let stderr_log = pump.join().unwrap_or_default();
        Ok((output.status.success(), stderr_log))
    };

    println!("  Compiling adaptor {name}...");

    // Try online first so a newer patch release can still be picked up;
    // fall back to offline when the registry is unreachable (sandboxed
    // CI, no network) and every crate is already in cargo's local cache.
    let (success, stderr) = match run(false) {
        Ok((true, _)) => return Ok(()),
        Ok((false, _)) => run(true)?,
        Err(_) => run(true)?,
    };
    if !success {
        return Err(crate::CargoError(format!(
            "building the `{}` adaptor failed:\n{stderr}",
            adaptor_dir.display()
        )));
    }
    Ok(())
}

/// Cargo prints `   Compiling <crate> v<version>` for each build unit. Return
/// the `crate vX` label from such a line, or `None` for every other line.
fn compiling_crate(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("Compiling ")?;
    let mut fields = rest.split_whitespace();
    let krate = fields.next()?;
    match fields.next() {
        Some(version) => Some(format!("{krate} {version}")),
        None => Some(krate.to_string()),
    }
}

/// Cargo never announces how many units it will compile, so the bar scrolls
/// an indeterminate block while the crate count climbs alongside it.
fn progress_bar(step: usize) -> String {
    const WIDTH: usize = 20;
    const BLOCK: usize = 5;
    let head = step % WIDTH;
    (0..WIDTH)
        .map(|i| {
            let offset = (i + WIDTH - head) % WIDTH;
            if offset < BLOCK { '█' } else { '░' }
        })
        .collect()
}

/// Redraw the single in-place progress line, cleared to end-of-line first.
fn render_progress(compiled: usize, krate: &str) {
    let line = format!("  [{}] {compiled} crates  {krate}", progress_bar(compiled));
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "\r\x1b[K{}", truncate_to_width(&line, 78));
    let _ = stderr.flush();
}

/// Erase the in-place progress line without leaving output behind.
fn clear_progress() {
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "\r\x1b[K");
    let _ = stderr.flush();
}

/// Replace the transient progress line with a permanent summary of the build.
fn finish_progress(label: &str, compiled: usize) {
    clear_progress();
    eprintln!("  adaptor `{label}` built ({compiled} crates compiled)");
}

/// Shorten `text` to `max` display columns, marking the cut with an ellipsis.
fn truncate_to_width(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut kept: String = text.chars().take(max.saturating_sub(1)).collect();
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staticlib_name_is_host_flavoured() {
        let name = staticlib_name("serde_json");
        if cfg!(target_os = "windows") {
            assert_eq!(name, "ntsc_adaptor_serde_json.lib");
        } else {
            assert_eq!(name, "libntsc_adaptor_serde_json.a");
        }
        // Hyphenated crate names normalize to underscores.
        assert!(!staticlib_name("some-crate").contains('-'));
    }

    #[test]
    fn reads_adaptor_version_from_metadata() {
        let dir = std::env::temp_dir().join(format!("ntsc_buildver_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"ntsc-adaptor-foo\"\nversion = \"0.0.0\"\n\n[package.metadata.ntsc]\ncrate-version = \"1.2.3\"\n",
        )
        .unwrap();
        assert_eq!(read_adaptor_version(&dir).unwrap(), "1.2.3");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_deps_dir_is_empty() {
        let cache = CacheRoot::at("/nonexistent-ntsc-cache");
        let archives = build_adaptors(&cache, Path::new("/nonexistent-project"), false).unwrap();
        assert!(archives.is_empty());
    }

    #[test]
    fn cache_key_is_stable_and_feature_sensitive() {
        let base = stable_cache_key(
            "foo",
            "1.2.3",
            "x86_64-linux",
            "debug",
            &["a".into(), "b".into()],
        );
        // Same inputs, any feature order: same key.
        assert_eq!(
            base,
            stable_cache_key(
                "foo",
                "1.2.3",
                "x86_64-linux",
                "debug",
                &["b".into(), "a".into()]
            )
        );
        // Every input participates.
        assert_ne!(
            base,
            stable_cache_key(
                "foo",
                "1.2.4",
                "x86_64-linux",
                "debug",
                &["a".into(), "b".into()]
            )
        );
        assert_ne!(
            base,
            stable_cache_key("foo", "1.2.3", "x86_64-linux", "debug", &["a".into()])
        );
        assert_ne!(
            base,
            stable_cache_key(
                "foo",
                "1.2.3",
                "x86_64-linux",
                "release",
                &["a".into(), "b".into()]
            )
        );
        assert_ne!(
            base,
            stable_cache_key(
                "bar",
                "1.2.3",
                "x86_64-linux",
                "debug",
                &["a".into(), "b".into()]
            )
        );
    }

    #[test]
    fn cache_features_exclude_machine_paths() {
        // The absolute `path` line must not participate: the same
        // dependency in two checkouts shares one cache entry.
        let left = "[dependencies]\nfoo = { version = \"1\", features = [\"std\"], path = \"/home/a/.ntsc/cargo/registry/src/foo-1.0.0\" }\n";
        let right = "[dependencies]\nfoo = { version = \"1\", features = [\"std\"], path = \"/home/b/.ntsc/cargo/registry/src/foo-1.0.0\" }\n";
        assert_eq!(
            adaptor_cache_features(left, "foo"),
            adaptor_cache_features(right, "foo")
        );
        assert_eq!(adaptor_cache_features(left, "foo"), ["std".to_string()]);
        // Async bridges pull Tokio: that changes the archive, so it
        // joins the key.
        let async_manifest =
            "[dependencies]\nfoo = { path = \"/x\" }\ntokio = { version = \"1\" }\n";
        assert_eq!(
            adaptor_cache_features(async_manifest, "foo"),
            ["tokio".to_string()]
        );
    }

    #[test]
    fn stale_schema_or_sources_require_rebuild() {
        let dir = std::env::temp_dir().join(format!("ntsc_buildfresh_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "// adaptor\n").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let archive = dir.join("archive.a");

        // No archive yet: rebuild.
        assert!(adaptor_needs_rebuild(&dir, "foo", &archive));
        std::fs::write(&archive, "archive").unwrap();
        // No export manifest at all: stale (an older generator wrote it).
        assert!(adaptor_needs_rebuild(&dir, "foo", &archive));

        // Current schema, archive newest: fresh.
        std::fs::write(
            dir.join("foo.export.json"),
            format!("{{\"schema_version\": {API_METADATA_SCHEMA_VERSION}}}"),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&archive, "archive").unwrap();
        assert!(!adaptor_needs_rebuild(&dir, "foo", &archive));

        // Older schema: stale even when every mtime is fresh.
        std::fs::write(dir.join("foo.export.json"), "{\"schema_version\": 1}").unwrap();
        assert!(adaptor_needs_rebuild(&dir, "foo", &archive));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collects_native_link_directives_from_build_outputs() {
        let root = std::env::temp_dir().join(format!("ntsc_native_links_{}", std::process::id()));
        let output_dir = root.join("debug/build/native/out");
        std::fs::create_dir_all(&output_dir).unwrap();
        std::fs::write(
            root.join("debug/build/native/output"),
            "cargo:rustc-link-search=native=/tmp/native\ncargo:rustc-link-lib=static=codec\ncargo:rustc-link-lib=m\n",
        )
        .unwrap();

        let mut args = Vec::new();
        collect_native_link_args(&root, "debug", &mut args);
        args.sort();
        assert_eq!(
            args,
            vec![
                "-L/tmp/native".to_string(),
                "-l:libcodec.a".to_string(),
                "-lm".to_string()
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn progress_line_recognizes_only_compiling_lines() {
        assert_eq!(
            compiling_crate("   Compiling serde_core v1.0.204"),
            Some("serde_core v1.0.204".into())
        );
        // A path-dependency unit prints `Compiling name vX (path)`; keep the
        // crate and version, drop the trailing source path.
        assert_eq!(
            compiling_crate("   Compiling ntsc-adaptor-serde v0.0.0 (/tmp/x)"),
            Some("ntsc-adaptor-serde v0.0.0".into())
        );
        assert_eq!(compiling_crate("    Finished dev [unoptimized]"), None);
        assert_eq!(
            compiling_crate("warning: unused variable: `x`"),
            None,
            "warnings and errors must stream, not drive the bar"
        );
    }

    #[test]
    fn progress_bar_is_a_fixed_width_scrolling_block() {
        let bar = progress_bar(0);
        assert_eq!(bar.chars().count(), 20);
        // The lit window has a constant size and only its position advances.
        assert_eq!(bar.chars().filter(|c| *c == '█').count(), 5);
        assert_eq!(
            progress_bar(7).chars().filter(|c| *c == '█').count(),
            progress_bar(42).chars().filter(|c| *c == '█').count()
        );
        assert_ne!(progress_bar(0), progress_bar(3), "the bar must move");
        // It wraps on a fixed period rather than running off the end.
        assert_eq!(progress_bar(0), progress_bar(20));
    }

    #[test]
    fn progress_line_is_truncated_for_display() {
        assert_eq!(truncate_to_width("short", 78), "short");
        let long = "a".repeat(100);
        let cut = truncate_to_width(&long, 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('…'));
    }
}
