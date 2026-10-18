<a name="readme-top"></a>

<br />

<div align="center">
  <h1 align="center">NTSC</h1>

  **Neutron Type-Safe Compiler**

  A statically typed, memory-safe systems language that compiles to native
  binaries through LLVM. The Rust-based successor to the Neutron language,
  built around first-class ownership.

</div>

<div align="center">

[![CI][ci-badge]][ci-link]
[![License][license-badge]](LICENSE)
[![Rust][rust-badge]][rust-link]

[Getting started](#getting-started) ·
[Documentation](#documentation) ·
[Usage](#usage) ·
[Contributing](#contributing)

</div>

---

## About the project

NTSC pairs a compact, readable syntax with rules the compiler enforces for you. Values move by default, borrows are declared with `view`, and nothing is garbage-collected: memory is released deterministically when ownership ends.

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

### Features

| Feature | Description |
| --- | --- |
| **Ownership & borrows** | Values move by default; `view` declares a borrow. The checker rejects use-after-move and move-while-viewed at compile time. |
| **`shared T`** | An explicit escape hatch for reference-counted, aliasable values with deterministic release. |
| **Reference-counted arrays** | Deterministic drop, with leak detection in debug builds. |
| **Classes, enums, `match`** | Structured programming with pattern matching and guards. |
| **Generics & traits** | Monomorphized generics with trait bounds, `dyn Trait` vtable dispatch, and associated types. |
| **Built-in tests** | `test` blocks discovered and run by `ntsc test`. |
| **Async & concurrency** | `async fun` coroutines, goroutines, and message-passing channels. |
| **C interop** | `extern "C"` declarations called from `unsafe` blocks, linked through `neutron.toml`. |
| **Native performance** | LLVM backend; release builds run `mem2reg`, `instcombine`, `simplifycfg`, `sccp`, `dce`, and `gvn`, and escape analysis keeps non-escaping objects on the stack. |
| **Static binaries** | Every output embeds the NTSC runtime — nothing to install on the target machine. |

### Built with

* [Rust](https://www.rust-lang.org) — the compiler, runtime, and tooling
* [LLVM](https://llvm.org) — codegen backend, linked through [inkwell](https://github.com/TheDan64/inkwell)

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Documentation

| Guide | Reference |
| --- | --- |
| [Getting started](docs/guide/getting-started.md) | [Language reference](docs/reference/language.md) |
| [Syntax and control flow](docs/guide/syntax-and-control-flow.md) | [Standard library](docs/reference/stdlib.md) |
| [Functions](docs/guide/functions.md) | [CLI reference](docs/reference/cli.md) |
| [Classes and enums](docs/guide/classes.md) | [Internals](docs/reference/internals.md) |
| [Ownership](docs/guide/ownership.md) | [Rust autogen standard](docs/reference/rust-autogen.md) |
| [Shared values](docs/guide/shared-values.md) | [Experimental features](docs/reference/experimental.md) |
| [Arrays and iterators](docs/guide/arrays-and-iterators.md) | |
| [Error handling](docs/guide/error-handling.md) | |
| [Modules](docs/guide/modules.md) | |
| [Concurrency](docs/guide/concurrency.md) | |
| [Testing](docs/guide/testing.md) | |
| [Unsafe and pointers](docs/guide/unsafe-and-pointers.md) | |
| [Conventions](docs/guide/conventions.md) | |
| [Rust adaptors](docs/guide/rust-adaptors.md) | |

The [`examples/`](examples/) directory has small, browsable programs for every feature category (classes, enums, low-level pointers, concurrency, modules, testing, C interop).

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Getting started

### Prerequisites

To build the compiler from source you need:

* **Rust** (stable) — [rustup.rs](https://rustup.rs)
* **LLVM 22** — with `llvm-config` on `PATH`; set `LLVM_SYS_221_PREFIX` if it lives elsewhere
* **A C linker** — `cc`/`clang` on Linux and macOS, MSVC `link.exe` or MinGW `gcc` on Windows

### Installation

Install a prebuilt binary from the [Releases](https://github.com/n1sc/ntsc/releases) page:

| Platform | Artifact | Install |
| --- | --- | --- |
| Debian / Ubuntu | `.deb` | `sudo dpkg -i ntsc_<version>_amd64.deb` |
| Fedora / RHEL | `.rpm` | `sudo dnf install ntsc-<version>-1.x86_64.rpm` |
| Arch Linux | `PKGBUILD` | `makepkg -si` |
| macOS | `.dmg` | Drag `ntsc.app` into `/Applications` |
| Windows | `.msi` | Run the installer; adds `ntsc` to `PATH` |

The compiler links LLVM 22 dynamically, so the Linux packages declare a dependency on the system LLVM runtime (`libLLVM-22` / `llvm-libs`), and the macOS and Windows installers bundle the LLVM libraries they need. See [`workflows/`](workflows/README.md) for the full recipe documentation.

Or build from source:

```console
$ arq install
```

This builds the release compiler (`ntsc`, `ntsc-lsp`, and the runtime archive) and installs it to `/usr/local/bin`. Set `PREFIX` to choose a different destination (e.g. `PREFIX=$HOME/.local/bin arq install`). Run `arq list` to see every available task.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Usage

Create and run a hello-world project:

```console
$ ntsc init hello
$ cd hello
$ cat neutron.toml
[package]
entry = "src/main.nt"
output = "hello"
$ ntsc run               # debug build
$ ntsc run --release     # optimized build
$ ntsc test              # run the test blocks
```

### Command-line interface

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
| `ntsc lsp` | Start the language server (LSP over stdio) |
| `ntsc --version` | Print the compiler version |

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Project layout

```
ntsc/
├── .github/                CI workflows (build/test, release packaging)
├── crates/                 The compiler, split into focused crates
│   ├── ntsc-ast            AST
│   ├── ntsc-lexer          Lexer
│   ├── ntsc-parser         Recursive-descent parser
│   ├── ntsc-typeck         Name resolution, type checking, linting, ownership
│   ├── ntsc-codegen        LLVM IR codegen and linking (inkwell / LLVM 22)
│   ├── ntsc-runtime        Runtime static library
│   ├── ntsc-build          neutron.toml parsing and module loading
│   ├── ntsc-cargo          Rust dependency fetching and generated adaptors
│   ├── ntsc-lsp            Language server
│   ├── ntsc-cli            The `ntsc` binary
│   └── ntsc-diag           Diagnostic rendering
├── benchmarks/             NTSC vs Rust benchmark suite
├── workflows/              Local installer recipes (.deb, .rpm, PKGBUILD, .dmg, .msi)
└── docs/                   User guide and reference
```

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Roadmap

Current milestones for the 0.1 release:

* [ ] Pattern matching on user-defined enum payloads with variable binding
* [ ] Lambda closures that capture outer variables
* [ ] Methods on enums
* [ ] Module privacy (`private`)
* [ ] Sequence combinators (`map`, `filter`, `fold`, `zip`)
* [ ] Derived implementations (`deriving`)
* [ ] `static var` / `static mut` module-level mutable state
* [ ] C-string conversion helpers (`strings.from_c_str` / `to_c_str`)

Post-0.1:

* [ ] Inlining / devirtualization optimization passes
* [ ] `\u{...}` Unicode escapes in string literals
* [ ] `strings.length` counting characters, not bytes

See the [open issues](https://github.com/n1sc/ntsc/issues) for a full list.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## Contributing

Contributions are what make the open-source community such an amazing place to learn, inspire, and create. Any contributions you make are **greatly appreciated**.

If you have a suggestion that would make this better, please fork the repo and create a pull request. You can also simply open an issue with the tag "enhancement".

1. Fork the Project
2. Create your Feature Branch (`git checkout -b feature/AmazingFeature`)
3. Commit your Changes (`git commit -m 'feat: add some AmazingFeature'`)
4. Push to the Branch (`git push origin feature/AmazingFeature`)
5. Open a Pull Request

Before submitting, make sure the checks pass locally:

```console
$ cargo test --workspace
$ cargo clippy --all-targets --all-features -- -D warnings
$ cargo fmt --check
```

<p align="right">(<a href="#readme-top">back to top</a>)</p>

## License

Distributed under the Neutron Permissive License (NPL) 1.1. See [`LICENSE`](LICENSE) for more information.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- MARKDOWN LINKS & IMAGES -->
[ci-badge]: https://img.shields.io/github/actions/workflow/status/n1sc/ntsc/ci.yml?branch=main&label=CI&logo=github
[ci-link]: https://github.com/n1sc/ntsc/actions/workflows/ci.yml
[license-badge]: https://img.shields.io/badge/License-NPL--1.1-blue.svg
[rust-badge]: https://img.shields.io/badge/Rust-stable-orange.svg?logo=rust
[rust-link]: https://www.rust-lang.org
