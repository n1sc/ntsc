# Syntax and control flow

Build on [Your first program](your-first-program.md): give values types,
choose between branches, and repeat work with loops. Start with variables and
the everyday operators, then work through control flow. The syntax details and
compile-time constants at the end are useful to revisit later.

## Variables

Declarations use `var`. A type annotation may precede the name; it is required
only where the initializer is ambiguous (the empty array literal, for
example).

The basic types you will use most often are:

| Type | Kind of value | Example |
| --- | --- | --- |
| `int` | Whole number | `42` |
| `float` | Number with a fractional part | `0.5` |
| `bool` | True or false | `true` |
| `string` | Text | `"Ada"` |

The `array[int]` and `option[int]` declarations below are a preview: an array
holds a list of values, while an option can hold an integer or no value (`nil`).

```ntsc
var count = 42
var int total = 0
var float ratio = 0.5
var bool ok = true
var string name = "ada"
var array[int] xs = [1, 2, 3]
var option[int] maybe = nil
```

Local variables need an initializer so their value and type are known before
use. Class fields can have default values; you will learn those rules in
[Classes and enums](classes.md).

## Operators

An operator combines or compares values: `+` adds, `==` tests equality, and
`&&` means both conditions must be true. Use parentheses when a calculation's
order is not obvious. The following table is for lookup; you do not need to
memorize it. Precedence runs from highest to lowest:

| Precedence | Operators |
| --- | --- |
| call | `f(x)` `a.b` `a[i]` `a?.b` `x++` `x--` |
| unary | `-x` `!x` `~x` `...x` |
| multiplicative | `*` `/` `%` |
| additive | `+` `-` |
| shift | `<<` `>>` |
| relational | `<` `<=` `>` `>=` |
| equality | `==` `!=` |
| bitwise and | `&` |
| bitwise xor | `^` |
| bitwise or | `\|` |
| logical and | `&&` |
| logical or | `\|\|` |
| ternary | `cond ? then : else` |
| assignment | `=` |

### Arithmetic

`+`, `-`, `*`, `/` and `%` operate on `int` and `float` values. `+` also
concatenates strings and arrays. The postfix `++` and `--` operators increment
or decrement a variable in place and are legal on assignment targets only:

```ntsc
var n = 1
n++
say("" + n)   // 2
```

Classes can overload these operators — see [Classes](classes.md#operator-overloading).

### Comparison and logic

`==`, `!=`, `<`, `<=`, `>`, `>=` compare scalar values. String equality compares
text contents; other heap values use identity unless a class defines its own
comparison operator. `!` negates a boolean. The words `and` and `or` are
not operators; use the symbols.

`&&` and `||` short-circuit: the right operand is evaluated only when the left
one does not already decide the result, so a bounds-checked read guarded by
`i < n && items[i] != ""` cannot read past the end.

Classes can overload comparison operators — see [Classes](classes.md#operator-overloading).

### Bitwise

`&`, `|`, `^`, `<<`, `>>` operate on `int`. `~` is the bitwise complement.

### Member access

`.` reads or assigns a field or method. `?.` is the optional chain: it
evaluates to `nil` (instead of throwing) when the receiver is `nil`.

### Ternary

```ntsc
var max = a > b ? a : b
```

## Control flow

### If / elif / else

```ntsc
var score = 85
if (score >= 90) {
    say("A")
} elif (score >= 80) {
    say("B")
} else {
    say("C")
}
```

### While and do-while

```ntsc
var i = 0
while (i < 10) {
    i = i + 1
}

var j = 0
do {
    j = j + 1
} while (j < 5)
```

### For

The C-style `for` accepts an optional initializer, condition, and step:

```ntsc
for (var i = 0; i < 5; i = i + 1) {
    say("" + i)
}
```

### For-in

`for-in` iterates a sequence using the iterator protocol. The loop variable
type is inferred from the element type:

```ntsc
var xs = [1, 2, 3]
for (var x in xs) {
    say("" + x)
}
```

See [Arrays and iterators](arrays-and-iterators.md) for the full protocol,
including how custom classes participate.

### Break and continue

`break` exits the innermost loop; `continue` starts its next iteration.

### Match

`match` dispatches on a value against a sequence of `case` arms, with an
optional `default`:

```ntsc
var direction = "north"
match (direction) {
    case "north" => say("up")
    case "south" => say("down")
    default => say("unknown")
}
```

Cases support patterns: literals, a wildcard `_`, variable bindings, and
destructuring patterns for arrays and objects:

```ntsc
var value = 2
match (value) {
    case 0 => say("zero")
    case 1 => say("one")
    case _ => say("many")
}
```

A `case` with a guard places `if <condition>` between the pattern and the
arrow. The first matching case wins; the match value is consumed once.

See [Classes and enums](classes.md) for matching on enum values.

## Tuples

Tuples group multiple values into a single value. Each element can have a
different type:

```ntsc
var t = (10, "hello", true)
```

You will learn indexing, destructuring, and multiple return values in the
[Tuples](tuples.md) chapter, after functions and arrays.

## Lexical structure

These are the rules for writing individual tokens: comments, strings, numbers,
and names.

### Comments

Two comment forms exist:

```ntsc
// Line comment, runs to the end of the line.

#{ This is a block comment. It does not nest. }
```

### Semicolons

Statement terminators are optional. The lexer inserts a semicolon where a
statement clearly ends at a newline, following the usual automatic semicolon
insertion rules. Both of these are valid:

```ntsc
var a = 1;
var b = 2
```

### String literals

Strings are written with double or single quotes. Escapes are processed:
`"line one\n"` ends with a newline and `\"` embeds a quote; see the
language reference for the supported list and the `r"..."` raw form.

Interpolation is written as `${expression}` inside a double-quoted string:

```ntsc
var name = "Ada"
say("Hello, ${name}!")
```

A raw string literal `r"..."` stores its content verbatim, which is convenient
for regular expressions and Windows paths.

Numbers are written in decimal notation. `1`, `1.5`, and `0.25` are all valid
literals; hexadecimal, octal, and binary literals are not currently supported.

### Identifiers and keywords

An identifier is a name you choose, such as `total` or `fruit_count`. It
starts with a letter or underscore and continues with letters, digits, or
underscores.

Reserved words cannot be used as identifiers:

```text
and       as        async     await     break
case      catch     class     continue  copy
default   do        elif      else      enum
false     finally   for       from      fun
if        in        int       match     mut
nil       or        option    retry     return
unsafe    say       shared    static    string
super     test      this      throw     true
try       use       var       view
while     quiet     bool      float     array
object    any
```

The type names (`int`, `float`, `bool`, `string`, `array`, `object`, `option`,
`any`) double as reserved words because type annotations use the prefix form.

## Static variables and compile-time constants

This section is optional on a first pass. Return after the
[Functions](functions.md) chapter if you want to use pure functions in constants.

`static var` declares a value that survives across calls:

```ntsc
static var int calls = 0
```

`static const` declares a constant evaluated at compile time. The initializer
must be a constant expression: literals, arithmetic on constants, references
to earlier constants, and calls to pure functions (functions whose body is a
single `return`):

```ntsc
static const var int SIZE = 1024
static const var int HALF = SIZE / 2
```

Constants can reference other constants declared above them:

```ntsc
static const var int BASE = 100
static const var int OFFSET = 23
static const var int LIMIT = BASE + OFFSET   // folded to 123
static const var bool OK = 3 < 5             // folded to true
```

### Pure functions at build time

A function whose body is a single `return` statement is considered *pure* and
can be called from a `static const` initializer. The call runs at compile time:

```ntsc
fun double(int x) -> int {
    return x * 2
}

static const var int VAL = double(21)   // folded to 42
```

Pure functions can compose with constants:

```ntsc
fun square(int x) -> int {
    return x * x
}

static const var int N = 5
static const var int SQ = square(N)   // folded to 25
```

### Using constants

Constants can be used anywhere a literal is accepted, including loop bounds
and initializers:

```ntsc
static const var int COUNT = 3 * 2

fun main() {
    var i = 0
    while (i < COUNT) {
        say("" + i)
        i = i + 1
    }
}
```

## Try it

Write a complete `main` that sets `score` to `85`, prints a message using an
`if`/`else`, and uses a `while` loop to print the numbers `1` through `3`.
Run it, then change `score` to exercise the other branch.

Next: [Functions](functions.md) turns repeated statements into reusable code.
