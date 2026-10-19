# NTSC Documentation

Neutron is a programming language; **NTSC** (the Neutron Type-Safe Compiler)
is the tool that builds and runs it. Source files end in `.nt`, and projects
use a `neutron.toml` manifest. The compiler checks types and ownership before
producing a native executable through LLVM.

**New here? Start with [Getting started](guide/getting-started.md), then follow
the chapters below in order.** You do not need to know Rust, LLVM, or memory
management to read the language guide. Rust and LLVM are needed when building
the compiler itself; the setup chapter explains that step.

## How to use this guide

The first twelve chapters take you from an empty project to a small program
with functions, data, imports, dependencies, error handling, and tests. Each
chapter names what you will learn and finishes with a small exercise and a
next step. Advanced topics come after that foundation.

- Run terminal commands in your project directory unless a step says otherwise.
- A complete example with `fun main()` can replace `src/main.nt`.
- Short statement examples belong inside `main`; function, class, enum, and
  `test` declarations belong outside it. Imports go at the top of the file.
- When a snippet calls a library module such as `arrays` or `fmt`, add
  `use arrays` or `use fmt` at the top of the file. The
  [Modules and imports](guide/modules.md) chapter explains this in detail.
- A comment such as `// 5` shows expected output. Examples marked **compile
  error** demonstrate a mistake deliberately; do not add them to a working
  program.
- Use the reference pages when you need an exact signature or rule. They are
  lookup material, rather than the next chapter of the tutorial.

## Start here

Get the tools working, then understand the program you just ran.

| Chapter | What you will learn |
| --- | --- |
| [Getting started](guide/getting-started.md) | Set up the compiler, create a project, and run Hello, World. |
| [Your first program](guide/your-first-program.md) | Read a complete program, change its output, and use a variable. |

## Language basics

Learn how to express calculations, decisions, repeated work, and small pieces
of reusable code.

| Chapter | What you will learn |
| --- | --- |
| [Syntax and control flow](guide/syntax-and-control-flow.md) | Name values, use basic types, and make decisions and loops. |
| [Functions](guide/functions.md) | Reuse code with parameters and return values, then meet lambdas. |
| [Arrays and iterators](guide/arrays-and-iterators.md) | Store a list of values, index it, and process it with a loop. |
| [Tuples](guide/tuples.md) | Group a few related values and return more than one result. |

## Building programs

Now that you can work with values and functions, learn how those values stay
alive and how to organize, recover from errors in, and test a larger program.

| Chapter | What you will learn |
| --- | --- |
| [Ownership](guide/ownership.md) | Keep one owner, transfer values with moves, and borrow with views. |
| [Classes and enums](guide/classes.md) | Model your own data with fields, methods, constructors, and named cases. |
| [Modules and imports](guide/modules.md) | Use the standard library and split a project into source files. |
| [Packages](guide/packages.md) | Install registry dependencies, pin versions with a lockfile, and publish your own module. |
| [Error handling](guide/error-handling.md) | Recover with try/catch, return results, and clean up after failures. |
| [Testing](guide/testing.md) | Write assertions and run repeatable checks with ntsc test. |

## Advanced topics

You can already write and test small programs. Continue in this order when
you need shared data, concurrent work, external resources, or interop.
These chapters assume the foundations above.

| Chapter | What you will learn |
| --- | --- |
| [Shared values](guide/shared-values.md) | Share a value explicitly when moves and short-lived views are not enough. |
| [Concurrency](guide/concurrency.md) | Coordinate async functions and goroutines with typed channels. |
| [Leak-free conventions](guide/conventions.md) | Review ownership, close resources, and check cleanup across error paths. |
| [Unsafe and raw pointers](guide/unsafe-and-pointers.md) | Understand raw-pointer boundaries and call external C functions. |
| [Rust adaptors](guide/rust-adaptors.md) | Add a Rust dependency and inspect its generated Neutron API. |

## Reference

Keep these pages nearby while coding. Look up the part you need; there is no
required reading order.

| Document | Contents |
| --- | --- |
| [Language reference](reference/language.md) | Look up types, statements, expressions, and grammar rules. |
| [Standard library](reference/stdlib.md) | Find a module, function signature, and its behavior. |
| [CLI reference](reference/cli.md) | Look up commands, manifest settings, flags, and environment variables. |
| [Experimental features](reference/experimental.md) | Review the `[experimental]` manifest table and what each unstable feature does. |

## Compiler development

Read these when working on NTSC itself or investigating how it is implemented.

| Document | What it covers |
| --- | --- |
| [Contributing](reference/contributing.md) | Build the compiler, find the relevant crate, and run the checks. |
| [Internals](reference/internals.md) | Understand the compilation pipeline, ABI, and memory model. |
| [Rust autogen standard](reference/rust-autogen.md) | Maintain the contract for generated Rust adaptors and export manifests. |

The chapter tables in this README also define the website's navigation and
reading order. When adding or moving a page, update its entry here. Keep
release snapshots separate so each version retains its own guide.

## Feature summary

The language currently provides:

- Primitive, array, option, object, and class types with prefix type
  annotations.
- Ownership with move semantics, block-scoped views (`view`, `view mut`),
  explicit deep copies (`copy`), and a `shared T` alias escape hatch.
- Escape analysis that stack-allocates non-escaping class instances.
- Classes with constructors and methods, single inheritance, enums,
  operator overloading, and `match` with guards and destructuring patterns.
- Lambdas and a first-class function type.
- Tuples for multiple return values and heterogeneous grouping.
- Compile-time constant evaluation (`static const`) with pure function
  folding.
- Exceptions (`throw`/`try`/`catch`/`finally`), `retry`, and unsafe blocks.
- The iterator protocol, letting any class with `length()` and `get(i)`
  participate in `for-in`.
- Threads, message-passing channels, and `async`/`await` coroutines lowered
  to state machines.
- A standard library of over twenty modules, from `strings` and `arrays` to
  `net`, `json`, `crypto`, `glob`, `paths`, and `archive`.
- First-class registry packages such as `term` and `yaml`, imported with bare
  module names after installation.
- Filesystem operations: recursive directory walk, symlinks, file locking.
- HTTP: range downloads, file downloads with progress, concurrent downloads.
- Crypto: SHA-256/384/224/512, MD5, HMAC, file hashing, hash verification.
- A test runner driven by `test` blocks.
- A package manager with lockfile-pinned, vendored dependencies resolved
  from a community registry.
- Debug-mode leak detection and a release-mode LLVM optimization pipeline.
- Cross-platform support for Linux, macOS, and Windows.
