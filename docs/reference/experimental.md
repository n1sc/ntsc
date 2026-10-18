# Experimental features

Some Neutron capabilities ship in an experimental state. They are off by
default, disabled unless the project opts in, and may change or be removed
in any release.

## How to opt in

Experimental features are enabled per project through the `[experimental]`
table in `neutron.toml`. Each feature is a boolean; unset keys default to
`false`.

```toml
[experimental]
rust_modules = true
```

Building a program that needs a feature you have not enabled fails before
resolution or codegen with an error explaining how to opt in. Duplicates and
non-boolean values are reported as `neutron.toml errors`.

## Enabled features

### rust_modules

Enables Rust crate imports (`use @crate`). Imports resolve to generated
Rust adaptors, and each supported bridge becomes an ordinary call in Neutron
source. Without the toggle, the compiler rejects `use @crate` with a
"disabled by default" error before any compilation happens.

See the [Rust adaptors guide](../guide/rust-adaptors.md) for declaring
dependencies and the [autogen standard](rust-autogen.md) for how the
generated adaptors are maintained.

## Stability

Experimental features:

- are disabled by default and require explicit opt-in;
- may change syntax or behavior at any time;
- may be removed between releases without a migration path;
- may graduate to core features once they stabilize.

Pin your compiler version when you rely on them, and read the release notes
before upgrading.
