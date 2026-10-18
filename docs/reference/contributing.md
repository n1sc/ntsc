# Contributing to NTSC

This manual is for people working on the compiler itself: building it,
understanding where things live, running the checks, and making a change
end to end. For how the compiler lowers NTSC to native code, see
[Internals](internals.md); for the language and the `ntsc` command, see
the [guide](../guide/getting-started.md) and the [CLI reference](cli.md).

## Prerequisites

- **Stable Rust** (edition 2024). The project must always compile on
  stable; nightly features are never used.
- **LLVM development libraries**, version 22. `ntsc-codegen` uses inkwell
  (`features = ["llvm22-1"]`) and links only the x86-64 and aarch64
  targets. `LLVM_SYS_221_PREFIX` must point at an LLVM 22 prefix
  (e.g. `LLVM_SYS_221_PREFIX=/usr/lib/llvm22` on Arch with the `llvm22`
  package). This must be *LLVM 22 exactly*: a newer system LLVM silently
  links its `libLLVM.so` anyway, and the version-skew corrupts memory
  inside the optimization pass pipeline (SIGSEGV in LLVM-heavy code, in
  and out of tests).
- A **C compiler** on `PATH` (for linking). `link_binary` uses `cc` on
  Unix, `ld.lld`/`link.exe`/`gcc` on Windows.

## Building

The compiler lives in the workspace root:

```console
$ cargo build            # builds all crates and the `ntsc` binary
$ cargo build -p ntsc-runtime   # the static library linked into programs
```

`ntsc` ends up at `target/debug/ntsc`. It locates the runtime archive
next to itself, in the Cargo target directory, or by building it from the
workspace when neither exists — so a plain `cargo build` is enough to
`ntsc run` a project in the workspace.

## Running the checks

Run all three before considering a change complete:

```console
$ cargo fmt
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
$ cargo test --workspace
```

- `cargo fmt` — formatting only; no settings are overridden.
- `cargo clippy` — must end with **zero warnings**. Never silence a
  Clippy warning without a documented reason.
- `cargo test --workspace` — every suite:

| Suite | Where | What it exercises |
| --- | --- | --- |
| Per-crate unit tests | `src/` in each crate | Parser, typeck, IR emission (`emit/tests.rs`), registry invariants |
| Runtime ABI tests | `crates/ntsc-runtime/tests/` | `handle_validity.rs` (kind safety through the public ABI), `memory_safety.rs`, `exception.rs` |
| End-to-end tests | `crates/ntsc-codegen/tests/*_e2e.rs` | Compile an NTSC program to a native binary and run it |

The e2e tests build the runtime automatically when needed. Individual
crates can be tested with `cargo test -p ntsc-<crate>`.

## Repository layout

```
crates/
  ntsc-lexer      tokenizer with automatic semicolon insertion
  ntsc-parser     recursive-descent parser producing the AST
  ntsc-ast        AST types and spans
  ntsc-typeck     name resolution, type checking, ownership, linting
  ntsc-codegen    LLVM IR generation, optimization, linking
  ntsc-runtime    the runtime static library linked into every binary
  ntsc-build      neutron.toml parsing, multi-file module loading
  ntsc-diag       diagnostic rendering and JSON output
  ntsc-cli        the ntsc binary (commands, install/runtime lookup)
  ntsc-cargo      Rust dependency resolution and adaptor generation
  ntsc-lsp        language-server support for editors
docs/            guide + reference
examples/        small Neutron programs (bank, builtins, modules, ...)
```

The pipeline is: `ntsc-lexer` → `ntsc-parser` → `ntsc-typeck` (resolve,
type check, ownership, lints) → `ntsc-codegen` (LLVM IR → object file),
then link against `libntsc_runtime.a`. `ntsc-build` runs before typeck
for projects: it resolves the file-import closure, parses modules in
parallel, merges the ASTs, and records per-statement span provenance so
diagnostics can be attributed to the right file.

### Maintaining documentation

The [documentation overview](../README.md) defines the learning sequence.
Its chapter tables supply the website's sidebar groups, descriptions, and
previous/next order. Add each new page to exactly one table, in prerequisite
order, and keep existing file paths stable when changing its place in the
guide.

Guide chapters should explain their goal, name prerequisites, distinguish
complete programs from snippets, and finish with an exercise and a next step.
Put exhaustive signatures in the reference and compiler implementation details
in this development section.

From the `neutron-rs-web` project, run `pnpm docs:sync` to update the development
snapshot. For a release, use `pnpm docs:sync --version <version> --ref <tag>
--source <path-to-release-docs>` with the docs checked out at that release.
Navigation is generated separately for each snapshot.

## Where things live

**`ntsc-cli/src/main.rs`** is the orchestrator. `cmd_build` shows the
whole driver sequence: `load_project` → `load_program` → `resolve_program`
→ `lint_program` (non-fatal) → `compile_program` → `link_binary`. The
`*_error` helpers convert each stage's failures into diagnostics,
attributing spans to source files through `ModuleLoadResult::localize`
(which rebases merged byte offsets back to file-local coordinates; see
`ntsc-build/src/modules.rs`).

**`ntsc-codegen/src/emit/`** is the emitter, split by concern. Find the
construct you are changing by its submodule:

| Submodule | Contents |
| --- | --- |
| `runtime.rs` | Forward declarations of every `ntsc_*` runtime/stdlib symbol (`declare_runtime_functions`) |
| `typing.rs` | NTSC type → LLVM type mapping and default values |
| `module.rs` | `emit_module`, test-block compilation, top-level emission |
| `function.rs` | User function emission |
| `async_sm.rs` | `async fun` → poll-based state machines |
| `class.rs` | Class layout, implicit init, top-level variables |
| `stmt.rs`, `expr.rs`, `literal.rs`, `binary.rs`, `member.rs` | Statement and expression emission |
| `call.rs` | Argument preparation, ownership transfer, spread, stdlib calls |
| `assign.rs`, `control.rs`, `exception.rs` | Assignment, loops, try/catch/retry, match |
| `helper.rs` | Shared conversions and coercions |
| `array.rs`, `drop.rs` | RC-backed array ops; owned-value drops and option boxes |
| `escape.rs` | Escape analysis (stack allocation, class drops) |
| `lookup.rs` | Class and function metadata lookups |
| `c_main.rs` | The generated `main` wrapper |
| `tests.rs` | IR emission unit tests |

**`ntsc-typeck/src/`** runs in stages: `names.rs` (name resolution,
`resolve_program`), `resolve.rs` (type checking, `check_program`),
`ownership.rs` (moves/views/borrows — the largest file), `warnings.rs`
(`lint_program`), with `scope.rs`, `ty.rs`, `exhaustiveness.rs`, and
`reachability.rs` supporting them.

**`ntsc-runtime/`** is split into `src/lib.rs` (entry/exit, strings,
arrays, shared boxes, exceptions, the async executor) and
`src/registry.rs` (the thread-safe handle registry that backs the whole
ABI), plus `src/modules/` — one file per stdlib module (`strings.rs`,
`net.rs`, `json.rs`, ...). The runtime contains no `unsafe` code.

## Conventions

- **Comments explain *why*, not *what*.** No restating the code;
  document invariants, ownership, and non-obvious behavior. Keep module
  docs to 1–3 lines and section banners (`// ── Name ──`).
- **No `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`** outside
  tests. Create proper error types instead of returning strings.
- **Never ignore a `Result`**; prefer `?`.
- **No new dependencies** unless they add significant value; prefer the
  standard library.
- **Enums over integer flags, pattern matching over casts**, borrowing
  over cloning.
- **Every bug fix ships a regression test.** Do not remove failing tests;
  fix the implementation instead.
- **Keep commits focused** and follow the conventional style of the log
  (`fix(compiler): ...`, `feat(safety): ...`, `chore: ...`). Never mix
  unrelated changes.

## Adding a stdlib module function

A stdlib function is a fixed-ABI `extern "C"` function in the runtime,
forward-declared in codegen and called by name. Three pieces:

1. **Runtime** — in `crates/ntsc-runtime/src/modules/<module>.rs`, write
   `#[unsafe(no_mangle)] pub extern "C" fn ntsc_<module>_<func>(...)`.
   String/array arguments are **borrowed handles** (`i64` keys into the
   registry); returned handles are **owned by the caller**. Use
   `registry::get_string`/`put_string` and friends, `throw_str` for
   failures, and `put_opaque`/`with_opaque` for resources like files or
   channels.
2. **Declaration** — add `declare!("ntsc_<module>_<func>", ...)` to
   `declare_runtime_functions` in `emit/runtime.rs`.
3. **Return type** — add the name to `stdlib_return_ty` in `emit/call.rs`
   (default `Ty::Any`). The stdlib call path handles argument borrowing
   and drops fresh temporaries after the call; functions that need
   element-type or function-type knowledge at compile time (arrays,
   `sort`, `testing`, `random.shuffle`/`weighted`,
   `process.spawn_thread`) are routed through codegen helpers instead —
   see `emit/array.rs` for the pattern.

Test in the runtime crate (`cargo test -p ntsc-runtime`) or with an
end-to-end test in `crates/ntsc-codegen/tests/`.

## Adding a language feature

A feature touches every stage in order:

1. `ntsc-lexer` — new tokens, if the syntax needs any.
2. `ntsc-ast` — new `Stmt`/`Expr` variants and spans.
3. `ntsc-parser` — parse the construct into the AST.
4. `ntsc-typeck` — name resolution, type checking, and ownership rules
   for it. New statements also need a `stmt_byte_range` arm in
   `ntsc-build/src/modules.rs` so diagnostics attribute their spans
   correctly.
5. `ntsc-codegen` — emission in the matching `emit/` submodule; add IR
   assertions to `emit/tests.rs`.
6. Documentation — the language reference and the relevant guide page.

## Debugging aids

- `ntsc --json` — every diagnostic path can emit machine-readable JSON
  instead of rendered text.
- `ntsc graph` — print the file-import dependency graph as DOT.
- IR verification failure — the compiler dumps the offending IR before
  erroring.
- Leak detection — debug builds report registry entries never dropped
  (`NTSC WARNING: Memory leak detected!`), which catches wrong-kind and
  duplicate drops at runtime.
- Scheduler race coverage — the opt-in `.github/workflows/tsan.yml` workflow
  uses nightly only for Rust's unstable ThreadSanitizer flag, builds the
  standard library with it, and repeats the scheduler stress test. The normal
  build and test gate remains on stable Rust.
- The registry's own tests assert `LIVE + PERMANENT == map.len()`, so a
  drop path that silently loses an entry fails the suite.
- `cargo test -p ntsc-codegen -- --nocapture` — shows the IR emitted by
  the `emit/tests.rs` tests.
- Platform-specific code is confined to a few well-marked spots: the
  linker selection in `ntsc-codegen/src/lib.rs`, the runtime archive name
  (`runtime_lib_name`), and the installer layout search in
  `ntsc-cli/src/main.rs`.
