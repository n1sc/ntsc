<div align="center">

# NTSC

**Neutron Type-Safe Compiler**

A statically typed, memory-safe systems language that compiles to native binaries through LLVM. The Rust-based successor to the Neutron language, built around first-class ownership.

[Getting started](docs/guide/getting-started.md) · [Language reference](docs/reference/language.md) · [Standard library](docs/reference/stdlib.md)

</div>

---

## At a glance

```ntsc
fun main() {
    var array[int] values = [1, 2, 3, 4]
    var int total = 0
    for (var v in values) {
        total = total + v * v
    }
    say("sum of squares: ${total}")
}
```

```console
$ ntsc run
sum of squares: 30
```

NTSC pairs a compact, readable syntax with rules the compiler enforces for you. Values move by default, borrows are declared with `view`, and nothing is garbage-collected: memory is released deterministically when ownership ends.

## Features

| Feature | Description |
| --- | --- |
| **Ownership & borrows** | Values move by default; `view` declares a borrow. The checker rejects use-after-move and move-while-viewed at compile time. |
| **`shared T`** | An explicit escape hatch for reference-counted, aliasable values with deterministic release. |
| **Reference-counted arrays** | Deterministic drop, with leak detection in debug builds. |
| **Classes, enums, `match`** | Structured programming with pattern matching. |
| **Built-in tests** | `test` blocks discovered and run by `ntsc test`. |
| **Async & concurrency** | `async fun` coroutines alongside threads and message-passing channels. |
| **Native performance** | LLVM backend; release builds run `mem2reg`, `instcombine`, `simplifycfg`, `sccp`, `dce`, and `gvn`, and escape analysis keeps non-escaping objects on the stack. |
| **Static binaries** | Every output embeds the NTSC runtime — nothing to install on the target machine. |

## Installation

Prebuilt installers are attached to each [GitHub Release](https://github.com/yasakei/neutron-rs/releases):

| Platform | Artifact | Install |
| --- | --- | --- |
| Debian / Ubuntu | `.deb` | `sudo dpkg -i ntsc_<version>_amd64.deb` |
| Fedora / RHEL | `.rpm` | `sudo dnf install ntsc-<version>-1.x86_64.rpm` |
| Arch Linux | `PKGBUILD` | `makepkg -si` |
| macOS | `.dmg` | Drag `ntsc.app` into `/Applications` |
| Windows | `.msi` | Run the installer; adds `ntsc` to `PATH` |

The compiler links LLVM 22 dynamically, so the Linux packages declare a
dependency on the system LLVM runtime (`libLLVM-22` / `llvm-libs`), and the
macOS and Windows installers bundle the LLVM libraries they need. See
[`workflows/`](workflows/README.md) for the full recipe documentation.

## Getting started

### Prerequisites

- **Rust** (stable) — [rustup.rs](https://rustup.rs)
- **LLVM 22** — with `llvm-config` on `PATH`; set `LLVM_SYS_221_PREFIX` if it lives elsewhere
- **A C linker** — `cc`/`clang` on Linux and macOS, MSVC `link.exe` or MinGW `gcc` on Windows

### Build the compiler

```console
$ arq install
```

This builds the release compiler (`ntsc`, `ntsc-lsp`, and the runtime
archive) and installs it to `/usr/local/bin` for local testing. Set
`PREFIX` to choose a different destination (e.g.
`PREFIX=$HOME/.local/bin arq install`). If the installer prints a PATH
hint, add that directory to your shell's PATH. Run `arq list` to see
every available task.

### Create and run a project

```console
$ ntsc init hello
$ cd hello
$ ntsc run               # debug build
$ ntsc run --release     # optimized build
$ ntsc test              # run the unit tests
```

`ntsc init` scaffolds a project with exactly two files: a `neutron.toml` manifest and `src/main.nt`.

```console
$ cat neutron.toml
[package]
entry = "src/main.nt"
output = "hello"
```

## Command-line interface

| Command | Description |
| --- | --- |
| `ntsc init [name]` | Scaffold a new project |
| `ntsc build [--release]` | Compile to `build/debug/` or `build/release/` |
| `ntsc test [--release]` | Discover and run `test` blocks |
| `ntsc run [--release]` | Build and execute |
| `ntsc clean` | Remove the `build/` directory |
| `ntsc clean --cache [--dry-run] [--max-age <days>]` | Prune stale global Rust adaptor caches |
| `ntsc clean --tmp [--dry-run]` | Remove stale `/tmp/ntsc_*` compiler scratch trees |
| `ntsc watch [--release]` | Rebuild when sources change |
| `ntsc pkg [args]` | Run the package manager |
| `ntsc graph` | Print the module dependency graph as DOT |
| `ntsc --version` | Print the compiler version |

## Documentation

| Guide | Reference |
| --- | --- |
| [Getting started](docs/guide/getting-started.md) | [Language reference](docs/reference/language.md) |
| [Syntax and control flow](docs/guide/syntax-and-control-flow.md) | [Standard library](docs/reference/stdlib.md) |
| [Functions](docs/guide/functions.md) | [CLI reference](docs/reference/cli.md) |
| [Classes and enums](docs/guide/classes.md) | [Internals](docs/reference/internals.md) |
| [Ownership](docs/guide/ownership.md) | |
| [Shared values](docs/guide/shared-values.md) | |
| [Arrays and iterators](docs/guide/arrays-and-iterators.md) | |
| [Error handling](docs/guide/error-handling.md) | |
| [Modules](docs/guide/modules.md) | |
| [Concurrency](docs/guide/concurrency.md) | |
| [Testing](docs/guide/testing.md) | |
| [Rust adaptors](docs/guide/rust-adaptors.md) | [Rust autogen standard](docs/reference/rust-autogen.md) |

## Project layout

```
rewrite/
├── .github/                CI workflows (build/test, release packaging)
├── crates/                 The compiler, split into focused crates
│   ├── ntsc-ast            AST
│   ├── ntsc-lexer          Lexer
│   ├── ntsc-parser         Recursive-descent parser
│   ├── ntsc-typeck         Name resolution, type checking, linting, ownership
│   ├── ntsc-codegen        LLVM IR codegen and linking (inkwell / LLVM 22)
│   ├── ntsc-runtime        Runtime static library
│   ├── ntsc-build          neutron.toml parsing and module loading
│   ├── ntsc-cli            The `ntsc` binary
│   └── ntsc-diag           Diagnostic rendering
├── benchmarks/             NTSC vs Rust benchmark suite
├── workflows/              Local installer recipes (.deb, .rpm, PKGBUILD, .dmg, .msi)
└── docs/                   User guide and reference
```

## Development

```console
$ cargo test --workspace
$ cargo clippy --all-targets --all-features -- -D warnings
$ cargo fmt --check
```

## Versioning

NTSC versions follow Semantic Versioning (`MAJOR.MINOR.PATCH`). The project remains at `0.0.0` until its first public release.

## License

[Neutron Permissive License (NPL) 1.1](LICENSE)
