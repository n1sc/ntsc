# Error handling

After [Modules and imports](modules.md), your program can call library code
and your own helpers. Some operations can fail: an input may not be a number,
or a file may not exist. Here you will recover with `try`/`catch`, return
expected failures as results, and use `finally` for external-resource cleanup.

NTSC uses exceptions. Any value can be thrown; a matching `catch` recovers
control. Uncaught exceptions exit the program with code 1 and print
`uncaught exception: <message>` to stderr.

## A complete example

```ntsc
use fmt

fun main() {
    try {
        var int number = fmt.to_int("oops")
        say("number: ${number}")
    } catch (err) {
        say("Could not parse the input: " + err)
    }
    say("The program can continue.")
}
```

`fmt.to_int` tries to convert text to an integer. `"oops"` is not an integer,
so it throws. Execution jumps into `catch`, where `err` contains the error
message. Change the input to `"42"` to see the successful path.


## Throw

`throw expr` raises an exception:

```ntsc
throw "something went wrong"
```

## Try / catch / finally

`try` wraps a block. `catch (name)` binds the thrown value to a variable.
`finally` runs whether or not an exception was thrown:

```ntsc
var int a = 3037000500
var int b = 3037000500
try {
    var n = fmt.to_int("oops")
    say("unreached")
} catch (err) {
    say("caught: " + err)
}
```

Both `catch` and `finally` are optional, but a `try` with neither is unusual.
`catch` and `finally` blocks see variables declared in the enclosing scope.

## Retry

`retry <count> <body>` attempts a block up to `count` times, with an optional
`catch` after it:

```ntsc
retry 3 {
    network_call()
} catch (err) {
    say("gave up: " + err)
}
```

`retry` runs the block up to `count` times. If a run throws, the attempt is
counted and the block runs again while attempts remain; once exhausted, control
passes to the `catch` handler, or the exception is re-thrown outward when there
is no `catch`.

## Results and `?`

For expected failures, return a `result[Ok, Err]` instead of throwing. The
global constructors `Ok(value)` and `Err(value)` build one, and postfix `?`
propagates an error to the caller:

```ntsc
fun parse_num(bool good) -> result[int, string] {
    if (good) {
        return Ok(42)
    }
    return Err("not a number")
}

fun caller() -> result[int, string] {
    var n = parse_num(true)?
    return Ok(n)      // skipped when parse_num failed
}
```

`expr?` unwraps an `Ok` payload, and on an `Err` immediately returns that
error from the enclosing function. When the function reports errors as
`string`, non-string payloads are converted automatically (`Err(7)?`
propagates `"7"`).

A `throw` inside a result-returning function becomes `Err(message)` at the
function boundary instead of escaping to `try`/`catch`.

Combinators keep chains flat: `unwrap_or(default)`, `map(f)`,
`and_then(f)`, `or_else(f)` work on results; `ok_or(default)` and
`ok_or_else(f)` turn an option into a result:

```ntsc
say(fmt.i64_to_str(Ok(5).unwrap_or(0)))   // 5
var m = Ok(21).map(fun(int x) -> int { return x * 2 })
say(fmt.i64_to_str(m.unwrap_or(0)))       // 42
```

## Numeric and indexing faults

Integer arithmetic and indexing throw ordinary catchable exceptions when an
operation cannot produce a correct answer, so a fault is handled with the same
`try`/`catch` as anything else:

```ntsc
try {
    var x = a * b
    say("" + x)
} catch (err) {
    say("too big: " + err)     // "integer multiplication overflow"
}
```

What throws: `+`, `-`, `*` (including `x++`, `x--`, and `-x`) when the result
does not fit in a 64-bit signed integer; `/` and `%` on a zero divisor or the
one overflowing division; `<<` and `>>` with a shift amount below 0 or above 63;
and an array index that is negative or past the end, on writes as well as reads.

This behavior is the same in debug and release builds. NTSC does not wrap on
overflow in one build mode and fault in the other, so a program cannot compute a
different answer once it is optimized.

Float arithmetic follows IEEE-754 and never throws — an out-of-range result is
infinity and an undefined one is NaN.

## Unsafe blocks

`unsafe { ... }` gates raw-pointer operations at type-check time. Inside an
`unsafe` block, raw pointer dereferences and `memory.raw_address(...)` are
permitted. The block itself is a plain block at runtime — it does not change
error-handling behavior. See [Unsafe and raw pointers](unsafe-and-pointers.md)
for details.

## Standard library error convention

Standard library functions report failures by throwing an exception whose
message identifies the failing call: `module.func: detail`. This lets callers
distinguish error kinds with `try`/`catch` instead of inspecting return
values.

```ntsc
try {
    var r = sys.read("/nonexistent")
} catch (err) {
    say(err)   // "sys.read: ..."
}
```

Functions that follow this convention include `fmt.to_int`, `fmt.to_float`,
`sys.read`, `sys.write`, `json.parse`, `regex.find`, `crypto.base64_decode`,
`crypto.hex_decode`, `math.sqrt` (negative input), `process.spawn`,
`http.get`, `io.open`, `net.tcp_connect`, `random.weighted` (empty weights),
and `random.int` (inverted bounds).

Functions that previously returned a default or empty value on failure now
throw instead, so a successful call can be trusted to have produced a real
result.

## Lint suppression

`quiet [name, ...] body` suppresses lint warnings inside a block. For example,
the unused-variable lint:

```ntsc
quiet [unused_variable] {
    var unused = compute()
}
```

With an empty list, `quiet { ... }` suppresses all suppresable lints in the
block.

## Try it

Run the complete example once with `"oops"` and once with `"42"`. Add a
`finally` block that prints `"Parse attempt finished."` and check that it
runs on both paths. Keep the final message after the entire `try` statement.

Next: [Testing](testing.md) turns those manual checks into repeatable tests.
