# Rust adaptor autogen standard

This is the contract for Neutron's Rust-crate interop autogen: what
`ntsc cargo install` / `ntsc build` generate, how the compiler consumes
it, and when it rebuilds. The normative schema is the `ExportManifest`
model in `crates/ntsc-cargo/src/metadata.rs`; this document describes
the contract in prose. Producers must emit it; readers must tolerate
it.

> **Note — this is an experimental workflow.** Nothing in this page
> applies until the project opts in with `[experimental]
> rust_modules = true` in `neutron.toml`. Rust crate imports
> (`use @crate`) are disabled by default, and builds fail with a
> "disabled by default" error before any autogen or compilation happens.
> See [Experimental features](experimental.md) for the opt-in and the
> [Rust adaptors guide](../guide/rust-adaptors.md) for day-to-day usage.

## Layout

Per project, under `<project>/.ntsc/<crate>/`:

| File | Contents |
| --- | --- |
| `Cargo.toml` | Package `ntsc-adaptor-<crate>`, `crate-type = ["staticlib"]`, one path dependency on the cached crate source plus `tokio` when async bridges need it. `[package.metadata.ntsc] crate-version` records the source version the adaptor was generated against. |
| `src/lib.rs` | Starts with `#![deny(unsafe_code)]`. One `#[no_mangle]` bridge per exported function (see Naming), private marshaling helpers, the object registry when struct handles cross the boundary, and one `__NtscCb_<Trait>` impl per callback trait. |
| `<crate>.export.json` | The export manifest (see Manifest). |

Heavy artifacts live outside the project in the global cache
(`~/.ntsc/cargo`, or `$NTSC_CARGO_HOME` when set): compressed
`.crate` archives, extracted sources, and compiled adaptor archives
under `build/<crate>-<version>/<target>-<key>/`.

## Scope and no-binder policy

Autogen is crate-wide. Discovery follows reachable public `pub mod` chains,
public re-exports, enabled feature branches, impl blocks, public fields, and
supported callback traits. One generated adaptor and one manifest cover the
crate; adding a public module does not require a separate binding file.

The project policy is to extend this shared generator rather than introduce
per-crate binders. A supported signature is emitted automatically. A public
item that cannot be represented safely is retained as an explicit manifest
entry with `unsupported`, `conditional`, or `probe_failed` status and a
reason. “All Rust modules” therefore means all reachable public modules, not
that arbitrary Rust lifetimes, unconstrained generics, unsafe contracts, or
process-specific runtime invariants can be inferred at the FFI boundary.

## Performance goals

Autogen must be fast both on a cold dependency and during ordinary edits:

- unchanged dependencies skip source discovery when their input fingerprint
  matches;
- generated files are rewritten only when their bytes change;
- multiple adaptors share one Cargo build graph and compiled artifacts are
  reused from the global cache;
- a change to one path dependency invalidates that dependency without forcing
  unrelated adaptors to rescan.

Performance work should compare cold generation, unchanged warm builds, and
single-dependency edits separately. New coverage should improve the shared
classifier/discovery pipeline, cache keys, or incremental invalidation rather
than adding a manual binder.

## Naming

- Bridge symbol for exported function `f` of crate `foo`:
  `ntsc_adaptor_foo_f`. The `_async` offload alias appends `_async`.
- Hyphenated crate names normalize `-` to `_` in symbols and lib names. The
  same rule keys the manifest tables, so a package `mewsic-tui` reached
  through `use @mewsic_tui` resolves its return types, async flags, and method
  bridges: both spellings find the same entry. Without it, a lookup missed
  the manifest and every call fell back to the string-handle assumption —
  silently mistyping a `bool` return as a handle.
- Name collisions across modules dedup as `name_2`, `name_3`, …
- Callback trait `T` gets struct `__NtscCb_T`, an event queue, and the
  poll family `__NtscCb_T_poll`, `__NtscCb_T_arg_count`,
  `__NtscCb_T_arg`, `__NtscCb_T_arg_str` (same symbol prefix scheme).
- Object retain/release entry points:
  `ntsc_adaptor_<crate>_object_retain` / `_object_release`.

## Crossing with a result

Returns use the manifest label and the payload's ownership-safe handle
representation. The important distinction is between scalar option cells and
nullable handles for values that already have a runtime handle:

| Return | Arrives as | Read it with |
| --- | --- | --- |
| Owned `Struct` | an object handle | generated `Type_field` accessors or method syntax |
| `Option<int>`, `Option<bool>`, `Option<float>` | an option cell handle, `0` for `None` | option operations / nullness checks |
| `Option<String>` / `Option<Vec<T>>` | a nullable string or array handle | test for `0` before using the handle |
| `Option<Struct>` | a nullable object handle | test for `0`, then use object accessors or methods |
| `Result<T, E>` | the `Ok` payload representation | `Err` becomes a catchable Neutron exception |

The generated manifest is authoritative for nested and foreign types. Do not
rewrite an API as an out-parameter merely to work around an old binding rule;
if a safe shape is missing, add it to the shared autogen classifier and emitter
so every crate benefits.

### Option-cell ownership across the boundary

Scalar option cells are libc-allocated (`_ntsc_option_cell` /
`_ntsc_option_float_cell`) and always have exactly one owner:

- **Return direction** — the generated bridge returns a cell that Neutron
  codegen adopts; every Neutron drop path (`option_free`) frees it. A `None`
  return is the handle `0` and allocates nothing.
- **Parameter direction** (Rust→Neutron callbacks) — the bridge mallocs a
  cell for each `Option<T>` argument and reads it back with
  `_ntsc_option_scalar` (`std::ptr::read`, no free). The *Neutron callback
  parameter owns the cell*: its standard option drop is the only free. Never
  read a callback's option cell twice, and never free it from the Rust side
  — the paired contract relies on Neutron's slot drop running exactly once.

## Manifest

Top level: `schema_version` (currently 3), `module` (the crate name),
`functions` (bridges, async aliases, and poll entries, then
unsupported items), `callbacks` (trait, generated type, methods with
queued argument labels in order). `types`/`enums` are reserved and
empty in v3.

A typed entry carries `name`, `path` (crate-rooted call path),
`status` (`"typed"`), `params` (`[{name, tier, type}]`),
`return` (`{tier, type}`), `symbol`, `async`, and optional `doc`.
An untyped entry carries `name`, `path`, `status` (one of
`"opaque"`, `"conditional"`, `"unsupported"`, `"probe_failed"`),
`reason`, and optional `signature`. Only tier-1 labels appear
(`"string"`, `"bytes"`, `"int"`, `"bool"`, `"float"`, `"void"`,
`"object[T]"`, `"option[…]"`, `"result[…]"`, `"array[…]"`,
`"callback[T]"`).

Version policy: a breaking shape change bumps `schema_version`.
Readers must ignore unknown fields, treat missing sections as empty,
default a missing entry `path` to its `name` and a missing `async`
to `false`, and skip entries they cannot use. A malformed manifest
reads as empty, and Rust calls then fall back to the string-handle
assumption.

## Coverage

`ntsc cargo coverage` reports export-manifest entry counts for every direct
`[rust-deps]` adaptor. Pass a crate name to restrict the report to one adaptor.
The report covers the functions, types, and enums discovered across all active
public modules in that dependency; it is manifest coverage, not Rust source-line
coverage. Treat unsupported counts and reasons as the input list for shared
autogen work, not as a list of binders to maintain. Missing manifests are
reported without failing the command.

## Determinism

Manifest bytes are a pure function of the discovered API: functions
sort by name, unsupported entries sort by name (then path, then
reason), serialization is `serde_json` pretty output in struct field
order. Identical APIs produce identical bytes.

## Freshness

Generation is idempotent: files are rewritten only when their bytes
change, so unchanged APIs never bump mtimes. A cached archive
rebuilds when any of these hold: the archive is missing, the export
manifest's `schema_version` differs from the generator's, or any of
`src/lib.rs`, `Cargo.toml`, `<crate>.export.json` is newer than the
archive.

The global-cache key for one build is a stable hash of the crate
name, crate version, target triple, profile, and sorted dependency
features (plus a `tokio` marker when async bridges need the
runtime). Machine-specific absolute paths never participate, so the
same dependency shares one cache entry across checkouts.
