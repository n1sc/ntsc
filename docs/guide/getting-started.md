# Getting started

By the end of this chapter, you will have a Neutron project that prints
`Hello, World!`. You need a terminal and a text editor.

If `ntsc --version` already works on your machine, jump to
[Your first project](#your-first-project). Otherwise, follow the setup steps
below. The project is currently pre-release; these instructions build the
compiler from source.

## Requirements

NTSC requires:

- Linux, macOS, or Windows.
- The LLVM 22 toolchain (specifically the libraries used by the `llvm-sys`
  crate). On Linux and Windows the LLVM shared libraries must be discoverable;
  on macOS they are linked into the binary at compile time.
- A C toolchain and linker (gcc or clang on Linux, clang and the Xcode command
  line tools on macOS, or the MSVC toolchain on Windows).
- A current stable Rust toolchain (edition 2024 support), if building from
  source.

### Finding LLVM

The compiler locates LLVM through the `LLVM_SYS_221_PREFIX` environment
variable, which must point at an LLVM 22 installation. For example:

```sh
export LLVM_SYS_221_PREFIX=/usr/local/opt/llvm@22
```

Without this variable, the build looks in the usual system locations. On
Windows, either set `LLVM_SYS_221_PREFIX` to the LLVM install directory or add
`llvm-config` to the `PATH`.

## Building from source

Clone the repository, enter its directory, and build the compiler:

```sh
git clone https://github.com/n1sc/ntsc.git
cd ntsc
cargo build --release
```

This produces the `ntsc` binary under `target/release/`. The runtime library is
built as part of the workspace and linked into every generated binary, so no
separate runtime build step is needed. Keep the runtime archive alongside the
compiler when installing or moving the build.

For this terminal session on Linux or macOS, add the build directory to your
command search path while still in the repository root:

```sh
export PATH="$PWD/target/release:$PATH"
```

In Windows PowerShell, the equivalent is:

```powershell
$env:Path = "$PWD\target\release;$env:Path"
```

This lets you call `ntsc` from a project directory. A new terminal session
needs the path set again, or a permanent `PATH` entry for that directory.

Verify the install:

```sh
ntsc --version
```

It prints a version banner similar to this (the build details may differ):

```
ntsc 0.1.0 — Neutron Type-Safe Compiler

LLVM   22.1.8
Host   x86_64-unknown-linux-gnu
Build  debug
Commit 33fe798
```

The `LLVM` line reports the linked LLVM revision, `Host` the target triple,
`Build` the profile of the `ntsc` binary itself, and `Commit` the git commit
it was built from (omitted for released binaries).

NTSC versions follow Semantic Versioning (`MAJOR.MINOR.PATCH`). The current
release line is `0.1.x`; breaking changes to the language bump the minor
version while the project is pre-1.0.

## Your first project

Choose a directory where you keep your projects. Run these commands there;
`cd hello` changes into the project you just created:

```sh
ntsc init hello
cd hello
ntsc run
```

`ntsc init` scaffolds a project with exactly two files:

```
hello/
  neutron.toml
  src/main.nt
```

`src/main.nt` contains a minimal program:

```ntsc
fun main() {
    say("Hello, World!")
}
```

`ntsc run` compiles the project and executes the resulting binary, printing
`Hello, World!`.

If that message appears, setup is complete. Your day-to-day work happens in
this `hello` directory, not in the compiler's source repository.

## The project layout

```
project/
  neutron.toml      Manifest: target triple, entry point, output name
  src/            Source files (.nt)
  build/          Generated output; debug/ and release/ subdirectories
```

Source files are plain UTF-8 text with the `.nt` extension. The entry file is
declared in `neutron.toml`; additional files are pulled in with `use` (see
[Modules and imports](modules.md)).

## Build, test, run, watch

| Command | Effect |
| --- | --- |
| `ntsc build` | Compile the project to `build/debug/`. |
| `ntsc build --release` | Compile with LLVM aggressive optimization to `build/release/`. |
| `ntsc test` | Compile and run every `test` block in the project. |
| `ntsc run` | Build and execute immediately. |
| `ntsc clean` | Remove the `build/` directory. |
| `ntsc watch` | Rebuild whenever a source file changes. |

Start with `ntsc run`. Add `ntsc test` after the
[Testing](testing.md) chapter. Cache management and the full flag list are
covered in the [CLI reference](../reference/cli.md).

Common flags:

- `--release` / `--debug`: select the build mode. Debug is the default and uses
  no optimization.
- `--json`: emit diagnostics as JSON on stdout instead of formatted text.

## Debug versus release

Debug builds:

- Apply no LLVM optimization.
- Enable leak detection, so heap values that were never released are reported
  on exit.
- Give the runtime its own `backtrace()` function for dumping the call stack.

Release builds:

- Apply LLVM's aggressive optimization pipeline.
- Skip leak detection.
- Remove runtime debugging helpers.

See [Internals](../reference/internals.md) for details of what each mode changes.
For coding patterns that keep debug builds leak-free, read the [Leak-free
conventions](conventions.md).

## If a step fails

- **`ntsc` is not found:** check the `PATH` step above and keep the
  `target/release` directory where it was built.
- **LLVM cannot be found during the compiler build:** check that
  `LLVM_SYS_221_PREFIX` points to the LLVM 22 installation, not to its `bin`
  directory.
- **The linker or runtime library cannot be found:** check that the C toolchain
  is installed and that the runtime archive stayed alongside the compiler.
- **No manifest is found:** run `ntsc run` inside `hello`, where
  `neutron.toml` lives.

## Try it

Open `src/main.nt`, change `Hello, World!` to your own greeting, save, and run
`ntsc run` again. You have completed your first edit-build-run cycle.

Next: [Your first program](your-first-program.md) explains each part of the
file and introduces your first variable.
