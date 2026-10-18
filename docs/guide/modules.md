# Modules and imports

You can now write functions and [Classes and enums](classes.md). This chapter
shows where library functions come from and how to split your own code into
files without changing how you call it.

A program is made of one or more `.nt` source files. The entry file is named
in `neutron.toml`; every other file is pulled in explicitly.

## The standard library

The standard library is exposed as a set of builtin modules. Bring a module
name into scope with `use`, then call its functions with `module.function`:

```ntsc
use strings
use sort
use fmt

fun main() {
    var n = strings.length("hello")
    var sorted = sort.stable_sort([3, 1, 2])
    say(fmt.i64_to_str(n))
}
```

The available modules are:

```
archive   arrays    async        collections  crypto
csv       encoding  fmt          glob         hash
http      io        json         math         memory
net       os        paths        process      random
regex     slices    sort         strings      sys
term      testing   time         toml         yaml
```

An alias lets you choose a local module name:

```ntsc
use strings
use sort as sorter
```

Use `use testing` in files that call its assertions. The `async` and `chan`
namespaces are always in scope; the concurrency chapter uses them without an
import.

See the [standard library reference](../reference/stdlib.md) for the full
function list.

## Selective imports

Symbols can be imported selectively from a module with
`use (a, b) = from module`. The same form works with file paths.

## Loading other source files

Here is a complete two-file example. In your existing project, make
`src/main.nt` contain:

```ntsc
use "arithmetic.nt"

fun main() {
    say("total: ${add(3, 2)}")
}
```

Create `src/arithmetic.nt` alongside it:

```ntsc
fun add(int a, int b) -> int {
    return a + b
}
```

Run `ntsc run` from the directory containing `neutron.toml`. It prints
`total: 5`. Only the entry file needs `main`; the imported file supplies the
function declaration.

`use` pulls another `.nt` file into the program when its argument is a
quoted path; an identifier after `use` (like `use strings`) refers to a
stdlib module instead. The path is resolved relative to the file that
contains the import, and the `.nt` extension is inferred when omitted:

```ntsc
use "lib"

fun main() {
    say("" + lib_value())
}
```

`lib.nt` may itself import other files:

```ntsc
// lib.nt
use "util.nt"

fun lib_value() -> int {
    return util_value() * 2
}
```

File imports cannot escape the project root: a path resolving outside the
entry file's directory (for example through `../`) is rejected.

The compiler loads the whole closure of files, resolves names across the
merged program, and compiles it as one unit. Declaration order does not
matter: `lib.nt` can call a function defined later in the same file, and
`main.nt` can use classes defined in an imported file.

Modules are loaded in parallel during the build, and the build output prints
the parse duration of each one.

## Dependency graph

`ntsc graph` prints the module dependency graph as DOT:

```sh
ntsc graph
```

Each node is a source file; each edge is a file import.

## Build manifest

The project manifest `neutron.toml` has a `[package]` section:

```toml
[package]
entry = "src/main.nt"
output = "my-project"
```

- `entry`: the entry source file, relative to the project root.
- `output`: the name of the produced binary.
- `target` (optional): an LLVM target triple; when omitted, it defaults to
  the host platform.

`entry` and `output` are required. `ntsc init` generates a correct manifest
automatically. See the
[CLI reference](../reference/cli.md).

Use the generated manifest for these examples; you do not need to choose a
target manually.

## Try it

Move the `Counter` class from the previous chapter into `src/counter.nt`.
Import it from `src/main.nt` with `use "counter.nt"` and run the same program.
The output should be unchanged.

Next: [Error handling](error-handling.md) handles operations that cannot
always succeed, such as reading a file or parsing a number.
