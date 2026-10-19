# CLI reference

`ntsc` is the compiler command line. It operates on a project rooted at the
current working directory.

## Synopsis

```
ntsc [--release | --debug] [--json] <command>
```

Flags may appear before or after the command.

## Commands

| Command | Description |
| --- | --- |
| `ntsc init [name]` | Scaffold a new project in `./name` (defaults to the current directory name). |
| `ntsc build` | Compile the project to `build/debug/`. |
| `ntsc test` | Compile in test mode and run every `test` block. |
| `ntsc run` | Build, then execute the resulting binary. |
| `ntsc clean` | Remove the `build/` directory. |
| `ntsc clean --cache [--dry-run] [--max-age <days>]` | Prune stale global Rust adaptor caches. |
| `ntsc clean --tmp [--dry-run]` | Remove stale `/tmp/ntsc_*` compiler scratch trees. |
| `ntsc watch` | Rebuild whenever a source file or `neutron.toml` changes. |
| `ntsc graph` | Print the module dependency graph as DOT. |
| `ntsc cargo doc <crate> [--no-open]` | Generate Neutron-facing HTML docs for a Rust adaptor; print a terminal summary and open the page unless `--no-open` is given. Hyphenated package names use underscore namespaces in Neutron. |
| `ntsc cargo coverage [<crate>]` | Report export-manifest coverage for all direct Rust adaptors, or for one named crate. |
| `ntsc cargo …` | Manage Rust crate dependencies; the generated adaptors follow [the autogen standard](rust-autogen.md). |
| `ntsc pkg <command>` | Install, update, remove, list, and search project dependencies. See [Packages](../guide/packages.md). |
| `ntsc version` / `--version` / `-V` | Print detailed version info (version, LLVM revision, host triple, build profile, git commit) and exit. |
| `ntsc --help` / `-h` | Print usage. |

`ntsc init` creates exactly two files:

```
name/
   neutron.toml      entry/output; target defaults to the host platform
  src/main.nt     fun main() { say("Hello, World!") }
```

## Flags

| Flag | Effect |
| --- | --- |
| `--release` | Build with LLVM aggressive optimization into `build/release/`. |
| `--debug` | Build with no optimization into `build/debug/` (the default). |
| `--json` | Emit diagnostics as JSON on stdout. |

## Build output

A successful build prints, for each module, its parse time:

```
  Modules (1):
    src/main.nt (0.1ms)
  load 0.2ms, codegen 3.4ms, link 1.1ms
Build complete (Debug): /path/to/build/debug/hello
```

`ntsc run` additionally prints `Running /path/to/build/debug/hello...`.

## The build manifest

`neutron.toml` is a TOML file with a `[package]` section. `entry` and `output`
are required; `target` is optional and defaults to the host platform:

```toml
[package]
target = "x86_64-unknown-linux-gnu"
entry = "src/main.nt"
output = "hello"
```

- `target`, when present, is an LLVM target triple.
- `entry` is the entry source file, relative to the project root.
- `output` is the output binary name (without extension; the platform
  extension is added automatically).

Missing or duplicate keys are reported as `neutron.toml errors`.

### Dependencies

A `[dependencies]` table maps package names to version requirements. `ntsc
pkg` reads and edits this table; the [Packages](../guide/packages.md) chapter
explains the requirement syntax:

```toml
[dependencies]
slug = "^1.2.0"
```

### Experimental features

An `[experimental]` section opts the project into unstable language
features; the [experimental features page](experimental.md) tracks them in
detail. `rust_modules` enables Rust crate imports (`use @crate`, see the
[Rust adaptors guide](../guide/rust-adaptors.md)); it defaults to `false`:

```toml
[experimental]
rust_modules = true
```

Building a program that uses `use @crate` without the toggle fails before
resolution or codegen with an error explaining how to opt in. Experimental
features may change or be removed in any release.

## Packages

`ntsc pkg` is a separate executable, `ntsc-pkg`, installed beside the
compiler by `arq install` and the release packages. `ntsc pkg` forwards its
arguments to it.

| Command | Description |
| --- | --- |
| `ntsc pkg install` | Install the manifest's dependencies, reusing `neutron.lock` where it still satisfies the requirements. |
| `ntsc pkg install <name>` | Add `<name> = "^<latest>"` to the manifest and install. |
| `ntsc pkg install <name>@<req>` | Add `<name>` with an explicit requirement. |
| `ntsc pkg remove <name>` | Remove the dependency and unvendor it, along with anything only it required. |
| `ntsc pkg update` | Ignore the lockfile and re-resolve to the newest allowed versions. |
| `ntsc pkg list` | Print the packages recorded in `neutron.lock`. |
| `ntsc pkg search <query>` | Search the registry by name, description, and keywords. |
| `ntsc pkg help` | Print the subcommand summary. |

Installed packages are copied into `.ntsc/<name>/` at the project root, with
a generated `.ntsc/.gitignore` containing `*`. `neutron.lock` records each
resolved version, its source repository, and a SHA-256 of the vendored tree.
Source checkouts are cached under `~/.ntsc/packages/<name>@<version>`.

## Diagnostics

Errors are formatted with annotated source snippets, spans, and diagnostic
codes (see the [language reference](language.md)). Render behavior is
controlled by the environment:

- `CLICOLOR_FORCE` (non-empty, not `0`): force color on.
- `NO_COLOR` (non-empty): force color off.
- `CLICOLOR=0`: color off.
- `TERM=dumb`: color off.
- Otherwise color is used only when stderr is a terminal.

The error limit defaults to 20 and can be changed with `NTSC_MAX_ERRORS`
(`0` means unlimited). With `--json`, diagnostics are emitted as structured
JSON on stdout instead; each object carries `severity`, `code`, `message`,
span fields, optional `help`, and — for warnings — the `lint` name that
silences it locally.

Errors aim to point at the fix, not just the problem:

- The offending source line is shown with a caret under the exact span.
- Typos get `did you mean ...?` suggestions automatically when a close
  candidate exists (variable names, struct literal fields, module functions).
- Common ownership mistakes carry fix-it hints showing where `copy(...)`
  or a `view var` declaration belongs.
- Warnings name their lint (e.g. `unused_variable`) and the help line shows
  the exact `quiet [lint] { ... }` form that suppresses it locally.

## Environment variables

| Variable | Meaning |
| --- | --- |
| `LLVM_SYS_231_PREFIX` | Location of an LLVM 23 installation. |
| `NTSC_MAX_ERRORS` | Maximum diagnostics shown per run; `0` is unlimited. |
| `NTSC_REGISTRY` | Package registry index URL; defaults to `https://ntsc.yasakei.dev/v1/registry`. |
| `CLICOLOR_FORCE`, `NO_COLOR`, `CLICOLOR`, `TERM` | Diagnostic color control. |

## The runtime library

The compiler links every binary against the NTSC runtime static library.
`find_runtime_lib` looks in the project's `target/` directory, then in the
workspace `target/debug/`, and builds `ntsc-runtime` with Cargo if it cannot
find it. The archive is named `libntsc_runtime.a` on Unix and MinGW and
`ntsc_runtime.lib` under MSVC.

## Test mode

`ntsc test` builds with the test harness substituted for `main`, then runs the
resulting binary. Each test prints `PASS name` or `FAIL name: message`, ending
with a `Summary N passed, M failed` line. The process exits non-zero if any
test failed. See [Testing](../guide/testing.md).

## Watch mode

`ntsc watch` snapshots the modification times of every file in the module
closure plus `neutron.toml`, rebuilds, and polls every 400 ms. When any file
changes it prints `Change detected, rebuilding...` and rebuilds. New imports
are detected because the snapshot includes the discovery result.
