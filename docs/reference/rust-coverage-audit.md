# Cached Rust crate coverage audit

Measured on November 4, 2026, and rechecked on November 5 using the same
cached inspected crate versions. The source audit uses the generator's
default-feature and host-target view; a second pass emits actual adaptors and
checks them with Cargo offline, using the repository's shared target directory.
The follow-up refreshed registry metadata and fetched missing dependency
archives for Rand and Tokio's optional features. Inspected sources were not
patched or copied.

**These are source-classification counts, not full upstream API coverage or
compile-verified support.** The scanner approximates cfg/macro handling; missed
macro-generated items and unavailable generic specializations are not proof of
an absent API. The denominator is the generator's classified candidates,
including recorded unsupported/conditional entries, not a compiler-derived
inventory. In particular, zero discovered items means unknown coverage.

| Cached crate | Version | Before: typed / classified | After: typed / classified | After classification rate |
| --- | --- | ---: | ---: | ---: |
| `regex` | 1.13.1 | 48 / 242 | 96 / 242 | 39.7% |
| `url` | 2.5.8 | 79 / 128 | 79 / 128 | 61.7% |
| `uuid` | 1.28.0 | 97 / 148 | 99 / 148 | 66.9% |
| `serde_json` | 1.0.154 | 75 / 288 | 75 / 288 | 26.0% |
| `base64` | 0.23.1 | 53 / 101 | 53 / 101 | 52.5% |
| `bytes` | 1.12.1 | 53 / 196 | 50 / 189 | 26.5% |
| `chrono` | 0.4.45 | 294 / 571 | 268 / 569 | 47.1% |
| `rand` | 0.10.3 | 26 / 139 | 22 / 130 | 16.9% |
| `log` | 0.4.34 | 28 / 62 | 27 / 64 | 42.2% |
| `tokio` | 1.53.2 | 59 / 1995 | 0 / 90 | 0.0% |
| `serde` | 1.0.229 | 0 / 0 | 1 / 303 | 0.3% |

The fixed-byte/fluent extension adds 60 classifications: 48 regex builder methods,
two UUID builder methods, and ten Tokio process builder methods. Fixed-size
byte parameters were already incorrectly classified as variable-size bytes;
their classification counts do not change, but generated code now constructs
the actual array type and checks its length. This fixes a compile-time mismatch
without pretending the old bridge worked.

The compile audit then corrects false positives rather than inflating coverage:
three Bytes methods belong to an unsized type, and 26 Chrono signatures were
classified through a same-named alias instead of their explicit import.
Imported `core::time::Duration` parameters now use the same millis bridge as
`std::time::Duration`. Unsupported impl blocks are recorded once, so excluding
the unsized Bytes impl also changes the classified denominator.
The November 5 follow-up also honors cfg attributes injected by item-wrapper
macros and treats unconfigured custom cfg flags as disabled. Tokio has no
default features, so its default view no longer claims unavailable runtime,
network, process, or timer APIs. Separate runtime and full-feature checks
below establish nonempty support. Serde's literal root macro and its pinned
`serde_core` re-exports now contribute classified candidates.

## Generated-adaptor compilation

All checks use the same pinned cached versions as the table above. Typed export
counts include generated offloaded async aliases and field accessors, so they
are not the source-classification numerator. Passing Cargo check validates the
emitted Rust bridges, not every upstream API or every runtime behavior.

| Crate | Typed emitted exports | Offline Cargo check |
| --- | ---: | --- |
| `regex` | 192 | pass |
| `url` | 158 | pass |
| `uuid` | 198 | pass |
| `serde_json` | 150 | pass |
| `base64` | 106 | pass |
| `bytes` | 96 | pass |
| `chrono` | 528 | pass |
| `log` | 54 | pass |
| `rand` | 44 | pass, after registry/dependency refresh |
| `tokio` (default features) | 0 | pass; empty, not evidence of runtime support |
| `tokio` (`rt,rt-multi-thread`) | 66 | pass |
| `tokio` (`full`) | 522 | pass |
| `serde` | 2 | pass; `de::IgnoredAny::default` and its offloaded alias |

The ten nonempty default-feature adaptors contain 1,528 typed exports.
Tokio's feature-selected exports are counted separately, not added twice.
Shared fixes
found by this pass cover optional/fallible fixed-byte return conversion,
mutable string scratch and copy-back, explicit standard-library trait dispatch,
`alloc` trait imports, explicit-import precedence over aliases, and rejection of
known unsized owned-handle types. The follow-up validates attribute-only item
wrappers and zero-argument literal item macros, preserves macro definition
scope, follows available path or exactly pinned dependency re-exports, and
uses the dependency's forwarded features during classification. Mutable path
dependency sources participate in adaptor cache invalidation.
Known non-`Send` field shapes are excluded from owned handles; the shared
registry still requires `Send + 'static`. Trait implementors now resolve in
their definition scope, rather than matching unrelated same-named types.
The runtime regression also exposed a caller-side leak of returned arrays;
manifested scalar/string array results now follow fresh-value ownership rules.

The compiled end-to-end regression uses a small local Rust fixture matching
these shapes. It exercises reassignment, discarded fluent results, shared
aliases blocking mutation, rejection of non-receiver references, null handles,
short/long byte inputs, panic recovery, mutable byte copy-back, field accessors,
empty arrays, single destruction, and runtime leak detection. It also compiles
optional/fallible fixed-byte returns, calls a specialized `Borrow<[u8]>` bridge,
checks mutable string copy-back, and checks an imported duration shadowing a
root alias. Array declarations, discarded direct/method results, and borrowed
array temporaries are checked for leaks. This runtime fixture complements, but
does not replace, the third-party Cargo checks.

## Remaining blockers

- Generic impl/self types need explicit, validated monomorphization; this audit
  does not implement universal generic specialization.
- Borrowed non-receiver objects and iterators need a sound ownership model.
  They are not converted into dangling owned handles.
- Callback/trait paths, overlapping trait impls, and some re-exported types
  remain unrepresentable or unnameable.
- Arbitrary macro expansion and build-script cfg values still prevent these
  source counts from establishing an exhaustive API inventory. Supported
  wrappers do not constitute a general Rust macro expander.
- Serde now has nonempty discovery, but its generic serialization/deserialization
  APIs remain unsupported without validated specialization.
- Non-`Send` objects remain deliberately unsupported. Source field inspection
  is not a complete Rust auto-trait solver; Cargo compilation is still required.
- Dependency re-export discovery requires available path sources or exactly
  pinned sibling registry sources. Unresolved registry ranges are not guessed.
- Feature and platform changes require a fresh audit; these cached versions are
  not asserted to be the latest releases.

## Reproduce without dependency builds

Run from the Neutron repository. Pass existing extracted source directories,
not crate names or network URLs:

```bash
CARGO_BUILD_JOBS=2 cargo run --offline -p ntsc-cargo --example coverage -- \
  /path/to/regex-1.13.1 \
  /path/to/url-2.5.8 \
  /path/to/uuid-1.28.0
```

This builds only the small audit tool and its existing compiler dependencies
in the repository's usual target directory. It writes sorted JSON to stdout
with versions, counts, function paths, and rejection reasons. It does not
resolve inspected crates, create `.ntsc` adapters, or maintain a second
Cargo target tree. Redirect stdout if a machine-readable snapshot is needed.

## Reproduce generated-adaptor checks

From the repository root, emit into a scratch project and check its manifest:

```bash
SOURCE=/path/to/regex-1.13.1 PROJ=/tmp/ntsc-rust-audit arq rust_adaptor_emit
MANIFEST=/tmp/ntsc-rust-audit/.ntsc/regex/Cargo.toml arq rust_adaptor_check
SOURCE=/path/to/regex-1.13.1 arq rust_coverage

# Feature-selected checks use the same production feature resolver.
FEATURES=full SOURCE=/path/to/tokio-1.53.2 PROJ=/tmp/ntsc-tokio-full arq rust_adaptor_emit
MANIFEST=/tmp/ntsc-tokio-full/.ntsc/tokio/Cargo.toml arq rust_adaptor_check
```

The emitter example accepts multiple source directories after the audit
directory when invoked directly. It uses the production generator, does not
copy inspected sources, and does not fetch foreign owners. Without a project
lockfile, unresolved foreign-owner functions remain unsupported. Cargo check
is offline; missing cached dependencies are blockers, not generator successes.
If resolution is blocked by stale registry metadata or missing archives,
prepare the generated manifest with `cargo fetch --manifest-path <manifest>`
before rerunning the offline check. Rand's original cache listed only a yanked
compatible `chacha20`; refreshing it made `chacha20 0.10.2` selectable. Do not
patch upstream sources, bypass yanks, or substitute incompatible versions.
