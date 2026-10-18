# Testing

You have practiced the success and failure paths in
[Error handling](error-handling.md). A **test** is a repeatable check, and an
**assertion** describes the answer you expect. In this chapter, you will put
checks beside your functions and run them with one command.

Tests are declared with `test` blocks inside source files. `ntsc test`
discovers them, compiles a harness in place of `main`, and runs each block.

## Writing tests

This is a complete source file. Replace `src/main.nt` with it, then run the
command in the next section from your project directory:

```ntsc
use testing

fun add(int a, int b) -> int {
    return a + b
}

test add_works {
    testing.assert_eq(add(2, 3), 5)
    testing.assert_ne(add(2, 3), 6)
}

test strings_compare {
    testing.assert_eq("hello", "hello")
}

fun main() {
    say("user main must not run in test mode")
}
```

A `test` block is a body of statements; a thrown exception or a failed
assertion fails the test. `fun main` is replaced by the test harness in test
mode, so the user `main` never runs under `ntsc test`.

## Running

```sh
ntsc test
```

Each test prints a result line:

```
PASS add_works
PASS strings_compare
  Summary 2 passed, 0 failed
```

A failing test prints its message:

```
FAIL failing_one: testing.assert_true: expected true, got false
  Summary 1 passed, 1 failed
```

The process exits non-zero when any test fails. Tests run in the order they
are declared.

## Assertions

The `testing` module provides these assertions:

| Function | Passes when |
| --- | --- |
| `testing.assert_true(bool)` | the argument is `true` |
| `testing.assert_false(bool)` | the argument is `false` |
| `testing.assert_eq(a, b)` | `a == b` |
| `testing.assert_ne(a, b)` | `a != b` |

`assert_eq` and `assert_ne` accept `int`, `float`, `string`, and `bool`
operands. On failure they throw an exception whose message names the failing
assertion, so the failure can also be caught and inspected:

```ntsc
try {
    testing.assert_eq(1, 2)
} catch (err) {
    say(err)   // "testing.assert_eq: ..."
}
```

## Release builds

`ntsc test --release` runs the test harness against the optimized build. The
`--json` flag applies to test output as it does to builds.

## Try it

Add a `square(int n) -> int` function and a `test square_works` block that
checks `square(3) == 9` with `testing.assert_eq`. Run `ntsc test`, deliberately
change the expected answer to `8`, and run it again to see a failure. Restore
`9` before continuing.

You have finished the foundations: a project, values, functions, data types,
ownership, imports, errors, and tests. Use the
[standard library reference](../reference/stdlib.md) when building your own
program.

Next: [Shared values](shared-values.md) begins the advanced chapters. Continue
when your program needs several places to retain the same value.
