//! Emitting the generated adaptor crate for one dependency.
//!
//! For each dependency `foo` the project gets a sibling crate under
//! `.ntsc/foo/` that path-depends on the cached `foo` source and
//! exposes a neutron-friendly bridge in `src/lib.rs`. The adaptor is a
//! standard path-dependency crate — the same mechanism any Rust project
//! uses to depend on crates.io crates — so nothing invasive is patched
//! into the dependency itself.
//!
//! The hard rule from the plan: the generated adaptor carries
//! `#![deny(unsafe_code)]` so no `unsafe` can ever enter the bridge
//! except the narrow, generated `extern "C"` helper calls, and the crate's
//! API surface is an explicit allowlist rather than a blanket re-export.
//!
//! # Marshaling model
//!
//! The compiled neutron codegen calls every bridge function with `i64`
//! arguments (string handles for strings, raw integers for primitives) and
//! expects an `i64` result (a string handle, or the null handle for
//! nothing). The bridge therefore converts between these `i64` values and
//! the actual Rust argument/return types of the dependency's function.
//!
//! Only *tier-1* signatures are exposed: parameters/returns that map
//! cleanly onto a handle (or raw integer) with no structural ambiguity.
//! Functions with `self`, unrepresentable generic `T` parameters, or
//! exotic types are omitted rather than wrapped into something that would
//! not compile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::CargoResult;
use crate::cache::{self, CacheRoot};
use crate::metadata::{
    API_METADATA_SCHEMA_VERSION, ApiStatus, ExportCallback, ExportCallbackMethod, ExportEntry,
    ExportManifest, ExportParam, ExportReturn, ExportTyped, ExportUntyped,
};

/// A discovered, marshalable free function of the dependency.
#[derive(Debug, Clone)]
pub struct Func {
    /// The exported name (`encode`, or `Regex_new` for an impl method).
    pub name: String,
    /// The path used at the call site relative to the crate root
    /// (`encode`, or `Regex::new` for an impl method).
    pub call_path: String,
    /// Whether the call is method syntax on the receiver (`x.f(args)`)
    /// rather than a path call (`Type::f(&x, args)`) — used for trait
    /// impl methods, where the trait path may not be nameable.
    pub method_call: bool,
    /// The call is a field read (`a0_obj.field.clone()`), not a function
    /// call — used for generated struct field getters (`Type_field`).
    pub is_field: bool,
    /// The call is a field write (`a0_obj.field = value`), returning 0 —
    /// used for generated struct field setters (`Type_set_field`).
    /// Setters never take async aliases.
    pub is_setter: bool,
    /// The trait path whose method-syntax call requires it in scope
    /// (`std::fmt::Display` for `impl Display for X`). Present only on
    /// method-call functions.
    pub trait_path: Option<String>,
    /// The call target is a unit enum variant used as a value, not a
    /// function: the bridge references the path without calling it.
    pub unit_variant: bool,
    /// The marshalable parameter kinds, in declaration order.
    pub params: Vec<ParamKind>,
    /// Field names for struct enum variants; empty for ordinary functions and tuple variants.
    pub field_names: Vec<String>,
    /// Whether each param was originally a reference (`&T`/`&mut T`) and must
    /// be passed as `&aN` (otherwise `aN`).
    pub param_ref: Vec<ParamRef>,
    /// The marshalable return kind.
    pub ret: RetKind,
    /// Whether the source function is declared `async`.
    ///
    /// Async Rust functions are described in export metadata but are not
    /// emitted as synchronous bridges until their future ABI is available.
    pub is_async: bool,
    /// The item's `///` doc comments (joined lines), rendered by
    /// `ntsc cargo doc`.
    pub doc: String,
    /// The source function is declared `unsafe fn`. The generated bridge
    /// constructs every argument from safe owned values and calls it inside
    /// an `unsafe` block, so Neutron callers see a safe wrapper.
    pub is_unsafe: bool,
}

/// Rejection reason for a discovered function that cannot be bridged.
#[derive(Debug, Clone)]
pub struct UnsupportedApi {
    pub name: String,
    pub path: String,
    pub status: crate::metadata::ApiStatus,
    pub reason: String,
    /// The unsupported Rust signature in compact notation
    /// (`fn set_global_commands(&self, commands: &[CreateCommand])`),
    /// empty when the item is an impl block rather than one function.
    pub signature: String,
}

/// Public API discovered in source, including entries that have no typed bridge.
#[derive(Debug, Clone, Default)]
pub struct DiscoveredApi {
    pub functions: Vec<Func>,
    pub unsupported: Vec<UnsupportedApi>,
    /// Callback traits implemented by the adaptor (one generated
    /// `__NtscCb_<Trait>` struct, event queue, and poll bridge family per
    /// entry), in first-use order.
    pub callbacks: Vec<CallbackBridge>,
}

/// One callback trait implemented by the adaptor: the generated
/// `__NtscCb_<Short>` struct implements `trait_path`, forwarding each
/// overridden method into a per-trait event queue that Neutron drains
/// through the generated poll bridges.
#[derive(Debug, Clone)]
pub struct CallbackBridge {
    /// Root-callable trait path (`client::EventHandler`).
    pub trait_path: String,
    /// C-identifier-safe short name (`EventHandler`).
    pub short: String,
    /// Overridden methods in declaration order.
    pub methods: Vec<CallbackMethod>,
}

/// One overridden method of a callback trait.
#[derive(Debug, Clone)]
pub struct CallbackMethod {
    pub name: String,
    pub params: Vec<ParamKind>,
    pub param_ref: Vec<ParamRef>,
    pub receiver: ReceiverKind,
    /// Crate-rooted paths (`model::channel::Message`) to import so the
    /// canonical parameter types resolve.
    pub imports: Vec<String>,
    pub is_async: bool,
    pub is_unsafe: bool,
}

pub enum Skip {
    /// A `pub fn` that is safe to ignore (private, method, test probe).
    Ignored,
    /// Present in the API but not marshalable by the tier-1 model.
    Unsupported(String),
}

/// A tier-1 parameter type, mapped from the dependency's real signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamKind {
    /// `String` / `&str` — read from a string handle.
    Str,
    /// `Vec<u8>` / `&[u8]` / `[u8]` — read from the raw bytes of a string
    /// handle.
    Bytes,
    /// `Cow<str>` — receives an owned string reconstructed from a Neutron
    /// string handle.
    CowStr,
    /// `Cow<[u8]>` — receives owned bytes reconstructed from a Neutron
    /// string handle.
    CowBytes,
    /// `Vec<T>` / `&[T]` for scalar `T` — read element-wise from an array
    /// handle (`array[string]`, `array[int]`, ...). `&mut` element
    /// collections are not bridged: there is no sound in-place write-back
    /// for resized or re-encoded elements.
    Array(ArrayElemKind),
    /// A fixed-size `[T; N]` parameter for scalar (or object-handle) `T`
    /// with a compile-time-known length `N`. It reads the same array
    /// handle as [`ParamKind::Array`] and length-checks it into `[T; N]`
    /// before the call, throwing on a wrong length. A `[T; N]` whose
    /// length is a const-generic or named constant stays unsupported.
    FixedArray(ArrayElemKind, usize),
    /// `Option<Vec<T>>` — the null handle for `None`, an array handle for
    /// `Some`. Only by-value (`Owned`) params bridge; `Option<&[T]>` stays
    /// unsupported.
    OptionArray(ArrayElemKind),
    /// `Option<T>` for scalar `T` — null handle for `None`, otherwise a
    /// pointer to the option cell allocated by Neutron.
    OptionInt(String),
    OptionBool,
    OptionFloat(String),
    /// `Option<String>` / `Option<&str>` — null handle for `None`,
    /// otherwise a string handle.
    OptionStr,
    /// `Option<&str>` / `Option<&String>` — like `OptionStr`, but the
    /// value borrows: the bridge keeps an owned scratch alongside and
    /// passes `as_deref()`. Setters and getters never bridge borrowed
    /// options (the referent would dangle after the call).
    OptionStrRef,
    /// `Option<T>` for object `T` — null handle for `None`, otherwise
    /// the value moves into shared space (by-value takes).
    OptionObject(ObjectType),
    /// An integer type (`i64`, `usize`, ...) — read as a raw `i64` and
    /// cast to the concrete Rust type.
    Int(String),
    /// `bool` — read as a raw `i64` (nonzero = true).
    Bool,
    /// `f64` / `f32` — read as a raw `f64` (bit-cast of the `i64` arg).
    Float(String),
    /// `std::time::Duration` — read as millisecond `u64` and built with
    /// `Duration::from_millis`. Durations cross the ABI as plain ints
    /// (millis); the manifest labels them `"int"`.
    Duration,
    /// A handle to a value of a bridged struct, held in the process-wide
    /// shared object space — for `self` receivers and every owned struct
    /// parameter. Handles created by one adaptor resolve in any other, so
    /// cross-crate flows (a builder built here, consumed there) share one
    /// ABI: the type tag, not the adaptor, decides what a handle holds.
    Object(ObjectType),
    /// A callback/closure parameter: the value implements the trait at
    /// `String` (e.g. `H: EventHandler + 'static` or `impl Fn(i64)`). The
    /// arg is a Neutron-registered callback key; the generated adaptor
    /// builds a struct implementing the trait whose methods enqueue
    /// events for Neutron to poll. Fully automatic, not per-crate.
    Callback(String),
}

/// The element type of a bridged collection. Strings, booleans, and the
/// plain scalar families and bridged object handles marshal element-wise
/// through array handles; nested collections and options stay unsupported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayElemKind {
    /// `String` / `&str` elements — `array[string]`.
    Str,
    /// `bool` elements — `array[bool]`, stored as 0/1.
    Bool,
    /// Integer elements — `array[int]`, cast through `i64`.
    Int(String),
    /// Floating-point elements — `array[float]`, carried as `f64` bits.
    Float(String),
    /// A bridged object element stored as an object handle.
    Object(ObjectType),
}

/// Whether a parameter is passed by value or by reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamRef {
    Owned,
    Ref,
    MutRef,
}

/// One bridged object type: a crate-rooted path plus which crate owns it.
/// `owner` is `None` for the analyzed crate itself and `Some(dep)` for a
/// type rooted in a direct dependency (`serenity::client::ClientBuilder`
/// inside the `songbird` adaptor). Handles for foreign types live in the
/// same shared space under the owner's tag, so any adaptor resolves them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectType {
    /// Owning dependency crate, or `None` for the analyzed crate.
    pub owner: Option<String>,
    /// Crate-rooted path within the owner (`client::ClientBuilder`).
    pub path: String,
    /// Smart-pointer wrapping around the path (`Arc<Songbird>`,
    /// `Arc<Mutex<Call>>`). Labels stay `object[path]` for every
    /// wrapping — Neutron handles are wrapper-agnostic — while tags
    /// distinguish wrappings so a plain handle never resolves as shared
    /// and vice versa.
    pub wrapper: ObjectWrapper,
}

/// Reference-counted smart-pointer wrapping around an object path.
/// Only `Arc` shapes bridge: a shared value is already refcounted by the
/// shared space, so the bridge stores the `Arc` itself (one clone) and
/// every handle to it aliases the same allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectWrapper {
    /// A plain owned value (`Widget`, `ClientBuilder`).
    Plain,
    /// `Arc<T>`: dereferenced for `&self` methods.
    Arc,
    /// `Arc<tokio::sync::Mutex<T>>`: `blocking_lock`ed for methods.
    /// Bridges never `.await` inside: argument setup runs outside any
    /// async context, so blocking the worker under the block guard is
    /// sound and cannot stall the runtime.
    TokioMutex,
    /// `Arc<tokio::sync::RwLock<T>>`: `blocking_read` for methods.
    TokioRwLock,
    /// `Arc<std::sync::Mutex<T>>`: locked for methods.
    StdMutex,
    /// `Arc<std::sync::RwLock<T>>`: read for `&self`, written for `&mut`.
    StdRwLock,
    /// `Arc<parking_lot::Mutex<T>>`: locked for methods.
    ParkingMutex,
    /// `Arc<parking_lot::RwLock<T>>`: read for `&self`, written for `&mut`.
    ParkingRwLock,
}

impl ObjectWrapper {
    /// The tag suffix distinguishing this wrapping (`""` for plain).
    pub fn tag_suffix(self) -> &'static str {
        match self {
            Self::Plain => "",
            Self::Arc => "#Arc",
            Self::TokioMutex => "#Arc<tokio::Mutex>",
            Self::TokioRwLock => "#Arc<tokio::RwLock>",
            Self::StdMutex => "#Arc<std::Mutex>",
            Self::StdRwLock => "#Arc<std::RwLock>",
            Self::ParkingMutex => "#Arc<parking_lot::Mutex>",
            Self::ParkingRwLock => "#Arc<parking_lot::RwLock>",
        }
    }
}

impl ObjectType {
    /// A type owned by the analyzed crate.
    pub fn local(path: impl Into<String>) -> Self {
        Self {
            owner: None,
            path: path.into(),
            wrapper: ObjectWrapper::Plain,
        }
    }

    /// A type owned by a direct dependency.
    pub fn foreign(owner: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            owner: Some(owner.into()),
            path: path.into(),
            wrapper: ObjectWrapper::Plain,
        }
    }

    /// Whether the type lives in another crate's adaptor.
    pub fn is_foreign(&self) -> bool {
        self.owner.is_some()
    }

    /// The same type under another wrapping.
    pub fn wrapped(mut self, wrapper: ObjectWrapper) -> Self {
        self.wrapper = wrapper;
        self
    }
}

impl From<String> for ObjectType {
    fn from(path: String) -> Self {
        Self::local(path)
    }
}

impl From<&str> for ObjectType {
    fn from(path: &str) -> Self {
        Self::local(path)
    }
}

/// A tier-1 return type, mapped from the dependency's real signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetKind {
    /// `String` / `&str`.
    Str,
    /// `PathBuf` / `&Path` — the path rendered to a string handle.
    Path,
    /// `Vec<u8>` / `&[u8]` — returned as the raw bytes of a string handle.
    Bytes,
    /// `Option<String>` — null handle for `None`.
    OptionStr,
    /// `Option<&KnownType>` — the value is cloned into the shared space,
    /// null handle for `None` (`json`'s `Object::get`).
    OptionObject(ObjectType),
    /// `Option<Vec<u8>>` — null handle for `None`.
    OptionBytes,
    /// `Option<Vec<T>>` for scalar `T` — null handle for `None`, an array
    /// handle for `Some`.
    OptionArray(ArrayElemKind),
    /// `Option<T>` for scalar `T` — null handle for `None`, otherwise a
    /// pointer to a freshly allocated Neutron option cell.
    OptionInt(String),
    OptionBool,
    OptionFloat(String),
    /// `Result<String, E>` — throws a generic error on `Err`.
    ResultStr,
    /// `Result<PathBuf, E>` — throws on `Err`, path as a string handle.
    ResultPath,
    /// `Result<Vec<u8>, E>` — throws a generic error on `Err`, result
    /// as raw bytes of a string handle.
    ResultBytes,
    /// `Result<Vec<T>, E>` for scalar `T` — throws a generic error on
    /// `Err`, an array handle on `Ok`.
    ResultArray(ArrayElemKind),
    /// `Vec<T>` / `&[T]` for scalar `T` — returned as an array handle.
    /// `Vec<u8>` stays [`RetKind::Bytes`].
    Array(ArrayElemKind),
    /// `Result<(), E>` — throws a generic error on `Err`, returns 0.
    ResultUnit,
    /// A bridged struct by value (`Regex`, `Self`) — stored in the
    /// shared object space, returned as a handle.
    Object(ObjectType),
    /// A borrowed known type that implements `Clone` (`fn index(&self,
    /// key: &str) -> &JsonValue`) — the value is cloned into the shared space.
    ObjectClone(ObjectType),
    /// `Result<Struct, E>` — stores on `Ok`, throws a generic error on
    /// `Err`.
    ResultObject(ObjectType),
    /// An owned tuple of object handles, in positional order.
    ResultObjectTuple(Vec<ObjectType>),
    /// An integer return — raw `i64` (cast from the concrete Rust type).
    Int(String),
    /// `bool` return — raw `i64` (0/1).
    Bool,
    /// `f64` / `f32` return — raw `f64` bit-cast to `i64`.
    Float(String),
    /// `()` — return the null handle.
    Unit,
}

/// One dependency's identity and the resolved inputs that shape its
/// generated adaptor. Grouped so the emit functions take one descriptor
/// instead of five positional fields that are easy to transpose.
pub struct AdaptorRequest<'a> {
    pub name: &'a str,
    pub version: &'a str,
    /// The dependency's source directory: a registry cache copy or a
    /// canonicalized local `path` dependency.
    pub source: &'a Path,
    pub requested_features: Option<&'a std::collections::HashSet<String>>,
    pub dependency_spec: Option<&'a crate::manifest::DependencySpec>,
}

/// Emit (or refresh) the adaptor crate for `name` at `version`.
///
/// Returns the adaptor crate directory.
pub fn emit_adaptor(
    cache: &CacheRoot,
    project_dir: &Path,
    name: &str,
    version: &str,
    cargo_registry_dir: &Path,
    requested_features: Option<&std::collections::HashSet<String>>,
    dependency_spec: Option<&crate::manifest::DependencySpec>,
) -> CargoResult<PathBuf> {
    // The path must exist in the cache before the adaptor can reference it;
    // fetch_source is idempotent, so this both validates and materializes.
    let dep_src = crate::fetch::fetch_source(cache, name, version, cargo_registry_dir)?;
    emit_adaptor_from_source(
        cache,
        project_dir,
        cargo_registry_dir,
        &AdaptorRequest {
            name,
            version,
            source: &dep_src,
            requested_features,
            dependency_spec,
        },
    )
}

/// Emit (or refresh) the adaptor crate from an already-located source
/// directory: registry cache copies and `{ path = ... }` local crates
/// alike. Callers that bypass the registry fetch (path dependencies)
/// resolve the source themselves.
pub fn emit_adaptor_from_source(
    cache: &CacheRoot,
    project_dir: &Path,
    cargo_registry_dir: &Path,
    request: &AdaptorRequest<'_>,
) -> CargoResult<PathBuf> {
    let AdaptorRequest {
        name,
        version,
        source: dep_src,
        requested_features,
        dependency_spec,
    } = *request;
    let dir = cache::project_deps_dir(project_dir).join(name);
    std::fs::create_dir_all(dir.join("src"))?;

    // The public-API scan and binding generation are by far the slowest part
    // of a build (parsing a crate like `serenity` with `syn` takes minutes),
    // yet a crate version's source is immutable. When every input that shapes
    // the generated adaptor is unchanged since the last emit, reuse the files
    // already on disk and skip the scan entirely.
    let fingerprint = emit_fingerprint(name, version, requested_features, dep_src, project_dir);
    let stamp = dir.join(".emit-stamp");
    if adaptor_up_to_date(&dir, name, &stamp, &fingerprint) {
        return Ok(dir);
    }

    // Discover the crate's public API so the adaptor can expose every
    // marshalable `pub fn`.
    let mut discovered = discover_public_api_with_features(dep_src, requested_features);
    // Foreign owners resolve against the project's lockfile: pinned
    // versions, feature sets, and dependency edges. Unresolvable owners
    // demote their functions to `unsupported` (see `resolve_foreign_deps`).
    let analyzed_deps = direct_dep_names(dep_src);
    let foreign = resolve_foreign_deps(
        cache,
        cargo_registry_dir,
        project_dir,
        &analyzed_deps,
        &mut discovered,
    )?;
    let async_needed = discovered.functions.iter().any(|func| func.is_async);
    let manifest = adaptor_manifest(
        name,
        version,
        dep_src,
        async_needed,
        dependency_spec,
        &foreign,
    );
    write_if_changed(&dir.join("Cargo.toml"), &manifest)?;
    // Object type tags bake the locked version and sorted requested
    // features in, so cross-adaptor lookups agree exactly.
    let mut feature_list: Vec<&str> = requested_features
        .map(|set| set.iter().map(String::as_str).collect())
        .unwrap_or_default();
    feature_list.sort_unstable();
    let feature_text = feature_list.join(",");
    let lib = adaptor_lib(
        name,
        version,
        &feature_text,
        &foreign,
        &discovered.functions,
        &discovered.callbacks,
    );
    write_if_changed(&dir.join("src").join("lib.rs"), &lib)?;

    let export_manifest = export_manifest_content(
        name,
        &discovered.functions,
        &discovered.unsupported,
        &discovered.callbacks,
    )?;
    write_if_changed(&dir.join(format!("{name}.export.json")), &export_manifest)?;

    // Record the fingerprint only after every output is in place, so an
    // interrupted emit never looks up to date.
    std::fs::write(&stamp, &fingerprint)?;

    Ok(dir)
}

/// Generated-adaptor content revision. Bump whenever the binding generator's
/// output changes in a way that `API_METADATA_SCHEMA_VERSION` does not already
/// capture — new marshalling rules, bridge templates, or type-tag encodings —
/// so adaptors cached by the previous generator are re-emitted.
pub const ADAPTOR_EMIT_REVISION: u32 = 2;

/// FNV-1a fingerprint of every input that shapes a generated adaptor: the
/// crate's identity and resolved features, a digest of its source tree (so an
/// edited `path` dependency is never reused stale), the project's dependency
/// resolution (manifest and lockfile, which fix foreign type tags), and the
/// generator and schema revisions. Any change yields a different fingerprint
/// and forces a re-emit.
fn emit_fingerprint(
    name: &str,
    version: &str,
    requested_features: Option<&std::collections::HashSet<String>>,
    dep_src: &Path,
    project_dir: &Path,
) -> String {
    let mut features: Vec<&str> = requested_features
        .map(|set| set.iter().map(String::as_str).collect())
        .unwrap_or_default();
    features.sort_unstable();
    let mut hash: u64 = 0xcbf29ce484222325;
    fnv_feed(&mut hash, name.as_bytes());
    fnv_feed(&mut hash, version.as_bytes());
    fnv_feed(&mut hash, features.join(",").as_bytes());
    fnv_feed(&mut hash, dep_src.as_os_str().to_string_lossy().as_bytes());
    fnv_feed(
        &mut hash,
        format!("{API_METADATA_SCHEMA_VERSION}-{ADAPTOR_EMIT_REVISION}").as_bytes(),
    );
    source_tree_digest(dep_src, &mut hash);
    for file in ["neutron.toml", "ntsc.lock"] {
        if let Ok(text) = std::fs::read_to_string(project_dir.join(file)) {
            fnv_feed(&mut hash, text.as_bytes());
        }
    }
    format!("{hash:016x}")
}

/// FNV-1a over `bytes`, with a trailing separator so distinct inputs that
/// concatenate to the same byte string still hash differently.
fn fnv_feed(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
    *hash ^= 0xff;
    *hash = hash.wrapping_mul(0x100000001b3);
}

/// Fold a dependency's source tree into `hash` from cheap per-file metadata —
/// relative path, length, and modification time — without reading contents.
/// An in-place edit to a `path` dependency changes a length or mtime and so
/// invalidates the fingerprint; an immutable registry copy keeps stable
/// mtimes, leaving the digest unchanged across rebuilds.
fn source_tree_digest(root: &Path, hash: &mut u64) {
    let mut files: Vec<(String, u64, u64)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|age| age.as_secs())
                .unwrap_or(0);
            files.push((relative, metadata.len(), modified));
        }
    }
    files.sort();
    for (relative, len, modified) in files {
        fnv_feed(hash, relative.as_bytes());
        fnv_feed(hash, format!("{len}:{modified}").as_bytes());
    }
}

/// Whether the adaptor outputs already reflect `fingerprint`: the stamp matches
/// and every generated file the build consumes is still present on disk.
fn adaptor_up_to_date(dir: &Path, name: &str, stamp: &Path, fingerprint: &str) -> bool {
    std::fs::read_to_string(stamp).ok().as_deref() == Some(fingerprint)
        && dir.join("Cargo.toml").is_file()
        && dir.join("src").join("lib.rs").is_file()
        && dir.join(format!("{name}.export.json")).is_file()
}

/// One foreign owner crate a generated adaptor depends on: the locked
/// version, the sorted requested feature set, and its extracted source.
/// The adaptor names the owner's types through a pinned dependency edge,
/// and both sides compute byte-identical shared-space tags from the
/// version and features recorded here.
pub struct ForeignDep {
    /// Extern key used in paths (`serenity` in `serenity::client::...`).
    pub key: String,
    /// Package name for lock lookups and renamed edges.
    pub package: String,
    /// Locked crate version.
    pub version: String,
    /// Sorted requested feature set.
    pub features: Vec<String>,
    /// Extracted owner source in the global cache.
    pub src: std::path::PathBuf,
}

/// Collect every foreign owner referenced anywhere in the discovered API.
fn foreign_owners(discovered: &DiscoveredApi) -> std::collections::BTreeSet<String> {
    fn param_owner(kind: &ParamKind, owners: &mut std::collections::BTreeSet<String>) {
        match kind {
            ParamKind::Object(obj) => owners.extend(obj.owner.clone()),
            ParamKind::Array(elem)
            | ParamKind::OptionArray(elem)
            | ParamKind::FixedArray(elem, _) => {
                if let ArrayElemKind::Object(obj) = elem {
                    owners.extend(obj.owner.clone());
                }
            }
            _ => {}
        }
    }
    fn ret_owner(ret: &RetKind, owners: &mut std::collections::BTreeSet<String>) {
        match ret {
            RetKind::Object(obj)
            | RetKind::OptionObject(obj)
            | RetKind::ObjectClone(obj)
            | RetKind::ResultObject(obj) => owners.extend(obj.owner.clone()),
            RetKind::Array(elem) | RetKind::OptionArray(elem) | RetKind::ResultArray(elem) => {
                if let ArrayElemKind::Object(obj) = elem {
                    owners.extend(obj.owner.clone());
                }
            }
            RetKind::ResultObjectTuple(objects) => {
                owners.extend(objects.iter().filter_map(|object| object.owner.clone()));
            }
            _ => {}
        }
    }
    let mut owners = std::collections::BTreeSet::new();
    for func in &discovered.functions {
        for kind in &func.params {
            param_owner(kind, &mut owners);
        }
        ret_owner(&func.ret, &mut owners);
    }
    for callback in &discovered.callbacks {
        for method in &callback.methods {
            for kind in &method.params {
                param_owner(kind, &mut owners);
            }
        }
    }
    owners
}

/// The foreign owners referenced by one function (params and return).
fn function_owners(func: &Func) -> std::collections::BTreeSet<String> {
    let mut owners = std::collections::BTreeSet::new();
    for kind in &func.params {
        match kind {
            ParamKind::Object(obj) => owners.extend(obj.owner.clone()),
            ParamKind::Array(elem)
            | ParamKind::OptionArray(elem)
            | ParamKind::FixedArray(elem, _) => {
                if let ArrayElemKind::Object(obj) = elem {
                    owners.extend(obj.owner.clone());
                }
            }
            _ => {}
        }
    }
    match &func.ret {
        RetKind::Object(obj)
        | RetKind::OptionObject(obj)
        | RetKind::ObjectClone(obj)
        | RetKind::ResultObject(obj) => owners.extend(obj.owner.clone()),
        RetKind::Array(elem) | RetKind::OptionArray(elem) | RetKind::ResultArray(elem) => {
            if let ArrayElemKind::Object(obj) = elem {
                owners.extend(obj.owner.clone());
            }
        }
        RetKind::ResultObjectTuple(objects) => {
            owners.extend(objects.iter().filter_map(|object| object.owner.clone()));
        }
        _ => {}
    }
    owners
}

/// Resolve every foreign owner to a pinned dependency: the locked version
/// from `ntsc.lock` and the requested feature set from `[rust-deps]`,
/// with the owner's source materialized in the cache. Owners that are
/// not locked project dependencies cannot be named soundly (no version
/// pins the tag or the dependency edge), so functions and callbacks
/// using them are demoted to `unsupported` with an actionable reason
/// instead of emitting code that cannot compile.
fn resolve_foreign_deps(
    cache: &CacheRoot,
    cargo_registry_dir: &Path,
    project_dir: &Path,
    analyzed_deps: &[(String, String)],
    discovered: &mut DiscoveredApi,
) -> CargoResult<std::collections::BTreeMap<String, ForeignDep>> {
    let needed = foreign_owners(discovered);
    if needed.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    let manifest = crate::manifest::Manifest::read(&project_dir.join("neutron.toml")).ok();
    let lock = manifest
        .as_ref()
        .and_then(|_| crate::manifest::read_lock(project_dir).ok())
        .flatten();
    let mut resolved = std::collections::BTreeMap::new();
    let mut missing = std::collections::BTreeSet::new();
    for owner in needed {
        let package = analyzed_deps
            .iter()
            .find(|(key, _)| key == &owner)
            .map(|(_, package)| package.clone())
            .unwrap_or_else(|| owner.clone());
        let version: Option<String> = lock.as_ref().and_then(|lock| {
            lock.version_for(&package)
                .or_else(|| lock.version_for(&owner))
                .map(str::to_owned)
        });
        let spec = manifest.as_ref().and_then(|manifest| {
            manifest
                .rust_deps
                .get(&package)
                .or_else(|| manifest.rust_deps.get(&owner))
        });
        let (Some(version), Some(spec)) = (version, spec) else {
            missing.insert(owner);
            continue;
        };
        let src = match crate::fetch::fetch_source(cache, &package, &version, cargo_registry_dir) {
            Ok(src) => src,
            Err(_) => {
                missing.insert(owner);
                continue;
            }
        };
        let mut features: Vec<String> = feature_set_for_spec(&src, spec).into_iter().collect();
        features.sort();
        resolved.insert(
            owner.clone(),
            ForeignDep {
                key: owner,
                package,
                version,
                features,
                src,
            },
        );
    }
    if !missing.is_empty() {
        demote_unresolved_foreign(discovered, &missing);
    }
    Ok(resolved)
}

/// Demote every function and callback touching an unresolvable foreign
/// owner: functions become `unsupported` entries (keeping the manifest
/// total), and callbacks are removed together with the functions that
/// would have used them.
fn demote_unresolved_foreign(
    discovered: &mut DiscoveredApi,
    missing: &std::collections::BTreeSet<String>,
) {
    let mut kept_functions = Vec::new();
    for func in std::mem::take(&mut discovered.functions) {
        let unresolvable: Vec<String> = function_owners(&func)
            .intersection(missing)
            .cloned()
            .collect();
        if unresolvable.is_empty() {
            kept_functions.push(func);
            continue;
        }
        discovered.unsupported.push(UnsupportedApi {
            name: func.name.clone(),
            path: func.call_path.clone(),
            status: crate::metadata::ApiStatus::Unsupported,
            reason: format!(
                "foreign type owner `{}` is not a locked [rust-deps] entry; add it to bridge this function",
                unresolvable.join("`, `"),
            ),
            signature: String::new(),
        });
    }
    discovered.functions = kept_functions;
    let mut kept_callbacks = Vec::new();
    let mut removed_traits = Vec::new();
    for callback in std::mem::take(&mut discovered.callbacks) {
        let uses_missing = callback.methods.iter().any(|method| {
            method.params.iter().any(|kind| match kind {
                ParamKind::Object(obj) => obj
                    .owner
                    .as_ref()
                    .is_some_and(|owner| missing.contains(owner)),
                ParamKind::Array(elem)
                | ParamKind::OptionArray(elem)
                | ParamKind::FixedArray(elem, _) => {
                    matches!(elem, ArrayElemKind::Object(obj) if obj.owner.as_ref().is_some_and(|owner| missing.contains(owner)))
                }
                _ => false,
            })
        });
        if uses_missing {
            removed_traits.push(callback.trait_path.clone());
        } else {
            kept_callbacks.push(callback);
        }
    }
    discovered.callbacks = kept_callbacks;
    if !removed_traits.is_empty() {
        let mut kept = Vec::new();
        for func in std::mem::take(&mut discovered.functions) {
            let uses_removed = func.params.iter().any(|kind| match kind {
                ParamKind::Callback(trait_path) => {
                    removed_traits.iter().any(|removed| removed == trait_path)
                }
                _ => false,
            });
            if uses_removed {
                discovered.unsupported.push(UnsupportedApi {
                    name: func.name.clone(),
                    path: func.call_path.clone(),
                    status: crate::metadata::ApiStatus::Unsupported,
                    reason: "callback trait is unbridgeable without its foreign owner".into(),
                    signature: String::new(),
                });
            } else {
                kept.push(func);
            }
        }
        discovered.functions = kept;
    }
    discovered
        .unsupported
        .sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
}
/// Write `content` to `path` only when it differs from what is there.
/// Rewriting identical bytes would bump the file's mtime, which both the
/// project's archive-freshness check and cargo's own fingerprints read —
/// an unchanged adaptor would rebuild on every `ntsc build`.
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path)
        .map(|existing| existing == content)
        .unwrap_or(false)
    {
        return Ok(());
    }
    std::fs::write(path, content)
}

/// The adaptor's `Cargo.toml`, depending on the cached source by path.
/// Async bridges additionally need the Tokio runtime their futures run on.
fn adaptor_manifest(
    name: &str,
    version: &str,
    dep_src: &Path,
    async_needed: bool,
    dependency_spec: Option<&crate::manifest::DependencySpec>,
    foreign: &std::collections::BTreeMap<String, ForeignDep>,
) -> String {
    let tokio_dep = if async_needed {
        "\n\
         # Awaited bridges drive the dependency's futures on this runtime.\n\
         tokio = { version = \"1\", features = [\"rt\", \"rt-multi-thread\"] }\n\
         tracing = \"0.1\"\n"
    } else {
        ""
    };
    format!(
        "# Generated by ntsc-cargo. Adaptor for the `{name}` crate.\n\
         [package]\n\
         name = \"ntsc-adaptor-{name}\"\n\
         version = \"0.0.0\"\n\
         edition = \"2021\"\n\
         \n\
         [lib]\n\
         crate-type = [\"staticlib\"]\n\
         \n\
         [dependencies]\n\
         {name} = {dependency_line}{tokio_dep}{foreign_edges}\n\
         {foreign_patches}\n\
         \n\
         # Record the source version the adaptor was generated against.\n\
         [package.metadata.ntsc]\n\
         crate-version = \"{version}\"\n",
        dependency_line = dependency_line(dependency_spec, dep_src),
        foreign_edges = foreign_dependency_edges(foreign),
        foreign_patches = foreign_dependency_patches(foreign),
    )
}

fn dependency_line(
    dependency_spec: Option<&crate::manifest::DependencySpec>,
    dep_src: &Path,
) -> String {
    let mut table = match dependency_spec {
        Some(crate::manifest::DependencySpec::Table(table)) => table.clone(),
        Some(crate::manifest::DependencySpec::Version(version)) => {
            let mut table = toml::map::Map::new();
            table.insert("version".into(), toml::Value::String(version.clone()));
            table
        }
        Some(crate::manifest::DependencySpec::Any) | None => toml::map::Map::new(),
    };
    table.insert(
        "path".into(),
        toml::Value::String(dep_src.display().to_string()),
    );
    toml::Value::Table(table).to_string()
}

/// Pinned dependency edges for every foreign owner named by the
/// adaptor's bridges: the exact locked version with the owner's
/// requested feature set, path-pinned to the cached source, so the
/// foreign type paths in bridge signatures resolve to the same build
/// the tags were computed from.
fn foreign_dependency_edges(foreign: &std::collections::BTreeMap<String, ForeignDep>) -> String {
    let mut edges = String::new();
    for dep in foreign.values() {
        let mut table = toml::map::Map::new();
        table.insert(
            "version".into(),
            toml::Value::String(format!("={}", dep.version)),
        );
        table.insert("default-features".into(), toml::Value::Boolean(false));
        table.insert(
            "features".into(),
            toml::Value::Array(
                dep.features
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
        table.insert(
            "path".into(),
            toml::Value::String(dep.src.display().to_string()),
        );
        if dep.package != dep.key {
            table.insert("package".into(), toml::Value::String(dep.package.clone()));
        }
        edges.push_str("# Shared-space owner pinned to the locked build.\n");
        edges.push_str(&format!("{} = {}\n", dep.key, toml::Value::Table(table)));
    }
    edges
}

/// Force crates.io dependencies of the analyzed crate to use the same cached
/// package source as the foreign bridge edge. Without this, Cargo can build a
/// second instance of a crate at the same version, making its types distinct
/// from the types appearing in the analyzed crate's signatures.
fn foreign_dependency_patches(foreign: &std::collections::BTreeMap<String, ForeignDep>) -> String {
    if foreign.is_empty() {
        return String::new();
    }
    let mut patches = String::from("[patch.crates-io]\n");
    for dep in foreign.values() {
        patches.push_str(&format!(
            "{} = {{ path = \"{}\", version = \"={}\" }}\n",
            dep.package,
            dep.src.display(),
            dep.version
        ));
    }
    patches
}

/// Per-emit context for bridge code generation: the dependency crate
/// being adapted plus the locked version and sorted requested features
/// that identify its build. Object type tags bake all three in
/// (`owner@version+features/path`), so the same path built from a
/// different version or feature set never resolves — layout may differ —
/// while every adaptor holding the same build agrees byte for byte.
struct EmitCtx<'a> {
    crate_escaped: &'a str,
    owner: &'a str,
    version: &'a str,
    features: &'a str,
    /// Resolved foreign owner crates, keyed by extern key. Empty in
    /// unit tests, which never classify foreign types.
    foreign: &'a std::collections::BTreeMap<String, ForeignDep>,
}

/// The shared-space type tag for one object type: local types use this
/// emit's owner, version, and features; foreign types use their owner's
/// locked version and feature set, so both sides compute byte-identical
/// tags. The wrapper suffix keeps `T` and `Arc<Mutex<T>>` entries
/// distinct: sharing a tag would reinterpret one layout as the other.
/// An owner missing from the emit's foreign table can only happen for
/// unresolvable imports, which never reach emission; the local tag
/// fallback keeps the emitter total.
fn object_tag(ctx: &EmitCtx<'_>, obj: &ObjectType) -> String {
    let suffix = obj.wrapper.tag_suffix();
    match &obj.owner {
        None => format!(
            "{}@{}+{}/{}{}",
            ctx.owner, ctx.version, ctx.features, obj.path, suffix
        ),
        Some(owner) => match ctx.foreign.get(owner) {
            Some(dep) => {
                format!(
                    "{owner}@{}+{}/{}{}",
                    dep.version,
                    dep.features.join(","),
                    obj.path,
                    suffix
                )
            }
            None => format!(
                "{}@{}+{}/{}{}",
                ctx.owner, ctx.version, ctx.features, obj.path, suffix
            ),
        },
    }
}

/// The Rust type expression naming one object type in the generated
/// adaptor: local types through the dependency crate root, foreign types
/// through the owning dependency (whose edge the emitter adds to the
/// adaptor manifest).
fn object_qualified(ctx: &EmitCtx<'_>, obj: &ObjectType) -> String {
    match &obj.owner {
        None => format!("{}::{}", ctx.crate_escaped, obj.path),
        Some(owner) => format!("{}::{}", owner.replace('-', "_"), obj.path),
    }
}

/// The full Rust type of one object value, including its `Arc` wrapping.
/// Borrowed receivers spell the plain base (the bridge dereferences or
/// locks either handle kind); takes, puts, and by-value expressions spell
/// the wrapped allocation the shared space actually holds.
fn object_wrapped_expr(ctx: &EmitCtx<'_>, obj: &ObjectType) -> String {
    let base = object_qualified(ctx, obj);
    match obj.wrapper {
        ObjectWrapper::Plain => base,
        ObjectWrapper::Arc => format!("std::sync::Arc<{base}>"),
        ObjectWrapper::TokioMutex => format!("std::sync::Arc<tokio::sync::Mutex<{base}>>"),
        ObjectWrapper::TokioRwLock => format!("std::sync::Arc<tokio::sync::RwLock<{base}>>"),
        ObjectWrapper::StdMutex => format!("std::sync::Arc<std::sync::Mutex<{base}>>"),
        ObjectWrapper::StdRwLock => format!("std::sync::Arc<std::sync::RwLock<{base}>>"),
        ObjectWrapper::ParkingMutex => format!("std::sync::Arc<parking_lot::Mutex<{base}>>"),
        ObjectWrapper::ParkingRwLock => format!("std::sync::Arc<parking_lot::RwLock<{base}>>"),
    }
}

/// Build the adaptor `src/lib.rs` from the discovered marshalable API.
///
/// The file declares the runtime string helpers as `extern "C"`, defines
/// private safe wrappers around them, then one `#[no_mangle]` bridge
/// function per exposed dependency function. Each bridge unmarshals its
/// `i64` arguments, calls the real dependency function, and marshals the
/// result back to an `i64`.
///
/// `version` is the locked crate version and `features` the sorted,
/// comma-joined requested feature set: both feed object type tags (see
/// [`EmitCtx`]).
fn adaptor_lib(
    name: &str,
    version: &str,
    features: &str,
    foreign: &std::collections::BTreeMap<String, ForeignDep>,
    functions: &[Func],
    callbacks: &[CallbackBridge],
) -> String {
    let crate_escaped = name.replace('-', "_");
    let ctx = EmitCtx {
        crate_escaped: &crate_escaped,
        owner: name,
        version,
        features,
        foreign,
    };
    let objects_needed =
        functions.iter().any(uses_objects) || callbacks.iter().any(callback_uses_objects);
    // Awaited bridges run the dependency's future on a real Tokio runtime:
    // serenity-style futures touch the reactor (DNS, timers), which the
    // park-based block-on executor cannot drive.
    let async_needed = functions.iter().any(|func| func.is_async);

    let externs = "\
extern \"C\" {
    fn ntsc_string_len(s: i64) -> i64;
    fn ntsc_string_to_bytes(handle: i64, out_buf: *mut u8, max_len: i64) -> i64;
    fn ntsc_string_from_bytes(ptr: *const u8, len: i64) -> i64;
    fn ntsc_array_new_typed(elem_size: i64, initial_capacity: i64, string_elements: i8) -> i64;
    fn ntsc_array_len(id: i64) -> i64;
    fn ntsc_array_get(id: i64, index: i64) -> i64;
    fn ntsc_array_push(id: i64, elem: i64) -> i64;
    fn ntsc_array_is_array(id: i64) -> i8;
    fn ntsc_string_clone(s: i64) -> i64;
    fn ntsc_string_assign_bytes(handle: i64, ptr: *const u8, len: i64) -> i8;
    fn ntsc_string_drop(s: i64);
    fn ntsc_throw(msg: i64) -> i64;
    fn ntsc_exception_pending() -> i8;
    fn ntsc_exception_take_message() -> i64;
    fn ntsc_print_f64(val: f64);
    fn ntsc_async_op_new(work: extern \"C\" fn(i64) -> i64, arg: i64) -> i64;
    fn ntsc_worker_will_block();
    fn ntsc_worker_did_unblock();
    fn ntsc_shared_object_tag(ptr: *const u8, len: i64) -> u64;
    fn ntsc_shared_object_tag_text(id: u64, out: *mut u8, cap: i64) -> i64;
    fn ntsc_shared_object_tag_of(handle: i64) -> u64;
    fn ntsc_shared_object_put(ptr: *mut std::ffi::c_void, drop_fn: Option<extern \"C\" fn(*mut std::ffi::c_void)>, tag: u64) -> i64;
    fn ntsc_shared_object_borrow(handle: i64, tag: u64) -> *mut std::ffi::c_void;
    fn ntsc_shared_object_borrow_mut(handle: i64, tag: u64) -> *mut std::ffi::c_void;
    fn ntsc_shared_object_release_mut(handle: i64);
    fn ntsc_shared_object_take(handle: i64, tag: u64) -> *mut std::ffi::c_void;
    fn ntsc_shared_object_release(handle: i64);
    fn ntsc_shared_object_retain(handle: i64) -> i64;
    fn malloc(size: i64) -> *mut u8;
}";

    let helpers = "\
#[allow(unsafe_code)]
unsafe fn _ntsc_handle_to_string(handle: i64) -> String {
    if handle == 0 { return String::new(); }
    let len = ntsc_string_len(handle);
    if len <= 0 { return String::new(); }
    // Bulk copy under a single registry lock. The previous per-byte loop
    // issued one FFI call and one lock acquisition per byte, so a 20 KiB
    // JSON body cost 20k calls; this is one call either way. The length
    // can race a concurrent mutation, so truncate to what was written.
    let mut buf = vec![0u8; len as usize];
    let got = ntsc_string_to_bytes(handle, buf.as_mut_ptr(), len);
    if got <= 0 { return String::new(); }
    buf.truncate(got as usize);
    String::from_utf8_lossy(&buf).into_owned()
}

#[allow(unsafe_code)]
unsafe fn _ntsc_handle_to_bytes(handle: i64) -> Vec<u8> {
    if handle == 0 { return Vec::new(); }
    let len = ntsc_string_len(handle);
    if len <= 0 { return Vec::new(); }
    let mut buf = vec![0u8; len as usize];
    let got = ntsc_string_to_bytes(handle, buf.as_mut_ptr(), len);
    if got <= 0 { return Vec::new(); }
    buf.truncate(got as usize);
    buf
}

#[allow(unsafe_code)]
unsafe fn _ntsc_str_to_handle(s: &str) -> i64 {
    ntsc_string_from_bytes(s.as_ptr(), s.len() as i64)
}

#[allow(unsafe_code)]
unsafe fn _ntsc_bytes_to_handle(bytes: &[u8]) -> i64 {
    ntsc_string_from_bytes(bytes.as_ptr(), bytes.len() as i64)
}

#[allow(unsafe_code)]
unsafe fn _ntsc_assign_bytes(handle: i64, ptr: *const u8, len: i64) {
    ntsc_string_assign_bytes(handle, ptr, len);
}

#[allow(unsafe_code)]
unsafe fn _ntsc_string_to_handle(s: String) -> i64 {
    _ntsc_str_to_handle(&s)
}

#[allow(unsafe_code)]
unsafe fn _ntsc_throw_text(text: &str) -> i64 {
    let handle = _ntsc_str_to_handle(text);
    ntsc_throw(handle)
}

#[allow(unsafe_code)]
unsafe fn _ntsc_option_scalar<T: Copy>(handle: i64) -> Option<T> {
    if handle == 0 { None } else { Some(std::ptr::read(handle as *const T)) }
}

#[allow(unsafe_code)]
unsafe fn _ntsc_option_cell<T: Copy>(value: Option<T>) -> i64 {
    let Some(value) = value else { return 0; };
    let ptr = malloc(std::mem::size_of::<T>().max(1) as i64);
    if ptr.is_null() { return 0; }
    std::ptr::write(ptr.cast::<T>(), value);
    ptr as i64
}

#[allow(unsafe_code)]
unsafe fn _ntsc_option_float_cell<T: Copy + Into<f64>>(value: Option<T>) -> i64 {
    let Some(value) = value else { return 0; };
    let ptr = malloc(8);
    if ptr.is_null() { return 0; }
    std::ptr::write(ptr.cast::<f64>(), value.into());
    ptr as i64
}

fn _ntsc_async_args() -> &'static std::sync::Mutex<std::collections::HashMap<i64, Vec<i64>>> {
    static ARGS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<i64, Vec<i64>>>> = std::sync::OnceLock::new();
    ARGS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn _ntsc_async_args_put(args: Vec<i64>) -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    _ntsc_async_args().lock().unwrap_or_else(|p| p.into_inner()).insert(id, args);
    id
}

fn _ntsc_async_args_take(id: i64) -> Vec<i64> {
    _ntsc_async_args().lock().unwrap_or_else(|p| p.into_inner()).remove(&id).unwrap_or_default()
}

fn _ntsc_block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Wake, Waker};

    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) { self.0.unpark(); }
        fn wake_by_ref(self: &Arc<Self>) { self.0.unpark(); }
    }

    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

#[allow(unsafe_code)]
unsafe fn _ntsc_panic_to_handle(payload: Box<dyn std::any::Any + Send>) -> i64 {
    let msg = if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        \"non-string payload\".to_string()
    };
    _ntsc_throw_text(&format!(\"rust panic: {msg}\"))
}

#[allow(unsafe_code)]
struct _NtscBlockGuard;

#[allow(unsafe_code)]
impl Drop for _NtscBlockGuard {
    fn drop(&mut self) {
        unsafe { ntsc_worker_did_unblock(); }
    }
}

#[allow(unsafe_code)]
fn _ntsc_block_guard() -> _NtscBlockGuard {
    unsafe { ntsc_worker_will_block(); }
    _NtscBlockGuard
}";

    let tokio_helpers = if async_needed {
        "\
fn _ntsc_tokio_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect(\"adaptor tokio runtime must build\")
    })
}

fn _ntsc_tokio_block_on<F: std::future::Future>(future: F) -> F::Output {
    _ntsc_tokio_runtime().block_on(future)
}

"
    } else {
        ""
    };

    // Method-syntax trait calls need their trait in scope. Absolute
    // std-library paths import verbatim; local traits import through the
    // dependency crate. Callback overrides additionally import the
    // concrete types of their canonical signatures.
    let mut import_paths: std::collections::HashSet<String> = functions
        .iter()
        .filter_map(|func| func.trait_path.as_ref())
        .cloned()
        .collect();
    for callback in callbacks {
        for method in &callback.methods {
            import_paths.extend(method.imports.iter().cloned());
        }
    }
    let mut import_paths: Vec<String> = import_paths.into_iter().collect();
    import_paths.sort();
    let objects = if objects_needed {
        "
/// One owned object plus its live Neutron-side reference count is held in
/// the runtime's process-wide shared space, so a handle created by one
/// adaptor resolves in any other. The producing call transfers a single
/// reference; every share retains. Takes-bridges move the value out
/// (failing while a borrow is outstanding); outstanding shares of a taken
/// handle fail lookups loudly instead of reading freed memory.
#[allow(unsafe_code)]
unsafe fn _ntsc_tag(text: &str) -> u64 {
    ntsc_shared_object_tag(text.as_ptr(), text.len() as i64)
}

#[allow(unsafe_code)]
unsafe fn _ntsc_shared_put<T: Send + 'static>(value: T, tag_text: &str) -> i64 {
    extern \"C\" fn drop_box<U>(ptr: *mut std::ffi::c_void) {
        unsafe { drop(Box::from_raw(ptr as *mut U)); }
    }
    let ptr = Box::into_raw(Box::new(value)) as *mut std::ffi::c_void;
    ntsc_shared_object_put(ptr, Some(drop_box::<T>), _ntsc_tag(tag_text))
}

#[allow(unsafe_code)]
unsafe fn _ntsc_shared_borrow<T: Send + 'static>(handle: i64, tag_text: &str) -> Option<*const T> {
    let ptr = ntsc_shared_object_borrow(handle, _ntsc_tag(tag_text)) as *const T;
    if ptr.is_null() { None } else { Some(ptr) }
}

#[allow(unsafe_code)]
unsafe fn _ntsc_shared_take<T: Send + 'static>(handle: i64, tag_text: &str) -> Option<T> {
    let ptr = ntsc_shared_object_take(handle, _ntsc_tag(tag_text)) as *mut T;
    if ptr.is_null() { None } else { Some(*Box::from_raw(ptr)) }
}

enum _NtscObjectBorrow {
    Shared(i64),
    Exclusive(i64),
}

#[allow(unsafe_code)]
impl Drop for _NtscObjectBorrow {
    fn drop(&mut self) {
        unsafe {
            match *self {
                Self::Shared(handle) => ntsc_shared_object_release(handle),
                Self::Exclusive(handle) => ntsc_shared_object_release_mut(handle),
            }
        }
    }
}

/// Borrow a plain object handle as `&T`, whether the entry holds `T`
/// itself or an `Arc<T>` aliasing the same allocation (dereferenced).
/// Exactly one retain is outstanding either way, released like any
/// borrow. The `Arc` leg casts through the raw extern call rather than
/// the generic helper, so it needs no `Sync` bound the plain leg does
/// not already have: the tag check guarantees the layout.
/// Mutex-flavored entries never match: methods through an async mutex
/// need flavor-specific locking the bridge does not generate, so they
/// fail closed with a tag mismatch naming both sides.
#[allow(unsafe_code)]
unsafe fn _ntsc_shared_borrow_or_arc<T: Send + 'static>(
    handle: i64,
    plain_tag: &str,
    arc_tag: &str,
) -> Option<*const T> {
    if let Some(ptr) = _ntsc_shared_borrow::<T>(handle, plain_tag) {
        return Some(ptr);
    }
    let raw = ntsc_shared_object_borrow(handle, _ntsc_tag(arc_tag));
    if raw.is_null() {
        return None;
    }
    Some((&**(raw as *const std::sync::Arc<T>)) as *const T)
}

/// Throw for a handle that is null, unknown, busy, or of an unexpected
/// shared type, naming the expected tag and the handle's actual tag when
/// it resolves to a different entry.
#[allow(unsafe_code)]
unsafe fn _ntsc_shared_mismatch(handle: i64, tag_text: &str, qualified: &str) -> i64 {
    let actual = ntsc_shared_object_tag_of(handle);
    if actual == 0 {
        return _ntsc_throw_text(&format!(\"invalid handle for {qualified}\"));
    }
    let mut buf = [0u8; 256];
    let len = ntsc_shared_object_tag_text(actual, buf.as_mut_ptr(), buf.len() as i64);
    let actual_text = String::from_utf8_lossy(&buf[..len.max(0) as usize]);
    _ntsc_throw_text(&format!(\"invalid handle for {qualified}: expected {tag_text}, found {actual_text}\"))
}
"
    } else {
        ""
    };

    // Routable retain/release for the Neutron compiler's drop glue. The
    // symbols carry the crate name (the compiler derives them from the
    // tracked variable's crate) and forward to the process-wide shared
    // space, so shares and releases work across adaptor boundaries. Both
    // are null- and unknown-handle-safe no-ops.
    let ownership = if objects_needed {
        format!(
            "
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn ntsc_adaptor_{crate_escaped}_object_retain(handle: i64) -> i64 {{
    unsafe {{ ntsc_shared_object_retain(handle) }}
}}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn ntsc_adaptor_{crate_escaped}_object_release(handle: i64) {{
    unsafe {{ ntsc_shared_object_release(handle) }};
}}
"
        )
    } else {
        String::new()
    };

    let trait_uses: Vec<String> = import_paths
        .into_iter()
        .map(|path| {
            let import = if let Some(first) = path.split("::").next()
                && matches!(first, "std" | "core" | "alloc")
            {
                path.clone()
            } else {
                format!("{crate_escaped}::{path}")
            };
            format!("use {import};")
        })
        .collect();
    let trait_uses = if trait_uses.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", trait_uses.join("\n"))
    };

    let mut bridges = Vec::new();
    for func in functions {
        if func.is_setter {
            bridges.push(field_setter_bridge_fn(&ctx, func));
        } else if func.is_field {
            bridges.push(field_bridge_fn(&ctx, func));
        } else if func.is_async {
            bridges.push(async_bridge_fn(&ctx, func));
        } else {
            bridges.push(bridge_fn(&ctx, func));
            if !func.name.ends_with("_async") {
                bridges.push(sync_async_bridge_fn(&ctx, func));
            }
        }
    }
    if !callbacks.is_empty() {
        bridges.push(emit_callback_bridges(&ctx, callbacks));
    }

    format!(
        "#![deny(unsafe_code)]\n//! Generated by ntsc-cargo. Bridges the `{name}` crate to neutron.\n//!\n//! The generated names (bridge fns like `Type_method`, trait-impl\n//! structs) and the dependency's own deprecations are intentionally\n//! outside this crate's style rules; the build output stays quiet.\n#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, deprecated, unused_mut, unused_variables, unused_imports, unused_qualifications, dead_code)]\n\n{externs}\n\n{helpers}\n\n{tokio_helpers}\n\n{objects}\n{ownership}\n{trait_uses}{}\n",
        bridges.join("\n\n")
    )
}

/// Whether a callback bridge needs the adaptor's object registry: any
/// overridden method taking a framework-owned value stores it for Neutron.
fn callback_uses_objects(callback: &CallbackBridge) -> bool {
    callback.methods.iter().any(|method| {
        method
            .params
            .iter()
            .any(|kind| matches!(kind, ParamKind::Object(_)))
    })
}

/// Crate-rooted import paths for the object types of one callback method's
/// canonical signature.
fn callback_imports(params: &[ParamKind]) -> Vec<String> {
    let mut imports = Vec::new();
    for kind in params {
        if let ParamKind::Object(obj) = kind
            && !imports.iter().any(|existing| existing == &obj.path)
        {
            imports.push(obj.path.clone());
        }
    }
    imports
}

/// The canonical Rust type of one callback parameter in an emitted
/// override signature: owned values by value, borrows as references, and
/// bridged structs through the dependency crate root.
fn callback_param_ty(ctx: &EmitCtx<'_>, kind: &ParamKind, rf: ParamRef) -> String {
    let owned = match kind {
        ParamKind::Str => "String".to_string(),
        ParamKind::Bytes => "Vec<u8>".to_string(),
        ParamKind::CowStr => "std::borrow::Cow<'_, str>".to_string(),
        ParamKind::CowBytes => "std::borrow::Cow<'_, [u8]>".to_string(),
        ParamKind::Array(elem) => format!("Vec<{}>", array_elem_rs_qualified(ctx, elem)),
        ParamKind::FixedArray(elem, n) => {
            format!("[{}; {}]", array_elem_rs_qualified(ctx, elem), n)
        }
        ParamKind::OptionArray(elem) => {
            format!("Option<Vec<{}>>", array_elem_rs_qualified(ctx, elem))
        }
        ParamKind::OptionInt(ty) => format!("Option<{ty}>"),
        ParamKind::OptionBool => "Option<bool>".to_string(),
        ParamKind::OptionFloat(ty) => format!("Option<{ty}>"),
        ParamKind::OptionStr => "Option<String>".to_string(),
        ParamKind::OptionStrRef => "Option<&str>".to_string(),
        ParamKind::OptionObject(obj) => format!("Option<{}>", object_wrapped_expr(ctx, obj)),
        ParamKind::Int(ty) | ParamKind::Float(ty) => ty.clone(),
        ParamKind::Duration => "std::time::Duration".to_string(),
        ParamKind::Bool => "bool".to_string(),
        ParamKind::Object(obj) => object_wrapped_expr(ctx, obj),
        // Nested callbacks never reach the emitter: the inventory skips
        // methods with callback-typed parameters.
        ParamKind::Callback(_) => "i64".to_string(),
    };
    match rf {
        ParamRef::Owned => owned,
        ParamRef::Ref => format!("&{owned}"),
        ParamRef::MutRef => format!("&mut {owned}"),
    }
}

/// Marshal one received callback argument into the `i64` stored in the
/// event queue: strings and bytes copy out into runtime handles, scalars
/// pass as raw bits, and owned framework values move into the shared
/// object space for Neutron to reference.
fn callback_marshal_arg(ctx: &EmitCtx<'_>, index: usize, kind: &ParamKind, rf: ParamRef) -> String {
    let arg = format!("a{index}");
    match (kind, rf) {
        (ParamKind::Str, ParamRef::Owned) => format!("unsafe {{ _ntsc_str_to_handle(&{arg}) }}"),
        (ParamKind::Str, ParamRef::Ref) => format!("unsafe {{ _ntsc_str_to_handle({arg}) }}"),
        (ParamKind::Str, ParamRef::MutRef) => format!("unsafe {{ _ntsc_str_to_handle(&*{arg}) }}"),
        (ParamKind::Bytes, ParamRef::Owned) => {
            format!("unsafe {{ _ntsc_bytes_to_handle(&{arg}) }}")
        }
        (ParamKind::Bytes, _) => format!("unsafe {{ _ntsc_bytes_to_handle(&*{arg}) }}"),
        (ParamKind::CowStr, _) => format!("unsafe {{ _ntsc_str_to_handle({arg}.as_ref()) }}"),
        (ParamKind::CowBytes, _) => format!("unsafe {{ _ntsc_bytes_to_handle({arg}.as_ref()) }}"),
        (ParamKind::Int(_), ParamRef::Owned) => format!("{arg} as i64"),
        (ParamKind::Int(_), _) => format!("(*{arg}) as i64"),
        (ParamKind::Bool, ParamRef::Owned) => format!("i64::from({arg})"),
        (ParamKind::Bool, _) => format!("i64::from(*{arg})"),
        (ParamKind::Float(_), ParamRef::Owned) => format!("({arg} as f64).to_bits() as i64"),
        (ParamKind::Duration, ParamRef::Owned) => {
            format!("std::time::Duration::from_millis({arg} as u64)")
        }
        (ParamKind::Duration, _) => {
            format!("std::time::Duration::from_millis((*{arg}) as u64)")
        }
        (ParamKind::Float(_), _) => format!("((*{arg}) as f64).to_bits() as i64"),
        // Borrowed framework values cannot be stored: the inventory only
        // admits owned object parameters, so this arm is unreachable.
        (ParamKind::Object(path), ParamRef::Owned) => {
            let tag = object_tag(ctx, path);
            format!("unsafe {{ _ntsc_shared_put({arg}, \"{tag}\") }}")
        }
        (ParamKind::Object(path), _) => {
            let tag = object_tag(ctx, path);
            format!("unsafe {{ _ntsc_shared_put({arg}.clone(), \"{tag}\") }}")
        }
        (ParamKind::Array(elem), _) => callback_array_marshal(ctx, &arg, elem),
        (ParamKind::FixedArray(elem, _), _) => callback_array_marshal(ctx, &arg, elem),
        (ParamKind::OptionArray(elem), _) => format!(
            "match {arg} {{ Some(__v) => {}, None => 0 }}",
            callback_array_marshal(ctx, "__v", elem)
        ),
        (ParamKind::OptionInt(_), _) | (ParamKind::OptionBool, _) => {
            format!("unsafe {{ _ntsc_option_cell({arg}) }}")
        }
        (ParamKind::OptionFloat(_), _) => format!("unsafe {{ _ntsc_option_float_cell({arg}) }}"),
        (ParamKind::OptionStr, ParamRef::Owned) => {
            format!("match {arg} {{ None => 0, Some(s) => unsafe {{ _ntsc_str_to_handle(&s) }} }}")
        }
        (ParamKind::OptionStr, _) => {
            format!("match {arg} {{ None => 0, Some(s) => unsafe {{ _ntsc_str_to_handle(s) }} }}")
        }
        (ParamKind::OptionStrRef, _) => {
            format!("match {arg} {{ None => 0, Some(s) => unsafe {{ _ntsc_str_to_handle(s) }} }}")
        }
        (ParamKind::OptionObject(obj), _) => {
            let tag = object_tag(ctx, obj);
            format!(
                "match {arg} {{ None => 0, Some(v) => unsafe {{ _ntsc_shared_put(v, \"{tag}\") }} }}"
            )
        }
        (ParamKind::Callback(_), _) => "0".to_string(),
    }
}

/// Marshal a received `Vec<T>` (or slice) into a fresh Neutron array
/// handle for the event queue. `source` is any expression with `.len()`
/// and `.iter()` (`Vec<T>`, `&[T]`, `&Vec<T>`); strings copy out through
/// fresh handles that the array adopts, scalars push by value.
fn callback_array_marshal(ctx: &EmitCtx<'_>, source: &str, elem: &ArrayElemKind) -> String {
    let flag = i8::from(matches!(elem, ArrayElemKind::Str));
    format!(
        "{{ let __arr = unsafe {{ ntsc_array_new_typed(8, {source}.len() as i64, {flag}) }}; \
         for __x in {source}.iter() {{ {} }} __arr }}",
        array_push_stmt(ctx, "__arr", "__x", elem)
    )
}

/// One `let a{i}: Vec<T> = ...;` statement reading an array handle
/// element-wise. A non-null handle of the wrong kind throws instead of
/// decoding garbage; the null handle reads as empty, matching the string
/// bridge's leniency. An `array_get` failure (only possible under a
/// concurrent mutation racing the length snapshot) aborts the bridge with
/// the pending exception.
fn array_param_conv(i: usize, elem: &ArrayElemKind, ctx: &EmitCtx<'_>) -> String {
    let an = format!("a{i}");
    format!(
        "let {an}: Vec<{}> = {};",
        array_elem_rs_qualified(ctx, elem),
        array_read_expr(&an, elem, ctx)
    )
}

/// An expression block evaluating to `Vec<T>` from the array `handle`.
fn array_read_expr(handle: &str, elem: &ArrayElemKind, ctx: &EmitCtx<'_>) -> String {
    let push = match elem {
        ArrayElemKind::Str => "__vec.push(unsafe { _ntsc_handle_to_string(__h) });".to_string(),
        ArrayElemKind::Bool => "__vec.push(__h != 0);".to_string(),
        ArrayElemKind::Int(ty) => format!("__vec.push(__h as {ty});"),
        ArrayElemKind::Float(ty) => {
            format!("__vec.push(f64::from_bits(__h as u64) as {ty});")
        }
        ArrayElemKind::Object(obj) => {
            let qualified = object_wrapped_expr(ctx, obj);
            let tag = object_tag(ctx, obj);
            format!(
                "let __v: {qualified} = match unsafe {{ _ntsc_shared_take(__h, \"{tag}\") }} {{ Some(v) => v, None => return unsafe {{ _ntsc_shared_mismatch(__h, \"{tag}\", \"{qualified}\") }}, }}; \
                 __vec.push(__v);"
            )
        }
    };
    format!(
        "{{ if {handle} != 0 && unsafe {{ ntsc_array_is_array({handle}) }} == 0 \
         {{ return unsafe {{ _ntsc_throw_text(\"expected array handle\") }}; }} \
         let __len = unsafe {{ ntsc_array_len({handle}) }}; \
         let mut __vec = Vec::with_capacity(__len.max(0) as usize); \
         let mut __k = 0i64; \
         while __k < __len {{ \
         let __h = unsafe {{ ntsc_array_get({handle}, __k) }}; \
         if unsafe {{ ntsc_exception_pending() }} != 0 {{ return 0; }} \
         {push} __k += 1; \
         }} __vec }}"
    )
}

/// One `ntsc_array_push` statement pushing the borrowed `__x` element of
/// an `.iter()` loop into a fresh array handle. String elements go
/// through a temporary handle the array deep-copies and the bridge drops.
fn array_push_stmt(ctx: &EmitCtx<'_>, arr: &str, item: &str, elem: &ArrayElemKind) -> String {
    match elem {
        ArrayElemKind::Str => format!(
            "let __h = unsafe {{ _ntsc_str_to_handle({item}) }}; \
             unsafe {{ ntsc_array_push({arr}, __h) }}; unsafe {{ ntsc_string_drop(__h) }};"
        ),
        ArrayElemKind::Bool => format!("unsafe {{ ntsc_array_push({arr}, i64::from(*{item})) }};"),
        ArrayElemKind::Int(_) => {
            format!("unsafe {{ ntsc_array_push({arr}, (*{item}) as i64) }};")
        }
        ArrayElemKind::Float(_) => {
            format!("unsafe {{ ntsc_array_push({arr}, (*{item} as f64).to_bits() as i64) }};")
        }
        ArrayElemKind::Object(path) => {
            let tag = object_tag(ctx, path);
            format!(
                "let __h = unsafe {{ _ntsc_shared_put((*{item}).clone(), \"{tag}\") }}; unsafe {{ ntsc_array_push({arr}, __h) }};"
            )
        }
    }
}

/// Emit the generated `__NtscCb_<Trait>` structs, their event queues, the
/// trait impls forwarding into the queues, and the poll bridges Neutron
/// drains.
///
/// Each override marshals its arguments into `i64` queue entries (see
/// [`callback_marshal_arg`]) tagged with the Neutron-supplied key.
/// Async overrides use the `async-trait 0.1` desugared shape (`fn
/// m<'life0, 'async_trait>(...) -> Pin<Box<dyn Future ...>>`), which is
/// what `#[async_trait]`-generated traits like serenity's `EventHandler`
/// expect; the adaptor build verifies every signature. Native (1.75+)
/// `async fn` traits use a different desugar and stay unbridgeable.
fn emit_callback_bridges(ctx: &EmitCtx<'_>, callbacks: &[CallbackBridge]) -> String {
    let mut sections = Vec::new();
    for callback in callbacks {
        let short = &callback.short;
        let struct_name = format!("__NtscCb_{short}");
        let event_name = format!("{struct_name}_Event");
        let queue_name = format!("__NTSCCB_{}_QUEUE", short.to_uppercase());
        let last_name = format!("__NTSCCB_{}_LAST", short.to_uppercase());
        let empty_name = format!("__NTSCCB_{}_EMPTY", short.to_uppercase());

        let mut overrides = Vec::new();
        for method in &callback.methods {
            let params = method
                .params
                .iter()
                .enumerate()
                .map(|(i, kind)| {
                    let rf = method.param_ref.get(i).copied().unwrap_or(ParamRef::Owned);
                    format!("a{i}: {}", callback_param_ty(ctx, kind, rf))
                })
                .collect::<Vec<_>>()
                .join(", ");
            let marshals = method
                .params
                .iter()
                .enumerate()
                .map(|(i, kind)| {
                    let rf = method.param_ref.get(i).copied().unwrap_or(ParamRef::Owned);
                    format!("let e{i} = {};", callback_marshal_arg(ctx, i, kind, rf))
                })
                .collect::<Vec<_>>()
                .join("\n        ");
            let args = (0..method.params.len())
                .map(|i| format!("e{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let push = format!(
                "{queue_name}.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push_back({event_name} {{ key, method: \"{method}\", args: vec![{args}] }});",
                method = method.name
            );
            let unsafe_prefix = if method.is_unsafe { "unsafe " } else { "" };
            let signature = if method.is_async {
                // Desugared `async fn`: two lifetimes for borrowed `self`,
                // one for by-value `self`, exactly as `async-trait 0.1`
                // expands the trait side.
                let (lifetimes, self_param, bound) = match method.receiver {
                    ReceiverKind::Owned => (
                        "'async_trait".to_string(),
                        "self".to_string(),
                        "Self: 'async_trait".to_string(),
                    ),
                    _ => (
                        "'life0, 'async_trait".to_string(),
                        method
                            .receiver
                            .as_self_param()
                            .replace("self", "'life0 self"),
                        "'life0: 'async_trait, Self: 'async_trait".to_string(),
                    ),
                };
                let full_params = if params.is_empty() {
                    self_param
                } else {
                    format!("{self_param}, {params}")
                };
                format!(
                    "{unsafe_prefix}fn {name}<{lifetimes}>({full_params}) -> std::pin::Pin<std::boxed::Box<dyn std::future::Future<Output = ()> + Send + 'async_trait>> where {bound}",
                    name = method.name
                )
            } else {
                let full_params = if params.is_empty() {
                    method.receiver.as_self_param().to_string()
                } else {
                    format!("{}, {params}", method.receiver.as_self_param())
                };
                format!(
                    "{unsafe_prefix}fn {name}({full_params})",
                    name = method.name
                )
            };
            let body = if method.is_async {
                format!(
                    "let key = self.key;\n        std::boxed::Box::pin(async move {{\n        {marshals}\n        {push}\n        }})"
                )
            } else {
                format!("let key = self.key;\n        {marshals}\n        {push}")
            };
            overrides.push(format!("    {signature} {{\n        {body}\n    }}"));
        }

        sections.push(format!(
            "#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]\nstruct {struct_name} {{ key: i64 }}\n\nimpl {struct_name} {{\n    fn new(key: i64) -> Self {{ Self {{ key }} }}\n}}\n\nstruct {event_name} {{ key: i64, method: &'static str, args: Vec<i64> }}\n\nstatic {queue_name}: std::sync::LazyLock<std::sync::Mutex<std::collections::VecDeque<{event_name}>>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::VecDeque::new()));\n\nstatic {last_name}: std::sync::Mutex<Option<{event_name}>> = std::sync::Mutex::new(None);\n\n#[allow(unsafe_code)]\nstatic {empty_name}: std::sync::LazyLock<i64> = std::sync::LazyLock::new(|| unsafe {{ _ntsc_str_to_handle(\"\") }});\n\n#[allow(unsafe_code)]\nimpl {crate}::{trait_path} for {struct_name} {{\n{overrides}\n}}",
            trait_path = callback.trait_path,
            overrides = overrides.join("\n"),
            crate = ctx.crate_escaped,
        ));

        // The poll family: `poll` pops the next event (its method name, or
        // empty when drained) into the `LAST` slot; `arg_count`/`arg`/`arg_str`
        // read the slot's raw entries. `_arg` and `_arg_str` return the same
        // `i64`; the manifest types one as int (handles, ints) and the other
        // as string, so Neutron applies the right value representation.
        //
        // Ownership: replacing `LAST` releases the previous event's argument
        // references, and each `arg()` call retains its own — so payloads
        // die with their last user instead of leaking per event. Raw scalar
        // entries retain/release as no-ops. Locks are never nested: values
        // are copied out before any registry call.
        let crate_poll = format!("ntsc_adaptor_{}___NtscCb_{short}", ctx.crate_escaped);
        sections.push(format!(
            "#[allow(unsafe_code, non_snake_case)]\n#[unsafe(no_mangle)]\npub fn {crate_poll}_poll() -> i64 {{\n    let event = {queue_name}.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).pop_front();\n    // An idle queue yields an empty-string handle, not the null handle:\n    // Neutron compares the method name against the empty string, and a\n    // null handle neither reads nor compares as one.\n    let name = match &event {{\n        Some(event) => unsafe {{ _ntsc_str_to_handle(event.method) }},\n        None => *{empty_name},\n    }};\n    let previous = {{\n        let mut last = {last_name}.lock().unwrap_or_else(|poisoned| poisoned.into_inner());\n        let previous = last.take();\n        *last = event;\n        previous\n    }};\n    if let Some(previous) = previous {{\n        for handle in previous.args {{\n            unsafe {{ ntsc_shared_object_release(handle) }};\n        }}\n    }}\n    name\n}}\n\n#[allow(unsafe_code, non_snake_case)]\n#[unsafe(no_mangle)]\npub fn {crate_poll}_arg_count() -> i64 {{\n    {last_name}.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().map_or(0, |event| event.args.len() as i64)\n}}\n\n#[allow(unsafe_code, non_snake_case)]\n#[unsafe(no_mangle)]\npub fn {crate_poll}_arg(a0: i64) -> i64 {{\n    let handle = {last_name}.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().and_then(|event| event.args.get(a0 as usize).copied()).unwrap_or(0);\n    unsafe {{ ntsc_shared_object_retain(handle) }}\n}}\n\n#[allow(unsafe_code, non_snake_case)]\n#[unsafe(no_mangle)]\npub fn {crate_poll}_arg_str(a0: i64) -> i64 {{\n    // Borrowed: string-typed entries are runtime strings (Neutron-owned),\n    // so only object entries would need ownership — and those stay valid\n    // exactly until the next `poll` replaces the slot.\n    {last_name}.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().and_then(|event| event.args.get(a0 as usize).copied()).filter(|handle| *handle != 0).unwrap_or(*{empty_name})\n}}"
        ));
    }
    sections.join("\n\n")
}

/// Emit an offloaded bridge for an async Rust function. The worker executes
/// the existing marshaling bridge on a worker thread, while the public symbol
/// returns an `ntsc_async_op` handle consumed by the async state machine.
fn async_bridge_fn(ctx: &EmitCtx<'_>, func: &Func) -> String {
    let public = format!("ntsc_adaptor_{}_{}", ctx.crate_escaped, func.name);
    let sync = format!("{public}_sync");
    let core = bridge_fn(ctx, func).replace(&public, &sync);
    let args = (0..func.params.len())
        .map(|i| format!("a{i}: i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let take = (0..func.params.len())
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let unpack = (0..func.params.len())
        .map(|i| format!("let a{i} = args.get({i}).copied().unwrap_or(0);"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"{core}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn {public}_work(id: i64) -> i64 {{
    let args = _ntsc_async_args_take(id);
    {unpack}
    // The panic barrier the sync core cannot always provide itself: a
    // taken-object call runs without one, and a panic must not unwind
    // across this `extern "C"` boundary. A caught panic becomes a throw,
    // which the pending-exception check below carries to the awaiter.
    let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{
        {sync}({take})
    }})) {{
        Ok(value) => value,
        Err(payload) => unsafe {{ _ntsc_panic_to_handle(payload) }},
    }};
    unsafe {{
        // A throw inside the sync bridge parks its message in this
        // worker's pending-exception slot, which nothing checks; carry it
        // to the awaiting goroutine as a negated handle so the op result
        // re-raises it there.
        if ntsc_exception_pending() != 0 {{
            let message = ntsc_exception_take_message();
            if message != 0 {{
                return -message;
            }}
        }}
    }}
    result
}}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn {public}({args}) -> i64 {{
    let id = _ntsc_async_args_put(vec![{take}]);
    unsafe {{ ntsc_async_op_new({public}_work, id) }}
}}"#
    )
}

/// Emit an offloaded alias for a synchronous Rust function. The dependency
/// call still uses the normal bridge, but it runs on the runtime's blocking
/// pool so an NTSC `async fun` can await it without pinning a scheduler
/// worker. The alias is named `<function>_async` and is advertised as
/// asynchronous in the export manifest.
fn sync_async_bridge_fn(ctx: &EmitCtx<'_>, func: &Func) -> String {
    let public = format!("ntsc_adaptor_{}_{}_async", ctx.crate_escaped, func.name);
    let sync = format!("{public}_sync");
    let original = bridge_fn(ctx, func);
    let original_public = format!("ntsc_adaptor_{}_{}", ctx.crate_escaped, func.name);
    let core = original.replace(&original_public, &sync);
    let args = (0..func.params.len())
        .map(|i| format!("a{i}: i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let take = (0..func.params.len())
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let unpack = (0..func.params.len())
        .map(|i| format!("let a{i} = args.get({i}).copied().unwrap_or(0);"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"{core}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn {public}_work(id: i64) -> i64 {{
    let args = _ntsc_async_args_take(id);
    {unpack}
    let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{
        {sync}({take})
    }})) {{
        Ok(value) => value,
        Err(payload) => unsafe {{ _ntsc_panic_to_handle(payload) }},
    }};
    unsafe {{
        if ntsc_exception_pending() != 0 {{
            let message = ntsc_exception_take_message();
            if message != 0 {{
                return -message;
            }}
        }}
    }}
    result
}}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn {public}({args}) -> i64 {{
    let id = _ntsc_async_args_put(vec![{take}]);
    unsafe {{ ntsc_async_op_new({public}_work, id) }}
}}"#
    )
}

/// Whether a function's signature involves bridged-struct handles.
fn uses_objects(func: &Func) -> bool {
    matches!(
        func.ret,
        RetKind::Object(_) | RetKind::ResultObject(_) | RetKind::ResultObjectTuple(_)
    ) || func
        .params
        .iter()
        .any(|param| matches!(param, ParamKind::Object(_)))
}

/// The call-site expression for an object parameter. A borrowed (`get`)
/// handle already is `&T` and passes as-is — autoref would make `&&T`,
/// which trait resolution does not deref (`PartialEq::eq` fails to
/// resolve). Taken (`remove`d) handles are owned values and borrow here.
fn object_arg_expr(index: usize, ref_kind: ParamRef) -> String {
    match ref_kind {
        ParamRef::MutRef => format!("&mut *a{index}_obj"),
        ParamRef::Owned | ParamRef::Ref => format!("a{index}_obj"),
    }
}

/// One bridge function: `ntsc_adaptor_{crate}_{fn}(i64...) -> i64`.
fn bridge_fn(ctx: &EmitCtx<'_>, func: &Func) -> String {
    // A trait-impl method is invoked through method syntax on its
    // receiver (`a0_obj.name(args)`); everything else through the crate
    // path (`crate::Type::name(args)`).
    let call_name = if func.method_call {
        let method = func
            .call_path
            .rsplit("::")
            .next()
            .unwrap_or(&func.call_path);
        format!("a0_obj.{method}")
    } else {
        format!("{}::{}", ctx.crate_escaped, func.call_path)
    };

    // Build a `let aN: Ty = marshal_arg(aN);` for each param. Object
    // params instead resolve out of the process-wide shared space: takes
    // move the value out, borrows retain it for exactly the call.
    let mut arg_setup = Vec::new();
    let mut write_backs = Vec::new();
    // First pass: plain values without the adaptor registry lock. String
    // and bytes conversion takes the runtime registry lock, so holding the
    // adaptor mutex across it serializes every bridge call and starves
    // event-queue producers while a large body converts.
    for (i, kind) in func.params.iter().enumerate() {
        let an = format!("a{i}");
        if matches!(kind, ParamKind::Object(_)) {
            continue;
        }
        match kind {
            ParamKind::Object(_) => {}
            ParamKind::Str => arg_setup.push(format!(
                "let {an} = unsafe {{ _ntsc_handle_to_string({an}) }};"
            )),
            ParamKind::CowStr => arg_setup.push(format!(
                "let {an} = std::borrow::Cow::Owned(unsafe {{ _ntsc_handle_to_string({an}) }});"
            )),
            ParamKind::CowBytes => arg_setup.push(format!(
                "let {an} = std::borrow::Cow::Owned(unsafe {{ _ntsc_handle_to_bytes({an}) }});"
            )),
            ParamKind::Bytes => {
                // A `&mut Vec<u8>`/`&mut [u8]` param is filled through the
                // reference; the mutated bytes are assigned back into the
                // caller's existing string handle, in place.
                if matches!(func.param_ref.get(i), Some(ParamRef::MutRef)) {
                    arg_setup.push(format!(
                        "let {an}_handle = {an};\n    let mut {an} = unsafe {{ _ntsc_handle_to_bytes({an}) }};"
                    ));
                    write_backs.push(format!(
                        "unsafe {{ _ntsc_assign_bytes({an}_handle, {an}.as_ptr(), {an}.len() as i64) }};"
                    ));
                } else {
                    arg_setup.push(format!(
                        "let {an} = unsafe {{ _ntsc_handle_to_bytes({an}) }};"
                    ));
                }
            }
            ParamKind::Array(elem) => {
                arg_setup.push(array_param_conv(i, elem, ctx));
            }
            ParamKind::FixedArray(elem, n) => {
                // Read the array handle into a `Vec<T>`, then length-check
                // it into `[T; N]`. A short or long array throws instead of
                // producing a wrong-size argument.
                let rs = array_elem_rs_qualified(ctx, elem);
                arg_setup.push(format!(
                    "let {an}: [{rs}; {n}] = {{ let __v: Vec<{rs}> = {}; match <[{rs}; {n}]>::try_from(__v) {{ Ok(__a) => __a, Err(_) => return unsafe {{ _ntsc_throw_text(\"expected array of length {n}\") }} }} }};",
                    array_read_expr(&an, elem, ctx),
                ));
            }
            ParamKind::OptionArray(elem) => {
                // Only `Owned` reaches the emitter (`Option<&[T]>` is
                // rejected at classification): the null handle is `None`.
                arg_setup.push(format!(
                    "let {an}: Option<Vec<{}>> = if {an} == 0 {{ None }} else {{ Some({}) }};",
                    array_elem_rs_qualified(ctx, elem),
                    array_read_expr(&an, elem, ctx),
                ));
            }
            ParamKind::OptionInt(rust_ty) => arg_setup.push(format!(
                "let {an}: Option<{rust_ty}> = unsafe {{ _ntsc_option_scalar({an}) }};"
            )),
            ParamKind::OptionBool => arg_setup.push(format!(
                "let {an}: Option<bool> = unsafe {{ _ntsc_option_scalar({an}) }};"
            )),
            ParamKind::OptionFloat(rust_ty) => arg_setup.push(format!(
                "let {an}: Option<{rust_ty}> = unsafe {{ _ntsc_option_scalar::<f64>({an}).map(|v| v as {rust_ty}) }};"
            )),
            ParamKind::OptionStr => arg_setup.push(format!(
                "let {an}: Option<String> = if {an} == 0 {{ None }} else {{ Some(unsafe {{ _ntsc_handle_to_string({an}) }}) }};"
            )),
            ParamKind::OptionStrRef => arg_setup.push(format!(
                "let {an}__owned: Option<String> = if {an} == 0 {{ None }} else {{ Some(unsafe {{ _ntsc_handle_to_string({an}) }}) }};
    let {an}: Option<&str> = {an}__owned.as_deref();"
            )),
            ParamKind::OptionObject(obj) => {
                let tag = object_tag(ctx, obj);
                let wrapped = object_wrapped_expr(ctx, obj);
                let qualified = object_qualified(ctx, obj);
                arg_setup.push(format!(
                    "let {an}: Option<{wrapped}> = if {an} == 0 {{ None }} else {{ match unsafe {{ _ntsc_shared_take({an}, \"{tag}\") }} {{ Some(v) => Some(v), None => return unsafe {{ _ntsc_shared_mismatch({an}, \"{tag}\", \"{qualified}\") }}, }} }};"
                ));
            }
            ParamKind::Int(rust_ty) => {
                arg_setup.push(format!("let {an}: {rust_ty} = {an} as {rust_ty};"))
            }
            ParamKind::Bool => arg_setup.push(format!("let {an} = {an} != 0;")),
            ParamKind::Float(rust_ty) => arg_setup.push(format!(
                "let {an}: {rust_ty} = f64::from_bits({an} as u64) as {rust_ty};"
            )),
            ParamKind::Duration => arg_setup.push(format!(
                "let {an} = std::time::Duration::from_millis({an} as u64);"
            )),
            // The callback key is passed through as-is; the generated
            // `__NtscCb_<T>::new(key)` wrapper is built at the call site.
            ParamKind::Callback(_) => {}
        }
    }
    // Second pass: object handles, now that no runtime-registry FFI is
    // outstanding. Takes move the value out of the shared space (failing
    // while a borrow is outstanding); borrows retain for exactly the call
    // and release right after it (see `borrow_releases`). Borrowed
    // receivers resolve plain and `Arc` entries alike (dereferenced);
    // mutex-flavored entries fail closed with a tag mismatch.
    for (i, kind) in func.params.iter().enumerate() {
        let an = format!("a{i}");
        let ParamKind::Object(obj) = kind else {
            continue;
        };
        // The adaptor refers to dependency types through the crate root,
        // so `Regex` becomes `regex::Regex`.
        let qualified = object_qualified(ctx, obj);
        let wrapped = object_wrapped_expr(ctx, obj);
        let tag = object_tag(ctx, obj);
        match func.param_ref.get(i) {
            Some(ParamRef::Owned) | None => {
                arg_setup.push(format!(
                    "let mut {an}_obj: {wrapped} = match unsafe {{ _ntsc_shared_take({an}, \"{tag}\") }} {{ Some(v) => v, None => return unsafe {{ _ntsc_shared_mismatch({an}, \"{tag}\", \"{qualified}\") }}, }};"
                ));
            }
            Some(ParamRef::MutRef) => {
                arg_setup.push(format!(
                    "let {an}_ptr = unsafe {{ ntsc_shared_object_borrow_mut({an}, _ntsc_tag(\"{tag}\")) }} as *mut {wrapped};\n    if {an}_ptr.is_null() {{ return unsafe {{ _ntsc_shared_mismatch({an}, \"{tag}\", \"{qualified}\") }}; }}\n    let _{an}_borrow = _NtscObjectBorrow::Exclusive({an});\n    let {an}_obj: &mut {wrapped} = unsafe {{ &mut *{an}_ptr }};"
                ));
            }
            _ if obj.wrapper != ObjectWrapper::Plain => {
                arg_setup.push(format!(
                    "let {an}_ptr = match unsafe {{ _ntsc_shared_borrow({an}, \"{tag}\") }} {{ Some(p) => p, None => return unsafe {{ _ntsc_shared_mismatch({an}, \"{tag}\", \"{qualified}\") }}, }};\n    let {an}_obj: &{wrapped} = unsafe {{ &*{an}_ptr }};"
                ));
                arg_setup.push(format!(
                    "let _{an}_borrow = _NtscObjectBorrow::Shared({an});"
                ));
            }
            _ => {
                let arc_tag = object_tag(
                    ctx,
                    &ObjectType {
                        owner: obj.owner.clone(),
                        path: obj.path.clone(),
                        wrapper: ObjectWrapper::Arc,
                    },
                );
                arg_setup.push(format!(
                    "let {an}_obj: &{qualified} = match unsafe {{ _ntsc_shared_borrow_or_arc({an}, \"{tag}\", \"{arc_tag}\") }} {{ Some(p) => unsafe {{ &*p }}, None => return unsafe {{ _ntsc_shared_mismatch({an}, \"{tag}\", \"{qualified}\") }}, }};"
                ));
                arg_setup.push(format!(
                    "let _{an}_borrow = _NtscObjectBorrow::Shared({an});"
                ));
            }
        }
    }

    // Method-syntax calls take the receiver as the implicit `self`, so
    // it is not part of the argument list.
    let skip_receiver = usize::from(func.method_call);
    let arg_names = func
        .params
        .iter()
        .enumerate()
        .skip(skip_receiver)
        .map(|(i, kind)| {
            if matches!(kind, ParamKind::Object(_)) {
                let ref_kind = func.param_ref.get(i).copied().unwrap_or(ParamRef::Owned);
                return object_arg_expr(i, ref_kind);
            }
            if let ParamKind::Callback(trait_path) = kind {
                // The callback key is wrapped in the generated trait-impl
                // struct so the dependency receives a value implementing
                // the trait (see `emit_callback_bridges`). A reference
                // bound borrows the temporary for the call.
                let wrapper = format!("__NtscCb_{}::new(a{i})", sanitize_ident(trait_path));
                return match func.param_ref.get(i).copied().unwrap_or(ParamRef::Owned) {
                    ParamRef::Owned => wrapper,
                    _ => format!("&{wrapper}"),
                };
            }
            // Static trait calls (`Type::try_from(x)`) resolve without the
            // argument coercion that method and inherent-path calls enjoy,
            // so string/byte borrows spell the slice form explicitly. This
            // assumes the idiomatic `&str`/`&[u8]` spelling over `&String`
            // /`&Vec<u8>` (see `ptr_arg`); the generated build fails loudly
            // on the rare mismatch instead of decoding wrongly.
            if !func.method_call && func.trait_path.is_some() {
                match (
                    kind,
                    func.param_ref.get(i).copied().unwrap_or(ParamRef::Owned),
                ) {
                    (ParamKind::Str, ParamRef::Ref) => return format!("a{i}.as_str()"),
                    (ParamKind::Bytes, ParamRef::Ref) => return format!("a{i}.as_slice()"),
                    _ => {}
                }
            }
            match func.param_ref.get(i).copied().unwrap_or(ParamRef::Owned) {
                ParamRef::Owned => format!("a{i}"),
                ParamRef::Ref => format!("&a{i}"),
                ParamRef::MutRef => format!("&mut a{i}"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    // Emit the call behind a panic barrier so a panic in the dependency
    // surfaces as a Neutron exception instead of aborting the process.
    // Methods that borrow the receiver immutably fit the barrier; a taken
    // `&mut self` receiver cannot cross it (the returned value may
    // reference it), so those call directly — a panic there still aborts,
    // which the generated code cannot prevent soundly without unsafe.
    // A unit enum variant is a value, not a function: no call, no parens.
    let plain_call = if func.unit_variant {
        call_name
    } else if !func.field_names.is_empty() {
        let fields = func
            .field_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let value = if matches!(func.params.get(i), Some(ParamKind::Object(_))) {
                    let ref_kind = func.param_ref.get(i).copied().unwrap_or(ParamRef::Owned);
                    // Struct constructors are always path position.
                    object_arg_expr(i, ref_kind)
                } else {
                    match func.param_ref.get(i).copied().unwrap_or(ParamRef::Owned) {
                        ParamRef::Owned => format!("a{i}"),
                        ParamRef::Ref => format!("&a{i}"),
                        ParamRef::MutRef => format!("&mut a{i}"),
                    }
                };
                format!("{name}: {value}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{call_name} {{ {fields} }}")
    } else if func.is_async {
        // The dependency's future touches the Tokio reactor (timers, DNS,
        // tokio channels), so drive it on the adaptor's runtime instead of
        // the park-based executor.
        let call = format!("_ntsc_tokio_block_on({call_name}({arg_names}))");
        if func.is_unsafe {
            format!("unsafe {{ {call} }}")
        } else {
            call
        }
    } else {
        let call = format!("{call_name}({arg_names})");
        if func.is_unsafe {
            format!("unsafe {{ {call} }}")
        } else {
            call
        }
    };
    let marshal = marshal_ret(ctx, &func.ret, &func.call_path);
    let args = (0..func.params.len())
        .map(|i| format!("a{i}: i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let setup = arg_setup.join("\n    ");
    let writes = write_backs.join("\n        ");
    let body = format!(
        "    {setup}\n    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{\n        let _ntsc_block_guard = _ntsc_block_guard();\n        let result = {plain_call};\n        drop(_ntsc_block_guard);\n        let __ntsc_ret = {marshal};\n        {writes}\n        __ntsc_ret\n    }})) {{\n        Ok(value) => value,\n        Err(payload) => unsafe {{ _ntsc_panic_to_handle(payload) }},\n    }}"
    );

    format!(
        "#[allow(unsafe_code)]\n#[unsafe(no_mangle)]\npub fn ntsc_adaptor_{}_{}({args}) -> i64 {{\n{body}\n}}",
        ctx.crate_escaped, func.name
    )
}

/// Build a fresh Neutron array handle from the `Vec<T>` in `source`.
/// String elements go through temporary handles the array deep-copies;
/// scalars push by value. `source` is borrowed, never moved.
fn array_ret_build(ctx: &EmitCtx<'_>, source: &str, elem: &ArrayElemKind) -> String {
    let flag = i8::from(matches!(elem, ArrayElemKind::Str));
    let loop_body = if let ArrayElemKind::Object(path) = elem {
        let tag = object_tag(ctx, path);
        format!(
            "for __x in {source}.into_iter() {{ let __h = unsafe {{ _ntsc_shared_put(__x, \"{tag}\") }}; unsafe {{ ntsc_array_push(__arr, __h) }}; }}"
        )
    } else {
        format!(
            "for __x in {source}.iter() {{ {} }}",
            array_push_stmt(ctx, "__arr", "__x", elem)
        )
    };
    format!(
        "{{ let __arr = unsafe {{ ntsc_array_new_typed(8, {source}.len() as i64, {flag}) }}; {loop_body} __arr }}"
    )
}

/// Marshal the `result` binding of a bridge body back to the `i64` the
/// Neutron runtime expects, per the declared return kind. Object results
/// move into the shared space under this emit's tag.
fn result_err_throw(func_name: &str, debug_err: bool) -> String {
    if debug_err {
        format!(
            "unsafe {{ _ntsc_throw_text(&format!(\"Rust call {func_name} returned Err({{__e:?}})\", __e = __e)) }}"
        )
    } else {
        format!(
            "unsafe {{ _ntsc_throw_text(&format!(\"Rust call {func_name} returned Err({{}})\", std::any::type_name_of_val(&__e))) }}"
        )
    }
}

fn marshal_ret(ctx: &EmitCtx<'_>, ret: &RetKind, func_name: &str) -> String {
    // `ureq::Error` carries the HTTP status (and transport reason) but no
    // `Display`, so its bridges format the error with `Debug` — naming the
    // status code instead of swallowing it. Every other crate keeps the
    // type-name fallback, which compiles for any error type.
    let debug_err = ctx.crate_escaped == "ureq";
    let object_put = |obj: &ObjectType, value: &str| {
        let tag = object_tag(ctx, obj);
        format!("unsafe {{ _ntsc_shared_put({value}, \"{tag}\") }}")
    };
    match ret {
        RetKind::Str => "unsafe { _ntsc_str_to_handle(&*result) }".to_string(),
        RetKind::Path => "unsafe { _ntsc_str_to_handle(&*result.to_string_lossy()) }".to_string(),
        RetKind::Bytes => "unsafe { _ntsc_bytes_to_handle(&*result) }".to_string(),
        RetKind::OptionStr => {
            "match result { Some(s) => unsafe { _ntsc_str_to_handle(&*s) }, None => 0 }".to_string()
        }
        RetKind::OptionBytes => {
            "match result { Some(v) => unsafe { _ntsc_bytes_to_handle(&*v) }, None => 0 }"
                .to_string()
        }
        RetKind::OptionObject(path) => format!(
            "match result {{ Some(v) => {}, None => 0 }}",
            object_put(path, "v.clone()")
        ),
        RetKind::ResultStr => format!(
            "match result {{\
             Ok(s) => unsafe {{ _ntsc_str_to_handle(&*s) }},\
             Err(__e) => {},\
             }}",
            result_err_throw(func_name, debug_err)
        ),
        RetKind::ResultPath => format!(
            "match result {{\
             Ok(p) => unsafe {{ _ntsc_str_to_handle(&*p.to_string_lossy()) }},\
             Err(__e) => {},\
             }}",
            result_err_throw(func_name, debug_err)
        ),
        RetKind::ResultBytes => format!(
            "match result {{\
             Ok(v) => unsafe {{ _ntsc_bytes_to_handle(&*v) }},\
             Err(__e) => {},\
             }}",
            result_err_throw(func_name, debug_err)
        ),
        RetKind::Array(elem) => array_ret_build(ctx, "result", elem),
        RetKind::OptionArray(elem) => format!(
            "match result {{ Some(__v) => {}, None => 0 }}",
            array_ret_build(ctx, "__v", elem)
        ),
        RetKind::OptionInt(_) | RetKind::OptionBool => {
            "unsafe { _ntsc_option_cell(result) }".to_string()
        }
        RetKind::OptionFloat(_) => "unsafe { _ntsc_option_float_cell(result) }".to_string(),
        RetKind::ResultArray(elem) => format!(
            "match result {{ Ok(__v) => {}, Err(__e) => {}, }}",
            array_ret_build(ctx, "__v", elem),
            result_err_throw(func_name, debug_err)
        ),
        RetKind::ResultUnit => format!(
            "match result {{\
             Ok(()) => 0,\
             Err(__e) => {},\
             }}",
            result_err_throw(func_name, debug_err)
        ),
        RetKind::Object(path) => object_put(path, "result"),
        RetKind::ObjectClone(path) => object_put(path, "result.clone()"),
        RetKind::ResultObject(path) => format!(
            "match result {{ Ok(v) => {}, Err(__e) => {}, }}",
            object_put(path, "v"),
            result_err_throw(func_name, debug_err)
        ),
        RetKind::ResultObjectTuple(objects) => {
            let fields = objects
                .iter()
                .enumerate()
                .map(|(index, object)| {
                    let put = object_put(object, &format!("v.{index}"));
                    format!("let handle = {put}; unsafe {{ ntsc_array_push(array, handle) }};")
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "match result {{ Ok(v) => {{ let array = unsafe {{ ntsc_array_new_typed(8, 0, 0) }}; {fields} array }}, Err(__e) => {}, }}",
                result_err_throw(func_name, debug_err)
            )
        }
        RetKind::Int(_) => "result as i64".to_string(),
        RetKind::Bool => "(i64::from(result))".to_string(),
        RetKind::Float(_) => "(result as f64).to_bits() as i64".to_string(),
        RetKind::Unit => "0".to_string(),
    }
}

/// One field-setter bridge: `ntsc_adaptor_<crate>_<Type>_set_<field>(handle,
/// value)` converts the value per its tier-1 kind, borrows the object out
/// of the shared space, assigns the field, releases, and returns 0.
/// Object values move in (their handle is consumed); everything else
/// copies. Like getters, setters never hold a lock across the foreign
/// call — there is no lock at all, only a retained borrow.
fn field_setter_bridge_fn(ctx: &EmitCtx<'_>, func: &Func) -> String {
    let field = func.field_names.first().cloned().unwrap_or_default();
    let obj = match func.params.first() {
        Some(ParamKind::Object(obj)) => obj.clone(),
        _ => ObjectType::local(""),
    };
    let qualified = object_qualified(ctx, &obj);
    let tag = object_tag(ctx, &obj);
    let value_kind = func.params.get(1);
    let convert = match value_kind {
        Some(ParamKind::Str) => "let a1 = unsafe { _ntsc_handle_to_string(a1) };".to_string(),
        Some(ParamKind::CowStr) => "let a1 = std::borrow::Cow::Owned(unsafe { _ntsc_handle_to_string(a1) });".to_string(),
        Some(ParamKind::Bytes) => "let a1 = unsafe { _ntsc_handle_to_bytes(a1) };".to_string(),
        Some(ParamKind::CowBytes) => "let a1 = std::borrow::Cow::Owned(unsafe { _ntsc_handle_to_bytes(a1) });".to_string(),
        Some(ParamKind::Int(rust_ty)) => format!("let a1: {rust_ty} = a1 as {rust_ty};"),
        Some(ParamKind::Bool) => "let a1 = a1 != 0;".to_string(),
        Some(ParamKind::Float(rust_ty)) => {
            format!("let a1: {rust_ty} = f64::from_bits(a1 as u64) as {rust_ty};")
        }
        Some(ParamKind::Duration) => {
            "let a1 = std::time::Duration::from_millis(a1 as u64);".to_string()
        }
        Some(ParamKind::OptionInt(rust_ty)) => format!(
            "let a1: Option<{rust_ty}> = unsafe {{ _ntsc_option_scalar(a1) }};"
        ),
        Some(ParamKind::OptionBool) => {
            "let a1: Option<bool> = unsafe { _ntsc_option_scalar(a1) };".to_string()
        }
        Some(ParamKind::OptionFloat(rust_ty)) => format!(
            "let a1: Option<{rust_ty}> = unsafe {{ _ntsc_option_scalar::<f64>(a1).map(|v| v as {rust_ty}) }};"
        ),
        Some(ParamKind::OptionStr) => "let a1: Option<String> = if a1 == 0 { None } else { Some(unsafe { _ntsc_handle_to_string(a1) }) };".to_string(),
        Some(ParamKind::OptionObject(option_obj)) => {
            let option_tag = object_tag(ctx, option_obj);
            let option_qualified = object_qualified(ctx, option_obj);
            let option_wrapped = object_wrapped_expr(ctx, option_obj);
            format!(
                "let a1: Option<{option_wrapped}> = if a1 == 0 {{ None }} else {{ match unsafe {{ _ntsc_shared_take(a1, \"{option_tag}\") }} {{ Some(v) => Some(v), None => return unsafe {{ _ntsc_shared_mismatch(a1, \"{option_tag}\", \"{option_qualified}\") }}, }} }};"
            )
        }
        Some(ParamKind::Object(value_obj)) => {
            let value_tag = object_tag(ctx, value_obj);
            let value_wrapped = object_wrapped_expr(ctx, value_obj);
            let value_qualified = object_qualified(ctx, value_obj);
            format!(
                "let a1: {value_wrapped} = match unsafe {{ _ntsc_shared_take(a1, \"{value_tag}\") }} {{ Some(v) => v, None => return unsafe {{ _ntsc_shared_mismatch(a1, \"{value_tag}\", \"{value_qualified}\") }}, }};"
            )
        }
        Some(ParamKind::Array(elem)) => {
            let array_ty = array_elem_rs_qualified(ctx, elem);
            let read = array_read_expr("a1", elem, ctx);
            format!("let a1: Vec<{array_ty}> = {read};")
        }
        Some(ParamKind::FixedArray(elem, n)) => {
            let array_ty = array_elem_rs_qualified(ctx, elem);
            let read = array_read_expr("a1", elem, ctx);
            format!("let a1: [{array_ty}; {n}] = match <[{array_ty}; {n}]>::try_from({read}) {{ Ok(__a) => __a, Err(_) => return unsafe {{ _ntsc_throw_text(\"expected array of length {n}\") }} }};")
        }
        Some(ParamKind::OptionArray(elem)) => {
            let array_ty = array_elem_rs_qualified(ctx, elem);
            let read = array_read_expr("a1", elem, ctx);
            format!("let a1: Option<Vec<{array_ty}>> = if a1 == 0 {{ None }} else {{ Some({read}) }};")
        }
        _ => "let a1 = 0;".to_string(),
    };
    let body = format!(
        "    {convert}\n    let a0_ptr = unsafe {{ ntsc_shared_object_borrow_mut(a0, _ntsc_tag(\"{tag}\")) }} as *mut {qualified};\n    if a0_ptr.is_null() {{ return unsafe {{ _ntsc_shared_mismatch(a0, \"{tag}\", \"{qualified}\") }}; }}\n    let _a0_borrow = _NtscObjectBorrow::Exclusive(a0);\n    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{\n        let obj = unsafe {{ &mut *a0_ptr }};\n        obj.{field} = a1;\n    }})) {{\n        Ok(()) => 0,\n        Err(payload) => unsafe {{ _ntsc_panic_to_handle(payload) }},\n    }}"
    );
    format!(
        "#[allow(unsafe_code)]\n#[unsafe(no_mangle)]\npub fn ntsc_adaptor_{}_{}(a0: i64, a1: i64) -> i64 {{\n{body}\n}}",
        ctx.crate_escaped, func.name
    )
}
/// borrows the object out of the shared space and clones the field into
/// a fresh Neutron value, releasing the borrow before marshaling.
fn field_bridge_fn(ctx: &EmitCtx<'_>, func: &Func) -> String {
    let field = func.field_names.first().cloned().unwrap_or_default();
    let obj = match func.params.first() {
        Some(ParamKind::Object(obj)) => obj.clone(),
        _ => ObjectType::local(""),
    };
    let qualified = object_qualified(ctx, &obj);
    let tag = object_tag(ctx, &obj);
    let arc_tag = object_tag(
        ctx,
        &ObjectType {
            owner: obj.owner.clone(),
            path: obj.path.clone(),
            wrapper: ObjectWrapper::Arc,
        },
    );
    // The receiver always borrows: getters never consume the handle. The
    // clone is owned, so the borrow releases before marshaling (which may
    // allocate handles); a borrowed field would fail to compile here
    // rather than dangle at runtime. Plain and `Arc` entries both resolve.
    let setup = format!(
        "let a0_obj: &{qualified} = match unsafe {{ _ntsc_shared_borrow_or_arc(a0, \"{tag}\", \"{arc_tag}\") }} {{ Some(p) => unsafe {{ &*p }}, None => return unsafe {{ _ntsc_shared_mismatch(a0, \"{tag}\", \"{qualified}\") }}, }};"
    );
    let catch_call = format!(
        "let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{\n    a0_obj.{field}.clone()\n}})) {{\n    Ok(value) => value,\n    Err(payload) => {{ unsafe {{ ntsc_shared_object_release(a0) }}; return unsafe {{ _ntsc_panic_to_handle(payload) }} }},\n}};\n    unsafe {{ ntsc_shared_object_release(a0) }};"
    );
    let marshal = marshal_ret(ctx, &func.ret, &func.call_path);
    let body = format!("    {setup}\n    {catch_call}\n    {marshal}");
    format!(
        "#[allow(unsafe_code)]\n#[unsafe(no_mangle)]\npub fn ntsc_adaptor_{}_{}(a0: i64) -> i64 {{\n{body}\n}}",
        ctx.crate_escaped, func.name
    )
}

/// JSON export manifest describing the marshalable surface for `name`.
///
/// Read by `ntsc-typeck` at resolve time to register `crate.func` as
/// callable, and by `ntsc-codegen` to declare the corresponding extern
/// symbols.
/// Build the canonical `<crate>.export.json` document: one typed entry
/// per bridge (plus its `_async` offload alias and each callback poll
/// entry), then one entry per discovered-but-unbridgeable item.
///
/// The document is the [`ExportManifest`] standard serialized with
/// `serde_json` — field order is struct order, escaping is serde's — so
/// identical APIs always produce identical bytes and freshness checks
/// never rewrite an unchanged manifest.
fn export_manifest_content(
    name: &str,
    functions: &[Func],
    unsupported: &[UnsupportedApi],
    callbacks: &[CallbackBridge],
) -> CargoResult<String> {
    let crate_escaped = name.replace('-', "_");
    let mut entries = Vec::new();
    for func in functions {
        let params = func
            .params
            .iter()
            .enumerate()
            .map(|(index, kind)| ExportParam {
                name: format!("a{index}"),
                tier: 1,
                ty: param_type_name(kind),
                origin: param_origin(kind),
            })
            .collect::<Vec<_>>();
        let ret = ExportReturn {
            tier: 1,
            ty: ret_type_name(&func.ret),
            origin: ret_origin(&func.ret),
        };
        entries.push(ExportEntry::Typed(ExportTyped {
            name: func.name.clone(),
            path: func.call_path.clone(),
            status: ApiStatus::Typed,
            params: params.clone(),
            ret: ret.clone(),
            symbol: format!("ntsc_adaptor_{crate_escaped}_{}", func.name),
            is_async: func.is_async,
            doc: if func.doc.is_empty() {
                None
            } else {
                Some(func.doc.clone())
            },
        }));
        if !func.is_async && !func.is_field && !func.name.ends_with("_async") {
            let async_path = if let Some((prefix, method)) = func.call_path.rsplit_once("::") {
                format!("{prefix}::{method}_async")
            } else {
                format!("{}_async", func.call_path)
            };
            entries.push(ExportEntry::Typed(ExportTyped {
                name: format!("{}_async", func.name),
                path: async_path,
                status: ApiStatus::Typed,
                params,
                ret,
                symbol: format!("ntsc_adaptor_{crate_escaped}_{}_async", func.name),
                is_async: true,
                doc: Some(format!("Offloaded asynchronous variant of {}", func.name)),
            }));
        }
    }
    // The poll family is emitted directly (not via `Func`), so its
    // manifest entries are synthesized here with the same symbol scheme.
    for callback in callbacks {
        let prefix = format!("__NtscCb_{}", callback.short);
        for (entry_name, param, ret) in [
            (format!("{prefix}_poll"), None, "string"),
            (format!("{prefix}_arg_count"), None, "int"),
            (format!("{prefix}_arg"), Some("int"), "int"),
            (format!("{prefix}_arg_str"), Some("int"), "string"),
        ] {
            entries.push(ExportEntry::Typed(ExportTyped {
                name: entry_name.clone(),
                path: entry_name.clone(),
                status: ApiStatus::Typed,
                params: param
                    .map(|ty| {
                        vec![ExportParam {
                            name: "a0".into(),
                            tier: 1,
                            ty: ty.into(),
                            origin: None,
                        }]
                    })
                    .unwrap_or_default(),
                ret: ExportReturn {
                    tier: 1,
                    ty: ret.into(),
                    origin: None,
                },
                symbol: format!("ntsc_adaptor_{crate_escaped}_{entry_name}"),
                is_async: false,
                doc: None,
            }));
        }
    }
    entries.extend(unsupported.iter().map(|item| {
        ExportEntry::Untyped(ExportUntyped {
            name: item.name.clone(),
            path: item.path.clone(),
            status: item.status.clone(),
            reason: item.reason.clone(),
            signature: if item.signature.is_empty() {
                None
            } else {
                Some(item.signature.clone())
            },
        })
    }));
    // The callback section documents each generated `__NtscCb` impl for
    // Neutron authors: which queue feeds which methods, and the queued
    // argument kinds in order (`_arg` for handles/ints, `_arg_str` for
    // strings).
    let manifest = ExportManifest {
        schema_version: API_METADATA_SCHEMA_VERSION,
        module: name.to_string(),
        functions: entries,
        callbacks: callbacks
            .iter()
            .map(|callback| ExportCallback {
                trait_path: callback.trait_path.clone(),
                ty: format!("__NtscCb_{}", callback.short),
                methods: callback
                    .methods
                    .iter()
                    .map(|method| ExportCallbackMethod {
                        name: method.name.clone(),
                        params: method.params.iter().map(param_type_name).collect(),
                        is_async: method.is_async,
                    })
                    .collect(),
            })
            .collect(),
        types: Vec::new(),
        enums: Vec::new(),
    };
    serde_json::to_string_pretty(&manifest)
        .map_err(|error| crate::CargoError(format!("cannot encode {name}.export.json: {error}")))
}

pub(crate) fn param_type_name(kind: &ParamKind) -> String {
    match kind {
        ParamKind::Str | ParamKind::CowStr => "string".into(),
        ParamKind::Bytes | ParamKind::CowBytes => "bytes".into(),
        ParamKind::Array(elem) => format!("array[{}]", array_elem_label(elem)),
        // A fixed-size `[T; N]` parameter is Neutron-visible as an array;
        // the length constraint is enforced at the boundary, not in the label.
        ParamKind::FixedArray(elem, _) => format!("array[{}]", array_elem_label(elem)),
        ParamKind::OptionArray(elem) => format!("option[array[{}]]", array_elem_label(elem)),
        ParamKind::OptionInt(_) => "option[int]".into(),
        ParamKind::OptionBool => "option[bool]".into(),
        ParamKind::OptionFloat(_) => "option[float]".into(),
        ParamKind::OptionStr => "option[string]".into(),
        ParamKind::OptionStrRef => "option[string]".into(),
        ParamKind::OptionObject(obj) => format!("option[object[{}]]", obj.path),
        ParamKind::Int(_) => "int".into(),
        ParamKind::Bool => "bool".into(),
        ParamKind::Float(_) => "float".into(),
        ParamKind::Duration => "int".into(),
        ParamKind::Object(obj) => format!("object[{}]", obj.path),
        ParamKind::Callback(trait_path) => format!("callback[{trait_path}]"),
    }
}

/// The owning dependency crate of one bridge parameter, when its type
/// lives in another adaptor. Readers resolve methods and drop glue
/// through the origin, since handles live in one shared space.
pub(crate) fn param_origin(kind: &ParamKind) -> Option<String> {
    match kind {
        ParamKind::Object(obj) | ParamKind::OptionObject(obj) => obj.owner.clone(),
        ParamKind::Array(elem) | ParamKind::OptionArray(elem) | ParamKind::FixedArray(elem, _) => {
            match elem {
                ArrayElemKind::Object(obj) => obj.owner.clone(),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The owning dependency crate of one bridge return, mirroring
/// [`param_origin`].
pub(crate) fn ret_origin(ret: &RetKind) -> Option<String> {
    match ret {
        RetKind::Object(obj)
        | RetKind::OptionObject(obj)
        | RetKind::ObjectClone(obj)
        | RetKind::ResultObject(obj) => obj.owner.clone(),
        RetKind::Array(elem) | RetKind::OptionArray(elem) | RetKind::ResultArray(elem) => {
            match elem {
                ArrayElemKind::Object(obj) => obj.owner.clone(),
                _ => None,
            }
        }
        _ => None,
    }
}

fn array_elem_rs_qualified(ctx: &EmitCtx<'_>, elem: &ArrayElemKind) -> String {
    match elem {
        ArrayElemKind::Object(obj) => object_wrapped_expr(ctx, obj),
        _ => array_elem_rs(elem),
    }
}

/// The canonical Rust element type of one collection element kind.
fn array_elem_rs(elem: &ArrayElemKind) -> String {
    match elem {
        ArrayElemKind::Str => "String".to_string(),
        ArrayElemKind::Bool => "bool".to_string(),
        ArrayElemKind::Int(ty) | ArrayElemKind::Float(ty) => ty.clone(),
        ArrayElemKind::Object(obj) => obj.path.clone(),
    }
}

/// The manifest element label of one collection element kind.
fn array_elem_label(elem: &ArrayElemKind) -> &'static str {
    match elem {
        ArrayElemKind::Str => "string",
        ArrayElemKind::Bool => "bool",
        ArrayElemKind::Int(_) => "int",
        ArrayElemKind::Float(_) => "float",
        ArrayElemKind::Object(_) => "object",
    }
}

/// A Rust path (`serenity::client::EventHandler`) → a C-identifier-safe
/// suffix used in generated type names (`EventHandler`).
fn sanitize_ident(path: &str) -> String {
    path.rsplit("::")
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .replace(|c: char| !(c.is_ascii_alphanumeric() || c == '_'), "_")
}

pub(crate) fn ret_type_name(kind: &RetKind) -> String {
    match kind {
        RetKind::Str => "string".into(),
        RetKind::Path => "string".into(),
        RetKind::Bytes => "bytes".into(),
        RetKind::OptionStr => "option[string]".into(),
        RetKind::OptionBytes => "option[bytes]".into(),
        RetKind::OptionArray(elem) => format!("option[array[{}]]", array_elem_label(elem)),
        RetKind::OptionInt(_) => "option[int]".into(),
        RetKind::OptionBool => "option[bool]".into(),
        RetKind::OptionFloat(_) => "option[float]".into(),
        RetKind::OptionObject(obj) => format!("option[object[{}]]", obj.path),
        RetKind::ResultStr => "result[string]".into(),
        RetKind::ResultPath => "result[string]".into(),
        RetKind::ResultBytes => "result[bytes]".into(),
        RetKind::ResultArray(elem) => format!("result[array[{}]]", array_elem_label(elem)),
        RetKind::ResultObjectTuple(_) => "result[array[object]]".into(),
        RetKind::ResultUnit => "result[void]".into(),
        RetKind::Array(elem) => format!("array[{}]", array_elem_label(elem)),
        RetKind::Object(obj) => format!("object[{}]", obj.path),
        RetKind::ObjectClone(obj) => format!("object[{}]", obj.path),
        RetKind::ResultObject(obj) => format!("result[object[{}]]", obj.path),
        RetKind::Int(_) => "int".into(),
        RetKind::Bool => "bool".into(),
        RetKind::Float(_) => "float".into(),
        RetKind::Unit => "void".into(),
    }
}

/// Inventory the deferred macro items: map each `macro_rules!` definition
/// to the trait it generates, register macro-generated trait names as
/// public types so `root_type_path` resolves them, and return the
/// event-style methods of matching invocations.
fn inventory_macro_traits(
    macro_items: &[(Vec<String>, syn::ItemMacro)],
    features: &std::collections::HashSet<String>,
    index: &mut CrateIndex,
) -> Vec<(Vec<String>, String, Vec<MacroTraitMethod>)> {
    let mut def_traits = HashMap::new();
    for (_, item) in macro_items {
        if let Some((macro_name, trait_name)) = macro_generated_trait(item) {
            def_traits.entry(macro_name).or_insert(trait_name);
        }
    }
    let mut out = Vec::new();
    for (module_path, item) in macro_items {
        if item.ident.is_some() {
            continue;
        }
        let Some(last) = item.mac.path.segments.last() else {
            continue;
        };
        let macro_name = last.ident.to_string();
        if macro_name == "bitflags" || macro_name == "id_u64" {
            continue;
        }
        let Some(trait_name) = def_traits.get(&macro_name) else {
            continue;
        };
        let methods = parse_macro_trait_methods(item, features);
        if methods.is_empty() {
            continue;
        }
        index
            .pub_types
            .entry(module_path.clone())
            .or_default()
            .insert(trait_name.clone());
        out.push((module_path.clone(), trait_name.clone(), methods));
    }
    out
}

/// Whether a return type is the unit type (absent or `()`).
fn is_unit_return(output: &syn::ReturnType) -> bool {
    match output {
        syn::ReturnType::Default => true,
        syn::ReturnType::Type(_, ty) => {
            matches!(ty.as_ref(), syn::Type::Tuple(tuple) if tuple.elems.is_empty())
        }
    }
}

/// Inventory one `pub trait` definition into overridable methods.
/// Parameter types classify with the crate-wide known-types map, so
/// framework-owned values (`Context`, `Message`) marshal as object
/// handles; anything else (including nested callbacks) skips the method.
fn inventory_trait_def(
    module_path: &[String],
    item: &syn::ItemTrait,
    index: &CrateIndex,
    exports_memo: &mut HashMap<(Vec<String>, String), bool>,
    known_types: &HashMap<String, String>,
) -> Option<(String, TraitInventory)> {
    // A generic trait cannot be implemented without choosing its params.
    if !item.generics.params.is_empty() {
        return None;
    }
    // A trait with supertraits the adaptor cannot name (`IntoUrl:
    // IntoUrlSealed`, sealed) cannot be implemented from the adaptor at
    // all: the generated impl would not compile. Auto traits ride along
    // on the generated struct and need no explicit impl.
    for bound in &item.supertraits {
        let syn::TypeParamBound::Trait(trait_bound) = bound else {
            continue;
        };
        let Some(segment) = trait_bound.path.segments.last() else {
            continue;
        };
        if matches!(
            segment.ident.to_string().as_str(),
            "Send" | "Sync" | "Unpin"
        ) {
            continue;
        }
        root_type_path(module_path, &segment.ident.to_string(), index, exports_memo)?;
    }
    let short = item.ident.to_string();
    let path = root_type_path(module_path, &short, index, exports_memo)?;
    let mut methods = Vec::new();
    let mut usable = true;
    for trait_item in &item.items {
        let syn::TraitItem::Fn(method) = trait_item else {
            continue;
        };
        // Method-level generics, `const`, and exotic ABIs have no bridge.
        if !method.sig.generics.params.is_empty()
            || method.sig.constness.is_some()
            || method.sig.abi.is_some()
        {
            if method.default.is_none() {
                usable = false;
            }
            continue;
        }
        let mut params = Vec::new();
        let mut param_ref = Vec::new();
        let mut receiver = None;
        let mut ok = true;
        for input in &method.sig.inputs {
            match input {
                syn::FnArg::Receiver(recv) => {
                    // The override must reproduce the trait's receiver; a
                    // typed receiver of a shape the registry cannot supply
                    // (`self: &Arc<Self>`) has no bridgeable signature.
                    let Some(kind) = receiver_kind(recv, None) else {
                        if method.default.is_none() {
                            usable = false;
                        }
                        ok = false;
                        break;
                    };
                    receiver = Some(kind);
                }
                syn::FnArg::Typed(typed) => {
                    // Borrowed framework values cannot be stored in the
                    // event queue, so `&mut` object parameters (like nested
                    // callbacks) skip the method. Foreign borrows skip for
                    // the same reason: the referent is call-local, and only
                    // owned foreign values can move into shared space.
                    let qualified_ty = qualify_foreign_ty(&typed.ty, module_path, index);
                    match classify_trait_param(
                        module_path,
                        &qualified_ty,
                        index,
                        exports_memo,
                        known_types,
                    ) {
                        Some((kind, ref_kind))
                            if !matches!(kind, ParamKind::Callback(_))
                                && !matches!(
                                    (&kind, ref_kind),
                                    (ParamKind::Object(_), ParamRef::MutRef)
                                        | (ParamKind::Array(_), ParamRef::MutRef)
                                        | (ParamKind::FixedArray(_, _), ParamRef::MutRef)
                                        | (ParamKind::CowStr, ParamRef::MutRef)
                                        | (ParamKind::CowBytes, ParamRef::MutRef)
                                        | (ParamKind::OptionArray(_), ParamRef::MutRef)
                                )
                                && !matches!(
                                    &kind,
                                    ParamKind::Object(obj)
                                        if obj.is_foreign()
                                            && !matches!(ref_kind, ParamRef::Owned)
                                )
                                && !matches!(
                                    (&kind, ref_kind),
                                    (ParamKind::OptionObject(_), ParamRef::Ref)
                                        | (ParamKind::OptionObject(_), ParamRef::MutRef)
                                ) =>
                        {
                            params.push(kind);
                            param_ref.push(ref_kind);
                        }
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
            }
        }
        let ret_unit = is_unit_return(&method.sig.output);
        let Some(receiver) = receiver else {
            // Static methods cannot tag events with the handler key.
            if method.default.is_none() {
                usable = false;
            }
            continue;
        };
        if !ok || !ret_unit {
            if method.default.is_none() {
                usable = false;
            }
            continue;
        }
        methods.push(InventoriedMethod {
            name: method.sig.ident.to_string(),
            params,
            param_ref,
            receiver,
            is_async: method.sig.asyncness.is_some(),
            is_unsafe: method.sig.unsafety.is_some(),
        });
    }
    Some((
        short,
        TraitInventory {
            path,
            methods,
            usable,
        },
    ))
}

/// Inventory the event-style methods of one macro invocation. Every entry
/// is `&self`-receiver, unit-returning, and defaulted (the macro
/// definition supplies the no-op bodies), so only parameter
/// classification can fail. Parameters resolve in the invoking module's
/// scope (`event_handler!`-generated traits live in a module whose glob
/// imports name the event payload types).
fn inventory_macro_invocation(
    module_path: &[String],
    methods: &[MacroTraitMethod],
    index: &CrateIndex,
    exports_memo: &mut HashMap<(Vec<String>, String), bool>,
    known_types: &HashMap<String, String>,
) -> Vec<InventoriedMethod> {
    let mut out = Vec::new();
    for method in methods {
        let mut params = Vec::new();
        let mut param_ref = Vec::new();
        let mut ok = true;
        for (_, ty) in &method.args {
            let qualified_ty = qualify_foreign_ty(ty, module_path, index);
            match classify_trait_param(module_path, &qualified_ty, index, exports_memo, known_types)
            {
                Some((kind, ref_kind))
                    if !matches!(kind, ParamKind::Callback(_))
                        && !matches!(
                            (&kind, ref_kind),
                            (ParamKind::Object(_), ParamRef::MutRef)
                                | (ParamKind::Array(_), ParamRef::MutRef)
                                | (ParamKind::FixedArray(_, _), ParamRef::MutRef)
                                | (ParamKind::CowStr, ParamRef::MutRef)
                                | (ParamKind::CowBytes, ParamRef::MutRef)
                                | (ParamKind::OptionArray(_), ParamRef::MutRef)
                        )
                        && !matches!(
                            &kind,
                            ParamKind::Object(obj)
                                if obj.is_foreign()
                                    && !matches!(ref_kind, ParamRef::Owned)
                        )
                        && !matches!(
                            (&kind, ref_kind),
                            (ParamKind::OptionObject(_), ParamRef::Ref)
                                | (ParamKind::OptionObject(_), ParamRef::MutRef)
                        ) =>
                {
                    params.push(kind);
                    param_ref.push(ref_kind);
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        out.push(InventoriedMethod {
            name: method.name.clone(),
            params,
            param_ref,
            receiver: ReceiverKind::Ref,
            is_async: method.is_async,
            is_unsafe: false,
        });
    }
    out
}

/// Maximum monomorphized bridges generated for one behavior-bound
/// parameter. A widely-implemented trait (`Serialize`) would otherwise
/// explode the adaptor; beyond the cap the function stays unsupported
/// with a reason instead.
///
/// The cap is 24 so two-way `CacheHttp` (`Context`, `Http`) pairs combined
/// with a second small behavior bound still bridge (2x12, 3x8, 4x6), while
/// truly open-ended bounds (`Serialize`, 76-way expansions) stay listed
/// instead of exploding the adaptor.
const MAX_MONO_FAMILY: usize = 24;

/// The `_with_<Type>` suffix naming one monomorphized bridge.
fn mono_suffix(type_path: &str) -> String {
    format!(
        "_with_{}",
        type_path.rsplit("::").next().unwrap_or(type_path)
    )
}

/// Rewrite `Callback(short)` parameters to their inventoried full paths
/// and expand behavior-bound parameters over in-crate implementors (one
/// bridge per concrete implementor, e.g. `ChannelId_send_message` over
/// `{Http, Context}` for `impl CacheHttp`). Functions whose callback
/// trait has neither implementors nor an inventory entry are removed and
/// reported as unsupported.
#[allow(clippy::too_many_arguments)]
fn resolve_callback_params(
    functions: &mut Vec<Func>,
    unsupported: &mut Vec<UnsupportedApi>,
    traits: &HashMap<String, TraitInventory>,
    impl_edges: &HashMap<String, Vec<(String, String)>>,
    known_types: &HashMap<String, String>,
) {
    let mut kept = Vec::new();
    for func in std::mem::take(functions) {
        if !func
            .params
            .iter()
            .any(|kind| matches!(kind, ParamKind::Callback(_)))
        {
            kept.push(func);
            continue;
        }
        let mut options: Vec<Vec<ParamKind>> = Vec::new();
        let mut suffixes: Vec<Vec<String>> = Vec::new();
        let mut failed: Option<String> = None;
        for kind in &func.params {
            let ParamKind::Callback(short) = kind else {
                options.push(vec![kind.clone()]);
                suffixes.push(vec![String::new()]);
                continue;
            };
            let short = short.rsplit("::").next().unwrap_or(short).to_string();
            // The bound may carry generic arguments (`PartialEq<JsonValue>`);
            // an implementor edge is only usable when its own arguments match.
            let (short, bound_args) = match short.split_once('<') {
                Some((bare, args)) => (bare.to_string(), format!("<{args}")),
                None => (short.clone(), String::new()),
            };
            let mut implementors: Vec<String> = impl_edges
                .get(&short)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|(edge_args, _)| *edge_args == bound_args)
                .filter_map(|(_, bare)| known_types.get(&bare).cloned())
                .collect();
            implementors.sort();
            implementors.dedup();
            if implementors.is_empty() {
                match traits.get(&short) {
                    Some(inventory) if inventory.usable => {
                        options.push(vec![ParamKind::Callback(inventory.path.clone())]);
                        suffixes.push(vec![String::new()]);
                    }
                    _ => {
                        failed = Some(format!(
                            "callback trait `{short}` has no bridgeable inventory"
                        ));
                        break;
                    }
                }
            } else {
                suffixes.push(implementors.iter().map(|p| mono_suffix(p)).collect());
                options.push(
                    implementors
                        .into_iter()
                        .map(|path| ParamKind::Object(path.into()))
                        .collect(),
                );
            }
        }
        if let Some(reason) = failed {
            unsupported.push(UnsupportedApi {
                name: func.name.clone(),
                path: func.call_path.clone(),
                status: crate::metadata::ApiStatus::Unsupported,
                reason,
                // The original signature is gone by this stage (only the
                // classified kinds remain); the reason names the cause.
                signature: String::new(),
            });
            continue;
        }
        let combos: usize = options.iter().map(Vec::len).product();
        if combos > MAX_MONO_FAMILY {
            unsupported.push(UnsupportedApi {
                name: func.name.clone(),
                path: func.call_path.clone(),
                status: crate::metadata::ApiStatus::Unsupported,
                reason: format!(
                    "behavior bound expands to {combos} bridges (cap {MAX_MONO_FAMILY})"
                ),
                signature: String::new(),
            });
            continue;
        }
        let mut acc = vec![func];
        for (pos, opts) in options.iter().enumerate() {
            let mut next = Vec::new();
            for func in acc {
                for (kind, suffix) in opts.iter().zip(&suffixes[pos]) {
                    let mut expanded = func.clone();
                    expanded.params[pos] = kind.clone();
                    expanded.name.push_str(suffix);
                    next.push(expanded);
                }
            }
            acc = next;
        }
        kept.extend(acc);
    }
    *functions = kept;
}

/// Count `(self type, trait, method)` triples over trait impl blocks.
/// Coherence forbids the same trait twice for one type, so a count above
/// one means distinct trait arguments (`AsRef<Http>` vs `AsRef<Cache>`)
/// whose shared methods (`as_ref`) no call site can name without a
/// turbofish the bridge cannot emit. Generic impls are excluded: their
/// methods never bridge, and coherence forbids them from overlapping a
/// concrete bridge on any real call.
fn count_trait_impl_methods(
    impls: &[(Vec<String>, syn::ItemImpl)],
) -> HashMap<(String, String, String), usize> {
    let mut counts = HashMap::new();
    for (_, impl_item) in impls {
        if !impl_item.generics.params.is_empty() {
            continue;
        }
        let Some((_, trait_path, _)) = &impl_item.trait_ else {
            continue;
        };
        let Some(trait_short) = trait_path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            continue;
        };
        let self_bare = match impl_item.self_ty.as_ref() {
            syn::Type::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => {
                path.path.segments[0].ident.to_string()
            }
            _ => continue,
        };
        for item in &impl_item.items {
            if let syn::ImplItem::Fn(method) = item {
                *counts
                    .entry((
                        self_bare.clone(),
                        trait_short.clone(),
                        method.sig.ident.to_string(),
                    ))
                    .or_default() += 1;
            }
        }
    }
    counts
}

/// Collect `(trait short name, bare self-type name)` edges from impl
/// blocks: `impl CacheHttp for Http` records `("CacheHttp", "Http")`.
/// Generic impls and non-plain self types record nothing — a bridge must
/// name one concrete, storable type.
/// One implementor edge with the trait's rendered generic arguments: the
/// monomorphization only uses an edge when the bound's arguments match.
/// Render a trait path's generic arguments for bound matching
/// (`PartialEq<JsonValue>` → `"<JsonValue>"`, no args → `""`).
fn trait_args_string(segment: &syn::PathSegment) -> String {
    match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => {
            let parts: Vec<String> = args
                .args
                .iter()
                .map(|arg| match arg {
                    syn::GenericArgument::Type(ty) => resolve_type_name(ty).unwrap_or_default(),
                    syn::GenericArgument::Lifetime(lt) => lt.to_string(),
                    _ => String::new(),
                })
                .collect();
            format!("<{}>", parts.join(", "))
        }
        _ => String::new(),
    }
}

fn collect_impl_edges(
    impls: &[(Vec<String>, syn::ItemImpl)],
) -> HashMap<String, Vec<(String, String)>> {
    let mut edges: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (_, impl_item) in impls {
        if !impl_item.generics.params.is_empty() {
            continue;
        }
        let Some((_, trait_path, _)) = &impl_item.trait_ else {
            continue;
        };
        let Some(trait_short) = trait_path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            continue;
        };
        let syn::Type::Path(self_path) = impl_item.self_ty.as_ref() else {
            continue;
        };
        if self_path.qself.is_some() || self_path.path.segments.len() != 1 {
            continue;
        };
        let segment = &self_path.path.segments[0];
        if !matches!(segment.arguments, syn::PathArguments::None) {
            continue;
        }
        // The trait's own generic arguments travel with the edge: a
        // monomorphization may only use it when the bound's arguments match
        // (`PartialEq<JsonValue>` never matches `impl PartialEq for Short` —
        // whether one type satisfies the other's bound is exactly what the
        // impl decides).
        let trait_args = trait_args_string(trait_path.segments.last().expect("non-empty"));
        edges
            .entry(trait_short)
            .or_default()
            .push((trait_args, segment.ident.to_string()));
    }
    edges
}

/// Scan the dependency source for marshalable free `pub fn`s.
///
/// Follows the crate's reachable module graph and keeps only functions whose
/// parameter and return types are tier-1 marshalable (see [`classify`]).
pub fn discover_public_functions(dep_src: &Path) -> Vec<Func> {
    discover_public_api(dep_src).functions
}

pub fn discover_public_api(dep_src: &Path) -> DiscoveredApi {
    discover_public_api_with_features(dep_src, None)
}

/// Discover a dependency's public API under an explicit Cargo feature set.
/// `None` preserves the historical default-feature build view used by docs
/// and callers that do not have a manifest dependency specification.
pub fn discover_public_api_with_features(
    dep_src: &Path,
    requested_features: Option<&std::collections::HashSet<String>>,
) -> DiscoveredApi {
    let root = crate_source_root(dep_src);
    if !root.is_file() {
        return DiscoveredApi::default();
    }

    // Parse only files reachable from the crate root's module graph. A
    // pre-pass over every `.rs` file made large crates pay for private,
    // platform-specific, and otherwise unreachable source before discovery
    // could filter it; `walk_file` already has the correct on-demand parser.
    let mut collector = FnCollector {
        features: requested_features
            .cloned()
            .unwrap_or_else(|| crate_default_features(dep_src)),
        ..FnCollector::default()
    };
    collector.index.direct_deps = direct_dep_names(dep_src);
    let mut visited = std::collections::HashSet::new();
    collector.index.public_chain.insert(Vec::new(), true);
    collector.walk_file(&root, root.parent().unwrap_or(dep_src), &mut visited);

    // Macro-generated items are inventoried before the type index is
    // built: `macro_rules!` definitions reveal which macros generate
    // traits, and matching invocations contribute their type and method
    // names to the public index.
    let macro_methods = inventory_macro_traits(
        &collector.macro_items,
        &collector.features,
        &mut collector.index,
    );

    // Classify impl blocks and free fns now that the whole module graph
    // and its re-export edges are known (an impl's type may be nameable
    // only through a re-export chain, and struct-typed params need the
    // full type index). The known-types map resolves a bare local type
    // name to its root-callable path.
    let mut exports_memo = HashMap::new();
    let mut known_types = HashMap::new();
    let mut ambiguous_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (module_path, type_name) in collector
        .index
        .pub_types
        .iter()
        .flat_map(|(module, types)| std::iter::repeat(module.clone()).zip(types.iter().cloned()))
    {
        let Some(type_path) = root_type_path(
            &module_path,
            &type_name,
            &collector.index,
            &mut exports_memo,
        ) else {
            continue;
        };
        // Qualified spellings resolve unambiguously by their written path
        // (`crate::model::id::ChannelId`), regardless of any bare-name
        // collision below — a foreign path (`reqwest::Client`) must never
        // match by its bare last segment.
        known_types.insert(
            format!("{}::{type_name}", module_path.join("::")),
            type_path.clone(),
        );
        if !module_path.is_empty() {
            known_types.insert(
                format!("crate::{}::{type_name}", module_path.join("::")),
                type_path.clone(),
            );
        }
        if collector.generic_types.contains(&type_name) || ambiguous_names.contains(&type_name) {
            // A generic type's bare name cannot supply its generic
            // arguments (`CreatePoll<S>`), and two distinct types sharing
            // a bare name (`serenity` has two `Ready`s) cannot be told
            // apart without import tracking — neither resolves.
            continue;
        }
        match known_types.entry(type_name.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(type_path);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                // A second definition site claims the same bare name
                // (`struct Check` plus `type Check = fn(...)`): which one a
                // signature means is unknowable without full type checking,
                // so neither may claim it.
                known_types.remove(&type_name);
                ambiguous_names.insert(type_name);
            }
        }
    }
    // Signatures commonly use a module-relative spelling imported through a
    // glob (`track::TrackData` while the root path is `model::track::TrackData`).
    // Preserve every unambiguous path suffix so nested collection elements
    // resolve through the same crate-wide map without crate-specific rules.
    let qualified_types: Vec<(String, String)> = known_types
        .iter()
        .filter(|(key, _)| key.contains("::") && !key.starts_with("crate::"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut ambiguous_suffixes = std::collections::HashSet::new();
    for (key, value) in qualified_types {
        let segments: Vec<&str> = key.split("::").collect();
        if segments
            .last()
            .is_some_and(|name| collector.generic_types.contains(*name))
        {
            continue;
        }
        for start in 1..segments.len() {
            let suffix = segments[start..].join("::");
            if ambiguous_suffixes.contains(&suffix) {
                continue;
            }
            match known_types.entry(suffix) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(value.clone());
                }
                std::collections::hash_map::Entry::Occupied(entry) if entry.get() != &value => {
                    ambiguous_suffixes.insert(entry.key().clone());
                    entry.remove();
                }
                _ => {}
            }
        }
    }
    let mut unsupported = Vec::new();
    // Behavior-bound parameters resolve after classification: the trait
    // inventory (plain `pub trait` defs plus macro-generated methods) and
    // the in-crate implementor edges are both complete only now.
    let mut trait_inventory: HashMap<String, TraitInventory> = HashMap::new();
    for (module_path, trait_item) in std::mem::take(&mut collector.trait_defs) {
        if let Some((short, inventory)) = inventory_trait_def(
            &module_path,
            &trait_item,
            &collector.index,
            &mut exports_memo,
            &known_types,
        ) {
            trait_inventory.entry(short).or_insert(inventory);
        }
    }
    for (module_path, trait_name, methods) in &macro_methods {
        // The macro trait resolves through the same re-export machinery
        // as plain types (its name was registered as a public type above).
        let Some(path) =
            root_type_path(module_path, trait_name, &collector.index, &mut exports_memo)
        else {
            continue;
        };
        trait_inventory
            .entry(trait_name.clone())
            .or_insert(TraitInventory {
                path,
                methods: inventory_macro_invocation(
                    module_path,
                    methods,
                    &collector.index,
                    &mut exports_memo,
                    &known_types,
                ),
                usable: true,
            });
    }
    let impl_edges = collect_impl_edges(&collector.impls);
    for (module_path, fn_item) in std::mem::take(&mut collector.free_fns) {
        if !matches!(fn_item.vis, syn::Visibility::Public(_)) {
            continue;
        }
        let module_public = collector
            .index
            .public_chain
            .get(&module_path)
            .copied()
            .unwrap_or(false);
        let fn_name = fn_item.sig.ident.to_string();
        let expanded_fn_item = expand_fn_aliases(&fn_item, &module_path, &collector.index);
        // A function in a private module may still be callable when the
        // crate `pub use`s it at a public module (e.g. base64 re-exports
        // `encode::encode`, `decode::decode` from private modules).
        let root_path = if module_public {
            None
        } else {
            root_function_path(&module_path, &fn_name, &collector.index)
        };
        let effective_path: &[String] = root_path.as_ref().map_or(&module_path, |(dest, _)| dest);
        // A bare parameter type shadowed by a foreign import
        // (`use reqwest::Client`) has a type outside the crate entirely.
        let foreign_param = expanded_fn_item.sig.inputs.iter().any(|input| {
            matches!(input, syn::FnArg::Typed(typed)
                if foreign_shadowed(&module_path, &typed.ty, &collector.index))
        });
        match classify_fn(
            &expanded_fn_item,
            effective_path,
            &collector.features,
            Some(&known_types),
            &collector.clone_types,
            &collector.index,
        ) {
            Some(mut func) if (module_public || root_path.is_some()) && !foreign_param => {
                // Rewrite the call path to the public re-export position so
                // the bridge names a reachable path, under the name the
                // re-export gives it.
                if let Some((root, exported_name)) = root_path
                    && !module_public
                {
                    func.call_path = if root.is_empty() {
                        exported_name
                    } else {
                        format!("{}::{exported_name}", root.join("::"))
                    };
                }
                collector.functions.push(func);
            }
            _ if module_public || root_path.is_some() => {
                let path = if module_path.is_empty() {
                    fn_name.clone()
                } else {
                    format!("{}::{fn_name}", module_path.join("::"))
                };
                let reason = if expanded_fn_item.sig.unsafety.is_some() {
                    "unsafe function is excluded from the safe bridge ABI"
                } else {
                    "public signature is outside the typed bridge ABI"
                };
                unsupported.push(UnsupportedApi {
                    name: fn_name,
                    path,
                    status: crate::metadata::ApiStatus::Unsupported,
                    reason: reason.into(),
                    signature: render_sig(&expanded_fn_item.sig),
                });
            }
            // A `pub fn` inside a private module is not part of the crate's
            // public API unless a root re-export reaches it. Do not turn
            // implementation helpers (serde hooks, parser internals, etc.)
            // into user-visible unsupported diagnostics.
            _ => {}
        }
    }
    for (module_path, foreign_item) in std::mem::take(&mut collector.foreign_functions) {
        if !matches!(foreign_item.vis, syn::Visibility::Public(_)) {
            continue;
        }
        let module_public = collector
            .index
            .public_chain
            .get(&module_path)
            .copied()
            .unwrap_or(false);
        let name = foreign_item.sig.ident.to_string();
        let root_path = if module_public {
            None
        } else {
            root_function_path(&module_path, &name, &collector.index)
        };
        if !module_public && root_path.is_none() {
            continue;
        }
        let path = if let Some((root, exported_name)) = root_path {
            if root.is_empty() {
                exported_name
            } else {
                format!("{}::{exported_name}", root.join("::"))
            }
        } else if module_path.is_empty() {
            name.clone()
        } else {
            format!("{}::{name}", module_path.join("::"))
        };
        unsupported.push(UnsupportedApi {
            name,
            path,
            status: crate::metadata::ApiStatus::Unsupported,
            reason: "foreign functions require an explicit unsafe FFI bridge and are not emitted into the safe adaptor".into(),
            signature: render_sig(&foreign_item.sig),
        });
    }
    // Shared trait methods (`as_ref` across `AsRef<Http>`/`AsRef<Cache>`)
    // are ambiguous at method-call sites; counted once up front so every
    // impl block sees the same totals.
    let trait_method_counts = count_trait_impl_methods(&collector.impls);
    for (module_path, impl_item) in std::mem::take(&mut collector.impls) {
        // The impl's methods bridge whenever the type itself is nameable
        // from the crate root — including through a `pub use` re-export
        // out of a private module (`builder::CreateMessage`). An
        // unreachable type fails `root_type_path` inside `classify_impl`
        // and yields nothing, so no separate module-visibility check is
        // needed here.
        let expanded_impl_item = expand_impl_aliases(&impl_item, &module_path, &collector.index);
        let functions = collector.functions;
        let (mut classified, mut impl_unsupported) = classify_impl(
            &expanded_impl_item,
            &module_path,
            ImplClassifyCtx {
                index: &collector.index,
                exports_memo: &mut exports_memo,
                features: &collector.features,
                known_types: Some(&known_types),
                clone_types: &collector.clone_types,
                trait_method_counts: &trait_method_counts,
            },
        );
        collector.functions = functions;
        collector.functions.append(&mut classified);
        unsupported.append(&mut impl_unsupported);
    }

    // Behavior bounds become concrete here: callbacks rewrite to their
    // inventoried trait paths, `impl Trait` over in-crate implementors
    // expands into monomorphized bridges, and the rest is reported.
    resolve_callback_params(
        &mut collector.functions,
        &mut unsupported,
        &trait_inventory,
        &impl_edges,
        &known_types,
    );

    // Macro-generated newtypes (`id_u64!`) gain `new(u64)` constructors
    // and borrowing `get()` readers; the macro definition (not the crate
    // source) owns the real methods, so both bridges are synthesized.
    for (module_path, type_name) in std::mem::take(&mut collector.id_types) {
        let Some(type_path) = root_type_path(
            &module_path,
            &type_name,
            &collector.index,
            &mut exports_memo,
        ) else {
            continue;
        };
        let short_name = type_path.rsplit("::").next().unwrap_or(&type_path);
        collector.functions.push(Func {
            name: sanitize_ident(&format!("{short_name}_new")),
            call_path: format!("{type_path}::new"),
            method_call: false,
            is_field: false,
            is_setter: false,
            trait_path: None,
            unit_variant: false,
            params: vec![ParamKind::Int("u64".into())],
            field_names: Vec::new(),
            param_ref: vec![ParamRef::Owned],
            ret: RetKind::Object(type_path.clone().into()),
            is_async: false,
            doc: String::new(),
            is_unsafe: false,
        });
        collector.functions.push(Func {
            name: sanitize_ident(&format!("{short_name}_get")),
            // A by-value `get(self)` on a `Copy` newtype reads through a
            // method call on the borrowed handle, so polling an id never
            // consumes the handle Neutron holds.
            call_path: format!("{type_path}::get"),
            method_call: true,
            is_field: false,
            is_setter: false,
            trait_path: None,
            unit_variant: false,
            params: vec![ParamKind::Object(type_path.clone().into())],
            field_names: Vec::new(),
            param_ref: vec![ParamRef::Ref],
            ret: RetKind::Int("u64".into()),
            is_async: false,
            doc: String::new(),
            is_unsafe: false,
        });
    }

    // `bitflags!` types gain their pure set operations. The methods exist
    // on every bitflags 1.x/2.x output (`empty`, `all`, `union`,
    // `intersection`, `difference`) but are invisible to the source scan,
    // so the bridges name them directly; the adaptor build verifies each
    // one.
    for (module_path, type_name) in std::mem::take(&mut collector.bitflags_types) {
        let Some(type_path) = root_type_path(
            &module_path,
            &type_name,
            &collector.index,
            &mut exports_memo,
        ) else {
            continue;
        };
        let short_name = type_path.rsplit("::").next().unwrap_or(&type_path);
        for nullary in ["empty", "all"] {
            collector.functions.push(Func {
                name: sanitize_ident(&format!("{short_name}_{nullary}")),
                call_path: format!("{type_path}::{nullary}"),
                method_call: false,
                is_field: false,
                is_setter: false,
                trait_path: None,
                unit_variant: false,
                params: Vec::new(),
                field_names: Vec::new(),
                param_ref: Vec::new(),
                ret: RetKind::Object(type_path.clone().into()),
                is_async: false,
                doc: String::new(),
                is_unsafe: false,
            });
        }
        for binary in ["union", "intersection", "difference"] {
            collector.functions.push(Func {
                name: sanitize_ident(&format!("{short_name}_{binary}")),
                call_path: format!("{type_path}::{binary}"),
                method_call: false,
                is_field: false,
                is_setter: false,
                trait_path: None,
                unit_variant: false,
                params: vec![
                    ParamKind::Object(type_path.clone().into()),
                    ParamKind::Object(type_path.clone().into()),
                ],
                field_names: Vec::new(),
                param_ref: vec![ParamRef::Owned, ParamRef::Owned],
                ret: RetKind::Object(type_path.clone().into()),
                is_async: false,
                doc: String::new(),
                is_unsafe: false,
            });
        }
    }

    // Public named fields with tier-1 types gain `Type_field` readers, so
    // Neutron can inspect event payloads (`Message.content`). Object
    // fields clone into fresh handles; the adaptor build (which sees the
    // real `Clone` impls) verifies each one. A field whose reader would
    // collide with an existing bridge keeps the existing one.
    let mut taken_names: std::collections::HashSet<String> =
        collector.functions.iter().map(|f| f.name.clone()).collect();
    for (module_path, struct_name, fields) in std::mem::take(&mut collector.struct_fields) {
        let Some(type_path) = root_type_path(
            &module_path,
            &struct_name,
            &collector.index,
            &mut exports_memo,
        ) else {
            continue;
        };
        let short_name = type_path.rsplit("::").next().unwrap_or(&type_path);
        for (field_name, field_ty) in &fields {
            let getter = format!("{short_name}_{field_name}");
            if !taken_names.insert(getter.clone()) {
                continue;
            }
            let Some((kind, _)) = classify_param_with_ref(
                field_ty,
                &HashMap::new(),
                Some(&known_types),
                &collector.index,
            ) else {
                continue;
            };
            if matches!(kind, ParamKind::Callback(_)) {
                continue;
            }
            let ret = match &kind {
                ParamKind::Str | ParamKind::CowStr => RetKind::Str,
                ParamKind::Bytes | ParamKind::CowBytes => RetKind::Bytes,
                ParamKind::Array(elem) => RetKind::Array(elem.clone()),
                // A fixed-array field getter returns the array handle,
                // exactly like a `Vec<T>` field (`[T; N]` supports the same
                // `len`/`iter` the array builder uses).
                ParamKind::FixedArray(elem, _) => RetKind::Array(elem.clone()),
                ParamKind::OptionArray(elem) => RetKind::OptionArray(elem.clone()),
                ParamKind::OptionInt(ty) => RetKind::OptionInt(ty.clone()),
                ParamKind::OptionBool => RetKind::OptionBool,
                ParamKind::OptionFloat(ty) => RetKind::OptionFloat(ty.clone()),
                ParamKind::OptionStr => RetKind::OptionStr,
                // Borrowed options would dangle past the borrow release;
                // they bridge as parameters only.
                ParamKind::OptionStrRef => continue,
                // Duration fields have no return label that round-trips
                // (the bridge would need a millis convention on top of a
                // field read); durations bridge as parameters only.
                ParamKind::Duration => continue,
                ParamKind::OptionObject(obj) => {
                    // Like plain object fields, option-wrapped objects
                    // clone (`None` passes through); the inner bare name
                    // must prove `Clone` the same way.
                    let inner_bare = match field_ty {
                        syn::Type::Path(path) if path.qself.is_none() => path
                            .path
                            .segments
                            .last()
                            .and_then(|segment| match &segment.arguments {
                                syn::PathArguments::AngleBracketed(args) => {
                                    first_type_arg(args).and_then(resolve_type_name)
                                }
                                _ => None,
                            }),
                        _ => None,
                    };
                    if !inner_bare.is_some_and(|name| collector.clone_types.contains(&name)) {
                        continue;
                    }
                    RetKind::OptionObject(obj.clone())
                }
                ParamKind::Int(ty) => RetKind::Int(ty.clone()),
                ParamKind::Bool => RetKind::Bool,
                ParamKind::Float(ty) => RetKind::Float(ty.clone()),
                ParamKind::Object(path) => {
                    // The getter clones the field value out of the stored
                    // object, so only `Clone` types can bridge
                    // (`WsClient` has no `Clone`).
                    let bare = resolve_type_name(field_ty);
                    if !bare.is_some_and(|name| collector.clone_types.contains(&name)) {
                        continue;
                    }
                    RetKind::Object(path.clone())
                }
                ParamKind::Callback(_) => continue,
            };
            collector.functions.push(Func {
                name: sanitize_ident(&getter),
                call_path: format!("{type_path}::{field_name}"),
                method_call: false,
                is_field: true,
                is_setter: false,
                trait_path: None,
                unit_variant: false,
                params: vec![ParamKind::Object(type_path.clone().into())],
                field_names: vec![field_name.clone()],
                param_ref: vec![ParamRef::Ref],
                ret,
                is_async: false,
                doc: String::new(),
                is_unsafe: false,
            });
            // The matching setter writes the same tier-1 kinds back:
            // scalars and strings copy, objects move in. Fields whose
            // type borrows call-local data (`&str`, `Option<&str>`)
            // cannot be stored soundly, and callbacks have no value
            // spelling: both stay read-only.
            if !matches!(kind, ParamKind::Callback(_)) && !type_has_top_borrow(field_ty) {
                let setter = format!("{short_name}_set_{field_name}");
                if taken_names.insert(setter.clone()) {
                    collector.functions.push(Func {
                        name: sanitize_ident(&setter),
                        call_path: format!("{type_path}::{field_name}"),
                        method_call: false,
                        is_field: true,
                        is_setter: true,
                        trait_path: None,
                        unit_variant: false,
                        params: vec![ParamKind::Object(type_path.clone().into()), kind.clone()],
                        field_names: vec![field_name.clone()],
                        param_ref: vec![ParamRef::Ref, ParamRef::Owned],
                        ret: RetKind::Unit,
                        is_async: false,
                        doc: String::new(),
                        is_unsafe: false,
                    });
                }
            }
        }
    }

    // Unit enum variants become zero-arg constructors returning an
    // object handle of the enum value.
    for (module_path, enum_name, variant_name) in std::mem::take(&mut collector.enum_unit_variants)
    {
        if let Some(type_path) = root_type_path(
            &module_path,
            &enum_name,
            &collector.index,
            &mut exports_memo,
        ) {
            let short_name = type_path.rsplit("::").next().unwrap_or(&type_path);
            collector.functions.push(Func {
                name: sanitize_ident(&format!("{short_name}_{variant_name}")),
                call_path: format!("{type_path}::{variant_name}"),
                method_call: false,
                is_field: false,
                is_setter: false,
                trait_path: None,
                unit_variant: true,
                params: Vec::new(),
                field_names: Vec::new(),
                param_ref: Vec::new(),
                ret: RetKind::Object(type_path.clone().into()),
                is_async: false,
                doc: String::new(),
                is_unsafe: false,
            });
        }
    }

    // Tuple enum variants with tier-1 field types become constructors
    // taking marshaled arguments, still returning an object handle. A
    // field shadowed by a foreign import (`Message` from tungstenite)
    // makes the variant unconstructable.
    for (module_path, enum_name, variant_name, field_types, field_names) in
        std::mem::take(&mut collector.enum_tuple_variants)
    {
        if field_types
            .iter()
            .any(|field_ty| foreign_shadowed(&module_path, field_ty, &collector.index))
        {
            continue;
        }
        let Some(type_path) = root_type_path(
            &module_path,
            &enum_name,
            &collector.index,
            &mut exports_memo,
        ) else {
            continue;
        };
        let mut params = Vec::new();
        let mut param_ref = Vec::new();
        let mut all_tier1 = true;
        for field_ty in &field_types {
            match classify_param_with_ref(
                field_ty,
                &HashMap::new(),
                Some(&known_types),
                &collector.index,
            ) {
                Some((kind, ref_kind)) => {
                    params.push(kind);
                    param_ref.push(ref_kind);
                }
                None => {
                    all_tier1 = false;
                    break;
                }
            }
        }
        if !all_tier1 {
            continue;
        }
        let short_name = type_path.rsplit("::").next().unwrap_or(&type_path);
        collector.functions.push(Func {
            name: sanitize_ident(&format!("{short_name}_{variant_name}")),
            call_path: format!("{type_path}::{variant_name}"),
            method_call: false,
            is_field: false,
            is_setter: false,
            trait_path: None,
            unit_variant: false,
            params,
            field_names,
            param_ref,
            ret: RetKind::Object(type_path.clone().into()),
            is_async: false,
            doc: String::new(),
            is_unsafe: false,
        });
    }

    // Compiler-derived `Default` impls have no `impl Default` block to
    // discover, so `#[derive(Default)]` types gain a `Type_default()`
    // bridge here, mirroring an explicit `Default::default` exactly
    // (path call, `Default` import). An inherent `default` already
    // bridged keeps its bridge: path calls resolve to the inherent
    // item, so synthesizing beside it would call the wrong function.
    for (module_path, type_name) in std::mem::take(&mut collector.default_types) {
        let Some(type_path) = root_type_path(
            &module_path,
            &type_name,
            &collector.index,
            &mut exports_memo,
        ) else {
            continue;
        };
        let short_name = type_path.rsplit("::").next().unwrap_or(&type_path);
        let bridge_name = sanitize_ident(&format!("{short_name}_default"));
        if collector
            .functions
            .iter()
            .any(|func| func.name == bridge_name)
        {
            continue;
        }
        collector.functions.push(Func {
            name: bridge_name,
            call_path: format!("{type_path}::default"),
            method_call: false,
            is_field: false,
            is_setter: false,
            trait_path: Some("std::default::Default".into()),
            unit_variant: false,
            params: Vec::new(),
            field_names: Vec::new(),
            param_ref: Vec::new(),
            ret: RetKind::Object(type_path.clone().into()),
            is_async: false,
            doc: String::new(),
            is_unsafe: false,
        });
    }

    // De-duplicate by name: the first occurrence keeps the plain name,
    // later ones are exposed as `name_2`, `name_3`, ... so two same-named
    // functions in different modules no longer silently drop one.
    let mut count_by_name = HashMap::new();
    let mut out = Vec::new();
    for mut func in collector.functions {
        let count = count_by_name.entry(func.name.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            func.name = format!("{}_{}", func.name, count);
        }
        out.push(func);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    // The manifest is byte-compared for freshness, so unsupported entries
    // sort the same way: discovery order follows source walks, not names.
    unsupported.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.reason.cmp(&b.reason))
    });
    // One callback bridge per trait Neutron provides behavior for, in
    // first-use order across the function list.
    let mut callbacks = Vec::new();
    let mut seen_traits = std::collections::HashSet::new();
    for func in &out {
        for kind in &func.params {
            let ParamKind::Callback(trait_path) = kind else {
                continue;
            };
            if !seen_traits.insert(trait_path.clone()) {
                continue;
            }
            let short = trait_path.rsplit("::").next().unwrap_or(trait_path);
            let Some(inventory) = trait_inventory.get(short) else {
                continue;
            };
            callbacks.push(CallbackBridge {
                trait_path: trait_path.clone(),
                short: sanitize_ident(short),
                methods: inventory
                    .methods
                    .iter()
                    .map(|method| CallbackMethod {
                        name: method.name.clone(),
                        params: method.params.clone(),
                        param_ref: method.param_ref.clone(),
                        receiver: method.receiver,
                        is_async: method.is_async,
                        is_unsafe: method.is_unsafe,
                        imports: callback_imports(&method.params),
                    })
                    .collect(),
            });
        }
    }
    DiscoveredApi {
        functions: out,
        unsupported,
        callbacks,
    }
}

/// Compute the feature set Cargo enables for a dependency specification.
/// Optional dependencies are intentionally ignored here; only feature names
/// participate in `#[cfg(feature = ...)]` source predicates.
pub fn feature_set_for_spec(
    dep_src: &Path,
    dependency_spec: &crate::manifest::DependencySpec,
) -> std::collections::HashSet<String> {
    let mut features = match dependency_spec {
        crate::manifest::DependencySpec::Table(table)
            if table.get("default-features").and_then(toml::Value::as_bool) == Some(false) =>
        {
            std::collections::HashSet::new()
        }
        _ => crate_default_features(dep_src),
    };
    let mut queue: Vec<String> = match dependency_spec {
        crate::manifest::DependencySpec::Table(table) => table
            .get("features")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    };
    let definitions = feature_definitions(dep_src);
    while let Some(name) = queue.pop() {
        if !features.insert(name.clone()) {
            continue;
        }
        if let Some(nested) = definitions.get(&name) {
            queue.extend(
                nested
                    .iter()
                    .filter(|entry| !entry.contains(':') && !entry.contains('/'))
                    .cloned(),
            );
        }
    }
    features
}

/// The crate's enabled feature set under the adaptor's build view
/// (`cargo` path dependency, default features on): the `[features] default`
/// list expanded transitively through feature-name references
/// (`default = ["std"]`, `std = ["alloc"]` enables both).
fn crate_default_features(dep_src: &Path) -> std::collections::HashSet<String> {
    let mut enabled = std::collections::HashSet::new();
    let features = feature_definitions(dep_src);

    // Worklist over feature names, resolving each to the list it enables.
    let mut queue: Vec<String> = features.get("default").cloned().unwrap_or_default();
    while let Some(name) = queue.pop() {
        if !enabled.insert(name.clone()) {
            continue;
        }
        if let Some(nested) = features.get(&name) {
            for entry in nested {
                // Only bare names recurse; entries like "dep:foo" or
                // "foo/bar" enable optional deps, which the feature-name
                // cfg predicate never matches anyway.
                if !entry.contains(':') && !entry.contains('/') {
                    queue.push(entry.to_string());
                }
            }
        }
    }
    enabled
}

fn feature_definitions(dep_src: &Path) -> std::collections::HashMap<String, Vec<String>> {
    let Ok(manifest) = std::fs::read_to_string(dep_src.join("Cargo.toml")) else {
        return std::collections::HashMap::new();
    };
    let Ok(manifest) = manifest.parse::<toml::Table>() else {
        return std::collections::HashMap::new();
    };
    manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .map(|features| {
            features
                .iter()
                .map(|(name, values)| {
                    (
                        name.clone(),
                        values
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(toml::Value::as_str)
                            .map(String::from)
                            .collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One `pub use` re-export edge in a module.
enum UseEdge {
    /// `pub use path::*` — everything the source module exports.
    Glob(Vec<String>),
    /// `pub use path::name as alias`.
    Named {
        src: Vec<String>,
        name: String,
        alias: String,
    },
}

/// The crate's module structure as seen from the outside: where public
/// types are defined, how modules re-export each other, and which modules
/// are reachable from the root through `pub mod` chains.
#[derive(Default)]
struct CrateIndex {
    /// Public type names defined per module path (root = `[]`).
    pub_types: HashMap<Vec<String>, std::collections::HashSet<String>>,
    /// Re-export edges per destination module.
    reexports: HashMap<Vec<String>, Vec<UseEdge>>,
    /// Whether each module is reachable from the root via `pub mod`
    /// chains only (private modules can still be re-export sources).
    public_chain: HashMap<Vec<String>, bool>,
    /// Plain (non-`pub`) imports per module: local name → the source path
    /// as written (`std::fmt::Display`, `crate::gadget::Inspect`), so a
    /// bare trait name in an impl can be resolved.
    imports: HashMap<(Vec<String>, String), String>,
    /// Glob (non-`pub`) imports per module: the normalized source modules
    /// whose exports are candidates for bare-name resolution
    /// (`use crate::model::prelude::*` → `[model, prelude]`).
    glob_imports: HashMap<Vec<String>, Vec<Vec<String>>>,
    /// Module-local type aliases (`type Check = fn(...)`) per module: a
    /// bare name shadowed by one resolves to the alias's opaque target,
    /// not to a same-named public type.
    local_type_aliases: HashMap<Vec<String>, std::collections::HashSet<String>>,
    /// Type aliases keyed by their defining module and name. Signatures are
    /// expanded through this table before ABI classification, so aliases such
    /// as `ResultAlias<T> = Result<T, Error>` remain bridgeable.
    type_aliases: HashMap<String, TypeAliasDef>,
    /// Direct dependencies of the analyzed crate as `(extern key,
    /// package name)` pairs. A path rooted in one of these keys names a
    /// foreign type, bridged through the shared object space under the
    /// owner's tag (see `ObjectType`). Renamed dependencies (`key =
    /// { package = "..." }`) resolve paths through the key and lock
    /// versions through the package name. Any importable foreign path
    /// implies its dependency is enabled: items gated off by features
    /// never reach classification, so no enablement check is needed.
    direct_deps: Vec<(String, String)>,
}

#[derive(Clone)]
struct TypeAliasDef {
    params: Vec<String>,
    target: syn::Type,
}

type EnumVariantRecord = (Vec<String>, String, String, Vec<syn::Type>, Vec<String>);

/// One public struct's named fields: module path, struct name, and each
/// public field's name with its type.
type StructFieldRecord = (Vec<String>, String, Vec<(String, syn::Type)>);

/// Whether a field type borrows call-local data at the top level
/// (`&str`, `&[u8]`, `Option<&…>`, `&T`): setters cannot store such
/// values — the referent would dangle after the bridge returns — so
/// only getters bridge them.
fn type_has_top_borrow(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Reference(_) => true,
        syn::Type::Path(path) if path.qself.is_none() => {
            path.path.segments.last().is_some_and(|segment| {
                segment.ident == "Option"
                    && type_args(segment)
                        .and_then(first_type_arg)
                        .is_some_and(type_has_top_borrow)
            })
        }
        _ => false,
    }
}

/// Whether a `macro_rules!` definition's pattern binds an `:item`
/// fragment: such a macro wraps items (`if_hyper!`, `if_wasm!`), so its
/// invocations expand to items in place and the walk must descend into
/// their tokens.
fn macro_binds_items(macro_item: &syn::ItemMacro) -> bool {
    macro_item.mac.tokens.to_string().contains(": item")
}

/// Direct dependencies of the analyzed crate as `(extern key, package
/// name)` pairs, from its `[dependencies]` table. Target-specific
/// dependency tables are out of scope: supported cross-crate flows name
/// their foreign types through the main table.
fn direct_dep_names(dep_src: &Path) -> Vec<(String, String)> {
    let Ok(manifest) = std::fs::read_to_string(dep_src.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Ok(manifest) = manifest.parse::<toml::Table>() else {
        return Vec::new();
    };
    manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .map(|deps| {
            let mut out: Vec<(String, String)> = deps
                .iter()
                .filter_map(|(key, spec)| {
                    let package = match spec {
                        toml::Value::Table(table) => table
                            .get("package")
                            .and_then(toml::Value::as_str)
                            .unwrap_or(key)
                            .to_string(),
                        _ => key.clone(),
                    };
                    // The crate itself never counts as its own dependency
                    // (workspace `[dependencies]` self-references exist).
                    (package != manifest_package_name(&manifest)).then(|| (key.clone(), package))
                })
                .collect();
            out.sort();
            out
        })
        .unwrap_or_default()
}

/// The analyzed crate's own package name, if its manifest parses.
fn manifest_package_name(manifest: &toml::Table) -> String {
    manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Bare type names the classifier handles by shape (`Vec`, `Option`,
/// scalars, `PathBuf`, ...): qualifying their standard-library imports
/// would only move them off those fast paths, so the foreign rewrite
/// leaves them as written.
const PRELUDE_SHAPES: &[&str] = &[
    "String", "str", "bool", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16",
    "u32", "u64", "u128", "usize", "Vec", "Option", "Result", "Cow", "PathBuf", "Path",
];

/// Whether `first` roots a standard-library path.
fn is_std_root(first: &str) -> bool {
    matches!(first, "std" | "core" | "alloc")
}

/// Whether `name` is a direct dependency of the analyzed crate.
fn is_direct_dep(index: &CrateIndex, name: &str) -> bool {
    index.direct_deps.iter().any(|(key, _)| key == name)
}

/// Split a dependency-rooted path (`serenity::client::ClientBuilder`)
/// into its owner key and crate-rooted remainder, when the first segment
/// is a direct dependency.
fn split_foreign_path(index: &CrateIndex, segments: &[String]) -> Option<(String, String)> {
    let (first, rest) = segments.split_first()?;
    if !is_direct_dep(index, first) {
        return None;
    }
    (!rest.is_empty()).then(|| (first.clone(), rest.join("::")))
}

fn imported_type_path(module: &[String], name: &str, index: &CrateIndex) -> Option<String> {
    // The glob graph (`use foo::*`) can be deeply overlapping and cyclic in
    // large crates, so a name is resolved once per module and cached: without
    // memoization the shared subtrees of a re-export hub like serenity's
    // `model::prelude` recompute exponentially. A module currently being
    // resolved marks a cyclic back-edge, which contributes no candidate.
    fn resolve(
        module: &[String],
        name: &str,
        index: &CrateIndex,
        cache: &mut HashMap<Vec<String>, Option<String>>,
        in_progress: &mut std::collections::HashSet<Vec<String>>,
    ) -> Option<String> {
        if let Some(cached) = cache.get(module) {
            return cached.clone();
        }
        if let Some(path) = index.imports.get(&(module.to_vec(), name.to_string())) {
            let resolved = Some(path.clone());
            cache.insert(module.to_vec(), resolved.clone());
            return resolved;
        }
        if !in_progress.insert(module.to_vec()) {
            return None;
        }
        let mut candidates = std::collections::BTreeSet::new();
        for source in index.glob_imports.get(module).into_iter().flatten() {
            if let Some(path) = resolve(source, name, index, cache, in_progress) {
                candidates.insert(path);
            }
        }
        in_progress.remove(module);
        let resolved = if candidates.len() == 1 {
            candidates.pop_first()
        } else {
            None
        };
        cache.insert(module.to_vec(), resolved.clone());
        resolved
    }
    let mut cache = HashMap::new();
    let mut in_progress = std::collections::HashSet::new();
    resolve(module, name, index, &mut cache, &mut in_progress)
}

/// Qualify a bare type name imported from a direct dependency
/// (`use reqwest::Client` + `client: Client` becomes
/// `reqwest::Client`), recursively through references, slices, arrays,
/// tuples, options, vectors, and `impl Trait` bounds — so classification
/// sees the foreign owner through one rule. Names shadowed by a local
/// alias, imports from the analyzed crate itself, unknown names, and
/// every other shape pass through unchanged; downstream scope
/// resolution and the foreign-shadow rejection handle those exactly as
/// before.
fn qualify_foreign_ty(ty: &syn::Type, module_path: &[String], index: &CrateIndex) -> syn::Type {
    // Rewrite one segment list's generic arguments in place.
    fn rewrite_args(args: &mut syn::PathArguments, module_path: &[String], index: &CrateIndex) {
        if let syn::PathArguments::AngleBracketed(bracketed) = args {
            for arg in &mut bracketed.args {
                if let syn::GenericArgument::Type(inner) = arg {
                    *inner = qualify_foreign_ty(inner, module_path, index);
                }
            }
        }
    }
    match ty {
        syn::Type::Path(path) if path.qself.is_none() => {
            let head_ident = path.path.segments[0].ident.to_string();
            // A prelude shape keeps its fast path in the classifier
            // however it is spelled (`Vec<T>`, `Option<T>`), so only
            // other heads qualify — including heads carrying generic
            // arguments (`Arc<Engine>`, `Mutex<Handle>`), whose arguments
            // recurse below.
            let singleton_prelude =
                path.path.segments.len() == 1 && PRELUDE_SHAPES.contains(&head_ident.as_str());
            if !singleton_prelude
                && !index
                    .local_type_aliases
                    .get(module_path)
                    .is_some_and(|aliases| aliases.contains(&head_ident))
                && let Some(written) = imported_type_path(module_path, &head_ident, index)
            {
                let first = written.split("::").next().unwrap_or_default();
                if (is_direct_dep(index, first) || is_std_root(first))
                    && let Ok(head_path) = syn::parse_str::<syn::Path>(&written)
                {
                    // Splice the imported root with the remaining
                    // segments: `use dep::foo` + `foo::Bar<T>` becomes
                    // `dep::foo::Bar<T>`, and `use std::sync::Arc` +
                    // `Arc<Engine>` becomes `std::sync::Arc<Engine>`.
                    // Anything else stays as written for scope resolution
                    // and the foreign-shadow rejection exactly as before.
                    let leading_colon = head_path.leading_colon;
                    let mut segments = head_path.segments;
                    // The head segment carries the original arguments
                    // when the written type had any (`Arc<Engine>`);
                    // recurse into them like every other segment below.
                    if let Some(first_segment) = segments.last_mut()
                        && path.path.segments.len() == 1
                    {
                        first_segment.arguments = path.path.segments[0].arguments.clone();
                    }
                    for segment in path.path.segments.iter().skip(1) {
                        let mut segment = segment.clone();
                        rewrite_args(&mut segment.arguments, module_path, index);
                        segments.push(segment);
                    }
                    // Recurse into spliced head arguments too.
                    let mut spliced = syn::Path {
                        leading_colon,
                        segments,
                    };
                    for segment in &mut spliced.segments {
                        rewrite_args(&mut segment.arguments, module_path, index);
                    }
                    return syn::Type::Path(syn::TypePath {
                        qself: None,
                        path: spliced,
                    });
                }
            }
            let mut rewritten = path.clone();
            for segment in &mut rewritten.path.segments {
                rewrite_args(&mut segment.arguments, module_path, index);
            }
            syn::Type::Path(rewritten)
        }
        syn::Type::Reference(reference) => {
            let mut rewritten = reference.clone();
            *rewritten.elem = qualify_foreign_ty(&reference.elem, module_path, index);
            syn::Type::Reference(rewritten)
        }
        syn::Type::Slice(slice) => {
            let mut rewritten = slice.clone();
            *rewritten.elem = qualify_foreign_ty(&slice.elem, module_path, index);
            syn::Type::Slice(rewritten)
        }
        syn::Type::Array(array) => {
            let mut rewritten = array.clone();
            *rewritten.elem = qualify_foreign_ty(&array.elem, module_path, index);
            syn::Type::Array(rewritten)
        }
        syn::Type::Tuple(tuple) => {
            let mut rewritten = tuple.clone();
            for elem in &mut rewritten.elems {
                *elem = qualify_foreign_ty(elem, module_path, index);
            }
            syn::Type::Tuple(rewritten)
        }
        syn::Type::Paren(paren) => {
            let mut rewritten = paren.clone();
            *rewritten.elem = qualify_foreign_ty(&paren.elem, module_path, index);
            syn::Type::Paren(rewritten)
        }
        syn::Type::Group(group) => {
            let mut rewritten = group.clone();
            *rewritten.elem = qualify_foreign_ty(&group.elem, module_path, index);
            syn::Type::Group(rewritten)
        }
        syn::Type::ImplTrait(impl_trait) => {
            let mut rewritten = impl_trait.clone();
            for bound in &mut rewritten.bounds {
                if let syn::TypeParamBound::Trait(trait_bound) = bound {
                    for segment in &mut trait_bound.path.segments {
                        if let syn::PathArguments::AngleBracketed(args) = &mut segment.arguments {
                            for arg in &mut args.args {
                                if let syn::GenericArgument::Type(inner) = arg {
                                    *inner = qualify_foreign_ty(inner, module_path, index);
                                }
                            }
                        }
                    }
                }
            }
            syn::Type::ImplTrait(rewritten)
        }
        _ => ty.clone(),
    }
}

/// Walks the crate's compiled module graph collecting bridgeable free
/// functions and the module/re-export index needed to name impl types.
#[derive(Default)]
struct FnCollector {
    functions: Vec<Func>,
    features: std::collections::HashSet<String>,
    /// Path of the module currently being walked.
    module_path: Vec<String>,
    index: CrateIndex,
    /// Impl blocks deferred to post-walk classification (their type
    /// paths may need re-export edges discovered later in the walk).
    impls: Vec<(Vec<String>, syn::ItemImpl)>,
    /// Free `pub fn`s deferred to post-walk classification for the same
    /// reason (struct/enum-typed params need the full type index).
    free_fns: Vec<(Vec<String>, syn::ItemFn)>,
    /// Unit enum variants (module path, enum name, variant name) found
    /// during the walk, materialized after the index is complete.
    enum_unit_variants: Vec<(Vec<String>, String, String)>,
    /// Tuple enum variants (module path, enum name, variant name, field
    /// types) found during the walk; bridged post-walk when every field
    /// is tier-1.
    enum_tuple_variants: Vec<EnumVariantRecord>,
    /// Deferred public trait definitions (module path + item), resolved
    /// post-walk into a method inventory when a callback parameter
    /// requires a generated trait impl.
    trait_defs: Vec<(Vec<String>, syn::ItemTrait)>,
    /// Deferred macro invocations (module path + item): `macro_rules!`
    /// definitions map a macro name to its generated trait, and
    /// macro-generated items (`event_handler!`-style) are inventoried
    /// post-walk once every definition is known.
    macro_items: Vec<(Vec<String>, syn::ItemMacro)>,
    /// `macro_rules!` definitions whose pattern binds `:item` fragments
    /// (`if_hyper!`, `if_wasm!`): their invocations carry items in place,
    /// so the walk descends into the invocation tokens directly.
    item_macro_names: std::collections::HashSet<String>,
    /// Bare names of public generic types. A bare-name lookup cannot
    /// supply their generic arguments, so they never marshal as objects.
    generic_types: std::collections::HashSet<String>,
    /// Bare names of public types that implement `Clone` (via derive or a
    /// manual impl): object-typed field getters clone the stored value.
    clone_types: std::collections::HashSet<String>,
    /// `id_u64!`-style newtype names (module path + name), materialized
    /// post-walk into `new`/`get` bridges.
    id_types: Vec<(Vec<String>, String)>,
    /// Non-generic public types deriving `Default` (module path + name),
    /// materialized post-walk into `Type_default()` bridges: the impl is
    /// compiler-generated, so no `impl Default` block exists to discover.
    default_types: Vec<(Vec<String>, String)>,
    /// `bitflags!` types (module path + name), materialized post-walk
    /// into set-operation bridges (`empty`/`all`/`union`/ ...).
    bitflags_types: Vec<(Vec<String>, String)>,
    /// Public struct fields (module path, struct name, and each public
    /// named field with its type), materialized post-walk into `Type_field`
    /// getter bridges.
    struct_fields: Vec<StructFieldRecord>,
    /// Public functions declared in Rust `extern` blocks. They cannot be
    /// emitted into the safe generated adaptor, but they must remain visible
    /// in the export manifest instead of disappearing during discovery.
    foreign_functions: Vec<(Vec<String>, syn::ForeignItemFn)>,
}

impl FnCollector {
    fn walk_file(
        &mut self,
        path: &Path,
        module_dir: &Path,
        visited: &mut std::collections::HashSet<PathBuf>,
    ) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if visited.contains(&canonical) {
            return;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(_) => return,
        };
        let file = match syn::parse_file(&content) {
            Ok(file) => file,
            Err(_) => return,
        };
        if !is_cfg_enabled(&file.attrs, &self.features) {
            return;
        }
        // A file can define several modules through separate #[path] declarations.
        // Only an active recursion cycle should suppress a visit.
        visited.insert(canonical.clone());
        self.walk_items(
            &file.items,
            module_dir,
            path.parent().unwrap_or(module_dir),
            visited,
        );
        visited.remove(&canonical);
    }

    fn walk_items(
        &mut self,
        items: &[syn::Item],
        module_dir: &Path,
        path_dir: &Path,
        visited: &mut std::collections::HashSet<PathBuf>,
    ) {
        let is_public_mod = self
            .index
            .public_chain
            .get(&self.module_path)
            .copied()
            .unwrap_or(false);
        let module_path = self.module_path.clone();
        for item in items.iter() {
            let attrs: &[syn::Attribute] = match item {
                syn::Item::Struct(item) => &item.attrs,
                syn::Item::Enum(item) => &item.attrs,
                syn::Item::Trait(item) => &item.attrs,
                syn::Item::Type(item) => &item.attrs,
                syn::Item::Macro(item) => &item.attrs,
                syn::Item::ForeignMod(item) => &item.attrs,
                _ => &[],
            };
            if !is_cfg_enabled(attrs, &self.features) {
                continue;
            }
            match item {
                syn::Item::Fn(fn_item) => {
                    // Deferred like impl blocks: struct/enum-typed params
                    // need the type index, which is complete only after
                    // the whole walk.
                    self.free_fns.push((module_path.clone(), fn_item.clone()));
                }
                syn::Item::ForeignMod(foreign_mod) => {
                    for foreign_item in &foreign_mod.items {
                        if let syn::ForeignItem::Fn(function) = foreign_item {
                            self.foreign_functions
                                .push((module_path.clone(), function.clone()));
                        }
                    }
                }
                syn::Item::Impl(impl_item) => {
                    if is_cfg_enabled(&impl_item.attrs, &self.features) {
                        self.impls.push((module_path.clone(), impl_item.clone()));
                    }
                    // A manual `impl Clone for X` makes object-typed field
                    // getters of X bridgeable (they clone the value).
                    if impl_item.generics.params.is_empty()
                        && let Some((_, trait_path, _)) = &impl_item.trait_
                        && trait_path
                            .segments
                            .last()
                            .is_some_and(|s| s.ident == "Clone")
                        && let syn::Type::Path(self_ty) = impl_item.self_ty.as_ref()
                        && self_ty.qself.is_none()
                        && self_ty.path.segments.len() == 1
                        && matches!(self_ty.path.segments[0].arguments, syn::PathArguments::None)
                    {
                        self.clone_types
                            .insert(self_ty.path.segments[0].ident.to_string());
                    }
                }
                syn::Item::Trait(trait_item) => {
                    if matches!(trait_item.vis, syn::Visibility::Public(_)) {
                        // A trait is a type name for import resolution
                        // (`impl Inspect for X` through `use ...::Inspect`).
                        self.index
                            .pub_types
                            .entry(module_path.clone())
                            .or_default()
                            .insert(trait_item.ident.to_string());
                        // Deferred so a callback bound can later generate
                        // an impl of this trait (see
                        // `emit_callback_bridges`).
                        self.trait_defs
                            .push((module_path.clone(), trait_item.clone()));
                    }
                }
                syn::Item::Struct(struct_item) => {
                    if matches!(struct_item.vis, syn::Visibility::Public(_)) {
                        // A pub type in a private module can still be the
                        // target of a `pub use` re-export; whether it is
                        // externally nameable is decided later by
                        // `root_type_path`.
                        self.index
                            .pub_types
                            .entry(module_path.clone())
                            .or_default()
                            .insert(struct_item.ident.to_string());
                        if !struct_item.generics.params.is_empty() {
                            self.generic_types.insert(struct_item.ident.to_string());
                        }
                        if derive_idents(&struct_item.attrs, &self.features)
                            .iter()
                            .any(|d| d == "Clone")
                        {
                            self.clone_types.insert(struct_item.ident.to_string());
                        }
                        if struct_item.generics.params.is_empty()
                            && derive_idents(&struct_item.attrs, &self.features)
                                .iter()
                                .any(|d| d == "Default")
                        {
                            self.default_types
                                .push((module_path.clone(), struct_item.ident.to_string()));
                        }
                        // Public named fields become `Type_field` getters
                        // post-walk, so Neutron can read event payloads.
                        // Generic structs have no concrete object type for
                        // the shared-space tag, so their fields stay out
                        // of the tier-1 bridge inventory.
                        if struct_item.generics.params.is_empty()
                            && let syn::Fields::Named(fields) = &struct_item.fields
                        {
                            let module_path = self.module_path.clone();
                            let pub_fields: Vec<(String, syn::Type)> = fields
                                .named
                                .iter()
                                .filter(|field| is_cfg_enabled(&field.attrs, &self.features))
                                .filter(|field| matches!(field.vis, syn::Visibility::Public(_)))
                                .filter_map(|field| {
                                    field.ident.as_ref().map(|ident| {
                                        (
                                            ident.to_string(),
                                            qualify_foreign_ty(
                                                &field.ty,
                                                &module_path,
                                                &self.index,
                                            ),
                                        )
                                    })
                                })
                                .collect();
                            if !pub_fields.is_empty() {
                                self.struct_fields.push((
                                    module_path.clone(),
                                    struct_item.ident.to_string(),
                                    pub_fields,
                                ));
                            }
                        }
                    }
                }
                syn::Item::Enum(enum_item) => {
                    if matches!(enum_item.vis, syn::Visibility::Public(_)) {
                        self.index
                            .pub_types
                            .entry(module_path.clone())
                            .or_default()
                            .insert(enum_item.ident.to_string());
                        if !enum_item.generics.params.is_empty() {
                            self.generic_types.insert(enum_item.ident.to_string());
                        }
                        if derive_idents(&enum_item.attrs, &self.features)
                            .iter()
                            .any(|d| d == "Clone")
                        {
                            self.clone_types.insert(enum_item.ident.to_string());
                        }
                        if enum_item.generics.params.is_empty()
                            && derive_idents(&enum_item.attrs, &self.features)
                                .iter()
                                .any(|d| d == "Default")
                        {
                            self.default_types
                                .push((module_path.clone(), enum_item.ident.to_string()));
                        }
                        // Unit variants construct directly (`Color::Red`);
                        // tuple variants with tier-1 field types construct
                        // with marshaled arguments (`Color::Rgb(r, g, b)`);
                        // struct variants would need named fields and stay
                        // unexposed. A `#[non_exhaustive]` enum cannot be
                        // constructed outside its defining crate at all,
                        // so none of its variants bridge.
                        if enum_item.generics.params.is_empty()
                            && !has_marker_attr(&enum_item.attrs, "non_exhaustive")
                        {
                            for variant in &enum_item.variants {
                                match &variant.fields {
                                    syn::Fields::Unit => {
                                        self.enum_unit_variants.push((
                                            module_path.clone(),
                                            enum_item.ident.to_string(),
                                            variant.ident.to_string(),
                                        ));
                                    }
                                    syn::Fields::Unnamed(fields) => {
                                        // Classify later (post-walk), like free
                                        // fns: a field may be a local type.
                                        self.enum_tuple_variants.push((
                                            module_path.clone(),
                                            enum_item.ident.to_string(),
                                            variant.ident.to_string(),
                                            fields
                                                .unnamed
                                                .iter()
                                                .map(|f| {
                                                    qualify_foreign_ty(
                                                        &f.ty,
                                                        &module_path,
                                                        &self.index,
                                                    )
                                                })
                                                .collect(),
                                            Vec::new(),
                                        ));
                                    }
                                    syn::Fields::Named(fields) => {
                                        self.enum_tuple_variants.push((
                                            module_path.clone(),
                                            enum_item.ident.to_string(),
                                            variant.ident.to_string(),
                                            fields
                                                .named
                                                .iter()
                                                .map(|f| {
                                                    qualify_foreign_ty(
                                                        &f.ty,
                                                        &module_path,
                                                        &self.index,
                                                    )
                                                })
                                                .collect(),
                                            fields
                                                .named
                                                .iter()
                                                .filter_map(|f| {
                                                    f.ident.as_ref().map(ToString::to_string)
                                                })
                                                .collect(),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                syn::Item::Use(use_item) => {
                    if !is_cfg_enabled(&use_item.attrs, &self.features) {
                        continue;
                    }
                    // A `pub use` re-exports items — even from a private
                    // module, where it forms one hop of a re-export chain
                    // that a public ancestor completes (`pub use` inside
                    // reqwest's private `mod async_impl`). Any other `use`
                    // is a plain import recorded so bare trait names in
                    // impls can resolve (glob imports are kept as candidate
                    // sources for module-scoped type resolution).
                    if matches!(use_item.vis, syn::Visibility::Public(_)) {
                        let edges = self.index.reexports.entry(module_path.clone()).or_default();
                        record_use_tree(&use_item.tree, &module_path, &mut Vec::new(), edges);
                    }
                    record_imports(
                        &use_item.tree,
                        &module_path,
                        &mut Vec::new(),
                        &mut self.index.imports,
                        &mut self.index.glob_imports,
                    );
                }
                syn::Item::Mod(module) => {
                    // cfg-gated modules absent from the build (e.g.
                    // `#[cfg(test)]`) are not compiled into the adaptor's
                    // dependency build, so their functions do not exist to
                    // bridge.
                    if !is_cfg_enabled(&module.attrs, &self.features) {
                        continue;
                    }
                    let child_public =
                        is_public_mod && matches!(module.vis, syn::Visibility::Public(_));
                    let child_path = {
                        let mut path = module_path.clone();
                        path.push(module.ident.to_string());
                        self.index.public_chain.insert(path.clone(), child_public);
                        path
                    };
                    match &module.content {
                        Some((_, inner)) => {
                            let child_dir = module_path_override(module, &self.features)
                                .map_or_else(
                                    || module_dir.join(module.ident.to_string()),
                                    |path| path_dir.join(path),
                                );
                            let parent = std::mem::replace(&mut self.module_path, child_path);
                            self.walk_items(inner, &child_dir, &child_dir, visited);
                            self.module_path = parent;
                        }
                        None => {
                            if let Some((mod_path, child_dir)) =
                                resolve_outer_mod(module, module_dir, path_dir, &self.features)
                            {
                                let parent = std::mem::replace(&mut self.module_path, child_path);
                                self.walk_file(&mod_path, &child_dir, visited);
                                self.module_path = parent;
                            }
                        }
                    }
                }

                syn::Item::Type(type_item) => {
                    // Every alias — public or private — shadows the bare
                    // name inside its own module.
                    self.index
                        .local_type_aliases
                        .entry(module_path.clone())
                        .or_default()
                        .insert(type_item.ident.to_string());
                    let alias_key = if module_path.is_empty() {
                        type_item.ident.to_string()
                    } else {
                        format!("{}::{}", module_path.join("::"), type_item.ident)
                    };
                    self.index.type_aliases.insert(
                        alias_key,
                        TypeAliasDef {
                            params: type_item
                                .generics
                                .type_params()
                                .map(|param| param.ident.to_string())
                                .collect(),
                            target: (*type_item.ty).clone(),
                        },
                    );
                    if matches!(type_item.vis, syn::Visibility::Public(_)) {
                        self.index
                            .pub_types
                            .entry(module_path.clone())
                            .or_default()
                            .insert(type_item.ident.to_string());
                        // A generic alias supplies no concrete object type;
                        // other aliases (`type Check = for<'fut> fn(...)`) still
                        // contend for the bare name — two definitions sharing it
                        // blacklist the name in the known-types pass.
                        if !type_item.generics.params.is_empty() {
                            self.generic_types.insert(type_item.ident.to_string());
                        }
                    }
                }
                syn::Item::Macro(macro_item) => {
                    // `macro_rules!` definitions: record the ones whose
                    // pattern binds `:item` fragments — their invocations
                    // expand to items in place, and the walk descends into
                    // them (`reqwest` gates its whole API behind
                    // `if_hyper! { mod async_impl; ... }`).
                    if macro_item.mac.path.is_ident("macro_rules") {
                        if let Some(name) = &macro_item.ident
                            && macro_binds_items(macro_item)
                        {
                            self.item_macro_names.insert(name.to_string());
                        }
                        self.macro_items
                            .push((module_path.clone(), macro_item.clone()));
                    } else if let Some(invoked) = macro_item
                        .mac
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        && self.item_macro_names.contains(invoked.as_str())
                        && let Ok(expanded) =
                            syn::parse2::<syn::File>(macro_item.mac.tokens.clone())
                    {
                        // The invocation tokens carry the expanded items;
                        // their cfg attributes evaluate against the host
                        // target like any other item's.
                        self.walk_items(&expanded.items, module_dir, path_dir, visited);
                    } else if let Some(flag_type) = parse_bitflags_type(macro_item, &self.features)
                    {
                        // Each flag is an associated const on the generated
                        // struct (e.g. `GatewayIntents::GUILD_CREATE`),
                        // bridged the same way as a unit enum variant: a
                        // zero-arg constructor returning an object handle.
                        for flag in &flag_type.flags {
                            self.enum_unit_variants.push((
                                module_path.clone(),
                                flag_type.name.clone(),
                                flag.clone(),
                            ));
                        }
                        self.index
                            .pub_types
                            .entry(module_path.clone())
                            .or_default()
                            .insert(flag_type.name.clone());
                        // bitflags 2 derives `Clone`/`Copy` on the struct.
                        self.clone_types.insert(flag_type.name.clone());
                        self.bitflags_types
                            .push((module_path.clone(), flag_type.name));
                    } else if let Some(names) = parse_id_newtype_names(macro_item) {
                        // `id_u64! { ChannelId: "..."; ... }` generates one
                        // public newtype per name with `new(u64)`/`get()`.
                        for name in &names {
                            self.index
                                .pub_types
                                .entry(module_path.clone())
                                .or_default()
                                .insert(name.clone());
                            // The macro derives `Clone`/`Copy` on each newtype.
                            self.clone_types.insert(name.clone());
                            self.id_types.push((module_path.clone(), name.clone()));
                        }
                    }
                    // Every other macro invocation is deferred: post-walk,
                    // `macro_rules!` definitions reveal which macros generate
                    // traits, and matching invocations are inventoried for
                    // their method lists.
                    self.macro_items
                        .push((module_path.clone(), macro_item.clone()));
                }
                _ => {}
            }
        }
    }
}

/// How a callback-trait method takes `self`, reproduced verbatim in the
/// generated override so its signature matches the trait exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverKind {
    Ref,
    MutRef,
    Owned,
}

impl ReceiverKind {
    fn as_self_param(self) -> &'static str {
        match self {
            ReceiverKind::Ref => "&self",
            ReceiverKind::MutRef => "&mut self",
            ReceiverKind::Owned => "self",
        }
    }
}

/// One inventoried method of a callback trait, before it is known whether
/// Neutron needs an impl of the trait.
#[derive(Debug, Clone)]
struct InventoriedMethod {
    name: String,
    /// Classified parameter kinds (excluding `self`).
    params: Vec<ParamKind>,
    param_ref: Vec<ParamRef>,
    receiver: ReceiverKind,
    is_async: bool,
    is_unsafe: bool,
}

/// Post-walk inventory of one callback trait: its root-callable path and
/// the methods a generated impl may override.
#[derive(Debug, Clone)]
struct TraitInventory {
    path: String,
    methods: Vec<InventoriedMethod>,
    /// Every required (default-less) method is overridable; otherwise no
    /// sound impl exists and functions needing the trait stay unsupported.
    usable: bool,
}

/// A `bitflags!`-macro-generated type as seen in source: the public struct
/// name and its flag constants.
struct BitflagsType {
    name: String,
    flags: Vec<String>,
}

/// Copy `text` with every `"..."` string literal blanked (escapes
/// honored, byte length preserved so indices map back). Token-rendered
/// doc attributes (`#[doc = "..."]`) carry arbitrary prose — punctuation
/// inside them (`]`, `=>`, `pub trait`) must never steer a structural
/// scan.
fn without_string_literals(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            out.push(c);
            continue;
        }
        out.push(' ');
        let mut escaped = false;
        for d in chars.by_ref() {
            for _ in 0..d.len_utf8() {
                out.push(' ');
            }
            if escaped {
                escaped = false;
            } else if d == '\\' {
                escaped = true;
            } else if d == '"' {
                break;
            }
        }
    }
    out
}

/// Split the leading `# [...]` attribute groups off `text` (bracket
/// balanced; string-aware so doc prose cannot end a group early).
/// Returns the groups plus the remainder, or `None` when unbalanced.
fn split_leading_attrs(text: &str) -> Option<(Vec<&str>, &str)> {
    let mut groups = Vec::new();
    let mut rest = text.trim_start();
    while rest.starts_with('#') {
        let bytes = rest.as_bytes();
        let mut i = 1;
        while i < bytes.len() && (bytes[i] == b'!' || bytes[i].is_ascii_whitespace()) {
            i += 1;
        }
        if bytes.get(i) != Some(&b'[') {
            return None;
        }
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        let mut end = None;
        while i < bytes.len() {
            let c = bytes[i];
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == b'\\' {
                    escaped = true;
                } else if c == b'"' {
                    in_string = false;
                }
            } else if c == b'"' {
                in_string = true;
            } else if c == b'[' {
                depth += 1;
            } else if c == b']' {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            i += 1;
        }
        let end = end?;
        groups.push(&rest[..=end]);
        rest = rest[end + 1..].trim_start();
    }
    Some((groups, rest))
}

/// The `feature = "..."` gated by one attribute group, if it is a
/// `cfg(feature = ...)` (possibly nested in `any`/`all`/`not`).
fn attr_cfg_feature(group: &str) -> Option<String> {
    let pos = group.find("feature")?;
    let after = &group[pos + "feature".len()..];
    let quote = after.find('"')? + 1;
    let end = after[quote..].find('"')?;
    Some(after[quote..quote + end].to_string())
}

/// Parse a `bitflags! { ... }` invocation body into the generated struct
/// name and its flag constants. Both spellings are accepted: the struct
/// head (`pub struct Name: Repr { ... }`) and the impl head (`impl Name:
/// Repr { ... }`), with each flag as `const NAME = ...;` (bitflags 1.x/2.x)
/// or the older `NAME: const = ...;` fragment. Returns `None` when the
/// macro is not a bitflags struct.
fn parse_bitflags_type(
    macro_item: &syn::ItemMacro,
    features: &std::collections::HashSet<String>,
) -> Option<BitflagsType> {
    if !macro_item
        .mac
        .path
        .segments
        .last()
        .is_some_and(|s| s.ident == "bitflags")
    {
        return None;
    }
    let body = macro_item.mac.tokens.to_string();
    // Extract `struct <Name> : <Repr> { ... }` (allow up to `pub` before it).
    let open = body.find('{')?;
    let close = body.rfind('}')?;
    let head = &body[..open];
    // The declaration keyword nearest the brace wins over the same words
    // elsewhere in the head.
    let (keyword_pos, keyword) = ["struct", "impl"]
        .into_iter()
        .filter_map(|keyword| {
            head.rmatch_indices(keyword)
                .find(|(pos, _)| {
                    head[..*pos]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
                })
                .map(|(pos, _)| (pos, keyword))
        })
        .max_by_key(|(pos, _)| *pos)?;
    let name = head[keyword_pos + keyword.len()..]
        .split([':', '<', '(', ' '])
        .map(str::trim)
        .find(|s| !s.is_empty())?;
    if name.is_empty() {
        return None;
    }
    let inner = &body[open + 1..close];
    let mut flags = Vec::new();
    for segment in inner.split(';') {
        // Leading attributes (`#[cfg(feature = "...")]`, docs) gate or
        // annotate the flag; anything else unrecognized ends the parse.
        let (groups, line) = split_leading_attrs(segment)?;
        let gated_out = groups
            .iter()
            .filter_map(|group| attr_cfg_feature(group))
            .any(|feature| !features.contains(&feature));
        if gated_out {
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // `const NAME = ...` (bitflags 1.x/2.x), tolerating a `: Ty`
        // annotation between the name and the value.
        if let Some(rest) = line
            .strip_prefix("const")
            .filter(|rest| rest.starts_with(char::is_whitespace))
        {
            let rest = rest.trim();
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let ident = &rest[..end];
            if ident.is_empty() {
                return None;
            }
            flags.push(ident.to_string());
            continue;
        }
        // Older `NAME: const = ...` fragments.
        let colon = line.find(':')?;
        let ident = line[..colon].trim();
        let rest = &line[colon + 1..];
        if ident.is_empty()
            || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !rest.contains("const")
        {
            return None;
        }
        flags.push(ident.to_string());
    }
    Some(BitflagsType {
        name: name.to_string(),
        flags,
    })
}

/// Parse an `id_u64! { Name: "doc"; ... }`-shaped newtype list into the
/// generated public type names. The shape is one `Name : "doc" ;` entry per
/// generated `pub struct Name(Inner)` with `new(u64)`/`get()` constructors.
/// Returns `None` when the macro is not such a list.
fn parse_id_newtype_names(macro_item: &syn::ItemMacro) -> Option<Vec<String>> {
    if !macro_item
        .mac
        .path
        .segments
        .last()
        .is_some_and(|s| s.ident == "id_u64")
    {
        return None;
    }
    let body = macro_item.mac.tokens.to_string();
    // Entries are `Name : "doc" ;` — every entry must match, so a foreign
    // macro that merely shares the name stays untouched.
    let mut names = Vec::new();
    for entry in body.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let colon = entry.find(':')?;
        let ident = entry[..colon].trim();
        let doc = entry[colon + 1..].trim();
        if ident.is_empty()
            || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !(doc.starts_with('"') && doc.ends_with('"'))
        {
            return None;
        }
        names.push(ident.to_string());
    }
    if names.is_empty() { None } else { Some(names) }
}

/// One method declared by a trait-generating macro invocation
/// (`event_handler! { Message { msg: Message } => async fn message(...); }`).
#[derive(Clone)]
struct MacroTraitMethod {
    name: String,
    /// `(parameter name, parameter type)` pairs, excluding `self`: the
    /// `fn(...)` arguments plus the variant payload fields (the macro
    /// expands the method to take both).
    args: Vec<(String, syn::Type)>,
    is_async: bool,
}

/// Map a `macro_rules!` definition to the trait it generates, by scanning
/// the definition body for `pub trait <Name>`. Returns `None` for macros
/// that do not define a trait.
/// Map a `macro_rules!` definition to the trait it generates, by scanning
/// the definition body for `pub trait <Name>`. String literals are
/// blanked first so doc prose can never supply the match. Returns `None`
/// for macros that do not define a trait.
fn macro_generated_trait(macro_item: &syn::ItemMacro) -> Option<(String, String)> {
    let name = macro_item.ident.as_ref()?.to_string();
    if !macro_item.mac.path.is_ident("macro_rules") {
        return None;
    }
    let body = without_string_literals(&macro_item.mac.tokens.to_string());
    let pos = body.find("pub trait")?;
    let name_start =
        body[pos + "pub trait".len()..].find(|c: char| c.is_ascii_alphabetic() || c == '_')?;
    let rest = &body[pos + "pub trait".len() + name_start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let trait_name = &rest[..end];
    if trait_name.is_empty() {
        return None;
    }
    Some((name, trait_name.to_string()))
}

/// Whether `text` at byte `pos` starts a whole word (not a longer ident).
fn is_word_boundary(text: &str, pos: usize, len: usize) -> bool {
    let before = text[..pos]
        .chars()
        .next_back()
        .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '#' || c == '!'));
    let after = text[pos + len..]
        .chars()
        .next()
        .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
    before && after
}

/// Parse one `Variant { fields } => async fn method(&self, args);` entry:
/// the `fn(...)` arguments (minus the receiver) plus the variant payload
/// fields, which the macro expands into additional method parameters.
/// Returns the method and the end position.
fn parse_macro_method_entry(
    tokens: &str,
    arrow: usize,
    head: &str,
) -> Option<(MacroTraitMethod, usize)> {
    let mut rest = tokens[arrow + 2..].trim_start();
    let mut is_async = false;
    if rest.starts_with("async") && is_word_boundary(rest, 0, "async".len()) {
        is_async = true;
        rest = rest["async".len()..].trim_start();
    }
    if !rest.starts_with("fn") || !is_word_boundary(rest, 0, "fn".len()) {
        return None;
    }
    rest = rest["fn".len()..].trim_start();
    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let name = rest[..name_end].to_string();
    if name.is_empty() {
        return None;
    }
    rest = rest[name_end..].trim_start();
    if !rest.starts_with('(') {
        return None;
    }
    // Capture the parenthesized receiver + args, nesting-aware.
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let args_text = rest[1..end].to_string();
    let consumed = tokens.len() - rest.len() + end + 1;
    let mut args = Vec::new();
    for part in split_top_level(&args_text, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // The receiver is `&self`/`&mut self`/`self`, never an arg.
        let bare: String = part.chars().filter(|c| !c.is_whitespace()).collect();
        if bare == "&self" || bare == "&mutself" || bare == "self" || bare == "mutself" {
            continue;
        }
        let colon = top_level_colon(part)?;
        let arg_name = part[..colon].trim().to_string();
        let arg_ty: syn::Type = syn::parse_str(part[colon + 1..].trim()).ok()?;
        args.push((arg_name, arg_ty));
    }
    // The variant payload (`Message { new_message: Message }`) becomes
    // trailing method parameters in the expansion.
    args.extend(parse_variant_fields(head)?);
    Some((
        MacroTraitMethod {
            name,
            args,
            is_async,
        },
        consumed,
    ))
}

/// Parse the `{ name: Type, ... }` payload of one macro entry head into
/// `(name, type)` pairs. String literals are blanked first so doc prose
/// cannot supply phantom braces.
fn parse_variant_fields(head: &str) -> Option<Vec<(String, syn::Type)>> {
    let blanked = without_string_literals(head);
    let open = blanked.find('{')?;
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in blanked[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    // Byte indices map back: blanking preserves length.
    let fields = &head[open + 1..close];
    let mut out = Vec::new();
    for part in split_top_level(fields, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let colon = top_level_colon(part)?;
        let arg_name = part[..colon].trim().to_string();
        if arg_name.is_empty() {
            return None;
        }
        let arg_ty: syn::Type = syn::parse_str(part[colon + 1..].trim()).ok()?;
        out.push((arg_name, arg_ty));
    }
    Some(out)
}

/// Split `text` on `sep` at nesting level zero (`<>()[]{}-aware`).
fn split_top_level(text: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if c == sep && depth == 0 {
            out.push(&text[start..i]);
            start = i + 1;
        }
    }
    out.push(&text[start..]);
    out
}

/// The first top-level single `:` in `text` (skipping `::` paths).
fn top_level_colon(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' | b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => {
                if bytes.get(i + 1) == Some(&b':') {
                    i += 1;
                } else {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Find the next `=>` at or after `from`, skipping `"..."` string
/// literals (doc prose may contain arrows of its own).
fn find_arrow(tokens: &str, from: usize) -> Option<usize> {
    let bytes = tokens.as_bytes();
    let mut i = from;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else if c == b'"' {
            in_string = true;
        } else if c == b'=' && bytes.get(i + 1) == Some(&b'>') {
            return Some(i);
        }
        i += 1;
    }
    None
}
/// Parse the method entries of a trait-generating macro invocation such as
/// serenity's `event_handler!`: `Variant { args } => async fn
/// method(&self, ...);` repeated, each optionally preceded by
/// `#[cfg(feature = "...")]`. Entries gated on a disabled feature are
/// dropped; anything that does not match the shape is skipped.
fn parse_macro_trait_methods(
    macro_item: &syn::ItemMacro,
    features: &std::collections::HashSet<String>,
) -> Vec<MacroTraitMethod> {
    let tokens = macro_item.mac.tokens.to_string();
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(arrow) = find_arrow(&tokens, cursor) {
        // The entry head (variant + optional cfg) runs back to the
        // previous `;` (or the start of the invocation). Attribute groups
        // are parsed structurally so doc prose cannot fake a gate.
        let head_start = tokens[..arrow].rfind(';').map_or(0, |pos| pos + 1);
        let head = &tokens[head_start..arrow];
        let (groups, _) = split_leading_attrs(head).unwrap_or((Vec::new(), head));
        let feature = groups
            .iter()
            .filter_map(|group| attr_cfg_feature(group))
            .next();
        if feature.as_ref().is_some_and(|f| !features.contains(f)) {
            cursor = arrow + 2;
            continue;
        }
        match parse_macro_method_entry(&tokens, arrow, head) {
            Some((method, consumed)) => {
                out.push(method);
                cursor = consumed;
            }
            None => cursor = arrow + 2,
        }
    }
    out
}

/// Flatten one plain-import `use` tree into (module, local name) → the
/// source path as written. The path is kept verbatim (including
/// `crate::`/`self::`/`super::` prefixes); resolution to an adaptor-scope
/// `use` happens later per impl.
fn record_imports(
    tree: &syn::UseTree,
    current: &[String],
    prefix: &mut Vec<String>,
    imports: &mut HashMap<(Vec<String>, String), String>,
    glob_sources: &mut HashMap<Vec<String>, Vec<Vec<String>>>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            record_imports(&path.tree, current, prefix, imports, glob_sources);
            prefix.pop();
        }
        syn::UseTree::Glob(_) => {
            // A glob import makes every export of its source module a
            // low-priority candidate for bare-name resolution in this
            // module (`use crate::model::prelude::*`).
            if let Some(source) = normalize_use_path(current, prefix) {
                glob_sources
                    .entry(current.to_vec())
                    .or_default()
                    .push(source);
            }
        }
        syn::UseTree::Name(name) => {
            let path = prefix.join("::");
            if !path.is_empty() {
                imports.insert(
                    (current.to_vec(), name.ident.to_string()),
                    format!("{path}::{}", name.ident),
                );
            }
        }
        syn::UseTree::Rename(rename) => {
            let path = prefix.join("::");
            if !path.is_empty() {
                imports.insert(
                    (current.to_vec(), rename.rename.to_string()),
                    format!("{path}::{}", rename.ident),
                );
            } else {
                imports.insert(
                    (current.to_vec(), rename.rename.to_string()),
                    rename.ident.to_string(),
                );
            }
        }
        syn::UseTree::Group(group) => {
            for inner in &group.items {
                record_imports(inner, current, prefix, imports, glob_sources);
            }
        }
    }
}

/// Flatten one `use` tree into re-export edges. `current` is the module
/// containing the `use` (for `self::`/`super::` prefixes); paths rooted
/// at other crates cannot be resolved and are dropped.
fn record_use_tree(
    tree: &syn::UseTree,
    current: &[String],
    prefix: &mut Vec<String>,
    edges: &mut Vec<UseEdge>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            record_use_tree(&path.tree, current, prefix, edges);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let Some(src) = normalize_use_path(current, prefix) else {
                return;
            };
            // `UseName` has no rename field; renames parse as `UseRename`.
            edges.push(UseEdge::Named {
                src,
                name: name.ident.to_string(),
                alias: name.ident.to_string(),
            });
        }
        syn::UseTree::Rename(rename) => {
            let Some(src) = normalize_use_path(current, prefix) else {
                return;
            };
            edges.push(UseEdge::Named {
                src,
                name: rename.ident.to_string(),
                alias: rename.rename.to_string(),
            });
        }
        syn::UseTree::Glob(_) => {
            if let Some(src) = normalize_use_path(current, prefix) {
                edges.push(UseEdge::Glob(src));
            }
        }
        syn::UseTree::Group(group) => {
            for inner in &group.items {
                record_use_tree(inner, current, prefix, edges);
            }
        }
    }
}

/// Translate a `use` path prefix into a module path in the index:
/// `crate::a::b` -> `[a, b]`, `self::x` -> the current module, `super::x`
/// -> the parent (one level only; deeper `super::super` is dropped).
/// A bare leading segment (`foo::Bar` inside module `m`) resolves as
/// the child module `m::foo`: Rust itself prefers the in-scope item over
/// an external crate of the same name, and edges whose source module was
/// never visited are ignored when the index is queried, so genuinely
/// external roots simply never match.
fn normalize_use_path(current: &[String], prefix: &[String]) -> Option<Vec<String>> {
    match prefix.first().map(String::as_str) {
        Some("crate") => Some(prefix[1..].to_vec()),
        Some("self") => {
            let mut path = current.to_vec();
            path.extend_from_slice(&prefix[1..]);
            Some(path)
        }
        Some("super") => {
            let mut path = current[..current.len().checked_sub(1)?].to_vec();
            if prefix.len() > 2 && prefix[1] == "super" {
                return None;
            }
            path.extend_from_slice(&prefix[1..]);
            Some(path)
        }
        // A bare path is a child of the current module (`pub use
        // create_message::*` inside `builder` means
        // `builder::create_message`). Whether the child exists is checked
        // when the edge is followed, not here.
        Some(_) => {
            let mut path = current.to_vec();
            path.extend_from_slice(prefix);
            Some(path)
        }
        None => Some(current.to_vec()),
    }
}

/// Resolve an outer `mod foo;` declaration to its source file.
///
/// An explicit `#[path = "..."]` wins and is interpreted relative to the
/// directory of the containing file. Otherwise the standard Rust layout
/// applies: `mod foo` in `src/lib.rs` (or `main.rs`/`mod.rs`) reads
/// `src/foo.rs` or `src/foo/mod.rs`, while `mod foo` in `src/bar.rs` reads
/// `src/bar/foo.rs` or `src/bar/foo/mod.rs`.
fn crate_source_root(dep_src: &Path) -> PathBuf {
    std::fs::read_to_string(dep_src.join("Cargo.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|manifest| {
            manifest
                .get("lib")?
                .get("path")?
                .as_str()
                .map(|path| dep_src.join(path))
        })
        .unwrap_or_else(|| dep_src.join("src/lib.rs"))
}

fn active_attribute_meta(
    meta: &syn::Meta,
    features: &std::collections::HashSet<String>,
    output: &mut Vec<syn::Meta>,
) {
    if let syn::Meta::List(list) = meta
        && list.path.is_ident("cfg_attr")
    {
        if let Ok(args) = list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        ) {
            let mut args = args.iter();
            if args
                .next()
                .is_some_and(|predicate| eval_cfg(predicate, features))
            {
                for attr in args {
                    active_attribute_meta(attr, features, output);
                }
            }
        }
    } else {
        output.push(meta.clone());
    }
}

fn active_attributes(
    attrs: &[syn::Attribute],
    features: &std::collections::HashSet<String>,
) -> Vec<syn::Meta> {
    let mut output = Vec::new();
    for attr in attrs {
        active_attribute_meta(&attr.meta, features, &mut output);
    }
    output
}

fn module_path_override(
    module: &syn::ItemMod,
    features: &std::collections::HashSet<String>,
) -> Option<PathBuf> {
    active_attributes(&module.attrs, features)
        .into_iter()
        .find_map(|meta| match meta {
            syn::Meta::NameValue(value) if value.path.is_ident("path") => match value.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(path),
                    ..
                }) => Some(PathBuf::from(path.value())),
                _ => None,
            },
            _ => None,
        })
}

fn resolve_outer_mod(
    module: &syn::ItemMod,
    module_dir: &Path,
    path_dir: &Path,
    features: &std::collections::HashSet<String>,
) -> Option<(PathBuf, PathBuf)> {
    if let Some(relative) = module_path_override(module, features) {
        let path = path_dir.join(relative);
        let child_dir = path.parent()?.to_path_buf();
        return path.is_file().then_some((path, child_dir));
    }
    let child_dir = module_dir.join(module.ident.to_string());
    [
        module_dir.join(format!("{}.rs", module.ident)),
        child_dir.join("mod.rs"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .map(|path| (path, child_dir))
}

/// Whether every `#[cfg(...)]` attribute on an item evaluates to true
/// under the adaptor's build view: the crate's default features, `test`
/// off, and the host target assumed to satisfy non-feature predicates
/// (`unix`, `target_os = "linux"`, ...).
fn is_cfg_enabled(attrs: &[syn::Attribute], features: &std::collections::HashSet<String>) -> bool {
    active_attributes(attrs, features)
        .iter()
        .filter(|meta| meta.path().is_ident("cfg"))
        .all(|meta| {
            match meta
                .require_list()
                .and_then(|list| list.parse_args::<syn::Meta>())
            {
                Ok(meta) => eval_cfg(&meta, features),
                Err(_) => false,
            }
        })
}

/// The idents inside a `#[derive(...)]` list; empty for other attributes.
fn derive_idents(
    attrs: &[syn::Attribute],
    features: &std::collections::HashSet<String>,
) -> Vec<String> {
    active_attributes(attrs, features)
        .into_iter()
        .filter_map(|meta| match meta {
            syn::Meta::List(list) if list.path.is_ident("derive") => list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                )
                .ok(),
            _ => None,
        })
        .flatten()
        .filter_map(|path| {
            path.segments
                .last()
                .map(|segment| segment.ident.to_string())
        })
        .collect()
}

/// Whether `attrs` carries a bare marker attribute such as
/// `#[non_exhaustive]`.
fn has_marker_attr(attrs: &[syn::Attribute], marker: &str) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident(marker))
}

/// Evaluate one cfg predicate against the build view. Unknown predicates
/// default to true: a wrongly-kept function fails the adaptor's compile
/// loudly, while a wrongly-skipped one silently loses API.
fn eval_cfg(meta: &syn::Meta, features: &std::collections::HashSet<String>) -> bool {
    match meta {
        syn::Meta::List(list) => {
            let children: Vec<syn::Meta> = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .map(|parsed| parsed.into_iter().collect())
                .unwrap_or_default();
            match list
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                .as_deref()
            {
                Some("any") => children.iter().any(|child| eval_cfg(child, features)),
                Some("all") => children.iter().all(|child| eval_cfg(child, features)),
                Some("not") => !children.iter().any(|child| eval_cfg(child, features)),
                _ => true,
            }
        }
        syn::Meta::NameValue(name_value) => {
            let key = name_value
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default();
            match (key.as_str(), &name_value.value) {
                (
                    "feature",
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit),
                        ..
                    }),
                ) => features.contains(&lit.value()),
                // Target predicates evaluate against the host build target;
                // a mismatched value skips the item (losing API is safe,
                // keeping it breaks the adaptor's compile).
                (
                    "target_arch",
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit),
                        ..
                    }),
                ) => Some(lit.value()) == build_target_arch(),
                (
                    "target_os",
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit),
                        ..
                    }),
                ) => Some(lit.value()) == build_target_os(),
                // Instruction-set features are unknowable statically;
                // assume absent so gated items are skipped.
                (
                    "target_feature",
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(_),
                        ..
                    }),
                ) => false,
                _ => true,
            }
        }
        syn::Meta::Path(path) => {
            let ident = path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default();
            match ident.as_str() {
                "test" | "doc" | "doctest" => false,
                "unix" => cfg!(unix),
                "windows" => cfg!(windows),
                _ => true,
            }
        }
    }
}

/// The host build target's architecture, as spelled in `cfg(target_arch)`.
fn build_target_arch() -> Option<String> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64".into()),
        "aarch64" => Some("aarch64".into()),
        "x86" => Some("x86".into()),
        "arm" => Some("arm".into()),
        "riscv64" => Some("riscv64gc".into()),
        "powerpc64" => Some("powerpc64".into()),
        "s390x" => Some("s390x".into()),
        other => Some(other.into()),
    }
}

/// The host build target's OS, as spelled in `cfg(target_os)`.
fn build_target_os() -> Option<String> {
    Some(std::env::consts::OS.into())
}

/// Classify one `pub fn` into a bridgeable [`Func`], or `None` when it is
/// not public, is a test probe, or has an unmarshalable signature.
/// `known_types` maps a bare local type name to its root-callable path,
/// enabling struct/enum-typed params to marshal as object handles.
fn classify_fn(
    fn_item: &syn::ItemFn,
    module_path: &[String],
    features: &std::collections::HashSet<String>,
    known_types: Option<&HashMap<String, String>>,
    clone_types: &std::collections::HashSet<String>,
    index: &CrateIndex,
) -> Option<Func> {
    if !matches!(fn_item.vis, syn::Visibility::Public(_)) {
        return None;
    }
    if !is_cfg_enabled(&fn_item.attrs, features)
        || fn_item
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("test"))
    {
        return None;
    }
    // An unsafe function carries caller-side invariants that cannot be
    // inferred from its signature. Never expose it as a safe Neutron bridge;
    // the discovery report will retain it as unsupported instead.
    if fn_item.sig.unsafety.is_some() {
        return None;
    }
    // Methods (`fn foo(&self, ...)`) live on types, not at module scope,
    // and cannot be called from a plain module path.
    if fn_item.sig.receiver().is_some() {
        return None;
    }

    // Generic parameters must each resolve to a concrete marshalable type
    // (`T: AsRef<[u8]>` bridges over `String`); otherwise the function is
    // not tier-1.
    let concretized = generic_concretizations(
        &fn_item.sig.generics,
        &fn_item.sig.output,
        known_types,
        index,
    )?;

    let mut params = Vec::new();
    let mut param_ref = Vec::new();
    for input in &fn_item.sig.inputs {
        let syn::FnArg::Typed(typed) = input else {
            return None;
        };
        let (kind, ref_kind) =
            classify_param_with_ref(&typed.ty, &concretized, known_types, index)?;
        // `&mut` collections have no sound in-place write-back, and
        // `Option<&[T]>` has no exact call-site spelling through the
        // generic `&a` emission — both stay unsupported.
        if matches!(
            (&kind, ref_kind),
            (ParamKind::Array(_), ParamRef::MutRef)
                | (ParamKind::FixedArray(_, _), ParamRef::MutRef)
                | (ParamKind::CowStr, ParamRef::MutRef)
                | (ParamKind::CowBytes, ParamRef::MutRef)
                | (ParamKind::OptionArray(_), ParamRef::Ref)
                | (ParamKind::OptionArray(_), ParamRef::MutRef)
        ) {
            return None;
        }
        params.push(kind);
        param_ref.push(ref_kind);
    }

    let (ret, returns_future) = classify_function_ret(
        &fn_item.sig.output,
        &concretized,
        known_types,
        clone_types,
        index,
    )?;

    Some(Func {
        name: sanitize_ident(&fn_item.sig.ident.to_string()),
        call_path: if module_path.is_empty() {
            fn_item.sig.ident.to_string()
        } else {
            format!("{}::{}", module_path.join("::"), fn_item.sig.ident)
        },
        method_call: false,
        is_field: false,
        is_setter: false,
        trait_path: None,
        unit_variant: false,
        params,
        field_names: Vec::new(),
        param_ref,
        ret,
        is_async: fn_item.sig.asyncness.is_some() || returns_future,
        doc: doc_text(&fn_item.attrs),
        is_unsafe: fn_item.sig.unsafety.is_some(),
    })
}

/// Shared discovery context for impl-block classification. Bundled so the
/// parameter list stays callable as the bridge model grows with the
/// run-modules plan.
struct ImplClassifyCtx<'a> {
    index: &'a CrateIndex,
    exports_memo: &'a mut HashMap<(Vec<String>, String), bool>,
    features: &'a std::collections::HashSet<String>,
    known_types: Option<&'a HashMap<String, String>>,
    clone_types: &'a std::collections::HashSet<String>,
    trait_method_counts: &'a HashMap<(String, String, String), usize>,
}

/// Classify the bridgeable functions of one `impl` block — inherent or
/// trait: constructors (associated fns returning the type), static
/// utility fns, `&self`/`&mut self` methods, by-value `self` methods
/// (consuming the handle), and trait methods with receivers (called
/// through method syntax). Generic impls are skipped — the registry
/// stores concrete types.
fn classify_impl(
    impl_item: &syn::ItemImpl,
    module_path: &[String],
    ctx: ImplClassifyCtx<'_>,
) -> (Vec<Func>, Vec<UnsupportedApi>) {
    let ImplClassifyCtx {
        index,
        exports_memo,
        features,
        known_types,
        clone_types,
        trait_method_counts,
    } = ctx;
    // Every early return below records *why* the block contributes no
    // bridges: unlike free functions (whose caller lists failures), impl
    // blocks used to vanish silently. `display` names the block as well as
    // the source scan can before its type resolves.
    let display = impl_self_display(impl_item, module_path);
    let mut out = Vec::new();
    let mut unsupported = Vec::new();
    if !impl_item.generics.params.is_empty() {
        unsupported.push(impl_unsupported(
            display.clone(),
            display.clone(),
            crate::metadata::ApiStatus::Unsupported,
            "generic impl blocks are not monomorphized",
            String::new(),
        ));
        return (out, unsupported);
    }
    // A trait impl (`impl Display for X`) is called through method syntax
    // on the value; its *path* may not be nameable from the crate root.
    let is_trait_impl = impl_item.trait_.is_some();
    // The trait path as usable in the adaptor's scope. Absolute
    // std-library paths (`std::fmt::Display`) pass through verbatim. A
    // local trait is resolved through the module's imports (a bare name
    // or a relative path) and rewritten to the dependency crate's public
    // path when the defining module is root-reachable; otherwise the
    // method-call bridge cannot import the trait and is skipped.
    let trait_path: Option<String> = impl_item.trait_.as_ref().and_then(|(_, trait_path, _)| {
        resolve_trait_path(trait_path, module_path, index, exports_memo)
    });
    let syn::Type::Path(type_path) = impl_item.self_ty.as_ref() else {
        unsupported.push(impl_unsupported(
            display.clone(),
            display.clone(),
            crate::metadata::ApiStatus::Unsupported,
            "unsupported self type",
            String::new(),
        ));
        return (out, unsupported);
    };
    if type_path.qself.is_none() && type_path.path.segments.len() != 1 {
        unsupported.push(impl_unsupported(
            display.clone(),
            display.clone(),
            crate::metadata::ApiStatus::Unsupported,
            "qualified self type is not nameable from the crate root",
            String::new(),
        ));
        return (out, unsupported);
    }
    let segment = &type_path.path.segments[0];
    // A parameterized self type (`impl Foo<u8>`) cannot be stored
    // concretely without knowing the type argument.
    if !matches!(segment.arguments, syn::PathArguments::None) {
        unsupported.push(impl_unsupported(
            display.clone(),
            display.clone(),
            crate::metadata::ApiStatus::Unsupported,
            "generic self type is not monomorphized",
            String::new(),
        ));
        return (out, unsupported);
    }
    let struct_name = segment.ident.to_string();

    // The type must be nameable from the crate root (directly, through a
    // `pub mod` chain, or via re-exports) for both the call path and the
    // registry downcast. `Self` refers to the same type. When the impl
    // block lives in a different module than the definition (a private
    // module importing the type), the crate-wide known-types map supplies
    // the canonical path instead.
    let local_definition = index
        .pub_types
        .get(module_path)
        .is_some_and(|types| types.contains(&struct_name));
    let resolved_type_path = if type_path.path.segments.len() == 1 {
        match resolve_type_in_module(module_path, &struct_name, index, exports_memo) {
            ScopeResolution::Path(path) => Some(path),
            ScopeResolution::Foreign => None,
            ScopeResolution::Unknown if local_definition => None,
            ScopeResolution::Unknown => {
                known_types.and_then(|types| types.get(&struct_name).cloned())
            }
        }
    } else {
        root_type_path(module_path, &struct_name, index, exports_memo)
    };
    let Some(type_path) = resolved_type_path else {
        unsupported.push(impl_unsupported(
            display.clone(),
            display.clone(),
            crate::metadata::ApiStatus::Unsupported,
            "type is not nameable from the crate root",
            String::new(),
        ));
        return (out, unsupported);
    };

    for item in &impl_item.items {
        let syn::ImplItem::Fn(fn_item) = item else {
            continue;
        };
        // Trait-impl methods are `pub`-invisible in syn (visibility is
        // inherent-API only); inherent impls still require it. Private
        // methods are not public API, so they stay silent.
        if !is_trait_impl && !matches!(fn_item.vis, syn::Visibility::Public(_)) {
            continue;
        }
        let method_name = fn_item.sig.ident.to_string();
        let short_name = type_path.rsplit("::").next().unwrap_or(&type_path);
        let mut bridge_name = sanitize_ident(&format!("{short_name}_{method_name}"));
        let mut bridge_path = format!("{type_path}::{method_name}");
        // `Drop::drop` cannot be named explicitly (E0040) — not even
        // through UFCS — so Drop impls contribute no bridges. (The free
        // `std::mem::drop` function is unaffected.)
        if is_trait_impl
            && method_name == "drop"
            && impl_item.trait_.as_ref().is_some_and(|(_, path, _)| {
                path.segments
                    .last()
                    .is_some_and(|segment| segment.ident == "Drop")
            })
        {
            unsupported.push(impl_unsupported(
                bridge_name,
                bridge_path,
                crate::metadata::ApiStatus::Unsupported,
                "explicit Drop::drop calls are forbidden",
                render_sig(&fn_item.sig),
            ));
            continue;
        }
        // `#[test]` probes are not public API and stay silent; cfg-gated
        // items are conditional on another target/feature set, so they are
        // listed rather than dropped.
        if fn_item
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("test"))
        {
            continue;
        }
        if !is_cfg_enabled(&fn_item.attrs, features) {
            unsupported.push(impl_unsupported(
                bridge_name,
                bridge_path,
                crate::metadata::ApiStatus::Conditional,
                "disabled by cfg under the current target/feature set",
                render_sig(&fn_item.sig),
            ));
            continue;
        }
        // Unsafe methods have no safe wrapper generated yet (unlike free
        // `unsafe fn`, whose arguments are all safe owned values).
        if fn_item.sig.unsafety.is_some() {
            unsupported.push(impl_unsupported(
                bridge_name,
                bridge_path,
                crate::metadata::ApiStatus::Unsupported,
                "unsafe method has no safe wrapper",
                render_sig(&fn_item.sig),
            ));
            continue;
        }

        let Some(concretized) = generic_concretizations(
            &fn_item.sig.generics,
            &fn_item.sig.output,
            known_types,
            index,
        ) else {
            unsupported.push(impl_unsupported(
                bridge_name,
                bridge_path,
                crate::metadata::ApiStatus::Unsupported,
                "unconstrained generic parameters",
                render_sig(&fn_item.sig),
            ));
            continue;
        };

        // Non-receiver params must be tier-1 (a struct-typed param other
        // than the receiver has no marshaling). `receiver` carries the
        // skip reason on an unbridgeable signature, `Ok(None)` for an
        // associated fn.
        let mut params = Vec::new();
        let mut param_ref = Vec::new();
        let mut receiver: Result<Option<ParamRef>, &'static str> = Ok(None);
        for input in &fn_item.sig.inputs {
            match input {
                syn::FnArg::Receiver(recv) => {
                    let Some(kind) = receiver_kind(recv, Some(&struct_name)) else {
                        receiver = Err("unsupported receiver type");
                        break;
                    };
                    receiver = Ok(Some(match kind {
                        ReceiverKind::Owned => ParamRef::Owned,
                        ReceiverKind::MutRef => ParamRef::MutRef,
                        ReceiverKind::Ref => ParamRef::Ref,
                    }));
                }
                syn::FnArg::Typed(typed) => {
                    // `Self` resolves to the impl's own type, so
                    // `PartialEq::eq(&self, other: &Self)` bridges like any
                    // borrowed-object parameter. Anything deeper
                    // (`Vec<Self>`, `Option<Self>`) stays unsupported.
                    if let Some((kind, ref_kind)) = self_param_kind(&typed.ty, &type_path) {
                        params.push(kind);
                        param_ref.push(ref_kind);
                        continue;
                    }
                    // A bare parameter type shadowed by a foreign import
                    // (`use reqwest::Client` in the impl's module) has a
                    // type outside the crate entirely.
                    if foreign_shadowed(module_path, &typed.ty, index) {
                        receiver = Err("parameter type is shadowed by a foreign import");
                        break;
                    }
                    let Some((kind, ref_kind)) =
                        classify_param_with_ref(&typed.ty, &concretized, known_types, index)
                    else {
                        receiver = Err("unsupported parameter type");
                        break;
                    };
                    if matches!(
                        (&kind, ref_kind),
                        (ParamKind::Array(_), ParamRef::MutRef)
                            | (ParamKind::FixedArray(_, _), ParamRef::MutRef)
                            | (ParamKind::CowStr, ParamRef::MutRef)
                            | (ParamKind::CowBytes, ParamRef::MutRef)
                            | (ParamKind::OptionArray(_), ParamRef::Ref)
                            | (ParamKind::OptionArray(_), ParamRef::MutRef)
                    ) {
                        receiver = Err("mutable collection borrows have no bridge");
                        break;
                    }
                    params.push(kind);
                    param_ref.push(ref_kind);
                }
            }
        }
        let receiver = match receiver {
            Ok(receiver) => receiver,
            Err(reason) => {
                unsupported.push(impl_unsupported(
                    bridge_name,
                    bridge_path,
                    crate::metadata::ApiStatus::Unsupported,
                    reason,
                    render_sig(&fn_item.sig),
                ));
                continue;
            }
        };

        // Trait impls need their trait in scope. Associated fns without a
        // receiver (`From::from`, `Default::default`) bridge as path calls
        // once the trait is nameable; methods additionally need the
        // receiver. A bare trait name cannot be brought into scope without
        // import tracking (or the prelude table).
        if is_trait_impl && trait_path.is_none() {
            unsupported.push(impl_unsupported(
                bridge_name,
                bridge_path,
                crate::metadata::ApiStatus::Unsupported,
                "trait path is not nameable from the crate root",
                render_sig(&fn_item.sig),
            ));
            continue;
        }
        let method_call = is_trait_impl && receiver.is_some();

        // One trait, several argument lists (`AsRef<Http>` vs
        // `AsRef<Cache>`): a shared method (`as_ref`) is ambiguous at
        // every method-call site without a turbofish the bridge cannot
        // emit. Static fns select by argument type through path
        // resolution and are unaffected.
        if method_call {
            let trait_short = impl_item
                .trait_
                .as_ref()
                .and_then(|(_, path, _)| path.segments.last())
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default();
            if trait_method_counts
                .get(&(struct_name.clone(), trait_short, method_name.clone()))
                .is_some_and(|count| *count > 1)
            {
                unsupported.push(impl_unsupported(
                    bridge_name,
                    bridge_path,
                    crate::metadata::ApiStatus::Unsupported,
                    "ambiguous trait method shared by several impls of one trait",
                    render_sig(&fn_item.sig),
                ));
                continue;
            }
        }

        // `impl IntoFuture` for a builder (`ClientBuilder` awaiting into a
        // `Client`): the declared return is the opaque `Self::IntoFuture`
        // alias, so the sibling `type Output = ...` of the same impl block
        // is classified instead and awaited like any future.
        let into_future_ret = if is_trait_impl
            && fn_item.sig.ident == "into_future"
            && trait_path
                .as_deref()
                .is_some_and(|path| path.rsplit("::").next() == Some("IntoFuture"))
            && let Some(output_ty) = into_future_output(impl_item)
        {
            classify_impl_ret_type(
                &output_ty,
                &struct_name,
                &type_path,
                &concretized,
                known_types,
                clone_types,
                index,
            )
        } else {
            None
        };
        let Some((ret, returns_future)) = into_future_ret.map(|ret| (ret, true)).or_else(|| {
            classify_impl_function_ret(
                &fn_item.sig.output,
                &struct_name,
                &type_path,
                &concretized,
                known_types,
                clone_types,
                index,
            )
        }) else {
            unsupported.push(impl_unsupported(
                bridge_name,
                bridge_path,
                crate::metadata::ApiStatus::Unsupported,
                "unsupported return type",
                render_sig(&fn_item.sig),
            ));
            continue;
        };

        if let Some(ref_kind) = receiver {
            // Method: the receiver becomes the leading object handle,
            // named by the root-callable type path.
            params.insert(0, ParamKind::Object(type_path.clone().into()));
            param_ref.insert(0, ref_kind);
        }
        // An associated fn without a receiver is a constructor when it
        // returns the struct, otherwise a static utility fn — both are
        // bridged with the `Type::name` call path either way. The bridge
        // name/path were computed up front for failure reporting.
        out.push(Func {
            name: std::mem::take(&mut bridge_name),
            call_path: std::mem::take(&mut bridge_path),
            // Trait methods are invoked through method-call syntax on the
            // receiver; inherent items — and static trait fns, whose trait
            // is imported — through the `Type::name` path.
            method_call,
            is_field: false,
            is_setter: false,
            // Bare trait names cannot be brought into scope without
            // import tracking; only qualified trait paths bridge.
            trait_path: if is_trait_impl {
                trait_path.clone()
            } else {
                None
            },
            unit_variant: false,
            params,
            field_names: Vec::new(),
            param_ref,
            ret,
            is_async: fn_item.sig.asyncness.is_some() || returns_future,
            doc: doc_text(&fn_item.attrs),
            is_unsafe: fn_item.sig.unsafety.is_some(),
        });
    }
    (out, unsupported)
}

/// One discovery failure: the name/path the item would have carried and
/// the reason it has no bridge. Recorded in the export manifest so no
/// public API vanishes silently.
fn impl_unsupported(
    name: String,
    path: String,
    status: crate::metadata::ApiStatus,
    reason: &str,
    signature: String,
) -> UnsupportedApi {
    UnsupportedApi {
        name,
        path,
        status,
        reason: reason.to_string(),
        signature,
    }
}

/// Best-effort display name for an impl block whose type may not resolve:
/// the bare self-type identifier qualified by its module.
fn impl_self_display(impl_item: &syn::ItemImpl, module_path: &[String]) -> String {
    let ident = match impl_item.self_ty.as_ref() {
        syn::Type::Path(path) if path.qself.is_none() => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_else(|| "?".to_string()),
        _ => "?".to_string(),
    };
    if module_path.is_empty() {
        ident
    } else {
        format!("{}::{ident}", module_path.join("::"))
    }
}

/// Resolve a type name defined in `module_path` to a path callable from
/// the crate root: the shortest of the module's `pub mod` chain and any
/// re-export chain ending at a public module. Returns `None` when the
/// type is not externally nameable.
fn root_type_path(
    module_path: &[String],
    type_name: &str,
    index: &CrateIndex,
    exports_memo: &mut HashMap<(Vec<String>, String), bool>,
) -> Option<String> {
    if module_exports(module_path, type_name, index, exports_memo, 0) {
        // The type is directly reachable through this module's visibility.
        if module_path.is_empty() {
            return Some(type_name.to_string());
        }
        if index
            .public_chain
            .get(module_path)
            .copied()
            .unwrap_or(false)
        {
            return Some(format!("{}::{type_name}", module_path.join("::")));
        }
    }

    // Walk the re-export graph outward from the definition: every hop is a
    // `pub use` edge whose source module names the type at some exported
    // name (`reqwest`'s `Client` reaches the root through two hops —
    // `async_impl::client` → `async_impl` → root). Among the destinations
    // that are themselves root-reachable, the shortest chain wins; the name
    // at the destination is the re-export's alias (`pub use ...::Error as
    // GatewayError` is callable only as `GatewayError`).
    let mut best: Option<(Vec<String>, String)> = None;
    let mut frontier = vec![(module_path.to_vec(), type_name.to_string())];
    let mut seen: std::collections::HashSet<(Vec<String>, String)> =
        frontier.iter().cloned().collect();
    let mut hops = 0;
    while !frontier.is_empty() && hops <= 8 {
        hops += 1;
        let mut next_frontier = Vec::new();
        for (current_path, current_name) in &frontier {
            for (dest, edges) in &index.reexports {
                for edge in edges {
                    let exported_name = match edge {
                        UseEdge::Glob(src)
                            if src.as_slice() == current_path.as_slice()
                                && module_exports(
                                    current_path,
                                    current_name,
                                    index,
                                    exports_memo,
                                    0,
                                ) =>
                        {
                            current_name.clone()
                        }
                        UseEdge::Named {
                            src, name, alias, ..
                        } if src.as_slice() == current_path.as_slice() && name == current_name => {
                            alias.clone()
                        }
                        _ => continue,
                    };
                    // The destination module must itself be publicly
                    // reachable (or be the root).
                    let reachable =
                        dest.is_empty() || index.public_chain.get(dest).copied().unwrap_or(false);
                    if reachable && best.as_ref().is_none_or(|(b, _)| dest.len() < b.len()) {
                        best = Some((dest.clone(), exported_name.clone()));
                    }
                    if seen.insert((dest.clone(), exported_name.clone())) {
                        next_frontier.push((dest.clone(), exported_name));
                    }
                }
            }
        }
        frontier = next_frontier;
    }
    if let Some((dest, exported_name)) = best {
        if dest.is_empty() {
            return Some(exported_name);
        }
        return Some(format!("{}::{exported_name}", dest.join("::")));
    }
    None
}

/// Resolve a `pub fn` defined in `module_path` (which may be a private
/// module) to a root-visible module that re-exports it via `pub use`,
/// mirroring [`root_type_path`] for functions. Returns the destination
/// module with the name the function carries there (a `pub use` rename
/// re-exports under the alias). Returns `None` when the function is not
/// externally nameable.
fn root_function_path(
    module_path: &[String],
    fn_name: &str,
    index: &CrateIndex,
) -> Option<(Vec<String>, String)> {
    let mut best: Option<(Vec<String>, String)> = None;
    for (dest, edges) in &index.reexports {
        for edge in edges {
            let exported_name = match edge {
                UseEdge::Named { src, name, alias } => {
                    if src == module_path && name == fn_name {
                        alias.clone()
                    } else {
                        continue;
                    }
                }
                UseEdge::Glob(src) => {
                    if src == module_path {
                        fn_name.to_string()
                    } else {
                        continue;
                    }
                }
            };
            let reachable =
                dest.is_empty() || index.public_chain.get(dest).copied().unwrap_or(false);
            if reachable && best.as_ref().is_none_or(|(b, _)| dest.len() < b.len()) {
                best = Some((dest.clone(), exported_name));
            }
        }
    }
    best
}

/// A prelude-provided trait's canonical path. The standard prelude is
/// always in scope, so dependencies name these traits without importing
/// them — leaving no `use` edge for the import tracker. The adaptor
/// imports the canonical path explicitly, which is valid regardless of
/// edition. Marker traits carry no methods and never reach bridging, but
/// listing them keeps resolution total over the prelude.
fn prelude_trait_path(short: &str) -> Option<&'static str> {
    Some(match short {
        "AsMut" => "std::convert::AsMut",
        "AsRef" => "std::convert::AsRef",
        "Borrow" => "std::borrow::Borrow",
        "BorrowMut" => "std::borrow::BorrowMut",
        "Clone" => "std::clone::Clone",
        "Copy" => "std::marker::Copy",
        "Debug" => "std::fmt::Debug",
        "Default" => "std::default::Default",
        "Deref" => "std::ops::Deref",
        "DerefMut" => "std::ops::DerefMut",
        "Display" => "std::fmt::Display",
        "Drop" => "std::ops::Drop",
        "Eq" => "std::cmp::Eq",
        "Error" => "std::error::Error",
        "Extend" => "std::iter::Extend",
        "Fn" => "std::ops::Fn",
        "FnMut" => "std::ops::FnMut",
        "FnOnce" => "std::ops::FnOnce",
        "From" => "std::convert::From",
        "FromIterator" => "std::iter::FromIterator",
        "FromStr" => "std::str::FromStr",
        "Hash" => "std::hash::Hash",
        "Into" => "std::convert::Into",
        "IntoIterator" => "std::iter::IntoIterator",
        "Iterator" => "std::iter::Iterator",
        "Ord" => "std::cmp::Ord",
        "PartialEq" => "std::cmp::PartialEq",
        "PartialOrd" => "std::cmp::PartialOrd",
        "Send" => "std::marker::Send",
        "Sized" => "std::marker::Sized",
        "Sync" => "std::marker::Sync",
        "ToOwned" => "std::borrow::ToOwned",
        "ToString" => "std::string::ToString",
        "TryFrom" => "std::convert::TryFrom",
        "TryInto" => "std::convert::TryInto",
        "Unpin" => "std::marker::Unpin",
        _ => return None,
    })
}

/// Resolve an impl's trait path to one usable in the adaptor's scope.
///
/// `std::`/`core::`/`alloc::` paths pass through. A bare trait name is
/// looked up in the module's plain imports; the imported path's module
/// part (after `crate::`/`self::`/`super::` normalization) must be a
/// publicly root-reachable module whose trait the adaptor can then
/// import as `{crate}::{module}::{Trait}`. A prelude-provided trait with
/// no explicit import resolves through [`prelude_trait_path`]. Anything
/// else stays unresolvable.
fn resolve_trait_path(
    trait_path: &syn::Path,
    module_path: &[String],
    index: &CrateIndex,
    exports_memo: &mut HashMap<(Vec<String>, String), bool>,
) -> Option<String> {
    let written: Vec<String> = trait_path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    if let Some("std" | "core" | "alloc") = written.first().map(String::as_str) {
        return Some(written.join("::"));
    }

    // A bare name resolves through the module's imports.
    let imported = if written.len() == 1 {
        match index
            .imports
            .get(&(module_path.to_vec(), written[0].clone()))
        {
            Some(imported) => imported.clone(),
            // The standard prelude leaves no `use` edge, yet its traits
            // are always nameable as `std::...` for the adaptor import.
            None => return prelude_trait_path(&written[0]).map(str::to_string),
        }
    } else {
        // A relative path written directly in the impl
        // (`gadget::Inspect`).
        written.join("::")
    };
    let imported = &imported;
    let segments: Vec<&str> = imported.split("::").collect();
    // An import that lands in the standard library (`use std::ops::Index`)
    // names the trait verbatim; it is not a module of the dependency.
    if let Some("std" | "core" | "alloc") = segments.first().copied() {
        return Some(imported.clone());
    }
    let trait_name = (*segments.last()?).to_string();
    let module: Vec<String> = match segments.first().copied() {
        Some("crate") => segments[1..segments.len() - 1]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        Some("self") => {
            let mut module = module_path.to_vec();
            module.extend(
                segments[1..segments.len() - 1]
                    .iter()
                    .map(|s| s.to_string()),
            );
            module
        }
        Some("super") => {
            let mut module = module_path[..module_path.len().checked_sub(1)?].to_vec();
            module.extend(
                segments[1..segments.len() - 1]
                    .iter()
                    .map(|s| s.to_string()),
            );
            module
        }
        // `std`-style absolute paths recorded from imports.
        Some("std" | "core" | "alloc") => return Some(segments.join("::")),
        _ => return None,
    };

    // The trait's module must be publicly reachable and actually export
    // the trait for the adaptor to import it.
    if module.is_empty() || !index.public_chain.get(&module).copied().unwrap_or(false) {
        return None;
    }
    if !module_exports(&module, &trait_name, index, exports_memo, 0) {
        return None;
    }
    Some(module.join("::") + "::" + &trait_name)
}

/// Resolve a direct `Self` parameter type to the impl's own object
/// handle (`Self` by value, `&Self`, `&mut Self`). A `'static` borrow can
/// never be satisfied by a call-local handle. Anything deeper
/// (`Vec<Self>`) falls through to normal classification, which rejects
/// the bare `Self` name as unresolvable.
fn self_param_kind(ty: &syn::Type, type_path: &str) -> Option<(ParamKind, ParamRef)> {
    match ty {
        syn::Type::Path(path)
            if path.qself.is_none()
                && path.path.segments.len() == 1
                && path.path.segments[0].ident == "Self"
                && matches!(path.path.segments[0].arguments, syn::PathArguments::None) =>
        {
            Some((
                ParamKind::Object(type_path.to_string().into()),
                ParamRef::Owned,
            ))
        }
        syn::Type::Reference(reference) => {
            if reference
                .lifetime
                .as_ref()
                .is_some_and(|lifetime| lifetime.ident == "static")
            {
                return None;
            }
            let (kind, _) = self_param_kind(&reference.elem, type_path)?;
            let ref_kind = if reference.mutability.is_some() {
                ParamRef::MutRef
            } else {
                ParamRef::Ref
            };
            // Only plain `Self` borrows resolve here; anything the
            // recursion accepted through another shape stays out.
            match kind {
                ParamKind::Object(_) => Some((kind, ref_kind)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether a module makes `type_name` available to importers: defined
/// there publicly, or re-exported by it (following one level of
/// re-export chains).
fn module_exports(
    module_path: &[String],
    type_name: &str,
    index: &CrateIndex,
    exports_memo: &mut HashMap<(Vec<String>, String), bool>,
    depth: usize,
) -> bool {
    if depth > 4 {
        return false;
    }
    let key = (module_path.to_vec(), type_name.to_string());
    if let Some(cached) = exports_memo.get(&key) {
        return *cached;
    }
    // Mark in-progress as false to break cycles.
    exports_memo.insert(key.clone(), false);

    let defined = index
        .pub_types
        .get(module_path)
        .is_some_and(|types| types.contains(type_name));
    let result = defined
        || index.reexports.get(module_path).is_some_and(|edges| {
            edges.iter().any(|edge| match edge {
                UseEdge::Glob(src) => {
                    module_exports(src, type_name, index, exports_memo, depth + 1)
                }
                UseEdge::Named { src, name, .. } => {
                    name == type_name
                        && module_exports(src, type_name, index, exports_memo, depth + 1)
                }
            })
        });
    exports_memo.insert(key, result);
    result
}

/// Classify a return type in an impl context, where the impl's type,
/// `Self`, and every other root-nameable type additionally bridge to an
/// object handle.
fn classify_impl_function_ret(
    output: &syn::ReturnType,
    struct_name: &str,
    type_path: &str,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    clone_types: &std::collections::HashSet<String>,
    index: &CrateIndex,
) -> Option<(RetKind, bool)> {
    let syn::ReturnType::Type(_, ty) = output else {
        return Some((RetKind::Unit, false));
    };
    if let Some(output_ty) = future_output_type(ty) {
        return classify_impl_ret_type(
            output_ty,
            struct_name,
            type_path,
            concretized,
            known_types,
            clone_types,
            index,
        )
        .map(|ret| (ret, true));
    }
    classify_impl_ret_type(
        ty,
        struct_name,
        type_path,
        concretized,
        known_types,
        clone_types,
        index,
    )
    .map(|ret| (ret, false))
}

/// The sibling `type Output = ...` of an `impl IntoFuture` block, the real
/// (awaitable) return of `into_future` hiding behind `Self::IntoFuture`.
fn into_future_output(impl_item: &syn::ItemImpl) -> Option<syn::Type> {
    impl_item.items.iter().find_map(|item| match item {
        syn::ImplItem::Type(assoc) if assoc.ident == "Output" => Some(assoc.ty.clone()),
        _ => None,
    })
}

fn classify_impl_ret_type(
    ty: &syn::Type,
    struct_name: &str,
    type_path: &str,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    clone_types: &std::collections::HashSet<String>,
    index: &CrateIndex,
) -> Option<RetKind> {
    // A returned *reference* to the struct borrows the receiver (builder
    // chaining: `fn f(&mut self) -> &mut Self`); the registry cannot
    // store it. Only owned struct returns become object handles.
    if let syn::Type::Reference(reference) = ty
        && is_struct_type(&reference.elem, struct_name)
    {
        return None;
    }
    if is_struct_type(ty, struct_name) {
        return Some(RetKind::Object(type_path.to_string().into()));
    }
    // `Arc<Self>` / `Arc<Mutex<Self>>` returns store the shared
    // allocation; the impl context resolves `Self`.
    if let syn::Type::Path(path) = ty
        && path.qself.is_none()
        && let Some(obj) = arc_object(&path.path, Some(type_path), known_types, index)
    {
        return Some(RetKind::Object(obj));
    }
    if let syn::Type::Path(path) = ty
        && path.qself.is_none()
    {
        let segment = path.path.segments.last()?;
        if segment.ident == "Result" {
            let args = type_args(segment)?;
            let inner = first_type_arg(args)?;
            if is_struct_type(inner, struct_name) {
                return Some(RetKind::ResultObject(type_path.to_string().into()));
            }
            // `Result<OtherKnownType, E>` stores the `Ok` value the same
            // way; the error side throws through `Display` as usual.
            if let Some(inner_name) = resolve_type_name(inner)
                && let Some(known) = known_types.and_then(|types| types.get(&inner_name))
                && !contains_non_static_lifetime(inner)
            {
                return Some(RetKind::ResultObject(known.clone().into()));
            }
            // `Result<Arc<Self>, E>` and its mutex flavors store the
            // shared allocation on `Ok`.
            if let Some(inner_path) = match inner {
                syn::Type::Path(path) if path.qself.is_none() => Some(&path.path),
                _ => None,
            } && let Some(obj) = arc_object(inner_path, Some(type_path), known_types, index)
                && !contains_non_static_lifetime(inner)
            {
                return Some(RetKind::ResultObject(obj));
            }
        }
    }
    classify_ret_type(ty, concretized, known_types, clone_types, index)
}

/// Whether a type is the impl's struct (`Self` or the bare struct path).
fn is_struct_type(ty: &syn::Type, struct_name: &str) -> bool {
    match ty {
        syn::Type::Reference(reference) => is_struct_type(&reference.elem, struct_name),
        syn::Type::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => {
            let ident = &path.path.segments[0].ident;
            ident == struct_name || ident == "Self"
        }
        _ => false,
    }
}

/// Whether a bare type name in this module is shadowed by a plain import
/// of a type from outside the crate (`use reqwest::Client`): such a
/// parameter's real type lives outside the dependency, while the bare-name
/// lookup would wrongly match a local type of the same name. Imports from
/// direct dependencies never reach this check — the foreign rewrite
/// qualifies them first, so they classify as foreign objects.
fn foreign_shadowed(module_path: &[String], ty: &syn::Type, index: &CrateIndex) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    if path.qself.is_some() || path.path.segments.len() != 1 {
        return false;
    }
    // Generic arguments are classified by shape, never by bare-name
    // lookup (`Arc<Engine>` is not a lookup of `Arc`).
    if !matches!(path.path.segments[0].arguments, syn::PathArguments::None) {
        return false;
    }
    let ident = path.path.segments[0].ident.to_string();
    // A module-local type alias shadows any same-named public type for
    // signatures in this module — its target is opaque to the bridge.
    if index
        .local_type_aliases
        .get(module_path)
        .is_some_and(|aliases| aliases.contains(&ident))
    {
        return true;
    }
    let Some(written) = index.imports.get(&(module_path.to_vec(), ident)) else {
        return false;
    };
    let first = written.split("::").next().unwrap_or_default();
    !matches!(first, "crate" | "self" | "super")
        && !index
            .public_chain
            .keys()
            .any(|module| module.first().is_some_and(|segment| segment == first))
}

fn alias_key_for(path: &syn::Path, module_path: &[String], index: &CrateIndex) -> Option<String> {
    let written = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");
    if path.segments.len() > 1 {
        let qualified = written.strip_prefix("crate::").unwrap_or(&written);
        if index.type_aliases.contains_key(qualified) {
            return Some(qualified.to_string());
        }
        return None;
    }
    if let Some(imported) = index.imports.get(&(module_path.to_vec(), written.clone())) {
        let imported = imported.strip_prefix("crate::").unwrap_or(imported);
        if index.type_aliases.contains_key(imported) {
            return Some(imported.to_string());
        }
    }
    for depth in (0..=module_path.len()).rev() {
        let key = if depth == 0 {
            written.clone()
        } else {
            format!("{}::{}", module_path[..depth].join("::"), written)
        };
        if index.type_aliases.contains_key(&key) {
            return Some(key);
        }
    }
    None
}

fn substitute_alias_type(ty: &syn::Type, substitutions: &HashMap<String, syn::Type>) -> syn::Type {
    if let syn::Type::Path(path) = ty
        && path.qself.is_none()
        && path.path.segments.len() == 1
        && path.path.segments[0].arguments.is_empty()
        && let Some(replacement) = substitutions.get(&path.path.segments[0].ident.to_string())
    {
        return replacement.clone();
    }
    let mut out = ty.clone();
    substitute_alias_type_in_place(&mut out, substitutions);
    out
}

fn substitute_alias_type_in_place(ty: &mut syn::Type, substitutions: &HashMap<String, syn::Type>) {
    match ty {
        syn::Type::Path(path) => {
            for segment in &mut path.path.segments {
                if let syn::PathArguments::AngleBracketed(args) = &mut segment.arguments {
                    for arg in &mut args.args {
                        if let syn::GenericArgument::Type(inner) = arg {
                            *inner = substitute_alias_type(inner, substitutions);
                        }
                    }
                }
            }
        }
        syn::Type::Reference(reference) => {
            substitute_alias_type_in_place(&mut reference.elem, substitutions)
        }
        syn::Type::Slice(slice) => substitute_alias_type_in_place(&mut slice.elem, substitutions),
        syn::Type::Array(array) => substitute_alias_type_in_place(&mut array.elem, substitutions),
        syn::Type::Tuple(tuple) => {
            for elem in &mut tuple.elems {
                substitute_alias_type_in_place(elem, substitutions);
            }
        }
        syn::Type::Paren(paren) => substitute_alias_type_in_place(&mut paren.elem, substitutions),
        syn::Type::Group(group) => substitute_alias_type_in_place(&mut group.elem, substitutions),
        _ => {}
    }
}

fn expand_type_aliases(ty: &syn::Type, module_path: &[String], index: &CrateIndex) -> syn::Type {
    fn expand(
        ty: &syn::Type,
        module_path: &[String],
        index: &CrateIndex,
        depth: usize,
    ) -> syn::Type {
        if depth >= 8 {
            return ty.clone();
        }
        let syn::Type::Path(path) = ty else {
            let mut out = ty.clone();
            substitute_alias_type_in_place(&mut out, &HashMap::new());
            return out;
        };
        let Some(key) = alias_key_for(&path.path, module_path, index) else {
            return ty.clone();
        };
        let Some(alias) = index.type_aliases.get(&key) else {
            return ty.clone();
        };
        let args = path
            .path
            .segments
            .last()
            .map(|segment| match &segment.arguments {
                syn::PathArguments::AngleBracketed(args) => args
                    .args
                    .iter()
                    .filter_map(|arg| match arg {
                        syn::GenericArgument::Type(ty) => Some(ty.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        let substitutions = alias
            .params
            .iter()
            .cloned()
            .zip(args)
            .collect::<HashMap<_, _>>();
        let alias_module: Vec<String> = key
            .rsplit_once("::")
            .map(|(module, _)| module.split("::").map(String::from).collect())
            .unwrap_or_default();
        let target = qualify_foreign_ty(&alias.target, &alias_module, index);
        let replaced = substitute_alias_type(&target, &substitutions);
        expand(&replaced, module_path, index, depth + 1)
    }
    expand(ty, module_path, index, 0)
}

fn expand_fn_aliases(
    fn_item: &syn::ItemFn,
    module_path: &[String],
    index: &CrateIndex,
) -> syn::ItemFn {
    let mut expanded = fn_item.clone();
    for input in &mut expanded.sig.inputs {
        if let syn::FnArg::Typed(typed) = input {
            let expanded_ty = expand_type_aliases(&typed.ty, module_path, index);
            let qualified_ty = qualify_foreign_ty(&expanded_ty, module_path, index);
            *typed.ty = qualified_ty;
        }
    }
    if let syn::ReturnType::Type(_, ty) = &mut expanded.sig.output {
        let expanded_ty = expand_type_aliases(ty, module_path, index);
        **ty = qualify_foreign_ty(&expanded_ty, module_path, index);
    }
    expanded
}

fn expand_impl_aliases(
    impl_item: &syn::ItemImpl,
    module_path: &[String],
    index: &CrateIndex,
) -> syn::ItemImpl {
    let mut expanded = impl_item.clone();
    for item in &mut expanded.items {
        if let syn::ImplItem::Fn(function) = item {
            for input in &mut function.sig.inputs {
                if let syn::FnArg::Typed(typed) = input {
                    let expanded_ty = expand_type_aliases(&typed.ty, module_path, index);
                    let qualified_ty = qualify_foreign_ty(&expanded_ty, module_path, index);
                    *typed.ty = qualified_ty;
                }
            }
            if let syn::ReturnType::Type(_, ty) = &mut function.sig.output {
                let expanded_ty = expand_type_aliases(ty, module_path, index);
                **ty = qualify_foreign_ty(&expanded_ty, module_path, index);
            }
        }
    }
    expanded
}

/// The bridgeable receiver kind of one method receiver, or `None` for
/// shapes the adaptor cannot supply: a typed receiver other than the
/// impl's struct itself (`self: &Arc<Self>` — syn parses the written type
/// in place of the shorthand) and any `'static`-borrowing receiver. In a
/// trait inventory (`struct_name` = `None`) the receiver must be spelled
/// with `Self`, since the implementor is not known yet.
fn receiver_kind(recv: &syn::Receiver, struct_name: Option<&str>) -> Option<ReceiverKind> {
    if let Some((_, lifetime)) = &recv.reference
        && lifetime
            .as_ref()
            .is_some_and(|lifetime| lifetime.ident == "static")
    {
        return None;
    }
    if recv.colon_token.is_some() {
        let shape_ok = match recv.ty.as_ref() {
            syn::Type::Path(_) if recv.reference.is_none() => {
                is_receiver_self(recv.ty.as_ref(), struct_name)
            }
            syn::Type::Reference(reference) => is_receiver_self(&reference.elem, struct_name),
            _ => false,
        };
        if !shape_ok {
            return None;
        }
    }
    Some(if recv.reference.is_none() {
        ReceiverKind::Owned
    } else if recv.mutability.is_some() {
        ReceiverKind::MutRef
    } else {
        ReceiverKind::Ref
    })
}

/// Whether a receiver type is exactly the unparameterized struct itself
/// (`Self`, or the bare struct name inside its own impl).
fn is_receiver_self(ty: &syn::Type, struct_name: Option<&str>) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    path.qself.is_none()
        && path.path.segments.len() == 1
        && matches!(path.path.segments[0].arguments, syn::PathArguments::None)
        && match path.path.segments[0].ident.to_string().as_str() {
            "Self" => true,
            name => struct_name.is_some_and(|s| s == name),
        }
}

/// Map each generic type parameter to the concrete marshaling kind it
/// bridges over, or `None` when any parameter (or a const parameter)
/// cannot be concretized. Bounds come from both the inline list (`T:
/// AsRef<str>`) and matching `where` predicates (`where T: AsRef<str>`).
/// `output` rejects behavior-parameterized generics whose type appears in
/// the return (`iter<T: FromStr>() -> Iter<'_, T>`): the caller cannot
/// choose a type the signature infers.
fn generic_concretizations(
    generics: &syn::Generics,
    output: &syn::ReturnType,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<HashMap<String, ParamKind>> {
    let mut map = HashMap::new();
    for param in &generics.params {
        let type_param = match param {
            syn::GenericParam::Lifetime(_) => continue,
            syn::GenericParam::Type(type_param) => type_param,
            syn::GenericParam::Const(_) => return None,
        };
        // Collect the `where`-clause bounds for this parameter alongside
        // the inline ones; either position expresses the same contract.
        let mut bounds = type_param.bounds.clone();
        if let Some(clause) = &generics.where_clause {
            for predicate in &clause.predicates {
                let syn::WherePredicate::Type(bounded) = predicate else {
                    continue;
                };
                if matches!(
                    &bounded.bounded_ty,
                    syn::Type::Path(path)
                        if path.qself.is_none()
                            && path.path.segments.len() == 1
                            && path.path.segments[0].ident == type_param.ident
                ) {
                    bounds.extend(bounded.bounds.clone());
                }
            }
        }
        let kind = concretize_trait_bounds(&bounds, known_types, index)?;
        if matches!(kind, ParamKind::Callback(_))
            && type_in_output(&type_param.ident.to_string(), output)
        {
            return None;
        }
        map.insert(type_param.ident.to_string(), kind);
    }
    Some(map)
}

/// Whether a bare type name is mentioned anywhere in a return type.
fn type_in_output(name: &str, output: &syn::ReturnType) -> bool {
    let syn::ReturnType::Type(_, ty) = output else {
        return false;
    };
    fn contains(ty: &syn::Type, name: &str) -> bool {
        match ty {
            syn::Type::Path(path) => path.path.segments.iter().any(|segment| {
                segment.ident == name
                    || match &segment.arguments {
                        syn::PathArguments::AngleBracketed(args) => {
                            args.args.iter().any(|arg| match arg {
                                syn::GenericArgument::Type(inner) => contains(inner, name),
                                _ => false,
                            })
                        }
                        _ => false,
                    }
            }),
            syn::Type::Reference(reference) => contains(&reference.elem, name),
            syn::Type::Slice(slice) => contains(&slice.elem, name),
            syn::Type::Tuple(tuple) => tuple.elems.iter().any(|elem| contains(elem, name)),
            syn::Type::Array(array) => contains(&array.elem, name),
            _ => false,
        }
    }
    contains(ty, name)
}

/// Resolve an `impl Trait` parameter/return type's bounds to a concrete
/// marshalable kind, e.g. `impl AsRef<str>` -> `Str`, `impl Into<u64>` ->
/// `Int("u64")`. Mirrors the per-parameter step of
/// [`generic_concretizations`] for inline `impl` types.
fn concretize_impl_trait(
    bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<ParamKind> {
    concretize_trait_bounds(bounds, known_types, index)
}

/// Map a trait-bound list (`AsRef<str>`, `Into<String>`, `Into<int>`)
/// to a concrete marshalable kind. The first recognized value bound wins;
/// any other single behavior bound (e.g. `H: EventHandler + 'static`)
/// becomes a [`ParamKind::Callback`] carrying the trait's short name, to be
/// resolved against the trait inventory post-walk (monomorphized over
/// in-crate implementors, or implemented by the generated `__NtscCb`
/// struct when Neutron provides the behavior).
fn concretize_trait_bounds(
    bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<ParamKind> {
    let mut callback: Option<String> = None;
    for bound in bounds {
        let syn::TypeParamBound::Trait(trait_bound) = bound else {
            continue;
        };
        let segment = trait_bound.path.segments.last()?;
        let ident = segment.ident.to_string();
        match ident.as_str() {
            // Marker traits compose with any bound and carry no behavior
            // the bridge must provide.
            "Send" | "Sync" | "Unpin" | "Sized" => continue,
            "AsRef" => {
                let ty = first_type_arg(type_args(segment)?)?;
                // Both `AsRef<str>` and `AsRef<[u8]>` bridge over `String`:
                // a string handle marshals to `String`, which is `AsRef` of
                // both, and the bridge passes it by reference. `OsStr` and
                // `Path` marshal the same way (`String: AsRef<OsStr>` and
                // `String: AsRef<Path>` — dotenv's `var<K: AsRef<OsStr>>`).
                if inner_is_str_like(ty)
                    || inner_is_byte_like(ty)
                    || is_u8(ty)
                    || matches!(
                        resolve_type_name(ty).as_deref(),
                        Some("OsStr") | Some("Path")
                    )
                {
                    return Some(ParamKind::Str);
                }
                // `AsRef<Http>` over a framework type has no reflexive impl
                // (`Http: AsRef<Http>` does not hold), so it monomorphizes
                // over the in-crate implementors (`Context: AsRef<Http>`)
                // through the behavior-bound machinery below, exactly like
                // `impl CacheHttp`. The bound arguments travel with the
                // callback key so only matching implementors apply.
                if object_bound_target(ty, known_types, index).is_some() {
                    if callback.is_some() {
                        return None;
                    }
                    callback = Some(format!("AsRef{}", trait_args_string(segment)));
                    continue;
                }
            }
            "Into" => {
                let ty = first_type_arg(type_args(segment)?)?;
                if matches!(resolve_type_name(ty).as_deref(), Some("Vec")) {
                    return classify_param_kind(ty, &HashMap::new(), known_types, index);
                }
                let Some(inner_name) = resolve_type_name(ty) else {
                    continue;
                };
                match inner_name.as_str() {
                    "String" | "str" => return Some(ParamKind::Str),
                    "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64"
                    | "isize" | "u128" | "i128" => return Some(ParamKind::Int(inner_name)),
                    "f32" | "f64" => return Some(ParamKind::Float(inner_name)),
                    "bool" => return Some(ParamKind::Bool),
                    // `Into<MessageId>` bridges over the object: the call
                    // passes it by value and inference unifies `T` through
                    // the reflexive `From<T> for T` impl. A dependency-rooted
                    // target (`Into<serenity::X>`) bridges over the foreign
                    // object the same way.
                    _ => {
                        if let Some(obj) = object_bound_target(ty, known_types, index) {
                            return Some(ParamKind::Object(obj));
                        }
                    }
                }
            }
            // A behavior bound Neutron must satisfy: closure sugar
            // (`FnOnce(u64)`) has no nameable trait to implement, and two
            // distinct behavior bounds would need two coordinated impls,
            // so both stay unbridgeable.
            _ => {
                if matches!(segment.arguments, syn::PathArguments::Parenthesized(_))
                    || callback.is_some()
                {
                    return None;
                }
                callback = Some(format!("{ident}{}", trait_args_string(segment)));
            }
        }
    }
    callback.map(ParamKind::Callback)
}

/// Resolve `Arc<T>` / `Arc<Mutex<T>>` / `Arc<RwLock<T>>` (tokio, std,
/// and parking_lot mutex flavors, spelled fully) to `(wrapper, inner
/// type)`. Anything else — including a bare `Mutex<T>` outside `Arc`,
/// an `Arc` with the wrong arity, or an ambiguous short spelling
/// (`sync::Mutex`, which could be std or tokio) — yields `None`.
fn arc_wrapper_parts(path: &syn::Path) -> Option<(ObjectWrapper, &syn::Type)> {
    let segment = path.segments.last()?;
    if segment.ident != "Arc" {
        return None;
    }
    let inner = first_type_arg(type_args(segment)?)?;
    let syn::Type::Path(inner_path) = inner else {
        return None;
    };
    if inner_path.qself.is_some() {
        return None;
    }
    if !inner_path
        .path
        .segments
        .last()
        .is_some_and(|segment| matches!(segment.ident.to_string().as_str(), "Mutex" | "RwLock"))
    {
        return Some((ObjectWrapper::Arc, inner));
    }
    let segments: Vec<String> = inner_path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    let (name, module) = segments.split_last()?;
    let flavor = match (module, name.as_str()) {
        ([a, b], "Mutex") if a.as_str() == "tokio" && b.as_str() == "sync" => {
            ObjectWrapper::TokioMutex
        }
        ([a, b], "RwLock") if a.as_str() == "tokio" && b.as_str() == "sync" => {
            ObjectWrapper::TokioRwLock
        }
        ([a, b], "Mutex") if a.as_str() == "std" && b.as_str() == "sync" => ObjectWrapper::StdMutex,
        ([a, b], "RwLock") if a.as_str() == "std" && b.as_str() == "sync" => {
            ObjectWrapper::StdRwLock
        }
        ([a], "Mutex") if a.as_str() == "parking_lot" => ObjectWrapper::ParkingMutex,
        ([a], "RwLock") if a.as_str() == "parking_lot" => ObjectWrapper::ParkingRwLock,
        _ => return None,
    };
    let lock_segment = inner_path.path.segments.last()?;
    let inner_inner = first_type_arg(type_args(lock_segment)?)?;
    Some((flavor, inner_inner))
}

/// Resolve one object base type to `(owner, path)`: bare `Self` through
/// the impl context, bare local names and qualified local spellings
/// through `known_types`, and dependency-rooted paths to the foreign
/// owner. Generic arguments and unknown names yield `None`.
fn object_base(
    ty: &syn::Type,
    self_path: Option<&str>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<(Option<String>, String)> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    if path.qself.is_none() && path.path.segments.len() == 1 {
        let segment = &path.path.segments[0];
        if !matches!(segment.arguments, syn::PathArguments::None) {
            return None;
        }
        let ident = segment.ident.to_string();
        if ident == "Self" {
            return self_path.map(|base| (None, base.to_string()));
        }
        return known_types
            .and_then(|types| types.get(&ident).cloned())
            .map(|base| (None, base));
    }
    if path.qself.is_some() {
        return None;
    }
    if path
        .path
        .segments
        .iter()
        .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
    {
        return None;
    }
    let segments: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    let written = segments.join("::");
    if let Some(base) = known_types.and_then(|types| types.get(&written).cloned()) {
        return Some((None, base));
    }
    split_foreign_path(index, &segments).map(|(owner, rest)| (Some(owner), rest))
}

/// Resolve one `Arc`-family type to an object type, or `None` when the
/// shape or its base is unbridgeable. `self_path` carries the impl
/// context for `Arc<Self>` (`None` outside `impl` blocks, where `Self`
/// cannot resolve).
fn arc_object(
    path: &syn::Path,
    self_path: Option<&str>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<ObjectType> {
    let (wrapper, inner) = arc_wrapper_parts(path)?;
    let (owner, base) = object_base(inner, self_path, known_types, index)?;
    Some(ObjectType {
        owner,
        path: base,
        wrapper,
    })
}

/// The root-callable object for an `AsRef<T>`/`Into<T>` bound target:
/// a bare local identifier resolves through `known_types`
/// (`Http` -> `client::Http`); a dependency-rooted path resolves to the
/// foreign type itself (`serenity::X` stays foreign). Qualified spellings
/// with generic arguments and unknown names yield `None`.
fn object_bound_target(
    ty: &syn::Type,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<ObjectType> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    if path.path.segments.len() > 1 {
        if path
            .path
            .segments
            .iter()
            .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
        {
            return None;
        }
        let segments: Vec<String> = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if let Some(known) = known_types.and_then(|types| types.get(&segments.join("::"))) {
            return Some(ObjectType::local(known.clone()));
        }
        return split_foreign_path(index, &segments)
            .map(|(owner, rest)| ObjectType::foreign(owner, rest));
    }
    let name = resolve_type_name(ty)?;
    known_types
        .and_then(|types| types.get(&name).cloned())
        .map(ObjectType::local)
}

/// Whether a type is `String`/`str` (behind any number of references).
fn inner_is_str_like(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Reference(reference) => inner_is_str_like(&reference.elem),
        syn::Type::Path(path) if path.qself.is_none() => {
            let Some(segment) = path.path.segments.last() else {
                return false;
            };
            if segment.ident == "String" || segment.ident == "str" {
                return true;
            }
            if segment.ident == "Cow" {
                return type_args(segment)
                    .and_then(first_type_arg)
                    .is_some_and(inner_is_str_like);
            }
            false
        }
        _ => false,
    }
}

/// Whether a type is a byte container (`[u8]`, `&[u8]`, `[u8; N]`,
/// `Vec<u8>`).
fn inner_is_byte_like(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Reference(reference) => inner_is_byte_like(&reference.elem),
        syn::Type::Slice(slice) => is_u8(&slice.elem),
        syn::Type::Array(array) => is_u8(&array.elem),
        syn::Type::Path(path) if path.qself.is_none() => {
            path.path.segments.last().is_some_and(|segment| {
                let inner = type_args(segment).and_then(first_type_arg);
                (segment.ident == "Vec" && inner.is_some_and(is_u8))
                    || (segment.ident == "Cow" && inner.is_some_and(inner_is_byte_like))
            })
        }
        _ => false,
    }
}

/// Extract an item's `///` doc comments into one string. Doc comments are
/// `#[doc = "..."]` attributes in syn; the conventional leading space of
/// each line is stripped and lines are joined with newlines.
fn doc_text(attrs: &[syn::Attribute]) -> String {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .filter_map(|attr| match &attr.meta {
            syn::Meta::NameValue(value) => match &value.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(text),
                    ..
                }) => Some(text.value()),
                _ => None,
            },
            _ => None,
        })
        .map(|line| line.strip_prefix(' ').unwrap_or(&line).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Classify a parameter type together with how it is passed at the call
/// site (`&T`/`&mut T` vs. by value).
/// Classify one trait-method parameter in the trait's own module scope. A
/// bare, argument-less name resolves through the module's local items,
/// explicit imports, and glob re-export chains, so a crate-wide ambiguous
/// name (`serenity` has two `Ready`s) still resolves to the type the
/// trait actually imports; a foreign import stays unbridgeable.
fn classify_trait_param(
    module_path: &[String],
    ty: &syn::Type,
    index: &CrateIndex,
    exports_memo: &mut HashMap<(Vec<String>, String), bool>,
    known_types: &HashMap<String, String>,
) -> Option<(ParamKind, ParamRef)> {
    if let syn::Type::Path(path) = ty
        && path.qself.is_none()
        && path.path.segments.len() == 1
        && matches!(path.path.segments[0].arguments, syn::PathArguments::None)
    {
        let name = path.path.segments[0].ident.to_string();
        match resolve_type_in_module(module_path, &name, index, exports_memo) {
            ScopeResolution::Path(canonical) => {
                return Some((ParamKind::Object(canonical.into()), ParamRef::Owned));
            }
            // Explicitly imported from another crate (`use reqwest::Client`):
            // the parameter's type is outside the crate entirely.
            ScopeResolution::Foreign => return None,
            ScopeResolution::Unknown => {}
        }
    }
    classify_param_with_ref(ty, &HashMap::new(), Some(known_types), index)
}

/// The outcome of a module-scoped bare-name lookup.
enum ScopeResolution {
    /// The type's root-callable canonical path (crate-internal).
    Path(String),
    /// The name is imported from another crate.
    Foreign,
    /// The name does not resolve in this scope; the caller falls back to
    /// the crate-wide map.
    Unknown,
}

/// Resolve a bare type name as the Rust name-resolution rules order it for
/// one module: local items and explicit imports first, then glob imports.
fn resolve_type_in_module(
    module_path: &[String],
    name: &str,
    index: &CrateIndex,
    exports_memo: &mut HashMap<(Vec<String>, String), bool>,
) -> ScopeResolution {
    if index
        .pub_types
        .get(module_path)
        .is_some_and(|types| types.contains(name))
    {
        return root_type_path(module_path, name, index, exports_memo)
            .map_or(ScopeResolution::Unknown, ScopeResolution::Path);
    }
    if let Some(written) = index.imports.get(&(module_path.to_vec(), name.to_string())) {
        let segments: Vec<String> = written.split("::").map(str::to_string).collect();
        let first = segments.first().map(String::as_str).unwrap_or_default();
        let local = matches!(first, "crate" | "self" | "super")
            || index
                .public_chain
                .keys()
                .any(|module| module.first().is_some_and(|segment| segment == first));
        if !local {
            return ScopeResolution::Foreign;
        }
        match normalize_use_path(module_path, &segments) {
            Some(normalized) if !normalized.is_empty() => {
                // The last segment is the type's name at its source; the
                // rest is the module it was imported from.
                let (source_name, source) = normalized.split_last().expect("non-empty");
                return root_type_path(source, source_name, index, exports_memo)
                    .map_or(ScopeResolution::Unknown, ScopeResolution::Path);
            }
            _ => return ScopeResolution::Foreign,
        }
    }
    let mut definitions = std::collections::HashSet::new();
    for source in index.glob_imports.get(module_path).into_iter().flatten() {
        collect_exported_definitions(source, name, index, 0, &mut definitions);
    }
    match definitions.len() {
        1 => {
            let defined_at = definitions.iter().next().expect("exactly one");
            root_type_path(defined_at, name, index, exports_memo)
                .map_or(ScopeResolution::Unknown, ScopeResolution::Path)
        }
        // Several definitions are reachable through the module's globs —
        // as ambiguous as the crate-wide view, so no resolution.
        _ => ScopeResolution::Unknown,
    }
}

/// Collect the modules that `module` exports `name` from, following its
/// re-export edges down to definitions (`pub use` chains and globs).
fn collect_exported_definitions(
    module: &[String],
    name: &str,
    index: &CrateIndex,
    depth: usize,
    out: &mut std::collections::HashSet<Vec<String>>,
) {
    if depth > 4 {
        return;
    }
    if index
        .pub_types
        .get(module)
        .is_some_and(|types| types.contains(name))
    {
        out.insert(module.to_vec());
        return;
    }
    for edge in index.reexports.get(module).into_iter().flatten() {
        match edge {
            UseEdge::Named {
                src,
                name: edge_name,
                ..
            } if edge_name == name => {
                collect_exported_definitions(src, name, index, depth + 1, out);
            }
            UseEdge::Glob(src) => {
                collect_exported_definitions(src, name, index, depth + 1, out);
            }
            _ => {}
        }
    }
}

fn classify_param_with_ref(
    ty: &syn::Type,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<(ParamKind, ParamRef)> {
    if let syn::Type::Reference(reference) = ty {
        // A borrowed parameter marshals as a call-local borrow, which can
        // never satisfy an explicit `'static` requirement
        // (`StandardFramework::group(&'static CommandGroup)`); named or
        // elided lifetimes are instantiated per call and stay bridgeable.
        if reference
            .lifetime
            .as_ref()
            .is_some_and(|lifetime| lifetime.ident == "static")
        {
            return None;
        }
        let kind = classify_param_kind(&reference.elem, concretized, known_types, index)?;
        let ref_kind = if reference.mutability.is_some() {
            ParamRef::MutRef
        } else {
            ParamRef::Ref
        };
        return Some((kind, ref_kind));
    }
    let kind = classify_param_kind(ty, concretized, known_types, index)?;
    Some((kind, ParamRef::Owned))
}

fn cow_param_kind(segment: &syn::PathSegment) -> Option<ParamKind> {
    let inner = first_type_arg(type_args(segment)?)?;
    match inner {
        syn::Type::Path(path)
            if path.qself.is_none()
                && path
                    .path
                    .segments
                    .last()
                    .is_some_and(|last| last.ident == "str") =>
        {
            Some(ParamKind::CowStr)
        }
        syn::Type::Slice(slice) if is_u8(&slice.elem) => Some(ParamKind::CowBytes),
        _ => None,
    }
}

/// Classify the value type of one parameter.
fn classify_param_kind(
    ty: &syn::Type,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<ParamKind> {
    match ty {
        // `impl AsRef<str>` / `impl Into<String>` / `impl AsRef<[u8]>` etc.
        // appear directly as parameter types (e.g. serenity's
        // `Client::new(token: impl AsRef<str>, ...)`). Resolve them the same
        // way bounded type parameters are concretized.
        syn::Type::ImplTrait(impl_trait) => {
            concretize_impl_trait(&impl_trait.bounds, known_types, index)
        }
        syn::Type::Reference(reference) => {
            classify_param_kind(&reference.elem, concretized, known_types, index)
        }
        syn::Type::Slice(slice) if is_u8(&slice.elem) => Some(ParamKind::Bytes),
        syn::Type::Array(array) if is_u8(&array.elem) => Some(ParamKind::Bytes),
        // A fixed-size `[T; N]` parameter for a scalar/object `T` and a
        // compile-time-known numeric `N`: read the array handle and
        // length-check it into `[T; N]`. A const-generic or named-constant
        // length yields `None` and stays unsupported rather than emitting an
        // unprovable conversion.
        syn::Type::Array(array) => {
            let elem = array_elem_kind(&array.elem, concretized, known_types, index)?;
            let len = const_array_len(&array.len)?;
            Some(ParamKind::FixedArray(elem, len))
        }
        // Slices of scalars read element-wise from an array handle.
        syn::Type::Slice(slice) => {
            array_elem_kind(&slice.elem, concretized, known_types, index).map(ParamKind::Array)
        }
        syn::Type::Path(path) if path.qself.is_none() => {
            // `Arc<T>` and its mutex flavors bridge over the shared
            // allocation (see `arc_object`); `Arc<Self>` cannot resolve
            // outside `impl` returns and falls through below.
            if let Some(obj) = arc_object(&path.path, None, known_types, index) {
                return Some(ParamKind::Object(obj));
            }
            // Durations marshal as millis ints (see `ParamKind::Duration`).
            if path.path.segments.len() == 3
                && path.path.segments[0].ident == "std"
                && path.path.segments[1].ident == "time"
                && path.path.segments[2].ident == "Duration"
                && matches!(path.path.segments[2].arguments, syn::PathArguments::None)
            {
                return Some(ParamKind::Duration);
            }
            // A qualified path resolves only by its written spelling
            // (`crate::model::id::ChannelId`, registered as such); a
            // foreign path (`reqwest::Client`) must never match by its
            // bare last segment. As a fallback, a path rooted in a direct
            // dependency names a foreign object bridged through the shared
            // space under the owner's tag.
            if path.path.segments.len() > 1 {
                if path.path.segments.len() == 3
                    && path.path.segments[0].ident == "std"
                    && path.path.segments[1].ident == "borrow"
                    && path.path.segments[2].ident == "Cow"
                {
                    return cow_param_kind(path.path.segments.last()?);
                }
                if path
                    .path
                    .segments
                    .iter()
                    .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
                {
                    return None;
                }
                let segments: Vec<String> = path
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect();
                let written = segments.join("::");
                if let Some(path) = known_types.and_then(|types| types.get(&written).cloned()) {
                    return Some(ParamKind::Object(path.into()));
                }
                return split_foreign_path(index, &segments)
                    .map(|(owner, rest)| ParamKind::Object(ObjectType::foreign(owner, rest)));
            }
            let segment = path.path.segments.last()?;
            let ident = segment.ident.to_string();

            // A generic parameter resolved by `generic_concretizations`.
            if let Some(kind) = concretized.get(&ident) {
                return Some(kind.clone());
            }
            match ident.as_str() {
                "String" | "str" => Some(ParamKind::Str),
                "bool" => Some(ParamKind::Bool),
                "f32" | "f64" => Some(ParamKind::Float(ident)),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => Some(ParamKind::Int(ident)),
                "Vec" => {
                    let args = type_args(segment)?;
                    let inner = first_type_arg(args)?;
                    if is_u8(inner) {
                        return Some(ParamKind::Bytes);
                    }
                    vec_array_elem(segment, concretized, known_types, index).map(ParamKind::Array)
                }
                "Cow" => cow_param_kind(segment),
                // `Option<Vec<T>>` for scalar `T`: the null handle is
                // `None`, an array handle is `Some`. Byte vectors stay
                // unbridged here — a null byte handle already reads as
                // empty, which would conflate `None` and `Some(vec![])`.
                "Option" => {
                    let inner = first_type_arg(type_args(segment)?)?;
                    if let Some(elem) = vec_ty_array_elem(inner, concretized, known_types, index) {
                        return Some(ParamKind::OptionArray(elem));
                    }
                    // A borrowed string inside the option (`Option<&str>`)
                    // needs scratch storage (see `OptionStrRef`).
                    if let syn::Type::Reference(reference) = inner
                        && inner_is_str_like(&reference.elem)
                    {
                        return Some(ParamKind::OptionStrRef);
                    }
                    match classify_param_kind(inner, concretized, known_types, index)? {
                        ParamKind::Int(ty) => Some(ParamKind::OptionInt(ty)),
                        ParamKind::Bool => Some(ParamKind::OptionBool),
                        ParamKind::Float(ty) => Some(ParamKind::OptionFloat(ty)),
                        // `Option<String>` reads a string handle, null for
                        // `None`; `Option<T>` objects move into shared
                        // space on `Some` (by-value takes, like arrays of
                        // objects — borrows have no meaning for `None`).
                        ParamKind::Str => Some(ParamKind::OptionStr),
                        ParamKind::Object(obj) => Some(ParamKind::OptionObject(obj)),
                        _ => None,
                    }
                }
                // A local struct/enum type (by name only; a path is used
                // as written and cannot be resolved without import
                // tracking) marshals as an object handle.
                _ => known_types.and_then(|types| {
                    types
                        .get(&ident)
                        .map(|type_path| ParamKind::Object(type_path.clone().into()))
                }),
            }
        }
        _ => None,
    }
}

/// Classify a function return, recognizing concrete Future wrappers and
/// exposing their `Output` through the same async operation ABI.
fn classify_function_ret(
    output: &syn::ReturnType,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    clone_types: &std::collections::HashSet<String>,
    index: &CrateIndex,
) -> Option<(RetKind, bool)> {
    match output {
        syn::ReturnType::Default => Some((RetKind::Unit, false)),
        syn::ReturnType::Type(_, ty) => {
            if let Some(output_ty) = future_output_type(ty) {
                return classify_ret_type(output_ty, concretized, known_types, clone_types, index)
                    .map(|ret| (ret, true));
            }
            classify_ret_type(ty, concretized, known_types, clone_types, index)
                .map(|ret| (ret, false))
        }
    }
}

fn future_output_type(ty: &syn::Type) -> Option<&syn::Type> {
    match ty {
        syn::Type::ImplTrait(impl_trait) => impl_trait.bounds.iter().find_map(future_bound_output),
        syn::Type::TraitObject(object) => object.bounds.iter().find_map(future_bound_output),
        syn::Type::Path(path) if path.qself.is_none() => {
            path.path
                .segments
                .iter()
                .rev()
                .find_map(|segment| match &segment.arguments {
                    syn::PathArguments::AngleBracketed(args)
                        if segment.ident == "Future"
                            || segment.ident == "Box"
                            || segment.ident == "Pin" =>
                    {
                        args.args.iter().find_map(|arg| match arg {
                            syn::GenericArgument::AssocType(binding)
                                if binding.ident == "Output" =>
                            {
                                Some(&binding.ty)
                            }
                            syn::GenericArgument::Type(inner) => future_output_type(inner),
                            _ => None,
                        })
                    }
                    _ => None,
                })
        }
        _ => None,
    }
}

fn future_bound_output(bound: &syn::TypeParamBound) -> Option<&syn::Type> {
    let syn::TypeParamBound::Trait(trait_bound) = bound else {
        return None;
    };
    let segment = trait_bound.path.segments.last()?;
    if segment.ident != "Future" {
        return None;
    }
    type_args(segment)?.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::AssocType(binding) if binding.ident == "Output" => Some(&binding.ty),
        _ => None,
    })
}

fn classify_ret_type(
    ty: &syn::Type,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    clone_types: &std::collections::HashSet<String>,
    index: &CrateIndex,
) -> Option<RetKind> {
    match ty {
        syn::Type::Reference(reference) => {
            let ret = classify_ret_type(
                &reference.elem,
                concretized,
                known_types,
                clone_types,
                index,
            );
            match ret {
                // A borrowed return marshals by copy (strings, bytes) or —
                // when the value is a known, clone-able type — by cloning it
                // into the registry (`json`'s `Index::index`).
                Some(RetKind::Object(path)) => {
                    let bare = resolve_type_name(&reference.elem);
                    if bare
                        .as_deref()
                        .is_some_and(|name| clone_types.contains(name))
                    {
                        return Some(RetKind::ObjectClone(path));
                    }
                    None
                }
                Some(RetKind::ResultObject(path)) => {
                    let bare = resolve_type_name(&reference.elem);
                    if bare
                        .as_deref()
                        .is_some_and(|name| clone_types.contains(name))
                    {
                        return Some(RetKind::ResultObject(path));
                    }
                    None
                }
                Some(RetKind::Array(ArrayElemKind::Object(_)))
                | Some(RetKind::OptionArray(ArrayElemKind::Object(_)))
                | Some(RetKind::ResultArray(ArrayElemKind::Object(_))) => None,
                Some(ret) => Some(ret),
                None => None,
            }
        }
        syn::Type::Path(path) if path.qself.is_none() => {
            // `Arc<T>` returns store the shared allocation (see
            // `arc_object`).
            if let Some(obj) = arc_object(&path.path, None, known_types, index) {
                return Some(RetKind::Object(obj));
            }
            // Qualified return paths resolve only by their written
            // spelling, mirroring the parameter rule — except when the
            // tail is a std primitive type (`io::Result<String>` in a
            // foreign-qualified spelling is `Result<String>`).
            let tail = path.path.segments.last()?;
            let tail_primitive = matches!(
                tail.ident.to_string().as_str(),
                "Result"
                    | "Option"
                    | "Vec"
                    | "String"
                    | "str"
                    | "bool"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "usize"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "isize"
                    | "i128"
                    | "f32"
                    | "f64"
            );
            if path.path.segments.len() > 1 && !tail_primitive {
                if path.path.segments.len() == 3
                    && path.path.segments[0].ident == "std"
                    && path.path.segments[1].ident == "borrow"
                    && path.path.segments[2].ident == "Cow"
                {
                    return match cow_param_kind(path.path.segments.last()?)? {
                        ParamKind::CowStr => Some(RetKind::Str),
                        ParamKind::CowBytes => Some(RetKind::Bytes),
                        _ => None,
                    };
                }
                if path
                    .path
                    .segments
                    .iter()
                    .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
                {
                    return None;
                }
                let segments: Vec<String> = path
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect();
                let written = segments.join("::");
                if let Some(path) = known_types.and_then(|types| types.get(&written).cloned()) {
                    return Some(RetKind::Object(path.into()));
                }
                return split_foreign_path(index, &segments)
                    .map(|(owner, rest)| RetKind::Object(ObjectType::foreign(owner, rest)));
            }
            let segment = tail;
            let ident = segment.ident.to_string();

            if let Some(kind) = concretized.get(&ident) {
                return match kind {
                    ParamKind::Str => Some(RetKind::Str),
                    ParamKind::Bytes => Some(RetKind::Bytes),
                    _ => None,
                };
            }
            match ident.as_str() {
                "String" | "str" => Some(RetKind::Str),
                // A path return renders to a string handle (`dotenv() ->
                // Result<PathBuf>`); string-like results lose nothing.
                "PathBuf" | "Path" => Some(RetKind::Path),
                // `Cow<str>` derefs to `str`, `Cow<[u8]>` to `[u8]`
                // (`urlencoding::encode`).
                "Cow" => match cow_param_kind(segment)? {
                    ParamKind::CowStr => Some(RetKind::Str),
                    ParamKind::CowBytes => Some(RetKind::Bytes),
                    _ => None,
                },
                "bool" => Some(RetKind::Bool),
                "f32" | "f64" => Some(RetKind::Float(ident)),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => Some(RetKind::Int(ident)),
                "Vec" => {
                    let args = type_args(segment)?;
                    let inner = first_type_arg(args)?;
                    if is_u8(inner) {
                        return Some(RetKind::Bytes);
                    }
                    vec_array_elem(segment, concretized, known_types, index).map(RetKind::Array)
                }
                "Option" | "Result" => {
                    let args = type_args(segment)?;
                    let inner = first_type_arg(args)?;
                    if ident == "Result"
                        && let syn::Type::Tuple(tuple) = inner
                        && !tuple.elems.is_empty()
                    {
                        let objects = tuple
                            .elems
                            .iter()
                            .map(|element| {
                                match classify_param_kind(element, concretized, known_types, index)?
                                {
                                    ParamKind::Object(object) => Some(object),
                                    _ => None,
                                }
                            })
                            .collect::<Option<Vec<_>>>()?;
                        return Some(RetKind::ResultObjectTuple(objects));
                    }
                    // `Result<(), E>`: the unit success carries no value.
                    if ident == "Result"
                        && matches!(inner, syn::Type::Tuple(tuple) if tuple.elems.is_empty())
                    {
                        return Some(RetKind::ResultUnit);
                    }
                    // `Result<PathBuf, E>`: the `Ok` path renders to a
                    // string handle (`dotenv() -> Result<PathBuf>`).
                    if ident == "Result"
                        && matches!(
                            resolve_type_name(inner).as_deref(),
                            Some("PathBuf") | Some("Path")
                        )
                    {
                        return Some(RetKind::ResultPath);
                    }
                    // `Option<&KnownType>`: the value is cloned into the
                    // registry on `Some` (`json`'s `Object::get`), null
                    // handle for `None`.
                    if ident == "Option" {
                        if let Some(ParamKind::Object(obj)) =
                            classify_param_kind(inner, concretized, known_types, index)
                            && obj.wrapper != ObjectWrapper::Plain
                        {
                            return Some(RetKind::OptionObject(obj));
                        }
                        let inner_bare = resolve_type_name(inner).or_else(|| match inner {
                            syn::Type::Reference(reference) if reference.mutability.is_none() => {
                                resolve_type_name(&reference.elem)
                            }
                            _ => None,
                        });
                        if let Some(known) = inner_bare
                            .as_deref()
                            .and_then(|name| known_types.and_then(|types| types.get(name)))
                            && clone_types.contains(inner_bare.as_deref().unwrap_or_default())
                        {
                            return Some(RetKind::OptionObject(known.clone().into()));
                        }
                    }
                    // `Result<KnownType, E>`: the `Ok` value is stored in
                    // the object registry like any other struct return.
                    if ident == "Result"
                        && let Some(inner_name) = resolve_type_name(inner)
                        && let Some(known) = known_types.and_then(|types| types.get(&inner_name))
                        && !contains_non_static_lifetime(inner)
                    {
                        return Some(RetKind::ResultObject(known.clone().into()));
                    }
                    // `Result<Arc<T>, E>`: the shared allocation moves into
                    // shared space on `Ok`, exactly like a plain object
                    // return; the error side throws through `Display`.
                    if ident == "Result"
                        && let Some(ParamKind::Object(obj)) =
                            classify_param_kind(inner, concretized, known_types, index)
                        && obj.wrapper != ObjectWrapper::Plain
                        && !contains_non_static_lifetime(inner)
                    {
                        return Some(RetKind::ResultObject(obj));
                    }
                    // `Option<Vec<T>>` / `Result<Vec<T>, E>` for scalar
                    // `T`: an array handle on `Some`/`Ok`. Byte vectors
                    // keep their byte-bridge below.
                    if let Some(elem) = vec_ty_array_elem(inner, concretized, known_types, index) {
                        return Some(if ident == "Option" {
                            RetKind::OptionArray(elem)
                        } else {
                            RetKind::ResultArray(elem)
                        });
                    }
                    if ident == "Option"
                        && let Some(kind) =
                            classify_param_kind(inner, concretized, known_types, index)
                    {
                        match kind {
                            ParamKind::Int(ty) => return Some(RetKind::OptionInt(ty)),
                            ParamKind::Bool => return Some(RetKind::OptionBool),
                            ParamKind::Float(ty) => return Some(RetKind::OptionFloat(ty)),
                            _ => {}
                        }
                    }
                    let str_kind = if inner_is_str_like(inner) {
                        Some(true)
                    } else if inner_is_byte_like(inner) {
                        Some(false)
                    } else {
                        match resolve_type_name(inner)
                            .as_deref()
                            .and_then(|name| concretized.get(name))
                        {
                            Some(ParamKind::Str) => Some(true),
                            Some(ParamKind::Bytes) => Some(false),
                            _ => None,
                        }
                    }?;
                    match ident.as_str() {
                        "Option" if str_kind => Some(RetKind::OptionStr),
                        "Option" => Some(RetKind::OptionBytes),
                        "Result" if str_kind => Some(RetKind::ResultStr),
                        _ => Some(RetKind::ResultBytes),
                    }
                }
                _ => {
                    // An object return is boxed; a value that borrows
                    // something non-`'static` (`RawArguments<'_>`) would
                    // dangle the moment the call returns. Copy-marshalable
                    // returns (strings, bytes) are unaffected.
                    if contains_non_static_lifetime(ty) {
                        return None;
                    }
                    known_types.and_then(|types| {
                        types
                            .get(&ident)
                            .cloned()
                            .map(|path| RetKind::Object(path.into()))
                    })
                }
            }
        }
        // A fixed-size `[T; N]` or slice `[T]` return of scalar (or
        // object-handle) `T` marshals through the array handle: the
        // bridge reads `result` in place, and `[T; N]`/`&[T]` expose the
        // same `len`/`iter` used to fill the handle, so no length-carrying
        // kind is needed on the return side. Borrowed object arrays are
        // rejected above (`contains_non_static_lifetime` / the reference
        // arm), so only owned or copy-marshalable elements bridge.
        syn::Type::Array(array) => {
            array_elem_kind(&array.elem, concretized, known_types, index).map(RetKind::Array)
        }
        syn::Type::Slice(slice) => {
            array_elem_kind(&slice.elem, concretized, known_types, index).map(RetKind::Array)
        }
        _ => None,
    }
}

/// Whether `ty` mentions any lifetime that is not `'static`: elided and
/// named lifetimes tie the value to a borrow that dies with the call.
fn contains_non_static_lifetime(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Reference(reference) => {
            match &reference.lifetime {
                Some(lifetime) if lifetime.ident == "static" => {}
                _ => return true,
            }
            contains_non_static_lifetime(&reference.elem)
        }
        syn::Type::Path(path) => {
            path.path
                .segments
                .iter()
                .any(|segment| match &segment.arguments {
                    syn::PathArguments::AngleBracketed(args) => {
                        args.args.iter().any(|arg| match arg {
                            syn::GenericArgument::Lifetime(lifetime) => lifetime.ident != "static",
                            syn::GenericArgument::Type(inner) => {
                                contains_non_static_lifetime(inner)
                            }
                            _ => false,
                        })
                    }
                    _ => false,
                })
        }
        syn::Type::Slice(slice) => contains_non_static_lifetime(&slice.elem),
        syn::Type::Tuple(tuple) => tuple.elems.iter().any(contains_non_static_lifetime),
        syn::Type::Array(array) => contains_non_static_lifetime(&array.elem),
        _ => false,
    }
}

/// The numeric length of a fixed-size array type (`[T; 4]` -> `4`), or
/// `None` for a const-generic or named-constant length the bridge cannot
/// prove at generation time (those stay unsupported).
fn const_array_len(len: &syn::Expr) -> Option<usize> {
    match len {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(value),
            ..
        }) => value.base10_parse::<usize>().ok(),
        _ => None,
    }
}

/// The bare name of a simple path type (`Vec<u8>` -> `Vec`, `T` -> `T`).
fn resolve_type_name(ty: &syn::Type) -> Option<String> {
    if let syn::Type::Path(path) = ty
        && path.qself.is_none()
    {
        return path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
    }
    None
}

fn is_u8(ty: &syn::Type) -> bool {
    resolve_type_name(ty).as_deref() == Some("u8")
}

/// The collection element kind of `ty` when it is a plain scalar value
/// (`String`/`str`, `bool`, an integer, or a float). A concretized generic
/// parameter (`T: AsRef<str>` bridging over `String`) resolves through
/// `concretized` — inference unifies the generated `Vec<String>` with the
/// declared `Vec<T>`. Borrowed elements (`Vec<&str>`) yield `None`: the
/// bridge would have to invent a borrow the caller never provided, so
/// they stay unsupported rather than generating code that cannot compile.
/// Byte blobs, nested collections, options, and objects also yield `None`.
fn array_elem_kind(
    ty: &syn::Type,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<ArrayElemKind> {
    match ty {
        syn::Type::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => {
            let segment = path.path.segments.last()?;
            let ident = segment.ident.to_string();
            if let Some(kind) = concretized.get(&ident) {
                return match kind {
                    ParamKind::Str => Some(ArrayElemKind::Str),
                    ParamKind::Bool => Some(ArrayElemKind::Bool),
                    ParamKind::Int(ty) => Some(ArrayElemKind::Int(ty.clone())),
                    ParamKind::Float(ty) => Some(ArrayElemKind::Float(ty.clone())),
                    _ => None,
                };
            }
            match ident.as_str() {
                "String" | "str" => Some(ArrayElemKind::Str),
                "bool" => Some(ArrayElemKind::Bool),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => Some(ArrayElemKind::Int(ident)),
                "f32" | "f64" => Some(ArrayElemKind::Float(ident)),
                _ => known_types
                    .and_then(|types| types.get(&ident))
                    .map(|path| ArrayElemKind::Object(path.clone().into())),
            }
        }
        syn::Type::Path(path) if path.qself.is_none() => {
            let segments: Vec<String> = path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let written = segments.join("::");
            if let Some(path) = known_types.and_then(|types| types.get(&written)) {
                return Some(ArrayElemKind::Object(path.clone().into()));
            }
            split_foreign_path(index, &segments)
                .map(|(owner, rest)| ArrayElemKind::Object(ObjectType::foreign(owner, rest)))
        }
        _ => None,
    }
}

/// The collection element kind of the single type argument of `Vec<T>`,
/// when `T` is a plain scalar. `Vec<u8>` is handled by the byte-bridge
/// before this is consulted.
fn vec_array_elem(
    segment: &syn::PathSegment,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<ArrayElemKind> {
    let args = type_args(segment)?;
    array_elem_kind(first_type_arg(args)?, concretized, known_types, index)
}

/// The collection element kind when `ty` is `Vec<T>` for scalar `T`
/// (`Vec<u8>` excluded — it bridges as bytes).
fn vec_ty_array_elem(
    ty: &syn::Type,
    concretized: &HashMap<String, ParamKind>,
    known_types: Option<&HashMap<String, String>>,
    index: &CrateIndex,
) -> Option<ArrayElemKind> {
    if let syn::Type::Path(path) = ty
        && path.qself.is_none()
        && path.path.segments.len() == 1
    {
        let segment = path.path.segments.last()?;
        if segment.ident == "Vec"
            && !type_args(segment)
                .and_then(first_type_arg)
                .is_some_and(is_u8)
        {
            return vec_array_elem(segment, concretized, known_types, index);
        }
    }
    None
}

/// The generic arguments of a path segment, if any.
fn type_args(segment: &syn::PathSegment) -> Option<&syn::AngleBracketedGenericArguments> {
    match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => Some(args),
        _ => None,
    }
}

/// The first generic argument, when it is a type.
fn first_type_arg(args: &syn::AngleBracketedGenericArguments) -> Option<&syn::Type> {
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

/// Render a `syn` signature in compact Rust notation for diagnostics
/// (`fn set_global_commands(&self, commands: &[CreateCommand])`).
/// Total over inputs: exotic patterns render as `_` rather than failing
/// discovery.
fn render_sig(sig: &syn::Signature) -> String {
    let mut args = Vec::new();
    for input in &sig.inputs {
        match input {
            syn::FnArg::Receiver(recv) => args.push(render_receiver(recv)),
            syn::FnArg::Typed(typed) => {
                let name = match typed.pat.as_ref() {
                    syn::Pat::Ident(pat) => pat.ident.to_string(),
                    _ => "_".to_string(),
                };
                args.push(format!("{name}: {}", render_ty(&typed.ty)));
            }
        }
    }
    let ret = match &sig.output {
        syn::ReturnType::Default => String::new(),
        syn::ReturnType::Type(_, ty) => format!(" -> {}", render_ty(ty)),
    };
    format!("fn {}({}){ret}", sig.ident, args.join(", "))
}

/// Render a `self` receiver (`&self`, `&'a mut self`, `self: Rc<Self>`).
fn render_receiver(recv: &syn::Receiver) -> String {
    if recv.colon_token.is_some() {
        return format!("self: {}", render_ty(&recv.ty));
    }
    let prefix = match (&recv.reference, &recv.mutability) {
        (None, _) => return "self".to_string(),
        (Some((_, lifetime)), None) => lifetime
            .as_ref()
            .map(|lt| format!("&'{} ", lt.ident))
            .unwrap_or_else(|| "&".to_string()),
        (Some((_, lifetime)), Some(_)) => lifetime
            .as_ref()
            .map(|lt| format!("&'{} mut ", lt.ident))
            .unwrap_or_else(|| "&mut ".to_string()),
    };
    format!("{prefix}self")
}

/// Render a `syn` type in compact Rust notation for diagnostics.
/// Deterministic and total: every shape renders something, with `_` for
/// the exotic remainder (macros, raw pointers, inferred types).
fn render_ty(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => {
            let mut out = String::new();
            if path.qself.is_some() {
                out.push_str("_.");
            }
            out + &path
                .path
                .segments
                .iter()
                .map(render_path_segment)
                .collect::<Vec<_>>()
                .join("::")
        }
        syn::Type::Reference(reference) => {
            let lifetime = reference
                .lifetime
                .as_ref()
                .map(|lt| format!("'{} ", lt.ident))
                .unwrap_or_default();
            let mutable = if reference.mutability.is_some() {
                "mut "
            } else {
                ""
            };
            format!("&{lifetime}{mutable}{}", render_ty(&reference.elem))
        }
        syn::Type::Slice(slice) => format!("[{}]", render_ty(&slice.elem)),
        syn::Type::Array(array) => {
            format!("[{}; {}]", render_ty(&array.elem), render_expr(&array.len))
        }
        syn::Type::Tuple(tuple) => format!(
            "({})",
            tuple
                .elems
                .iter()
                .map(render_ty)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        syn::Type::Paren(paren) => format!("({})", render_ty(&paren.elem)),
        syn::Type::Group(group) => render_ty(&group.elem),
        syn::Type::ImplTrait(tr) => format!(
            "impl {}",
            tr.bounds
                .iter()
                .map(render_bound)
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        syn::Type::TraitObject(tr) => format!(
            "dyn {}",
            tr.bounds
                .iter()
                .map(render_bound)
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        syn::Type::BareFn(func) => {
            let args = func
                .inputs
                .iter()
                .map(|arg| render_ty(&arg.ty))
                .collect::<Vec<_>>()
                .join(", ");
            let ret = match &func.output {
                syn::ReturnType::Default => String::new(),
                syn::ReturnType::Type(_, ty) => format!(" -> {}", render_ty(ty)),
            };
            format!("fn({args}){ret}")
        }
        syn::Type::Never(_) => "!".to_string(),
        syn::Type::Infer(_) => "_".to_string(),
        _ => "_".to_string(),
    }
}

/// Render one path segment with its generic arguments
/// (`HashMap<String, Vec<u8>>`, `FromStr`, `Fn(u64)`).
fn render_path_segment(segment: &syn::PathSegment) -> String {
    let base = segment.ident.to_string();
    match &segment.arguments {
        syn::PathArguments::None => base,
        syn::PathArguments::AngleBracketed(args) => {
            let rendered = args
                .args
                .iter()
                .map(|arg| match arg {
                    syn::GenericArgument::Lifetime(lt) => format!("'{}", lt.ident),
                    syn::GenericArgument::Type(ty) => render_ty(ty),
                    syn::GenericArgument::Const(expr) => render_expr(expr),
                    syn::GenericArgument::AssocType(binding) => {
                        format!("{} = {}", binding.ident, render_ty(&binding.ty))
                    }
                    _ => "_".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{base}<{rendered}>")
        }
        syn::PathArguments::Parenthesized(args) => {
            let inputs = args
                .inputs
                .iter()
                .map(render_ty)
                .collect::<Vec<_>>()
                .join(", ");
            let ret = match &args.output {
                syn::ReturnType::Default => String::new(),
                syn::ReturnType::Type(_, ty) => format!(" -> {}", render_ty(ty)),
            };
            format!("{base}({inputs}){ret}")
        }
    }
}

/// Render one trait bound (`AsRef<str>`, `CacheHttp + 'static`).
fn render_bound(bound: &syn::TypeParamBound) -> String {
    match bound {
        syn::TypeParamBound::Trait(tr) => tr
            .path
            .segments
            .iter()
            .map(render_path_segment)
            .collect::<Vec<_>>()
            .join("::"),
        syn::TypeParamBound::Lifetime(lt) => format!("'{}", lt.ident),
        _ => "_".to_string(),
    }
}

/// Render a const-expression bound (`[T; 4]`, `[T; MAX]`, `[T; _]`).
fn render_expr(expr: &syn::Expr) -> String {
    match expr {
        syn::Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Int(int) => int.base10_digits().to_string(),
            syn::Lit::Bool(bool) => bool.value.to_string(),
            syn::Lit::Str(s) => format!("{:?}", s.value()),
            _ => "_".to_string(),
        },
        syn::Expr::Path(path) => path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        syn::Expr::Unary(unary) => format!("-{}", render_expr(&unary.expr)),
        _ => "_".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ntsc_adaptor_test_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Render one adaptor `src/lib.rs` for tests: version `0.1.0`, no
    /// features, no foreign owners (fixtures without `[dependencies]`
    /// never classify foreign types).
    fn emit_lib(name: &str, discovered: &DiscoveredApi) -> String {
        adaptor_lib(
            name,
            "0.1.0",
            "",
            &std::collections::BTreeMap::new(),
            &discovered.functions,
            &discovered.callbacks,
        )
    }

    #[test]
    fn bridge_names_sanitize_raw_identifiers() {
        assert_eq!(sanitize_ident("ToolCall_r#type"), "ToolCall_r_type");
        assert_eq!(sanitize_ident("r#match"), "r_match");
    }

    #[test]
    fn emit_fingerprint_is_stable_and_shares_every_input() {
        let tmp = TempDir::new("emit_fingerprint");
        let src = tmp.0.join("serenity-0.12.5");
        fs::create_dir_all(src.join("src")).unwrap();
        fs::write(src.join("Cargo.toml"), "[package]\nname = \"serenity\"\n").unwrap();
        fs::write(src.join("src").join("lib.rs"), "pub fn gateway() {}\n").unwrap();
        let project = tmp.0.join("proj");
        fs::create_dir_all(&project).unwrap();

        let base = || emit_fingerprint("serenity", "0.12.5", None, &src, &project);
        let before = base();
        // Deterministic for unchanged inputs.
        assert_eq!(base(), before);

        // Crate identity changes reshape the output.
        assert_ne!(
            before,
            emit_fingerprint("serenity", "0.12.6", None, &src, &project)
        );
        assert_ne!(
            before,
            emit_fingerprint("gateway", "0.12.5", None, &src, &project)
        );
        // An enabled feature adds bridges.
        let features = std::collections::HashSet::from(["voice".to_string()]);
        assert_ne!(
            before,
            emit_fingerprint("serenity", "0.12.5", Some(&features), &src, &project)
        );
        // An edited source tree (a `path` dependency change) must re-emit.
        fs::write(
            src.join("src").join("lib.rs"),
            "pub fn edited_and_longer() {}\n",
        )
        .unwrap();
        assert_ne!(before, base());
        // A new source file likewise changes the digest.
        fs::write(src.join("src").join("extra.rs"), "pub struct Added;\n").unwrap();
        assert_ne!(before, base());
    }

    #[test]
    fn adaptor_up_to_date_requires_matching_stamp_and_all_outputs() {
        let tmp = TempDir::new("up_to_date");
        let dir = tmp.0.join("serenity");
        fs::create_dir_all(dir.join("src")).unwrap();
        let stamp = dir.join(".emit-stamp");
        let fingerprint = "deadbeefdeadbeef";

        // Empty: not up to date.
        assert!(!adaptor_up_to_date(&dir, "serenity", &stamp, fingerprint));

        fs::write(dir.join("Cargo.toml"), "manifest").unwrap();
        fs::write(dir.join("src").join("lib.rs"), "lib").unwrap();
        fs::write(dir.join("serenity.export.json"), "export").unwrap();
        fs::write(&stamp, fingerprint).unwrap();
        // A matching stamp with every output present is a hit.
        assert!(adaptor_up_to_date(&dir, "serenity", &stamp, fingerprint));
        // A different fingerprint (changed inputs) forces a re-emit.
        assert!(!adaptor_up_to_date(
            &dir,
            "serenity",
            &stamp,
            "cafebabecafebabe"
        ));
        // A missing output must not be trusted even with a matching stamp.
        fs::remove_file(dir.join("src").join("lib.rs")).unwrap();
        assert!(!adaptor_up_to_date(&dir, "serenity", &stamp, fingerprint));
    }

    #[test]
    fn discovers_all_module_layouts_and_conditional_attributes() {
        let dir = TempDir::new("module_layouts");
        let files = [
            (
                "Cargo.toml",
                "[package]\nname = \"layouts\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"code/root.rs\"\n[features]\ndefault = [\"native\"]\nnative = []\n",
            ),
            (
                "code/root.rs",
                r#"
                pub mod outer;
                pub mod inline { pub mod child; }
                #[path = "shared.rs"] pub mod first;
                #[path = "shared.rs"] pub mod second;
                #[cfg_attr(feature = "native", path = "platform/native.rs")]
                pub mod platform;
                #[cfg_attr(feature = "native", cfg(any()))]
                pub mod missing;
                mod hidden;
                pub use hidden::exported;
            "#,
            ),
            ("code/outer.rs", "pub mod inner;"),
            ("code/outer/inner/mod.rs", "pub mod leaf;"),
            ("code/outer/inner/leaf.rs", "pub fn nested() -> u64 { 1 }"),
            ("code/inline/child.rs", "pub fn inline_child() -> u64 { 2 }"),
            ("code/shared.rs", "pub fn shared() -> u64 { 3 }"),
            (
                "code/platform/native.rs",
                r#"
                pub mod sibling;
                #[cfg_attr(feature = "native", derive(Clone, Default))]
                pub struct Settings { pub value: u64 }
                #[cfg_attr(feature = "native", cfg(any()))]
                pub struct Disabled { pub value: u64 }
                #[cfg_attr(feature = "native", cfg(any()))]
                pub fn absent() -> u64 { 0 }
            "#,
            ),
            (
                "code/platform/sibling.rs",
                "pub fn platform_sibling() -> u64 { 4 }",
            ),
            ("code/hidden.rs", "pub fn exported() -> u64 { 5 }"),
        ];
        for (path, content) in files {
            let path = dir.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        let discovered = discover_public_api(&dir.0);
        let paths: Vec<_> = discovered
            .functions
            .iter()
            .map(|func| func.call_path.as_str())
            .collect();
        for path in [
            "outer::inner::leaf::nested",
            "inline::child::inline_child",
            "first::shared",
            "second::shared",
            "platform::sibling::platform_sibling",
            "platform::Settings::default",
            "exported",
        ] {
            assert!(paths.contains(&path), "missing {path}: {paths:?}");
        }
        assert!(
            !paths
                .iter()
                .any(|path| path.contains("Disabled") || path.contains("absent"))
        );
        let output = std::process::Command::new("cargo")
            .args(["check", "--offline", "--manifest-path"])
            .arg(dir.0.join("Cargo.toml"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn skips_bridges_for_generic_types() {
        let crate_dir = TempDir::new("generic_types");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub struct Generic<T> { pub value: String }
            pub enum Response<T> { Err { error: String }, Ok(T) }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        assert!(
            discovered
                .functions
                .iter()
                .all(|func| !func.name.starts_with("Generic_")
                    && !func.name.starts_with("Response_"))
        );
    }

    #[test]
    fn render_signatures_for_diagnostics() {
        let sig = |source: &str| {
            let item: syn::ItemFn = syn::parse_str(source).unwrap();
            render_sig(&item.sig)
        };
        assert_eq!(
            sig("pub fn encode(input: &str) -> String { String::new() }"),
            "fn encode(input: &str) -> String"
        );
        assert_eq!(
            sig("pub fn set(ids: &[UserId], flag: Option<bool>) -> Result<(), Error> { Ok(()) }"),
            "fn set(ids: &[UserId], flag: Option<bool>) -> Result<(), Error>"
        );
        assert_eq!(
            sig("pub fn get_map() -> std::collections::HashMap<String, Vec<u8>> { todo!() }"),
            "fn get_map() -> std::collections::HashMap<String, Vec<u8>>"
        );
        // The generic parameter list itself is not rendered (only the
        // argument and return types); what matters is that every shape
        // renders deterministically without failing.
        let rendered =
            sig("pub fn run<H: Handler + 'static>(handler: H, f: impl Fn(u64)) -> H { todo!() }");
        assert_eq!(rendered, "fn run(handler: H, f: impl Fn(u64)) -> H");
        let method: syn::ImplItemFn =
            syn::parse_str("fn push(&mut self, tags: Vec<String>) -> usize { 0 }").unwrap();
        assert_eq!(
            render_sig(&method.sig),
            "fn push(&mut self, tags: Vec<String>) -> usize"
        );
    }

    #[test]
    fn classify_string_int_bytes_and_skip_tests_and_methods() {
        let crate_dir = TempDir::new("crate");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        // Default features expand transitively: `default = ["std"]` and
        // `std = ["alloc"]` enable `alloc`, so `cfg(any(feature = "alloc",
        // test))` items are kept while `cfg(feature = "nondefault")` ones
        // are skipped.
        fs::write(
            crate_dir.0.join("Cargo.toml"),
            "[features]\nalloc = []\nstd = [\"alloc\"]\ndefault = [\"std\"]\n",
        )
        .unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub fn encode(input: &str) -> String { input.to_uppercase() }
            pub fn width(count: usize) -> usize { count }
            pub fn raw(data: &[u8]) -> Vec<u8> { data.to_vec() }
            fn private_helper(x: i32) -> i32 { x }
            #[cfg(test)]
            mod tests {
                pub fn probe(s: &str) -> String { s.to_lowercase() }
            }
            #[cfg(any(feature = "alloc", test))]
            pub mod alloc_gated {
                pub fn into_it(s: &str) -> String { s.into() }
            }
            #[cfg(feature = "nondefault")]
            pub mod off_by_default {
                pub fn hidden(s: &str) -> String { s }
            }
            pub mod first {
                pub fn duplicate(s: &str) -> usize { s.len() }
            }
            pub mod second {
                pub fn duplicate(s: &str) -> usize { s.len() + 1 }
            }
            pub fn generic_encode<T: AsRef<[u8]>>(input: T) -> String { String::new() }
            pub fn generic_decode<T: AsRef<[u8]>>(input: T) -> Result<Vec<u8>, DecodeError> { Ok(Vec::new()) }
            pub fn unsupported<E>(e: E) -> E { e }
            #[test]
            pub fn test_probe(s: &str) -> String { s.into() }
            "#,
        )
        .unwrap();

        let found = discover_public_functions(&crate_dir.0);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        // Sorted, duplicates suffixed; private fns, test mods/probes,
        // methods, unconcretizable generics, and non-default-feature
        // items are skipped; default-feature-gated mods are kept.
        assert_eq!(
            names,
            [
                "duplicate",
                "duplicate_2",
                "encode",
                "generic_decode",
                "generic_encode",
                "into_it",
                "raw",
                "width"
            ]
        );

        let encode = found.iter().find(|f| f.name == "encode").unwrap();
        assert_eq!(encode.params, [ParamKind::Str]);
        assert_eq!(encode.param_ref, [ParamRef::Ref]);
        assert_eq!(encode.ret, RetKind::Str);

        // `&[u8]` must marshal as bytes, not as an integer.
        let raw = found.iter().find(|f| f.name == "raw").unwrap();
        assert_eq!(raw.params, [ParamKind::Bytes]);
        assert_eq!(raw.param_ref, [ParamRef::Ref]);
        assert_eq!(raw.ret, RetKind::Bytes);

        let width = found.iter().find(|f| f.name == "width").unwrap();
        assert_eq!(width.params, [ParamKind::Int("usize".into())]);
        assert_eq!(width.ret, RetKind::Int("usize".into()));

        // `T: AsRef<[u8]>` bridges over `String`.
        let generic_encode = found.iter().find(|f| f.name == "generic_encode").unwrap();
        assert_eq!(generic_encode.params, [ParamKind::Str]);
        assert_eq!(generic_encode.param_ref, [ParamRef::Owned]);
        assert_eq!(generic_encode.ret, RetKind::Str);

        let generic_decode = found.iter().find(|f| f.name == "generic_decode").unwrap();
        assert_eq!(generic_decode.params, [ParamKind::Str]);
        assert_eq!(generic_decode.ret, RetKind::ResultBytes);
    }

    #[test]
    fn discovers_scalar_option_parameters_and_returns() {
        let crate_dir = TempDir::new("scalar_options");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub fn maybe_count(value: Option<u64>) -> Option<u64> { value }
            pub fn maybe_flag(value: Option<bool>) -> Option<bool> { value }
            pub fn maybe_ratio(value: Option<f32>) -> Option<f32> { value }
            "#,
        )
        .unwrap();
        let found = discover_public_functions(&crate_dir.0);
        let count = found.iter().find(|f| f.name == "maybe_count").unwrap();
        assert_eq!(count.params, [ParamKind::OptionInt("u64".into())]);
        assert_eq!(count.ret, RetKind::OptionInt("u64".into()));
        let flag = found.iter().find(|f| f.name == "maybe_flag").unwrap();
        assert_eq!(flag.params, [ParamKind::OptionBool]);
        assert_eq!(flag.ret, RetKind::OptionBool);
        let ratio = found.iter().find(|f| f.name == "maybe_ratio").unwrap();
        assert_eq!(ratio.params, [ParamKind::OptionFloat("f32".into())]);
        assert_eq!(ratio.ret, RetKind::OptionFloat("f32".into()));
    }

    #[test]
    fn discovers_future_returning_functions() {
        let crate_dir = TempDir::new("futures");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            use std::future::Future;
            pub fn ready_text() -> impl Future<Output = String> { async { String::from("ok") } }
            pub fn ready_bytes() -> std::pin::Pin<Box<dyn Future<Output = Vec<u8>> + Send>> {
                Box::pin(async { vec![1, 2, 3] })
            }
            pub fn not_a_future() -> String { String::new() }
        "#,
        )
        .unwrap();
        let found = discover_public_functions(&crate_dir.0);
        let text = found.iter().find(|f| f.name == "ready_text").unwrap();
        assert!(text.is_async);
        assert_eq!(text.ret, RetKind::Str);
        let bytes = found.iter().find(|f| f.name == "ready_bytes").unwrap();
        assert!(bytes.is_async);
        assert_eq!(bytes.ret, RetKind::Bytes);
        assert!(
            !found
                .iter()
                .find(|f| f.name == "not_a_future")
                .unwrap()
                .is_async
        );
    }

    #[test]
    fn skips_target_gated_and_unsafe_functions() {
        let crate_dir = TempDir::new("cfg_target");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            #[cfg(target_arch = "aarch64")]
            pub fn arm_only() -> String { String::new() }

            pub fn native() -> i32 { 0 }

            pub unsafe fn dangerous() -> i32 { 1 }
            "#,
        )
        .unwrap();
        let found = discover_public_functions(&crate_dir.0);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains(&"native"),
            "host native fn must bridge: {names:?}"
        );
        // Unsafe fns are excluded from the safe adaptor surface because
        // their caller-side invariants cannot be proven from the signature.
        let dangerous = found.iter().find(|f| f.name == "dangerous");
        assert!(
            dangerous.is_none(),
            "unsafe fn leaked into the safe bridge surface: {names:?}"
        );
        let discovered = discover_public_api(&crate_dir.0);
        let dangerous = discovered
            .unsupported
            .iter()
            .find(|item| item.name == "dangerous")
            .expect("unsafe fn must be reported as unsupported");
        assert!(dangerous.reason.contains("unsafe function"));
        // `arm_only` is gated to a non-host arch (unless this test runs on
        // aarch64), so it must not bridge.
        if std::env::consts::ARCH == "aarch64" {
            assert!(names.contains(&"arm_only"));
        } else {
            assert!(
                !names.contains(&"arm_only"),
                "arm-gated fn leaked: {names:?}"
            );
        }
    }

    #[test]
    fn bridges_reexported_functions_from_private_modules() {
        let crate_dir = TempDir::new("reexport_fn");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            mod encode {
                pub fn encode(input: &str) -> String { String::new() }
                pub fn encoded_len(bytes: &[u8]) -> usize { 0 }
                pub fn helper() -> i32 { 0 }
            }
            mod decode {
                pub fn decode(input: &str) -> Result<Vec<u8>, std::io::Error> { Ok(Vec::new()) }
            }
            pub use crate::encode::encode;
            pub use crate::encode::encoded_len;
            pub use crate::decode::decode;
            "#,
        )
        .unwrap();
        let found = discover_public_functions(&crate_dir.0);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains(&"encode"),
            "re-exported fn must bridge: {names:?}"
        );
        assert!(
            names.contains(&"decode"),
            "re-exported fn must bridge: {names:?}"
        );
        assert!(
            names.contains(&"encoded_len"),
            "re-exported fn must bridge: {names:?}"
        );
        assert!(
            !names.contains(&"helper"),
            "non-re-exported private-module fn must be skipped: {names:?}"
        );
        // The re-exported functions must bridge through a root-visible path.
        let encode = found.iter().find(|f| f.name == "encode").unwrap();
        assert_eq!(encode.call_path, "encode");
        let decode = found.iter().find(|f| f.name == "decode").unwrap();
        assert_eq!(decode.call_path, "decode");
    }

    #[test]
    fn discovers_impl_constructors_and_methods() {
        let crate_dir = TempDir::new("impls");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub struct Widget;

            impl Widget {
                pub fn new(label: &str) -> Result<Widget, Error> { Ok(Widget) }
                pub fn create(label: String) -> Widget { Widget }
                pub fn name(&self) -> String { String::new() }
                pub fn set_name(&mut self, label: &str) {}
                pub fn takes_self(self) -> i32 { 1 }
                pub fn helper(label: &str) -> usize { label.len() }
                fn private(&self) -> i32 { 2 }
            }

            impl std::fmt::Display for Widget {
                fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) }
            }
            "#,
        )
        .unwrap();

        let found = discover_public_functions(&crate_dir.0);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        // Constructors, statics, ref methods, and by-value `self` methods
        // are bridged; private methods and trait impls with non-tier-1
        // signatures are skipped.
        assert_eq!(
            names,
            [
                "Widget_create",
                "Widget_helper",
                "Widget_name",
                "Widget_new",
                "Widget_set_name",
                "Widget_takes_self"
            ]
        );

        let new = found.iter().find(|f| f.name == "Widget_new").unwrap();
        assert_eq!(new.call_path, "Widget::new");
        assert_eq!(new.params, [ParamKind::Str]);
        assert_eq!(new.ret, RetKind::ResultObject("Widget".into()));

        let create = found.iter().find(|f| f.name == "Widget_create").unwrap();
        assert_eq!(create.ret, RetKind::Object("Widget".into()));

        let name = found.iter().find(|f| f.name == "Widget_name").unwrap();
        // The `&self` receiver is the leading object handle.
        assert_eq!(name.params, [ParamKind::Object("Widget".into())]);
        assert_eq!(name.param_ref, [ParamRef::Ref]);
        assert_eq!(name.ret, RetKind::Str);

        let set_name = found.iter().find(|f| f.name == "Widget_set_name").unwrap();
        assert_eq!(set_name.params.len(), 2);
        assert_eq!(set_name.param_ref, [ParamRef::MutRef, ParamRef::Ref]);
        assert_eq!(set_name.ret, RetKind::Unit);

        // By-value `self` consumes the handle (no put-back).
        let takes_self = found
            .iter()
            .find(|f| f.name == "Widget_takes_self")
            .unwrap();
        assert_eq!(takes_self.params, [ParamKind::Object("Widget".into())]);
        assert_eq!(takes_self.param_ref, [ParamRef::Owned]);
    }

    #[test]
    fn resolves_impl_types_in_their_defining_module() {
        let crate_dir = TempDir::new("same_named_impl_types");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            mod hidden {
                pub struct Output;
                impl Output {
                    pub fn hidden(&self) -> String { String::from("hidden") }
                }
            }

            mod public {
                pub struct Output;
                impl Output {
                    pub fn visible(&self) -> String { String::from("visible") }
                }
            }

            pub use public::Output as PublicOutput;
            "#,
        )
        .unwrap();

        let discovered = discover_public_functions(&crate_dir.0);
        assert!(
            discovered
                .iter()
                .any(|func| func.name == "PublicOutput_visible"
                    && func.call_path == "PublicOutput::visible")
        );
        assert!(
            !discovered.iter().any(
                |func| func.name == "Output_hidden" || func.call_path == "PublicOutput::hidden"
            )
        );
    }

    #[test]
    fn discovers_trait_methods_and_enum_variants() {
        let crate_dir = TempDir::new("traits");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub struct Machine;

            // A local trait in a public module, reachable through an
            // import and the `crate::` path.
            pub mod gadget {
                pub trait Inspect {
                    fn label(&self) -> String;
                }
            }
            use crate::gadget::Inspect;
            impl Inspect for Machine {
                fn label(&self) -> String { String::new() }
            }

            impl std::error::Error for Machine {
                fn description(&self) -> &str { "machine" }
            }

            impl std::fmt::Display for Machine {
                fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) }
            }

            pub struct Shade;

            pub enum Color {
                Red,
                Green,
                Rgb(u8, u8, u8),
                Shaded(Shade),
                Named { red: u8, label: String },
            }
            "#,
        )
        .unwrap();

        let found = discover_public_functions(&crate_dir.0);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        // Std and imported local trait methods with receivers bridge
        // (method-call syntax); `Display` has no tier-1 method; unit and
        // tier-1 tuple enum variants construct.
        assert_eq!(
            names,
            [
                "Color_Green",
                "Color_Named",
                "Color_Red",
                "Color_Rgb",
                "Color_Shaded",
                "Machine_description",
                "Machine_label"
            ]
        );

        let description = found
            .iter()
            .find(|f| f.name == "Machine_description")
            .unwrap();
        assert!(description.method_call);
        assert_eq!(description.trait_path.as_deref(), Some("std::error::Error"));
        assert_eq!(description.params, [ParamKind::Object("Machine".into())]);

        // The local trait resolves through the import to its public
        // module path.
        let label = found.iter().find(|f| f.name == "Machine_label").unwrap();
        assert!(label.method_call);
        assert_eq!(label.trait_path.as_deref(), Some("gadget::Inspect"));

        let red = found.iter().find(|f| f.name == "Color_Red").unwrap();
        assert_eq!(red.call_path, "Color::Red");
        assert!(red.params.is_empty());
        assert_eq!(red.ret, RetKind::Object("Color".into()));

        // A tuple variant with tier-1 fields constructs with arguments.
        let rgb = found.iter().find(|f| f.name == "Color_Rgb").unwrap();
        assert_eq!(rgb.call_path, "Color::Rgb");
        assert_eq!(
            rgb.params,
            [
                ParamKind::Int("u8".into()),
                ParamKind::Int("u8".into()),
                ParamKind::Int("u8".into())
            ]
        );
        assert_eq!(rgb.ret, RetKind::Object("Color".into()));

        // Local object-typed fields use the complete known-type map.
        let shaded = found.iter().find(|f| f.name == "Color_Shaded").unwrap();
        assert_eq!(shaded.params, [ParamKind::Object("Shade".into())]);
        assert_eq!(shaded.ret, RetKind::Object("Color".into()));

        let named = found.iter().find(|f| f.name == "Color_Named").unwrap();
        assert_eq!(named.params, [ParamKind::Int("u8".into()), ParamKind::Str]);
        assert_eq!(named.field_names, ["red", "label"]);
        assert_eq!(named.ret, RetKind::Object("Color".into()));
    }

    #[test]
    fn emits_adaptor_with_path_dep_and_forbid_unsafe() {
        let registry = TempDir::new("registry");
        let crate_src = registry.0.join("index.crates.io-0000").join("fake-1.0.0");
        fs::create_dir_all(crate_src.join("src")).unwrap();
        fs::write(
            crate_src.join("src").join("lib.rs"),
            r#"
            pub fn escape(pattern: &str) -> String { pattern.to_string() }
            pub fn maybe(input: &str) -> Option<Vec<u8>> { Some(input.as_bytes().to_vec()) }
            pub unsafe fn danger(x: i64) -> i64 { x }
            pub struct Machine;
            pub fn make_machine() -> Machine { Machine }
            impl Machine {
                pub fn new(pattern: &str) -> Result<Machine, Error> { Ok(Machine) }
                pub fn check(&self, input: &str) -> bool { false }
            }
            "#,
        )
        .unwrap();
        fs::write(crate_src.join("Cargo.toml"), "[package]\nname=\"fake\"\n").unwrap();

        let cache_tmp = TempDir::new("cache");
        let cache = CacheRoot::at(cache_tmp.0.clone());
        let project = TempDir::new("project");
        let dir =
            emit_adaptor(&cache, &project.0, "fake", "1.0.0", &registry.0, None, None).unwrap();

        let lib = fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();
        assert!(lib.starts_with("#![deny(unsafe_code)]"));
        // A `&str` param is marshaled to `String` then borrowed.
        assert!(lib.contains("fake::escape(&a0)"));
        assert!(lib.contains("_ntsc_str_to_handle(&*result)"));
        // Unsafe fns are excluded from the safe bridge ABI and reported in
        // the export manifest instead of being called from generated code.
        assert!(!lib.contains("fake::danger"));
        let export = fs::read_to_string(dir.join("fake.export.json")).unwrap();
        assert!(export.contains("unsafe function is excluded from the safe bridge ABI"));
        // Calls run behind a panic barrier that converts a panic into a
        // thrown Neutron exception.
        assert!(lib.contains("std::panic::catch_unwind"));
        assert!(lib.contains("_ntsc_panic_to_handle"));
        // `Option<Vec<u8>>` returns the raw bytes of a string handle.
        assert!(lib.contains("_ntsc_bytes_to_handle(&*v)"));
        // Blocking sections: every sync bridge enters around the
        // dependency call so a worker parked inside still leaves cover.
        // The invalid-handle early return precedes entry, and the panic
        // arm leaves before returning, so entry/exit always balance.
        assert!(lib.contains("fn ntsc_worker_will_block();"));
        assert!(lib.contains("fn ntsc_worker_did_unblock();"));
        let check = lib
            .find("ntsc_adaptor_fake_Machine_check")
            .and_then(|start| {
                lib[start..]
                    .find("\n}\n")
                    .map(|end| &lib[start..start + end])
            })
            .expect("check bridge must be emitted");
        let will = check
            .find("_ntsc_block_guard = _ntsc_block_guard()")
            .expect("bridge must enter a blocking section");
        let call = check
            .find("Machine::check(")
            .expect("bridge must call through");
        let did = check
            .find("drop(_ntsc_block_guard)")
            .expect("bridge must leave the blocking section");
        assert!(
            will < call && call < did,
            "guard must span exactly the call"
        );
        // Constructors store the struct in the process-wide shared space
        // under its type tag; methods borrow it back with a tag check.
        assert!(lib.contains("Machine::new(&a0)"));
        assert!(lib.contains("_ntsc_shared_put(v, \"fake@1.0.0+/Machine\")"));
        // A borrowed receiver handle already is `&T`, so path calls take
        // it as-is (no `&&T` autoref).
        assert!(lib.contains("Machine::check(a0_obj, &a1)"));
        assert!(lib.contains(
            "_ntsc_shared_borrow_or_arc(a0, \"fake@1.0.0+/Machine\", \"fake@1.0.0+/Machine#Arc\")"
        ));
        // No per-adaptor map remains: handles live in the runtime's
        // shared space, reachable from any adaptor.
        assert!(!lib.contains("_NTSC_OBJECTS"));
        assert!(!lib.contains("_ntsc_objects()"));
        assert!(lib.contains("unsafe { ntsc_shared_object_retain(handle) }"));
        assert!(lib.contains("unsafe { ntsc_shared_object_release(handle) };"));
        // Strings cross the boundary with one bulk copy, not one FFI call
        // per byte: a large JSON body held the adaptor lock for thousands
        // of calls before this.
        assert!(lib.contains("fn ntsc_string_to_bytes("));
        assert!(lib.contains("ntsc_string_to_bytes(handle, buf.as_mut_ptr(), len)"));
        assert!(
            !lib.contains("ntsc_string_byte_at"),
            "per-byte string reads must not be generated"
        );
        // `check(&self, input)` converts its string arg, borrows the
        // receiver out of the shared space, and releases the borrow after
        // the call — no lock is ever held across the foreign call, so a
        // blocking method cannot serialize every other bridge.
        let check = lib
            .find("ntsc_adaptor_fake_Machine_check")
            .and_then(|start| {
                lib[start..]
                    .find("\n}\n")
                    .map(|end| &lib[start..start + end])
            })
            .expect("check bridge must be emitted");
        let str_pos = check
            .find("_ntsc_handle_to_string")
            .expect("check must convert its string arg");
        let borrow_pos = check
            .find("_ntsc_shared_borrow_or_arc(a0")
            .expect("check must borrow its receiver");
        let guard_pos = check
            .find("_NtscObjectBorrow::Shared(a0)")
            .expect("check must guard its borrow through marshaling");
        let call_pos = check
            .find("Machine::check(")
            .expect("check must call through");
        assert!(
            str_pos < borrow_pos && borrow_pos < guard_pos && guard_pos < call_pos,
            "convert, borrow, guard, call must nest in order"
        );
        assert!(lib.contains("Self::Shared(handle) => ntsc_shared_object_release(handle)"));

        // The adaptor path-depends on the cached copy, not the registry.
        let cached_src = cache.crate_src_path("fake", "1.0.0");
        let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(manifest.contains(&format!("path = {:?}", cached_src)));
        assert!(manifest.contains("crate-version = \"1.0.0\""));

        let export = fs::read_to_string(dir.join("fake.export.json")).unwrap();
        assert!(export.contains(r#""schema_version": 3"#));
        assert!(export.contains(r#""callbacks": ["#));
        assert!(export.contains(r#""module": "fake""#));
        assert!(export.contains(r#""status": "typed""#));
        assert!(export.contains(r#""symbol": "ntsc_adaptor_fake_escape""#));
        assert!(export.contains(r#""type": "option[bytes]""#));
        assert!(export.contains(r#""name": "make_machine""#));
        assert!(export.contains(r#""type": "object[Machine]""#));
        assert!(lib.contains("_ntsc_shared_put(result, \"fake@1.0.0+/Machine\")"));
        assert!(lib.contains("ntsc_adaptor_fake_escape_async"));
        assert!(lib.contains("ntsc_async_op_new(ntsc_adaptor_fake_escape_async_work"));
        assert!(export.contains(r#""name": "escape_async""#));
        assert!(export.contains(r#""path": "escape_async""#));
    }

    /// A fixture crate exercising the whole callback slice: a sync
    /// handler trait consumed both as `impl Trait` and as a generic
    /// bound, a second trait monomorphized over in-crate implementors,
    /// an `IntoFuture` builder, and a public field for the getter.
    const CALLBACK_FIXTURE: &str = r#"
    pub struct Greeter { pub prefix: String }

    pub trait Notifier {
        fn notify(&self, msg: String);
        fn count(&self, n: u64);
        fn ping(&self) {}
    }

    impl Greeter {
        pub fn new(prefix: String) -> Self { Self { prefix } }
        pub fn run(&self, handler: impl Notifier) -> String {
            handler.notify("hello".to_string());
            handler.count(3);
            "ran".to_string()
        }
        pub fn run_generic<H: Notifier + Send + Sync + 'static>(&self, handler: H) -> u64 {
            handler.count(7);
            7
        }
    }

    pub trait Cache {
        fn fetch(&self, key: String) -> String;
    }
    pub struct Http;
    pub struct Ctx;
    impl Cache for Http {
        fn fetch(&self, key: String) -> String { key }
    }
    impl Cache for Ctx {
        fn fetch(&self, key: String) -> String { key }
    }
    pub struct Store;
    impl Store {
        pub fn load(cache: impl Cache, key: String) -> String { cache.fetch(key) }
    }

    pub struct BuildError(pub String);
    impl std::fmt::Display for BuildError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    pub struct Client;
    pub struct Builder;
    impl std::future::IntoFuture for Builder {
        type Output = Result<Client, BuildError>;
        type IntoFuture =
            std::pin::Pin<Box<dyn std::future::Future<Output = Self::Output>>>;
        fn into_future(self) -> Self::IntoFuture {
            Box::pin(async { Ok(Client) })
        }
    }
    "#;

    fn write_callback_fixture(tag: &str) -> (TempDir, PathBuf) {
        let crate_dir = TempDir::new(tag);
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), CALLBACK_FIXTURE).unwrap();
        fs::write(
            crate_dir.0.join("Cargo.toml"),
            "[package]\nname = \"cbtest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let src = crate_dir.0.clone();
        (crate_dir, src)
    }

    #[test]
    fn callback_inventory_resolves_and_emits_impl() {
        let (_dir, src) = write_callback_fixture("cb_inv");
        let discovered = discover_public_api(&src);

        // `impl Notifier` and `H: Notifier` both bridge as callbacks on the
        // inventoried trait path.
        let run = discovered
            .functions
            .iter()
            .find(|f| f.name == "Greeter_run")
            .unwrap();
        assert_eq!(
            run.params,
            [
                ParamKind::Object("Greeter".into()),
                ParamKind::Callback("Notifier".into())
            ]
        );
        let run_generic = discovered
            .functions
            .iter()
            .find(|f| f.name == "Greeter_run_generic")
            .unwrap();
        assert!(
            run_generic
                .params
                .contains(&ParamKind::Callback("Notifier".into()))
        );

        // The public field gains a getter.
        let getter = discovered
            .functions
            .iter()
            .find(|f| f.name == "Greeter_prefix")
            .unwrap();
        assert_eq!(getter.ret, RetKind::Str);

        // `impl Cache` monomorphizes over the two in-crate implementors;
        // no callback remains.
        let with_http = discovered
            .functions
            .iter()
            .find(|f| f.name == "Store_load_with_Http")
            .unwrap();
        assert_eq!(
            with_http.params,
            [ParamKind::Object("Http".into()), ParamKind::Str]
        );
        assert!(
            discovered
                .functions
                .iter()
                .any(|f| f.name == "Store_load_with_Ctx")
        );
        assert!(!discovered.functions.iter().any(|f| f.name == "Store_load"));

        // `IntoFuture` bridges through the sibling `type Output`.
        let into_future = discovered
            .functions
            .iter()
            .find(|f| f.name == "Builder_into_future")
            .unwrap();
        assert!(into_future.is_async);
        assert_eq!(into_future.ret, RetKind::ResultObject("Client".into()));

        // One callback bridge carries the three trait methods.
        assert_eq!(discovered.callbacks.len(), 1);
        let bridge = &discovered.callbacks[0];
        assert_eq!(bridge.trait_path, "Notifier");
        assert_eq!(bridge.short, "Notifier");
        let methods: Vec<&str> = bridge.methods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(methods, ["notify", "count", "ping"]);

        let lib = emit_lib("cbtest", &discovered);
        assert!(lib.contains("impl cbtest::Notifier for __NtscCb_Notifier"));
        assert!(lib.contains("fn notify(&self, a0: String)"));
        assert!(lib.contains("fn count(&self, a0: u64)"));
        assert!(lib.contains("__NTSCCB_NOTIFIER_QUEUE"));
        assert!(lib.contains("ntsc_adaptor_cbtest___NtscCb_Notifier_poll"));
        assert!(lib.contains("ntsc_adaptor_cbtest___NtscCb_Notifier_arg_str"));

        let export = export_manifest_content(
            "cbtest",
            &discovered.functions,
            &discovered.unsupported,
            &discovered.callbacks,
        )
        .unwrap();
        // The manifest is hand-rendered JSON: it must always parse.
        let parsed: serde_json::Value = serde_json::from_str(&export).unwrap();
        assert_eq!(parsed["schema_version"], 3);
        assert!(export.contains(r#""trait": "Notifier""#));
        assert!(export.contains(r#""type": "callback[Notifier]""#));
        assert!(export.contains("ntsc_adaptor_cbtest___NtscCb_Notifier_poll"));
    }

    #[test]
    fn parses_bitflags_with_doc_attributes() {
        // Token-rendered docs (`#[doc = "..."]`) carry arbitrary prose —
        // brackets and punctuation inside them must not end the parse.
        let item: syn::ItemMacro = syn::parse_str(
            r#"bitflags! {
                # [doc = " Flags [like this] will limit events."]
                # [derive (Copy , Clone)]
                pub struct Intents : u64 {
                    # [doc = " Enables guilds."]
                    const GUILDS = 1 ;
                    const MESSAGES = 1 << 2 ;
                }
            }"#,
        )
        .unwrap();
        let parsed = parse_bitflags_type(&item, &std::collections::HashSet::new()).unwrap();
        assert_eq!(parsed.name, "Intents");
        assert_eq!(parsed.flags, ["GUILDS", "MESSAGES"]);
    }

    #[test]
    fn parses_macro_trait_methods_with_variant_payloads() {
        // The macro expands `Variant { payload } => async fn m(&self,
        // ctx)` into `m(&self, ctx, payload)`: both halves are captured.
        let item: syn::ItemMacro = syn::parse_str(
            r#"event_handler! {
                Message { new_message: Message } => async fn message(&self, ctx: Context);
                Ratelimit { data: RatelimitInfo } => async fn ratelimit(&self);
            }"#,
        )
        .unwrap();
        let methods = parse_macro_trait_methods(&item, &std::collections::HashSet::new());
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "message");
        assert!(methods[0].is_async);
        let arg_names: Vec<&str> = methods[0].args.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(arg_names, ["ctx", "new_message"]);
        let arg_names: Vec<&str> = methods[1].args.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(arg_names, ["data"]);
    }

    #[test]
    fn callback_adaptor_builds_offline() {
        let (_dir, src) = write_callback_fixture("cb_build");
        let discovered = discover_public_api(&src);
        assert!(!discovered.functions.is_empty());

        // Assemble the adaptor crate against the fixture by path and
        // compile it: staticlib output never resolves the `extern "C"`
        // runtime helpers, so this verifies every generated signature —
        // bridges, trait impl, queue, and poll family — type-checks.
        let adaptor_dir = TempDir::new("cb_adaptor");
        fs::create_dir_all(adaptor_dir.0.join("src")).unwrap();
        let async_needed = discovered.functions.iter().any(|func| func.is_async);
        let manifest = adaptor_manifest(
            "cbtest",
            "0.1.0",
            &src,
            async_needed,
            None,
            &std::collections::BTreeMap::new(),
        );
        fs::write(adaptor_dir.0.join("Cargo.toml"), manifest).unwrap();
        let lib = emit_lib("cbtest", &discovered);
        fs::write(adaptor_dir.0.join("src").join("lib.rs"), lib).unwrap();

        let output = std::process::Command::new("cargo")
            .arg("build")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(adaptor_dir.0.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(adaptor_dir.0.join("target"))
            .output()
            .expect("failed to run cargo");
        assert!(
            output.status.success(),
            "callback adaptor must compile:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    const ALIAS_FIXTURE: &str = r#"
    pub struct Client;
    pub struct Error;
    pub type ClientResult<T> = Result<T, Error>;
    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "error") }
    }
    pub fn connect() -> ClientResult<Client> { Ok(Client) }
    impl Client {
        pub async fn request(&self) -> ClientResult<String> { Ok(String::from("ok")) }
    }
    "#;

    #[test]
    fn expands_public_type_aliases_before_classification() {
        let crate_dir = TempDir::new("alias_fixture");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), ALIAS_FIXTURE).unwrap();
        let discovered = discover_public_api(&crate_dir.0);
        let connect = discovered
            .functions
            .iter()
            .find(|f| f.name == "connect")
            .unwrap();
        assert_eq!(connect.ret, RetKind::ResultObject("Client".into()));
        let request = discovered
            .functions
            .iter()
            .find(|f| f.name == "Client_request")
            .unwrap();
        assert!(request.is_async);
        assert_eq!(request.ret, RetKind::ResultStr);
    }

    /// A fixture crate exercising collection bridges: `Vec<T>` and `&[T]`
    /// parameters and returns over the scalar element families, an
    /// `Option`/`Result`-wrapped vector, object vectors, a generic vector over a
    /// string-like bound, and a vector-typed struct field — plus the shapes that
    /// stay unsupported (`&mut` collections, borrowed
    /// elements, maps, and `[T; N]` with a non-numeric length).
    const COLLECTION_FIXTURE: &str = r#"
    #[derive(Debug, Clone)]
    pub struct Error(pub String);
    #[derive(Debug, Clone)]
    pub struct Item(pub String);
    pub const SIZE: usize = 4;
    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    pub fn join_all(parts: Vec<String>) -> String { parts.join(",") }
    pub fn total(xs: &[i64]) -> i64 { xs.iter().sum() }
    pub fn flags(on: Vec<bool>) -> Vec<bool> { on }
    pub fn scale(v: Vec<f64>, f: f64) -> Vec<f64> { v.into_iter().map(|x| x * f).collect() }
    pub fn maybe_names(def: Option<Vec<String>>) -> Option<Vec<String>> { def }
    pub fn load(ok: bool) -> Result<Vec<String>, Error> {
        if ok { Ok(Vec::new()) } else { Err(Error("no".into())) }
    }
    pub fn ids() -> Vec<u64> { Vec::new() }
    pub fn objects() -> Vec<Item> { Vec::new() }
    pub fn take_objects(items: Vec<Item>) -> Result<Vec<Item>, Error> { Ok(items) }
    pub fn generic_list<T: AsRef<str>>(v: Vec<T>) -> String { String::new() }

    pub struct Store { pub tags: Vec<String> }
    impl Store {
        pub fn store_new() -> Store { Store { tags: Vec::new() } }
        pub fn push(&mut self, tags: Vec<String>) -> usize {
            self.tags.extend(tags);
            self.tags.len()
        }
        pub fn fetch(&self, n: usize) -> Vec<String> {
            self.tags.iter().take(n).cloned().collect()
        }
        pub fn bad_mut(&mut self, xs: &mut [String]) -> usize { xs.len() }
        pub fn fixed(xs: [String; 2]) -> String { xs.join(",") }
        pub fn fixed_ids() -> [u32; 4] { [7; 4] }
        pub fn named_len(xs: [u32; SIZE]) -> u32 { xs[0] }
        pub fn borrowed(refs: Vec<&str>) -> String { refs.join(",") }
        pub fn mapped(m: std::collections::HashMap<String, String>) -> usize { m.len() }
    }
    "#;

    fn write_collection_fixture(tag: &str) -> (TempDir, PathBuf) {
        let crate_dir = TempDir::new(tag);
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), COLLECTION_FIXTURE).unwrap();
        fs::write(
            crate_dir.0.join("Cargo.toml"),
            "[package]\nname = \"colltest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let src = crate_dir.0.clone();
        (crate_dir, src)
    }

    #[test]
    fn bridges_vec_and_slice_collections() {
        let (_dir, src) = write_collection_fixture("coll_inv");
        let discovered = discover_public_api(&src);
        let find = |name: &str| {
            discovered
                .functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} must bridge"))
        };

        let join = find("join_all");
        assert_eq!(join.params, [ParamKind::Array(ArrayElemKind::Str)]);
        assert_eq!(join.param_ref, [ParamRef::Owned]);
        assert_eq!(join.ret, RetKind::Str);

        let total = find("total");
        assert_eq!(
            total.params,
            [ParamKind::Array(ArrayElemKind::Int("i64".into()))]
        );
        assert_eq!(total.param_ref, [ParamRef::Ref]);

        assert_eq!(find("flags").ret, RetKind::Array(ArrayElemKind::Bool));
        assert_eq!(
            find("scale").ret,
            RetKind::Array(ArrayElemKind::Float("f64".into()))
        );
        assert_eq!(
            find("maybe_names").ret,
            RetKind::OptionArray(ArrayElemKind::Str)
        );
        assert_eq!(find("load").ret, RetKind::ResultArray(ArrayElemKind::Str));
        // A fixed-size `[T; N]` return bridges through the array handle.
        assert_eq!(
            find("Store_fixed_ids").ret,
            RetKind::Array(ArrayElemKind::Int("u32".into()))
        );
        assert_eq!(
            find("ids").ret,
            RetKind::Array(ArrayElemKind::Int("u64".into()))
        );
        assert_eq!(
            find("objects").ret,
            RetKind::Array(ArrayElemKind::Object("Item".into()))
        );
        assert_eq!(
            find("take_objects").params,
            [ParamKind::Array(ArrayElemKind::Object("Item".into()))]
        );
        assert_eq!(
            find("take_objects").ret,
            RetKind::ResultArray(ArrayElemKind::Object("Item".into()))
        );
        // `Vec<T: AsRef<str>>` concretizes its element through the bound.
        assert_eq!(
            find("generic_list").params,
            [ParamKind::Array(ArrayElemKind::Str)]
        );
        // A vector-typed public field gains an array-returning getter.
        assert_eq!(find("Store_tags").ret, RetKind::Array(ArrayElemKind::Str));
        // `&mut self` methods still bridge; the receiver is taken.
        assert!(discovered.functions.iter().any(|f| f.name == "Store_push"));

        // The deferred shapes are listed, never silent.
        let missing: Vec<&str> = discovered
            .unsupported
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        for name in [
            "Store_bad_mut",
            "Store_borrowed",
            "Store_mapped",
            // A `[T; N]` whose length is a named constant cannot be proven
            // at generation time, so it stays reported, never silent.
            "Store_named_len",
        ] {
            assert!(
                missing.contains(&name),
                "{name} must be reported, got {missing:?}"
            );
        }
        // A fixed-array *parameter* with a numeric length now bridges: it
        // reads the array handle and length-checks it into `[T; N]`.
        let fixed = find("Store_fixed");
        assert_eq!(fixed.params, [ParamKind::FixedArray(ArrayElemKind::Str, 2)]);
        assert_eq!(fixed.ret, RetKind::Str);
        // A fixed-array *return* bridges, so it is never reported.
        assert!(!missing.contains(&"Store_fixed_ids"));
        for name in [
            "join_all",
            "total",
            "flags",
            "Store_push",
            "Store_fetch",
            "Store_tags",
        ] {
            assert!(
                !missing.contains(&name),
                "{name} must bridge, but is reported: {missing:?}"
            );
        }

        let export = export_manifest_content(
            "colltest",
            &discovered.functions,
            &discovered.unsupported,
            &discovered.callbacks,
        )
        .unwrap();
        assert!(export.contains(r#""type": "array[string]""#));
        assert!(export.contains(r#""type": "option[array[string]]""#));
        assert!(export.contains(r#""type": "result[array[string]]""#));
        assert!(export.contains(r#""type": "array[int]""#));
        assert!(export.contains(r#""status": "unsupported""#));
        assert!(export.contains("mutable collection borrows have no bridge"));

        let lib = emit_lib("colltest", &discovered);
        assert!(lib.contains("fn ntsc_array_is_array(id: i64) -> i8;"));
        assert!(lib.contains("ntsc_array_new_typed(8,"));
        assert!(lib.contains("ntsc_array_len("));
        assert!(lib.contains("expected array handle"));
    }

    #[test]
    fn collection_adaptor_builds_offline() {
        let (_dir, src) = write_collection_fixture("coll_build");
        let discovered = discover_public_api(&src);
        assert!(!discovered.functions.is_empty());

        // The array bridges must name real Rust types: a `Vec<&str>`
        // parameter or a `[T; N]` conversion would fail here, which is why
        // those shapes stay unsupported instead of generating dead code.
        let adaptor_dir = TempDir::new("coll_adaptor");
        fs::create_dir_all(adaptor_dir.0.join("src")).unwrap();
        let async_needed = discovered.functions.iter().any(|func| func.is_async);
        let manifest = adaptor_manifest(
            "colltest",
            "0.1.0",
            &src,
            async_needed,
            None,
            &std::collections::BTreeMap::new(),
        );
        fs::write(adaptor_dir.0.join("Cargo.toml"), manifest).unwrap();
        let lib = emit_lib("colltest", &discovered);
        fs::write(adaptor_dir.0.join("src").join("lib.rs"), lib).unwrap();

        let output = std::process::Command::new("cargo")
            .arg("build")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(adaptor_dir.0.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(adaptor_dir.0.join("target"))
            .output()
            .expect("failed to run cargo");
        assert!(
            output.status.success(),
            "collection adaptor must compile:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A fixture for object-bound generics, static trait constructors,
    /// and `Self` parameters: `impl AsRef<Http>` and `impl Into<Id>`
    /// concretize over the object, `From`/`Default` bridge as path calls
    /// (with `Default` resolving through the prelude table — the fixture
    /// deliberately does not import it), and `&Self` resolves to the
    /// impl's own type.
    const BOUNDS_FIXTURE: &str = r#"
    pub struct Http;
    pub struct Ctx;
    pub struct Cache;
    impl AsRef<Http> for Ctx {
        fn as_ref(&self) -> &Http { &HTTP }
    }
    impl AsRef<Cache> for Ctx {
        fn as_ref(&self) -> &Cache { &CACHE }
    }
    static HTTP: Http = Http;
    static CACHE: Cache = Cache;
    #[derive(Clone)]
    pub struct MessageId(pub u64);
    pub struct Builder { pub prefix: String }

    pub fn fetch(h: impl AsRef<Http>) -> String { "ok".into() }
    pub fn load(id: impl Into<MessageId>) -> MessageId { id.into() }

    impl MessageId {
        pub fn raw(&self) -> u64 { self.0 }
    }
    impl std::convert::From<u64> for MessageId {
        fn from(v: u64) -> Self { MessageId(v) }
    }
    impl Default for Builder {
        fn default() -> Self { Builder { prefix: String::new() } }
    }
    impl Builder {
        pub fn same(&self, other: &Self) -> bool { self.prefix == other.prefix }
    }
    "#;

    fn write_bounds_fixture(tag: &str) -> (TempDir, PathBuf) {
        let crate_dir = TempDir::new(tag);
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), BOUNDS_FIXTURE).unwrap();
        fs::write(
            crate_dir.0.join("Cargo.toml"),
            "[package]\nname = \"boundtest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let src = crate_dir.0.clone();
        (crate_dir, src)
    }

    #[test]
    fn bridges_object_bounds_static_traits_and_self_params() {
        let (_dir, src) = write_bounds_fixture("bounds_inv");
        let discovered = discover_public_api(&src);
        let find = |name: &str| {
            discovered
                .functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} must bridge"))
        };

        // `impl AsRef<Http>` monomorphizes over the in-crate implementor:
        // there is no reflexive `Http: AsRef<Http>`, so a direct bridge
        // could never compile.
        let fetch = find("fetch_with_Ctx");
        assert_eq!(fetch.params, [ParamKind::Object("Ctx".into())]);
        // Two `as_ref` impls for one type share one method-call spelling:
        // ambiguous at every call site, so listed instead of generated.
        assert!(
            discovered
                .unsupported
                .iter()
                .any(|item| item.name == "Ctx_as_ref"
                    && item.reason.contains("ambiguous trait method")),
            "Ctx_as_ref must be reported as ambiguous"
        );
        // `impl Into<MessageId>` unifies with an owned handle through the
        // reflexive impl.
        let load = find("load");
        assert_eq!(load.params, [ParamKind::Object("MessageId".into())]);
        assert_eq!(load.ret, RetKind::Object("MessageId".into()));
        // Static trait constructors bridge as path calls.
        let from = find("MessageId_from");
        assert_eq!(from.params, [ParamKind::Int("u64".into())]);
        assert_eq!(from.ret, RetKind::Object("MessageId".into()));
        assert!(!from.method_call);
        let default = find("Builder_default");
        assert!(default.params.is_empty());
        assert_eq!(default.ret, RetKind::Object("Builder".into()));
        // `&Self` is the impl's own type.
        let same = find("Builder_same");
        assert_eq!(
            same.params,
            [
                ParamKind::Object("Builder".into()),
                ParamKind::Object("Builder".into())
            ]
        );
        assert_eq!(same.param_ref, [ParamRef::Ref, ParamRef::Ref]);

        let export = export_manifest_content(
            "boundtest",
            &discovered.functions,
            &discovered.unsupported,
            &discovered.callbacks,
        )
        .unwrap();
        assert!(export.contains(r#""name": "MessageId_from""#));
        assert!(export.contains(r#""name": "Builder_default""#));
    }

    #[test]
    fn bounds_adaptor_builds_offline() {
        let (_dir, src) = write_bounds_fixture("bounds_build");
        let discovered = discover_public_api(&src);
        assert!(!discovered.functions.is_empty());

        // The static trait calls (`MessageId::from`, `Builder::default`)
        // only compile with their traits imported — this pins the import
        // emission, including the prelude-table path for `Default`.
        let adaptor_dir = TempDir::new("bounds_adaptor");
        fs::create_dir_all(adaptor_dir.0.join("src")).unwrap();
        let async_needed = discovered.functions.iter().any(|func| func.is_async);
        let manifest = adaptor_manifest(
            "boundtest",
            "0.1.0",
            &src,
            async_needed,
            None,
            &std::collections::BTreeMap::new(),
        );
        fs::write(adaptor_dir.0.join("Cargo.toml"), manifest).unwrap();
        let lib = emit_lib("boundtest", &discovered);
        assert!(lib.contains("use std::convert::From;"));
        assert!(lib.contains("use std::default::Default;"));
        fs::write(adaptor_dir.0.join("src").join("lib.rs"), lib).unwrap();

        let output = std::process::Command::new("cargo")
            .arg("build")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(adaptor_dir.0.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(adaptor_dir.0.join("target"))
            .output()
            .expect("failed to run cargo");
        assert!(
            output.status.success(),
            "bounds adaptor must compile:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn records_impl_failures_as_unsupported() {
        let crate_dir = TempDir::new("impl_unsupported");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub struct Widget;
            impl Widget {
                pub fn ok() -> Widget { Widget }
                pub unsafe fn risky(&self) -> i64 { 0 }
                pub fn mapped(m: std::collections::HashMap<String, String>) -> i64 {
                    m.len() as i64
                }
                pub fn generic<T>(&self, v: T) -> u64 { 0 }
            }
            pub struct Pair<T>(pub T);
            impl<T> Pair<T> {
                pub fn get(&self) -> i64 { 0 }
            }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        assert!(discovered.functions.iter().any(|f| f.name == "Widget_ok"));

        // Previously every one of these vanished without a trace; now each
        // carries its path, status, and reason into the manifest.
        let by_name = |name: &str| {
            discovered
                .unsupported
                .iter()
                .find(|item| item.name == name)
                .unwrap_or_else(|| {
                    panic!(
                        "{name} must be reported, got {:?}",
                        discovered
                            .unsupported
                            .iter()
                            .map(|item| &item.name)
                            .collect::<Vec<_>>()
                    )
                })
        };
        assert_eq!(by_name("Widget_risky").path, "Widget::risky");
        assert!(by_name("Widget_mapped").reason.contains("parameter"));
        assert!(by_name("Widget_generic").reason.contains("generic"));
        assert!(by_name("Pair").reason.contains("generic"));

        let export = export_manifest_content(
            "impltest",
            &discovered.functions,
            &discovered.unsupported,
            &discovered.callbacks,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&export).unwrap();
        let entries = parsed["functions"].as_array().unwrap();
        assert!(entries.iter().any(|entry| {
            entry["name"] == "Widget_risky"
                && entry["status"] == "unsupported"
                && entry["reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty())
        }));
    }

    /// Signatures the bridge ABI cannot marshal are skipped instead of
    /// emitting Rust that fails to compile: typed receivers of shapes the
    /// registry cannot supply (`self: &Arc<Self>`), explicit `'static`
    /// borrows, object returns behind borrows, generics whose behavior
    /// bound appears in the return type, and parameters shadowed by a
    /// foreign import (`use reqwest::Client`). Each rule must reject only
    /// its own case: every "still bridged" assertion guards a working
    /// signature shape.
    #[test]
    fn skips_unbridgeable_signatures() {
        let crate_dir = TempDir::new("unbridgeable");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub struct Ctx;
            pub struct Presence;

            pub struct Session;
            impl Session {
                pub fn valid(self: &Self) -> i32 { 1 }
                pub fn arc_receiver(self: &std::sync::Arc<Self>) -> i32 { 2 }
                pub fn static_borrow(tag: &'static str) -> i32 { 3 }
                pub fn presence(&self) -> &Presence { unreachable!() }
                pub fn run<H: Handler>(&self, handler: H) -> u64 { handler.done() }
                pub fn iter_generic<T: std::str::FromStr>(&self) -> Vec<T> { Vec::new() }
            }

            pub trait Handler {
                fn done(&self);
            }

            pub mod shard {
                use crate::foreign::Client;
                pub struct Shard;
                impl Shard {
                    pub fn with_client(client: Client) -> i32 { 4 }
                    pub fn count(n: u64) -> u64 { n }
                }
            }
            "#,
        )
        .unwrap();
        // A foreign crate-local module so `use crate::foreign::Client`
        // resolves to a real local type for the plain import record, while
        // the bridging side must not mistake it for `crate::shard`'s bare
        // names.
        fs::write(src_dir.join("foreign.rs"), "pub struct Client;\n").unwrap();
        let mut lib = fs::read_to_string(src_dir.join("lib.rs")).unwrap();
        lib.push_str("\nmod foreign;\n");
        fs::write(src_dir.join("lib.rs"), lib).unwrap();

        let found = discover_public_functions(&crate_dir.0);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"Session_valid"));
        assert!(names.contains(&"Session_run"));
        assert!(names.contains(&"Shard_count"));
        assert!(!names.contains(&"Session_arc_receiver"));
        assert!(!names.contains(&"Session_static_borrow"));
        assert!(!names.contains(&"Session_presence"));
        assert!(!names.contains(&"Session_iter_generic"));
        assert!(!names.contains(&"Shard_with_client"));
    }

    /// `#[non_exhaustive]` enums cannot be constructed outside their
    /// crate; object-typed field getters clone the value, so only `Clone`
    /// types bridge; a bare type name that maps to two different types is
    /// unresolvable while its qualified spelling still resolves; generic
    /// types never resolve by bare name.
    #[test]
    fn skips_unconstructable_and_uncloneable_types() {
        let crate_dir = TempDir::new("unconstructable");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            #[derive(Clone)]
            pub struct Cloneable;
            pub struct Unique;
            pub struct Open;
            pub struct NotCloneable;

            #[derive(Clone)]
            pub enum Plain {
                On,
            }

            #[derive(Clone)]
            #[non_exhaustive]
            pub enum Gateway {
                Building,
            }

            pub struct Held {
                pub clonable: Cloneable,
                pub unique: Unique,
                pub open: Open,
            }

            pub mod north {
                pub struct Ready;
            }
            pub mod builder {
                pub struct Ready;
            }
            pub fn bare_ready(r: Ready) -> u32 { 0 }
            pub fn qualified_ready(r: crate::north::Ready) -> u32 { 1 }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let names: Vec<&str> = discovered
            .functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(names.contains(&"Plain_On"));
        assert!(!names.contains(&"Gateway_Building"));
        assert!(names.contains(&"Held_clonable"));
        assert!(!names.contains(&"Held_unique"));
        // `Open` is a local type whose definition the index saw without a
        // `Clone` derive: no getter, even though it is nameable.
        assert!(!names.contains(&"Held_open"));
        // The bare `Ready` name is ambiguous (two definitions); the
        // qualified spelling still resolves.
        assert!(!names.contains(&"bare_ready"));
        assert!(names.contains(&"qualified_ready"));
    }

    /// `AsRef<OsStr>` / `AsRef<Path>` parameters and `PathBuf` returns
    /// marshal as strings (`String` implements both bounds; a path renders
    /// lossily) — dotenv's whole practical surface (`var`, `dotenv`,
    /// `from_path`, `from_filename`) bridges, while iterator returns stay
    /// unsupported.
    #[test]
    fn bridges_path_and_osstr_surfaces() {
        let crate_dir = TempDir::new("paths");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            use std::path::{Path, PathBuf};

            #[derive(Clone)]
            pub struct Error;

            impl std::fmt::Display for Error {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(f, "error")
                }
            }

            pub fn var<K: AsRef<std::ffi::OsStr>>(key: K) -> Result<String, Error> {
                Ok(key.as_ref().to_string_lossy().into_owned())
            }
            pub fn dotenv() -> Result<PathBuf, Error> {
                Ok(PathBuf::from(".env"))
            }
            pub fn from_path<P: AsRef<Path>>(path: P) -> Result<(), Error> {
                Ok(())
            }
            pub fn vars() -> std::vec::IntoIter<(String, String)> {
                Vec::new().into_iter()
            }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let find = |name: &str| {
            discovered
                .functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} must bridge"))
        };
        let var = find("var");
        assert_eq!(var.params, vec![ParamKind::Str]);
        assert_eq!(var.ret, RetKind::ResultStr);
        assert_eq!(find("dotenv").ret, RetKind::ResultPath);
        assert_eq!(find("from_path").ret, RetKind::ResultUnit);
        assert_eq!(find("from_path").params, vec![ParamKind::Str]);

        // The path result renders through `to_string_lossy` before it
        // becomes a string handle.
        let lib = emit_lib("paths", &discovered);
        assert!(lib.contains("to_string_lossy"));

        // Iterator returns stay unbridgeable.
        assert!(!discovered.functions.iter().any(|f| f.name == "vars"));
    }

    #[test]
    fn bridges_result_cow_strings_as_owned_strings() {
        let crate_dir = TempDir::new("cow_strings");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            use std::borrow::Cow;

            #[derive(Clone)]
            pub struct Error;

            impl std::fmt::Display for Error {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(f, "error")
                }
            }

            pub fn decode(data: &str) -> Result<Cow<'_, str>, Error> {
                Ok(Cow::Borrowed(data))
            }
            pub fn decode_bytes(data: &[u8]) -> Result<Cow<'_, [u8]>, Error> {
                Ok(Cow::Borrowed(data))
            }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let decode = discovered
            .functions
            .iter()
            .find(|f| f.name == "decode")
            .expect("Cow string result should bridge");
        assert_eq!(decode.params, vec![ParamKind::Str]);
        assert_eq!(decode.ret, RetKind::ResultStr);
        let decode_bytes = discovered
            .functions
            .iter()
            .find(|f| f.name == "decode_bytes")
            .expect("Cow byte result should bridge");
        assert_eq!(decode_bytes.params, vec![ParamKind::Bytes]);
        assert_eq!(decode_bytes.ret, RetKind::ResultBytes);

        let lib = emit_lib("cow_strings", &discovered);
        assert!(lib.contains("_ntsc_str_to_handle"));
        assert!(lib.contains("returned Err({})"));
        assert!(lib.contains("std::any::type_name_of_val(&__e)"));
        assert!(
            !discovered
                .unsupported
                .iter()
                .any(|item| item.name == "decode")
        );
    }

    #[test]
    fn bridges_cow_parameters_without_binders() {
        let crate_dir = TempDir::new("cow_params");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            use std::borrow::Cow;
            use std::path::Path;

            pub fn text(value: Cow<'_, str>) -> Cow<'_, str> { value }
            pub fn bytes(value: Cow<'_, [u8]>) -> Cow<'_, [u8]> { value }
            pub fn borrowed(value: &Cow<'_, str>) -> usize { value.len() }
            pub fn mutated(value: &mut Cow<'_, str>) { value.to_mut().push('!'); }
            pub fn bad_path(value: Cow<'_, Path>) -> Cow<'_, Path> { value }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let find = |name: &str| discovered.functions.iter().find(|func| func.name == name);
        let text = find("text").expect("Cow<str> parameter should bridge");
        assert_eq!(text.params, vec![ParamKind::CowStr]);
        assert_eq!(text.ret, RetKind::Str);
        let bytes = find("bytes").expect("Cow<[u8]> parameter should bridge");
        assert_eq!(bytes.params, vec![ParamKind::CowBytes]);
        assert_eq!(bytes.ret, RetKind::Bytes);
        let borrowed = find("borrowed").expect("borrowed Cow should bridge");
        assert_eq!(borrowed.params, vec![ParamKind::CowStr]);
        assert_eq!(borrowed.param_ref, vec![ParamRef::Ref]);
        assert!(find("mutated").is_none(), "mutable Cow needs write-back");
        assert!(
            discovered
                .unsupported
                .iter()
                .any(|item| item.name == "mutated")
        );
        assert!(
            find("bad_path").is_none(),
            "Cow<Path> must stay unsupported"
        );
        assert!(
            discovered
                .unsupported
                .iter()
                .any(|item| item.name == "bad_path")
        );

        let lib = emit_lib("cow_params", &discovered);
        assert!(lib.contains("Cow::Owned"));
        assert!(lib.contains("_ntsc_bytes_to_handle"));
    }

    /// A renamed re-export is the type's public name
    /// (`pub use self::error::Error as GatewayError`): variant bridges
    /// must construct through it, not the private definition path.
    #[test]
    fn resolves_renamed_reexports() {
        let crate_dir = TempDir::new("reexport");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub mod gateway {
                mod error {
                    #[derive(Clone)]
                    pub enum Error {
                        Bad,
                    }
                }
                pub use self::error::Error as GatewayError;
            }
            "#,
        )
        .unwrap();

        let found = discover_public_functions(&crate_dir.0);
        let variant = found
            .iter()
            .find(|f| f.name == "GatewayError_Bad")
            .expect("renamed re-export must name the variant bridge");
        assert!(
            variant.call_path.contains("GatewayError::Bad"),
            "call path must use the public alias, got {}",
            variant.call_path
        );
    }

    /// Trait-method parameters resolve in the trait's own module scope:
    /// a bare name that is ambiguous crate-wide (`Ready` here, serenity's
    /// `serenity::all::Ready` vs `create_poll::Ready` in reality) still
    /// resolves through the trait's glob import to the type it actually
    /// imports, so the method stays in the callback inventory.
    #[test]
    fn resolves_trait_params_through_module_scope() {
        let crate_dir = TempDir::new("scopes");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub mod model {
                pub mod gateway {
                    pub struct Ready;
                }
                pub mod prelude {
                    pub use super::gateway::Ready;
                }
            }
            pub mod builder {
                pub struct Ready;
            }
            pub mod client {
                use crate::model::prelude::*;

                pub struct Context;

                pub trait Handler {
                    fn ready(&self, ctx: Context, data: Ready);
                    fn tick(&self);
                }

                // `event_handler!`-style generation: the trait's methods
                // exist only inside the macro body, so the invoking
                // module's scope is all the resolver has.
                macro_rules! event_macro {
                    ( $( $variant:ident { $( $arg:ident: $ty:ty ),* } => async fn $method:ident(&self, ctx: Context); )* ) => {
                        pub trait MacroHandler: Send + Sync {
                            $( async fn $method(&self, ctx: Context, $( $arg: $ty ),*); )*
                        }
                    };
                }
                event_macro! {
                    ReadyEvent { data: Ready } => async fn macro_ready(&self, ctx: Context);
                }
            }
            pub struct Widget;
            impl Widget {
                pub async fn make(handler: impl client::Handler) -> i32 { 0 }
                pub async fn make_macro(handler: impl client::MacroHandler) -> i32 { 0 }
            }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let handler = discovered
            .callbacks
            .iter()
            .find(|cb| cb.trait_path == "client::Handler")
            .expect("glob-resolved trait must be inventoried");
        let ready = handler
            .methods
            .iter()
            .find(|m| m.name == "ready")
            .expect("`ready` must stay in the inventory");
        assert_eq!(
            ready.params,
            vec![
                ParamKind::Object("client::Context".into()),
                ParamKind::Object("model::gateway::Ready".into()),
            ]
        );
        assert!(handler.methods.iter().any(|m| m.name == "tick"));
        let macro_handler = discovered
            .callbacks
            .iter()
            .find(|cb| cb.trait_path == "client::MacroHandler")
            .expect("macro-generated trait must be inventoried");
        let macro_ready = macro_handler
            .methods
            .iter()
            .find(|m| m.name == "macro_ready")
            .expect("macro-generated `ready` must stay in the inventory");
        assert_eq!(
            macro_ready.params,
            vec![
                ParamKind::Object("client::Context".into()),
                ParamKind::Object("model::gateway::Ready".into()),
            ]
        );

        // The emitted async bridge carries worker-thread exceptions to the
        // awaiting goroutine (the work fn encodes a throw as a negated
        // message handle the op result re-raises).
        let lib = emit_lib("scopes", &discovered);
        assert!(lib.contains("ntsc_exception_pending()"));
        assert!(lib.contains("ntsc_exception_take_message()"));
        assert!(lib.contains("return -message"));
    }

    /// Generic declarations never resolve as concrete types through the
    /// shared known-types map: a generic struct and a generic alias supply
    /// no bare-name entry, while an ordinary struct and a concrete alias
    /// (including a generic alias applied to concrete arguments) resolve
    /// through that same map.
    #[test]
    fn resolves_concrete_types_and_skips_generic_declarations() {
        let crate_dir = TempDir::new("concrete_types");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub struct Widget;
            pub struct Holder<T> { pub inner: T }
            pub type Label = String;
            pub type Boxed<T> = Vec<T>;

            pub fn make_widget() -> Widget { Widget }
            pub fn greet(name: Label) -> Label { name }
            pub fn joined(items: Boxed<String>) -> String { items.join(",") }
            pub fn take_holder(h: Holder) -> String { String::new() }
            pub fn take_boxed(h: Boxed) -> String { String::new() }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let find = |name: &str| {
            discovered
                .functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} must bridge"))
        };

        assert_eq!(find("make_widget").ret, RetKind::Object("Widget".into()));
        let greet = find("greet");
        assert_eq!(greet.params, [ParamKind::Str]);
        assert_eq!(greet.ret, RetKind::Str);
        let joined = find("joined");
        assert_eq!(joined.params, [ParamKind::Array(ArrayElemKind::Str)]);
        assert_eq!(joined.ret, RetKind::Str);

        let missing: Vec<&str> = discovered
            .unsupported
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        for name in ["take_holder", "take_boxed"] {
            assert!(
                missing.contains(&name),
                "{name} must be reported, got {missing:?}"
            );
        }
        assert!(
            !discovered.functions.iter().any(|f| f.name == "take_holder"),
            "generic struct must not resolve as a concrete type"
        );
        assert!(
            !discovered.functions.iter().any(|f| f.name == "take_boxed"),
            "generic alias must not resolve as a concrete type"
        );
    }

    /// The manifest is byte-compared for freshness, so its bytes must be
    /// a pure function of the API: rediscovery emits identical bytes, and
    /// unsupported entries list in name order however the source ordered
    /// them.
    #[test]
    fn export_manifest_bytes_are_deterministic() {
        let crate_dir = TempDir::new("manifest_order");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            pub fn zeta_bad(m: std::collections::HashMap<String, String>) -> i64 {
                m.len() as i64
            }
            pub fn alpha_ok(name: &str) -> String { name.into() }
            pub fn middle_bad(m: std::collections::HashMap<String, String>) -> i64 {
                m.len() as i64
            }
            "#,
        )
        .unwrap();

        let first = discover_public_api(&crate_dir.0);
        let second = discover_public_api(&crate_dir.0);
        let first_text = export_manifest_content(
            "order",
            &first.functions,
            &first.unsupported,
            &first.callbacks,
        )
        .unwrap();
        let second_text = export_manifest_content(
            "order",
            &second.functions,
            &second.unsupported,
            &second.callbacks,
        )
        .unwrap();
        assert_eq!(first_text, second_text);

        // Read back as plain JSON, the way every consumer sees it:
        // unsupported entries (no `return` key) sort by name.
        let value: serde_json::Value = serde_json::from_str(&first_text).unwrap();
        let entries = value["functions"].as_array().unwrap();
        assert!(entries.iter().any(|entry| entry["name"] == "alpha_ok"));
        let untyped: Vec<&str> = entries
            .iter()
            .filter(|entry| entry.get("return").is_none())
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert_eq!(untyped, ["middle_bad", "zeta_bad"]);
    }

    /// Paths rooted in a direct dependency classify as foreign objects
    /// carrying their owner: written qualified, or bare behind an import
    /// (rewritten before classification). Names from crates that are not
    /// dependencies never resolve, however they are spelled.
    #[test]
    fn classifies_dependency_rooted_types_as_foreign_objects() {
        let root = TempDir::new("foreign_owner");
        let dep_src = root.0.join("dep");
        fs::create_dir_all(dep_src.join("src")).unwrap();
        fs::write(
            dep_src.join("Cargo.toml"),
            "[package]\nname = \"dep\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        fs::write(
            dep_src.join("src").join("lib.rs"),
            "pub struct Widget;\npub struct Gadget;\n",
        )
        .unwrap();

        let consumer = root.0.join("consumer");
        fs::create_dir_all(consumer.join("src")).unwrap();
        fs::write(
            consumer.join("Cargo.toml"),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n",
        )
        .unwrap();
        fs::write(
            consumer.join("src").join("lib.rs"),
            r#"
            use dep::Gadget;

            pub fn make() -> dep::Widget { dep::Widget }
            pub fn take(w: dep::Widget) -> String { String::new() }
            pub fn take_imported(g: Gadget) -> String { String::new() }
            pub fn take_unknown(x: other::Thing) -> String { String::new() }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&consumer);
        let foreign = |name: &str| {
            discovered
                .functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} must bridge"))
        };
        let widget = ObjectType::foreign("dep", "Widget");
        assert_eq!(foreign("make").ret, RetKind::Object(widget.clone()));
        assert_eq!(foreign("take").params, [ParamKind::Object(widget.clone())]);
        assert_eq!(
            foreign("take_imported").params,
            [ParamKind::Object(ObjectType::foreign("dep", "Gadget"))]
        );
        // `other` is not a dependency: nothing bridges, and the failure
        // is reported rather than silently dropped.
        assert!(
            !discovered
                .functions
                .iter()
                .any(|f| f.name == "take_unknown"),
            "non-dependency paths must not resolve"
        );
        assert!(
            discovered
                .unsupported
                .iter()
                .any(|item| item.name == "take_unknown"),
            "non-dependency paths must be reported"
        );
    }

    /// Two-crate fixture: `dep` owns `Widget`, `consumer` takes and
    /// returns it by value (including through a `use` import).
    fn write_foreign_registry(root: &std::path::Path) {
        for (name, version, manifest, lib) in [
            (
                "dep",
                "0.2.0",
                "[package]\nname = \"dep\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
                "pub struct Widget(pub String);\n",
            ),
            (
                "consumer",
                "1.0.0",
                "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndep = \"0.2\"\n",
                "use dep::Widget;\n\npub fn make() -> dep::Widget { dep::Widget(String::new()) }\n pub fn take(w: Widget) -> dep::Widget { w }\n",
            ),
        ] {
            let dir = root.join(format!("{name}-{version}"));
            fs::create_dir_all(dir.join("src")).unwrap();
            fs::write(dir.join("Cargo.toml"), manifest).unwrap();
            fs::write(dir.join("src").join("lib.rs"), lib).unwrap();
        }
    }

    fn write_foreign_project(project: &std::path::Path, with_dep: bool) {
        let mut neutron = String::from("[rust-deps]\nconsumer = \"1.0\"\n");
        if with_dep {
            neutron.push_str("dep = \"0.2\"\n");
        }
        fs::write(project.join("neutron.toml"), neutron).unwrap();
        let mut deps = vec![crate::manifest::LockedDep {
            name: "consumer".into(),
            version: "1.0.0".into(),
            source: "registry".into(),
            checksum: None,
        }];
        if with_dep {
            deps.push(crate::manifest::LockedDep {
                name: "dep".into(),
                version: "0.2.0".into(),
                source: "registry".into(),
                checksum: None,
            });
        }
        crate::manifest::write_lock(project, &deps).unwrap();
    }

    /// End to end across two adaptors: the consumer's bridges take and
    /// return the owner's type through the shared space, with pinned
    /// dependency edges, owner tags both sides agree on, and manifest
    /// origins — all with no per-crate support code.
    #[test]
    fn emit_foreign_bridges_with_pinned_edges_and_origins() {
        let registry = TempDir::new("foreign_registry");
        write_foreign_registry(&registry.0);
        let cache_tmp = TempDir::new("foreign_cache");
        let cache = CacheRoot::at(cache_tmp.0.clone());
        let project = TempDir::new("foreign_project");
        write_foreign_project(&project.0, true);

        let dir = emit_adaptor(
            &cache,
            &project.0,
            "consumer",
            "1.0.0",
            &registry.0,
            None,
            None,
        )
        .unwrap();
        let lib = fs::read_to_string(dir.join("src").join("lib.rs")).unwrap();
        // Takes move the foreign value out; puts store under the owner's
        // tag, which the owner computes identically from its lock entry.
        assert!(lib.contains("_ntsc_shared_take(a0, \"dep@0.2.0+/Widget\")"));
        assert!(lib.contains("_ntsc_shared_put(result, \"dep@0.2.0+/Widget\")"));
        assert!(lib.contains("dep::Widget"));
        // The adaptor pins the owner edge to the locked build.
        let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(
            manifest.contains("dep = {"),
            "owner edge missing: {manifest}"
        );
        assert!(
            manifest.contains("version = \"=0.2.0\""),
            "unpinned: {manifest}"
        );
        // Origins travel in the manifest for method dispatch and typing.
        let export = fs::read_to_string(dir.join("consumer.export.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&export).unwrap();
        let take = value["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "take")
            .expect("take must be exported");
        assert_eq!(take["params"][0]["type"], "object[Widget]");
        assert_eq!(take["params"][0]["origin"], "dep");

        // The generated adaptor compiles against the real dependency
        // sources: foreign paths resolve through the pinned edge. The
        // fixture consumer's own version requirement would resolve over
        // the network in production; the offline sandbox path-pins it.
        let cached_consumer = cache.crate_src_path("consumer", "1.0.0");
        let cached_dep = cache.crate_src_path("dep", "0.2.0");
        let consumer_manifest = fs::read_to_string(cached_consumer.join("Cargo.toml")).unwrap();
        fs::write(
            cached_consumer.join("Cargo.toml"),
            consumer_manifest.replace(
                "dep = \"0.2\"",
                &format!("dep = {{ path = {:?} }}", cached_dep),
            ),
        )
        .unwrap();
        let output = std::process::Command::new("cargo")
            .arg("build")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(dir.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(dir.join("target"))
            .output()
            .expect("failed to run cargo");
        assert!(
            output.status.success(),
            "foreign adaptor must compile:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// An owner missing from the lockfile demotes its functions to
    /// `unsupported` with an actionable reason instead of emitting code
    /// that cannot compile.
    #[test]
    fn unresolved_foreign_owners_demote_with_reasons() {
        let registry = TempDir::new("foreign_registry_missing");
        write_foreign_registry(&registry.0);
        let cache_tmp = TempDir::new("foreign_cache_missing");
        let cache = CacheRoot::at(cache_tmp.0.clone());
        let project = TempDir::new("foreign_project_missing");
        write_foreign_project(&project.0, false);

        let dir = emit_adaptor(
            &cache,
            &project.0,
            "consumer",
            "1.0.0",
            &registry.0,
            None,
            None,
        )
        .unwrap();
        let export = fs::read_to_string(dir.join("consumer.export.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&export).unwrap();
        let entries = value["functions"].as_array().unwrap();
        assert!(
            !entries
                .iter()
                .any(|entry| { entry["name"] == "take" && entry.get("return").is_some() }),
            "unresolvable foreign bridges must not emit"
        );
        let missing = entries
            .iter()
            .find(|entry| entry["name"] == "take")
            .expect("demoted function must be reported");
        assert_eq!(missing["status"], "unsupported");
        assert!(
            missing["reason"]
                .as_str()
                .unwrap()
                .contains("not a locked [rust-deps] entry"),
            "reason must be actionable: {missing}"
        );
    }

    /// Compiler-derived `Default` bridges without an `impl` block.
    #[test]
    fn synthesizes_default_for_derived_impls() {
        let crate_dir = TempDir::new("derive_default");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            #[derive(Default)]
            pub struct Config {
                pub retries: u32,
            }

            pub struct Manual;
            impl Default for Manual {
                fn default() -> Self { Manual }
            }
            "#,
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let default = discovered
            .functions
            .iter()
            .find(|f| f.name == "Config_default")
            .expect("derived Default must bridge");
        assert_eq!(default.call_path, "Config::default");
        assert!(default.params.is_empty());
        assert_eq!(default.ret, RetKind::Object(ObjectType::local("Config")));
        // Exactly one bridge: an explicit impl still wins its single name.
        assert_eq!(
            discovered
                .functions
                .iter()
                .filter(|f| f.name == "Manual_default")
                .count(),
            1
        );
        let lib = emit_lib("defaults", &discovered);
        assert!(lib.contains("use std::default::Default;"));
        assert!(lib.contains("defaults::Config::default()"));
    }

    /// Option, Duration, setter, and derived-Default shapes bridge with
    /// no per-crate rules: option params, option callbacks, option field
    /// getters, millis durations, and public-field setters.
    #[test]
    fn bridges_option_duration_setter_shapes() {
        let crate_dir = TempDir::new("option_shapes");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            use std::time::Duration;

            #[derive(Clone, Default)]
            pub struct Item(pub String);

            pub struct Store {
                pub label: String,
                pub count: Option<u64>,
                pub current: Option<Item>,
                pub ratio: f64,
            }

            pub trait Watcher {
                fn ping(&self, note: Option<String>);
                fn done(&self);
            }

            pub fn wait_for(ms: u64, timeout: Duration) -> u64 { ms }
            pub fn pick(first: Option<Item>, second: Option<String>) -> String { String::new() }
            pub fn watch(w: impl Watcher) -> String { String::from("ok") }
            "#,
        )
        .unwrap();
        fs::write(
            crate_dir.0.join("Cargo.toml"),
            "[package]\nname = \"shapetest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let find = |name: &str| {
            discovered
                .functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} must bridge"))
        };
        assert_eq!(
            find("wait_for").params,
            [ParamKind::Int("u64".into()), ParamKind::Duration]
        );
        assert_eq!(
            find("pick").params,
            [
                ParamKind::OptionObject(ObjectType::local("Item")),
                ParamKind::OptionStr,
            ]
        );
        // Option field getters and setters.
        assert_eq!(find("Store_label").ret, RetKind::Str);
        assert_eq!(find("Store_count").ret, RetKind::OptionInt("u64".into()));
        assert_eq!(
            find("Store_current").ret,
            RetKind::OptionObject(ObjectType::local("Item"))
        );
        assert!(
            discovered
                .functions
                .iter()
                .any(|f| f.name == "Store_set_label")
        );
        assert!(
            discovered
                .functions
                .iter()
                .any(|f| f.name == "Store_set_count")
        );
        assert!(
            discovered
                .functions
                .iter()
                .any(|f| f.name == "Store_set_current")
        );
        // Derived Default plus the option-carrying callback.
        assert!(
            discovered
                .functions
                .iter()
                .any(|f| f.name == "Item_default")
        );
        let watch = find("watch");
        assert_eq!(watch.params, [ParamKind::Callback("Watcher".into())]);
        let callback = discovered
            .callbacks
            .iter()
            .find(|cb| cb.short == "Watcher")
            .expect("watcher must be inventoried");
        let ping = callback
            .methods
            .iter()
            .find(|m| m.name == "ping")
            .expect("ping keeps its option param");
        assert_eq!(ping.params, [ParamKind::OptionStr]);

        // Everything above compiles together.
        let lib = emit_lib("shapetest", &discovered);
        let adaptor_dir = TempDir::new("shapes_adaptor");
        fs::create_dir_all(adaptor_dir.0.join("src")).unwrap();
        fs::write(
            adaptor_dir.0.join("Cargo.toml"),
            adaptor_manifest(
                "shapetest",
                "0.1.0",
                &crate_dir.0,
                false,
                None,
                &std::collections::BTreeMap::new(),
            ),
        )
        .unwrap();
        fs::write(adaptor_dir.0.join("src").join("lib.rs"), lib).unwrap();
        let output = std::process::Command::new("cargo")
            .arg("build")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(adaptor_dir.0.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(adaptor_dir.0.join("target"))
            .output()
            .expect("failed to run cargo");
        assert!(
            output.status.success(),
            "shapes adaptor must compile:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// `Arc`-family shapes bridge through the shared allocation with no
    /// per-crate rules: `Arc<Self>` returns, by-value `Arc` parameters,
    /// `Arc<Mutex<T>>` returns, and borrowed receivers resolving plain
    /// and `Arc` entries alike. Labels stay `object[T]`; tags carry the
    /// wrapping.
    #[test]
    fn bridges_arc_family_through_shared_allocations() {
        let crate_dir = TempDir::new("arc_shapes");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("lib.rs"),
            r#"
            use std::sync::{Arc, Mutex};

            pub struct Engine;
            pub struct Handle;

            impl Engine {
                pub fn shared() -> Arc<Engine> { Arc::new(Engine) }
                pub fn describe(&self) -> String { String::from("engine") }
                pub fn run_shared(state: Arc<Engine>) -> String { String::from("ran") }
            }

            impl Handle {
                pub fn open() -> Arc<Mutex<Handle>> { Arc::new(Mutex::new(Handle)) }
            }
            "#,
        )
        .unwrap();
        fs::write(
            crate_dir.0.join("Cargo.toml"),
            "[package]\nname = \"arctest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let discovered = discover_public_api(&crate_dir.0);
        let find = |name: &str| {
            discovered
                .functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} must bridge"))
        };
        let arc_engine = ObjectType {
            owner: None,
            path: "Engine".into(),
            wrapper: ObjectWrapper::Arc,
        };
        assert_eq!(
            find("Engine_shared").ret,
            RetKind::Object(arc_engine.clone())
        );
        assert_eq!(
            find("Engine_run_shared").params,
            [ParamKind::Object(arc_engine)]
        );
        let describe = find("Engine_describe");
        assert_eq!(
            describe.params,
            [ParamKind::Object(ObjectType::local("Engine"))]
        );
        assert_eq!(
            find("Handle_open").ret,
            RetKind::Object(ObjectType {
                owner: None,
                path: "Handle".into(),
                wrapper: ObjectWrapper::StdMutex,
            })
        );

        // Labels stay wrapper-agnostic; the lib carries both tags and the
        // borrow-or-Arc receiver path, and it all compiles.
        let export = export_manifest_content(
            "arctest",
            &discovered.functions,
            &discovered.unsupported,
            &discovered.callbacks,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&export).unwrap();
        let shared = value["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "Engine_shared")
            .unwrap();
        assert_eq!(shared["return"]["type"], "object[Engine]");
        assert!(shared["return"].get("origin").is_none());

        let lib = emit_lib("arctest", &discovered);
        assert!(lib.contains("_ntsc_shared_borrow_or_arc"));
        assert!(lib.contains("std::sync::Arc<arctest::Engine>"));
        // Mutex flavors travel in tags (puts are generic); the build
        // below proves the spelling resolves.
        assert!(lib.contains("arctest@0.1.0+/Handle#Arc<std::Mutex>"));

        let adaptor_dir = TempDir::new("arc_adaptor");
        fs::create_dir_all(adaptor_dir.0.join("src")).unwrap();
        fs::write(
            adaptor_dir.0.join("Cargo.toml"),
            adaptor_manifest(
                "arctest",
                "0.1.0",
                &crate_dir.0,
                false,
                None,
                &std::collections::BTreeMap::new(),
            ),
        )
        .unwrap();
        fs::write(adaptor_dir.0.join("src").join("lib.rs"), lib).unwrap();
        let output = std::process::Command::new("cargo")
            .arg("build")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(adaptor_dir.0.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(adaptor_dir.0.join("target"))
            .output()
            .expect("failed to run cargo");
        assert!(
            output.status.success(),
            "arc adaptor must compile:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn discovery_terminates_on_cyclic_glob_reexports() {
        // A re-export hub whose modules glob-import each other (as serenity's
        // `model::prelude` does) forms a highly connected, cyclic glob graph.
        // Without memoization, resolving each bare type name recurses through
        // every path of that graph and blows up exponentially; the walk must
        // instead resolve each module once and terminate promptly.
        let crate_dir = TempDir::new("glob_cycle");
        let src_dir = crate_dir.0.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            crate_dir.0.join("Cargo.toml"),
            "[package]\nname = \"hub\"\n",
        )
        .unwrap();

        let modules = ["a", "b", "c", "d", "e", "f"];
        let glob_all: String = modules
            .iter()
            .map(|m| format!("use crate::{m}::*;\n"))
            .collect();
        let mut lib = String::from("pub mod prelude;\n");
        for m in &modules {
            lib.push_str(&format!("pub mod {m};\n"));
        }
        fs::write(src_dir.join("lib.rs"), lib).unwrap();
        fs::write(src_dir.join("prelude.rs"), &glob_all).unwrap();
        for m in &modules {
            let body = format!(
                "{glob_all}use crate::prelude::*;\n\
                 pub struct Widget_{m};\n\
                 pub enum Node_{m} {{ Many(Vec<Widget_{m}>), One(Widget_{m}) }}\n\
                 pub fn make_{m}(items: Vec<Widget_{m}>) -> Node_{m} {{ Node_{m}::Many(items) }}\n"
            );
            fs::write(src_dir.join(format!("{m}.rs")), body).unwrap();
        }

        let found = discover_public_functions(&crate_dir.0);
        let mut names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        names.sort_unstable();
        // Every module's free function and both enum-variant constructors are
        // discovered: the walk reached and finished the variant-type
        // qualification that previously looped, for all modules in the cycle.
        assert_eq!(
            names,
            [
                "Node_a_Many",
                "Node_a_One",
                "Node_b_Many",
                "Node_b_One",
                "Node_c_Many",
                "Node_c_One",
                "Node_d_Many",
                "Node_d_One",
                "Node_e_Many",
                "Node_e_One",
                "Node_f_Many",
                "Node_f_One",
                "make_a",
                "make_b",
                "make_c",
                "make_d",
                "make_e",
                "make_f"
            ]
        );
    }
}
