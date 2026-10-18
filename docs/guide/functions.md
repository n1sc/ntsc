# Functions

After [Syntax and control flow](syntax-and-control-flow.md), you can write
statements and loops. A **function** gives a group of statements a name so
you can call it more than once. In this chapter, you will pass inputs as
parameters and send an answer back with `return`.

## A complete example

Replace `src/main.nt` with this program and run `ntsc run`:

```ntsc
fun add(int a, int b) -> int {
    return a + b
}

fun main() {
    var int total = add(3, 2)
    say("total: ${total}")
}
```

The output is `total: 5`. `a` and `b` are the function's **parameters**;
`3` and `2` are the **arguments** supplied by this call. `-> int` tells the
compiler what kind of result the function returns.

## Declarations

A function is declared with `fun`, a parameter list, an optional return type,
and a body. The return type follows an arrow; when omitted it is `void`.

```ntsc
fun add(int a, int b) -> int {
    return a + b
}

fun greet() {
    say("hello")
}
```

Parameters are typed inline. A function that returns nothing simply omits the
arrow and return type; its implicit return type is `void`:

```ntsc
fun log(string msg) {
    say(msg)
}
```

## Main entry point

`main` is the entry point. It may omit a return type or return `int` (used as
the process exit code):

```ntsc
fun main() {
    say("Hello, World!")
}
```

```ntsc
fun main() -> int {
    return 0
}
```

In test mode the user `main` is replaced by the test harness, so a project with
`test` blocks can still define `main`.

## Forward references

Functions may call other functions declared later in the program, or in
another module loaded through `use`:

```ntsc
fun main() {
    say("" + f())
}

fun f() -> int {
    return 42
}
```

## The `return` statement

`return` exits the function early. The return type is checked against the
declared type at compile time. Class instances, arrays, and strings are moved
out of the function rather than copied.

## Expression-bodied functions and lambdas

A function or lambda whose body is a single expression can be written with an
arrow, returning the expression's value:

```ntsc
fun square(int n) -> int => n * n

var add = fun(int a, int b) -> int => a + b
```

## Function types and lambdas

A lambda is an anonymous function:

```ntsc
var square = fun(int n) -> int {
    return n * n
}

say("" + square(3))
```

Lambdas are first-class values. A lambda stored in a variable can be assigned
to another variable and called through either handle:

```ntsc
var add = fun(int a, int b) -> int { return a + b }
var combine = add
say("" + combine(2, 3))   // 5
```

Lambdas are used by higher-order standard library functions such as
`sort.sort_by`:

```ntsc
var desc = sort.sort_by(nums, fun(int a, int b) -> bool {
    return a > b
})
```

A lambda passed to `process.spawn_thread` becomes the body of a new thread;
see [Concurrency](concurrency.md).

## Type checking

Every call site is checked against the parameter types and return type at
compile time. Returning the wrong type, passing the wrong argument count, or
calling an undefined name is a compile error. Undefined-name errors carry the
`NTSC-E0101` diagnostic code and may include a "did you mean" suggestion.

## Views and moves across calls

This is a preview of [Ownership](ownership.md). You can continue to arrays
now and return to this section after learning moves and views.

Parameters are passed by move for owned heap values, and by borrow for `view`
parameters:

```ntsc
fun total(array[int] xs) -> int {
    var s = 0
    for (var x in xs) { s = s + x }
    return s
}

fun read(view array[int] xs) {
    say("" + xs[0])
}
```

`total` takes ownership of the caller's array; `read` borrows it and the
caller keeps ownership. See [Ownership](ownership.md).

## Try it

Add `fun square(int n) -> int` to the complete example. Call it twice from
`main`, with different arguments, and print the two results. The function
declaration belongs outside `main`; the calls belong inside it.

Next: [Arrays and iterators](arrays-and-iterators.md) gives you a list of
values to process with your functions.
