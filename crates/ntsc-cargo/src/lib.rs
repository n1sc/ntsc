//! Rust-crate dependency support for the Neutron compiler.
//!
//! Neutron `install`s and `use`s crates from the cargo registry: the crate's
//! source is fetched through the real `cargo` binary into a global cache, a
//! small adaptor crate is generated next to the project, and `ntsc build`
//! compiles neutron code, the adaptor, and the dependency source in one link
//! session — so there is no ABI boundary and no `unsafe` anywhere.
//!
//! The crate is split by concern:
//!
//! - [`manifest`] reads and writes `[rust-deps]` in `neutron.toml` plus the
//!   project's `ntsc.lock`,
//! - [`resolve`] delegates resolution to the real `cargo` binary,
//! - [`fetch`] obtains exact crate source into the global cache,
//! - [`cache`] owns the `~/.ntsc/cargo/` directory layout,
//! - [`adaptor`] emits the generated adaptor crate,
//! - [`run`] is the CLI entry point used by `ntsc-cli`.

pub mod adaptor;
pub mod build;
pub mod cache;
pub mod coverage;
pub mod doc;
pub mod fetch;
pub mod manifest;
pub mod metadata;
pub mod resolve;

use std::path::Path;

/// An error surfaced to the `ntsc cargo` command line. Errors here are
/// infrastructure failures (cargo missing, network, manifest corruption),
/// so they carry a plain message rather than source-located diagnostics.
#[derive(Debug)]
pub struct CargoError(pub String);

impl std::fmt::Display for CargoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CargoError {}

impl From<std::io::Error> for CargoError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

pub type CargoResult<T> = Result<T, CargoError>;

/// Run one `ntsc cargo <subcommand>` from raw CLI arguments.
///
/// Dispatch mirrors the shape of the CLI's own command enum: `install`,
/// `remove`, `update`, `list`, and `gc` all live behind this entry point,
/// parallel to how `ntsc pkg` dispatches to its own manager.
pub fn run(args: &[String], project_dir: &Path) -> CargoResult<()> {
    let cache = cache::CacheRoot::from_env();
    let sub = args.first().map(String::as_str).unwrap_or_default();
    match sub {
        "install" => {
            let name = args
                .get(1)
                .ok_or_else(|| CargoError("usage: ntsc cargo install <crate>".into()))?;
            install(&cache, name, project_dir)
        }
        "list" => list(project_dir),
        "doc" => doc::run(project_dir, &args[1..]),
        "coverage" => match args.get(1).map(String::as_str) {
            Some(name) => coverage::coverage(project_dir, name),
            None => coverage::coverage_all(project_dir),
        },
        "remove" => {
            let name = args
                .get(1)
                .ok_or_else(|| CargoError("usage: ntsc cargo remove <crate>".into()))?;
            remove(name, project_dir)
        }
        "update" => update(args.get(1).map(String::as_str), project_dir),
        "features" => {
            let name = args.get(1).ok_or_else(|| {
                CargoError("usage: ntsc cargo features <crate> [--add|--remove] <feature>".into())
            })?;
            let action = args.get(2).map(String::as_str).unwrap_or("--add");
            let feature = args.get(3).ok_or_else(|| {
                CargoError("usage: ntsc cargo features <crate> [--add|--remove] <feature>".into())
            })?;
            edit_feature(name, action, feature, project_dir)
        }
        "gc" => {
            let mut dry_run = false;
            let mut force_rebuild = false;
            let mut max_age = None;
            let mut args = args.iter().skip(1);
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--dry-run" => dry_run = true,
                    "--force-rebuild" => force_rebuild = true,
                    "--max-age" => {
                        let value = args.next().ok_or_else(|| CargoError("usage: ntsc cargo gc [--dry-run] [--max-age <days>] [--force-rebuild]".into()))?;
                        max_age = Some(value.parse().map_err(|_| {
                            CargoError("--max-age requires a number of days".into())
                        })?);
                    }
                    _ => return Err(CargoError(format!("unknown gc option `{arg}`"))),
                }
            }
            cache::gc(&cache, dry_run, max_age, force_rebuild)
        }
        _ => Err(CargoError(format!(
            "unknown `ntsc cargo` subcommand `{sub}`"
        ))),
    }
}

/// Add `crate` to `[rust-deps]`, resolve through cargo, fetch source into
/// the global cache, write `ntsc.lock`, and emit the adaptor crate.
fn remove(name: &str, project_dir: &Path) -> CargoResult<()> {
    let path = project_dir.join("neutron.toml");
    let mut manifest = manifest::Manifest::read(&path)?;
    if !manifest.remove_rust_dep(name) {
        return Err(CargoError(format!("crate `{name}` is not in [rust-deps]")));
    }
    manifest.write(&path)?;
    let _ = std::fs::remove_dir_all(cache::project_deps_dir(project_dir).join(name));
    if let Some(lock) = manifest::read_lock(project_dir)? {
        let deps: Vec<_> = lock.deps.into_iter().filter(|d| d.name != name).collect();
        manifest::write_lock(project_dir, &deps)?;
    }
    println!("Removed {name}");
    Ok(())
}

fn update(name: Option<&str>, project_dir: &Path) -> CargoResult<()> {
    let path = project_dir.join("neutron.toml");
    let mut manifest = manifest::Manifest::read(&path)?;
    if let Some(name) = name
        && !manifest.has_rust_dep(name)
    {
        return Err(CargoError(format!("crate `{name}` is not in [rust-deps]")));
    }
    resolve::resolve_and_fetch(
        &cache::CacheRoot::from_env(),
        project_dir,
        &mut manifest,
        &path,
    )?;
    Ok(())
}

fn edit_feature(name: &str, action: &str, feature: &str, project_dir: &Path) -> CargoResult<()> {
    let path = project_dir.join("neutron.toml");
    let mut manifest = manifest::Manifest::read(&path)?;
    match action {
        "--add" => manifest.add_feature(name, feature)?,
        "--remove" => {
            manifest.remove_feature(name, feature)?;
        }
        _ => return Err(CargoError("expected --add or --remove".into())),
    }
    manifest.write(&path)?;
    println!("Updated features for {name}");
    Ok(())
}

fn install(cache: &cache::CacheRoot, name: &str, project_dir: &Path) -> CargoResult<()> {
    let manifest_path = project_dir.join("neutron.toml");
    let mut manifest = manifest::Manifest::read(&manifest_path)?;

    if manifest.has_rust_dep(name) {
        println!("`{name}` is already in [rust-deps]");
    } else {
        manifest.add_rust_dep(name, manifest::DependencySpec::Any);
        println!("Resolving `{name}` through cargo...");
    }

    resolve::resolve_and_fetch(cache, project_dir, &mut manifest, &manifest_path)?;
    Ok(())
}

/// Print the crates in `[rust-deps]` with their locked versions.
fn list(project_dir: &Path) -> CargoResult<()> {
    let manifest_path = project_dir.join("neutron.toml");
    let manifest = manifest::Manifest::read(&manifest_path)?;
    let locked = manifest::read_lock(project_dir)?;

    if manifest.rust_deps.is_empty() {
        println!("no [rust-deps] in neutron.toml");
        return Ok(());
    }
    for (name, spec) in &manifest.rust_deps {
        let version = locked
            .as_ref()
            .and_then(|lock| lock.version_for(name))
            .unwrap_or("unresolved");
        println!("{name:<24} {version:<12} {}", spec.display());
    }
    Ok(())
}
