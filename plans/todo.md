# TODO — Neutron Language Completeness

The goal is to add the missing language features that would make Neutron a fully expressive, general-purpose programming language on par with modern systems languages.

---

## [x] Generics with monomorphization

- [x] **Generic functions and constraints:** function type parameters, inferred specialization, trait bounds, compile-fail diagnostics, and e2e coverage.
- [x] **Generic data types:** generic classes and enums, nested generic substitutions, constructor/member typing, and specialized LLVM layouts.
- [x] **Generic type model and ergonomics:** aliases, `where` clauses, associated types, bound diagnostics, and an explicit static-by-default fallback policy.

## [x] Traits and interfaces

- [x] Add `trait` keyword for defining shared method contracts: `trait Printable { fun format() -> string; }`.
- [x] Add `impl Trait for Type` blocks for implementing traits on concrete types.
- [x] Add default method implementations in traits.
- [x] Support trait bounds on generic parameters: `fun print_all<T: Printable>(items: array[T])`.
- [x] Add `dyn Trait` for dynamic dispatch via vtable indirection.
- [x] Add `Box<dyn Trait>` for heap-allocated trait objects.
- [x] Support multiple trait implementations per type.
- [x] Support supertraits (trait inheritance): `trait Eq: Printable { ... }`.
- [x] Add `impl Trait` in return position for opaque types.
- [x] Add compile-fail tests for missing trait implementations and signature mismatches.
- [x] Add e2e tests for static trait dispatch and generic bounds.
- [x] Add compile-fail and e2e tests for trait objects, vtables, object safety, and supertraits.

## [x] Result type and `?` operator

- [x] Add `Result<T, E>` as a built-in enum: `Ok(T)` and `Err(E)`.
- [x] Add the `?` operator for ergonomic error propagation in functions returning `Result`.
- [x] Support `From`/`Into` trait conversions for error types (or a simpler `convert` mechanism).
- [x] Integrate with `try`/`catch` — a `throw` inside a `Result`-returning function wraps the value in `Err`.
- [x] Add `unwrap_or`, `map`, `and_then`, `or_else` combinator methods on `Result`.
- [x] Add `Option<T>` combinators: `ok_or`, `ok_or_else` to bridge `Option` and `Result`.
- [x] Add e2e tests: `?` propagation, error conversion, combinator chaining.

## [X] Operator overloading

Make custom types feel built-in:

- Declare an operator like any method — `fun +(view Vec other) -> Vec` inside `class Vec` — then `a + b` just works.
- Covers `+ - * / %`, unary `-` and `!`; comparisons return bool and immediately work with `==` and `sort`.
- Using an operator a type doesn't define gives a plain error naming the missing method.
- Add e2e tests: vector math, class equality, sorting custom types.

## [ ] Enhanced pattern matching

Take data apart in one line instead of manual tag checks:

- `case Ok(n) => ...` binds payloads straight to variables; nested forms like `case [head, rest]` unpack arrays.
- Conditions read naturally: ranges `case 1..10`, alternatives `case "a" | "b"`, guards `case n if n > 0`.
- Forgetting a case prints a friendly warning listing exactly which ones are missing.
- Bound values are usable immediately — the compiler handles view/copy details silently.
- Add e2e tests: result matching, array destructuring, ranges, exhaustive switch.

## [x] Compile-time evaluation

Constants cost nothing at runtime and need no new syntax to learn:

- [x] `static const LIMIT = 4 * 1024` accepts any constant expression, including references to earlier constants.
- [x] Constants work anywhere fixed sizes are needed (array lengths).
- [x] A pure function called from a constant context runs at build time — same code, same spelling.
- [x] Circular references get a clear error showing the cycle.
- [x] Add e2e tests: folded arithmetic, constants as array sizes, build-time function call.

## [ ] Derived implementations

One word deletes boilerplate:

- `class Point { ... } deriving Eq, Format` writes field-wise equality and string conversion for you.
- Data enums get `.name()` and payload accessors free, so trivial checks stay one-liners instead of full matches.
- Deriving is always equivalent to the code you would have written by hand — no surprises.
- Add e2e tests: derived equality/formatting, enum accessors.

## [x] Tuples and multiple return values

Return two values as easily as one:

- `fun bounds() -> (int, int)` and `var (width, height) = bounds()` — declare, return, destructure.
- Grab single pieces with `t.0`, `t.1` when names aren't needed.
- Works with option and generics: `option[(int, int)]`, `array[(int, string)]`.
- Add e2e tests: multi-return, destructuring, tuples in generic contexts.

## [ ] Lambdas that remember state

Lambdas exist today; capturing should be automatic:

- A lambda captures whatever it uses — no annotations for the common case.
- Returning or storing a lambda just works; when a lifetime genuinely conflicts, the error suggests the one-word fix (`copy x`).
- Explicit capture lists stay available for control, never required for correctness.
- A lambda may call itself by name for recursion.
- Add e2e tests: returned counter, stored lambda outliving its frame, recursive lambda.

## [ ] Enums with associated data

- [x] Extend enums to carry data per variant: `enum Shape { Circle(float), Rectangle(float, float) }`.
- [x] Support generic enums: `enum Option<T> { Some(T), None }`.
- [x] Ship `result[Ok, Err]` as the built-in data enum for failures (completed above).
- [ ] Methods on enums: `fun area(view Shape self) -> float`.
- [x] Payload pattern matching for the builtin result enum:
  `case Ok(v)` / `case Err(e)` bind the payload in match arms, with `_`
  skip and guards reading the binder (user-defined enum payloads still
  tracked under Enhanced pattern matching).
- [ ] Add e2e tests: enum method dispatch, generic enum specialization.

## [ ] Module privacy

Opt-in hiding — nothing changes unless you ask:

- Files stay fully shareable as today; mark an item `private` when a module has internals worth hiding.
- Facade modules can re-export in one line: `export use "./collections"`, so users import a single path.
- Touching something private gives a clear error naming the item and suggesting the public way.
- Add e2e tests: private item hidden, re-export facade, helpful private-access error.

## [ ] Sequence combinators

Everyday loops become one readable line:

- `xs.map(f)`, `xs.filter(p)`, `xs.fold(init, f)` replace index bookkeeping; chains compose naturally.
- Chains run in a single pass automatically — pretty code stays fast, no tuning required.
- `zip` pairs two lists; `enumerate` adds positions.
- If a step throws, the error surfaces through ordinary try/catch with no partial results left behind.
- Add e2e tests: chained map/filter/fold, single-pass check, zip/enumerate.

## [x] Struct literals and field initialization shorthand

- [x] Add struct literal syntax: `Point { x: 1, y: 2 }`.
- [x] Add field init shorthand: `Point { x, y }` when variable names match field names.
- [x] Add `..other` syntax for struct update from another instance.
- [x] Add e2e tests: struct literals, shorthand, update syntax.

## [x] Easy concurrency

Concurrent code with the shape of sequential code:

- [x] `async { ... }` starts work inline; `await` (existing) collects it — concurrency without ceremony.
- [x] `wait_any(a, b)` returns whichever finishes first, `wait_all` waits for both; the losers are cleaned up automatically.
- [x] Timeouts are one argument away and arrive as normal catchable errors.
- [x] `for await x in producer` consumes streams of results like any loop.
- Work spawned inside a block never outlives the block — nothing to track or free.
- [x] Add e2e tests: inline async block, for-await, wait_any, wait_all, timeout-catchable-error.

## [ ] Static and global variables

- [x] Add `static const` for true compile-time constants shared across threads.
- [x] Add e2e tests: static initialization, thread safety, mut access.
- [ ] `static var` initializes safely on first use, even under concurrent access — just works, no setup.
- [ ] `static mut` allowed only inside `unsafe`.

## [ ] Calling C libraries

Use any C library by declaring its shape once:

- `extern "C" fun strlen(*const byte) -> int` — then call it like any other function.
- Text converts through `strings.from_c_str` / `strings.to_c_str`; no manual pointer math for strings.
- Libraries are linked via project build metadata, declared once per project.
- C failures surface as ordinary catchable exceptions.
- Add e2e tests: libc calls, struct passing, linking a small fixture.

## [x] Helpful diagnostics

Errors that point at the fix:

- [x] Every error shows the offending source line with a caret and a short explanation.
- [x] Typos get "did you mean?" suggestions automatically (undefined names, struct
  literal fields, `slices.*` functions — anything with a close candidate).
- [x] Common mistakes carry fix-it hints: borrow-return, view stores, array/field
  element writes, view args to owned params, container elements (`copy(...)`).
- [x] Warnings name their lint; the help line shows the exact `quiet [lint] { ... }`
  form, and JSON output includes `"lint"` for editors.
- [x] e2e tests: exact rendered snapshots, suggestions for all three typo sites,
  warning lint/help/JSON round-trip.

## [x] Standard library expansion

Fill the real gaps only — most modules already ship built-in:

- [x] `csv`, `toml`, `yaml` join `json`: config and data files parse in one call.
- [x] A benchmark harness inside existing `test` blocks.
- [x] Add e2e tests for each new module.

## [ ] Performance optimizations

Fast without heroics:

- Stack-allocate what doesn't escape, inline small functions, devirtualize known-class calls — all automatic.
- No flags to tune for typical programs; optimizations are on by default.
- An in-repo benchmark suite guards regressions per language feature.

## [x] Features needed before a package manager

- [x] **Recursive directory walk:** `sys.walk(path)` yields every file/directory under a root as newline-separated paths.
- [x] **Glob pattern matching:** `glob.matches(pattern, path)`, `glob.find(root, pattern)`, `glob.is_match(pattern, path)` — supports `*`, `**`, `?`, and character classes.
- [x] **Archive extraction (tar/zip):** `archive.extract_tar_gz(path, dest)`, `archive.extract_tar(path, dest)`, `archive.extract_zip(path, dest)` — extracts archives to a destination directory. `archive.list_tar` and `archive.list_zip` list entry names.
- [x] **Path manipulation:** `paths.join`, `paths.parent`, `paths.file_name`, `paths.extension`, `paths.with_extension`, `paths.stem`, `paths.absolute`, `paths.relative`, `paths.is_absolute`, `paths.components`, `paths.normalize`.
- [x] **Streaming HTTP downloads:** `http.get_file(url, dest)` downloads to a file; `http.get_range(url, start, len)` fetches a byte range; `http.download_with_progress(url, dest, chunk)` streams in chunks.
- [x] **Concurrent downloads:** `http.concurrent_download(urls, dest, max_parallel)` downloads multiple URLs in parallel.
- [x] **File locking:** `os.file_lock(path)` / `os.file_unlock(path)` — advisory file locks for concurrent access control.
- [x] **Symlink support:** `sys.symlink(target, link)`, `sys.readlink(path)`, `sys.is_symlink(path)`.
- [x] **Hash verification:** `crypto.sha512`, `crypto.sha384`, `crypto.sha224`, `crypto.md5`, `crypto.hmac_sha256`, `crypto.hmac_sha512`, `crypto.verify_sha256`, `crypto.verify_sha512`, `crypto.file_sha256`, `crypto.file_sha512`.

Semver and lockfile logic belong in the package manager binary (a Rust
toolchain component), not in the Neutron runtime. See `pkgman.md`.

---

## Completion criteria

Before marking an item complete:

- Add focused unit, integration, compile-pass, and compile-fail tests as applicable.
- Add regression coverage for exceptional control flow.
- Test debug and optimized release builds where behavior could differ.
- Run `./scripts/check.sh fmt`.
- Run `./scripts/check.sh lint`.
- Run `./scripts/check.sh full`.
- Update the language and runtime reference documentation.

## Design rules

New features must grow Neutron's own design and stay easy to use:

- **Easy first**: the common case needs no new syntax, annotations, or ceremony; power controls exist but stay optional. Every feature should be demonstrable in about five lines.
- Reuse the existing pillars before adding syntax: Own–Move–View for lifetimes, exceptions-first for failures, `quiet` lints for diagnostics policy, builtin types over wrapper types, file paths as modules.
- Name mechanisms after Neutron concepts, not after the language a feature was borrowed from.
- If an existing mechanism already covers part of a task (lambdas, iterator protocol, static const, stdlib modules), extend it instead of introducing a parallel one.
- One capability should have one spelling; avoid shipping two overlapping features for migration's sake.
- Errors teach: a failed compile should say what went wrong and how to fix it in one sentence.
