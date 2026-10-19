# Language reference

This is the exhaustive reference for NTSC source code. For a tutorial
introduction, see the [guide](../guide/syntax-and-control-flow.md).

## Program structure

A program is one or more UTF-8 `.nt` files. The entry file is given in
`neutron.toml`; other files are loaded with `use "file.nt"`. The compiler merges all
files into a single program and resolves names across the whole closure.

Top-level declarations:

```
program      := declaration*
declaration  := var-declaration
              | function-declaration
              | async-function-declaration
              | class-declaration
              | enum-declaration
              | test-declaration
              | use-statement
              | statement
```

`main` is the entry point. It may omit a return type (implicit `void`) or
return `int`.

## Lexical grammar

### Whitespace and comments

Whitespace separates tokens. Two comment forms:

- `//` runs to end of line.
- `#{ ... }` is a block comment. It does not nest.

### Semicolons

Semicolons are optional; the lexer performs automatic semicolon insertion.
A statement terminator is inserted at the end of a line or input when the
statement is syntactically complete.

### Identifiers

`[A-Za-z_][A-Za-z0-9_]*`. Case-sensitive.

### Literals

| Literal | Examples |
| --- | --- |
| integer | `0`, `42`, `-1` |
| float | `0.5`, `1.0`, `-3.25` |
| string | `"hi"`, `'hi'`, `r"raw"` |
| boolean | `true`, `false` |
| nil | `nil` |

String escapes are processed in single- and double-quoted literals:
`\n`, `\t`, `\r`, `\0`, `\\`, `\"`, `\'`, and `\$`. `\$` writes a literal
dollar without starting an interpolation; any other escape stays as the two
characters it is written as. String literals may span multiple lines, so a
literal newline can also be written by breaking the line inside the quotes.
Interpolation `${expr}` is available inside double-quoted strings. `r"..."`
strings are stored verbatim: no escapes and no interpolation.

Integers are decimal by default, with optional radix prefixes `0x`
(hexadecimal), `0o` (octal), and `0b` (binary). Expressions may span line
breaks when a binary operator leads the next line or ends the previous one.

### Keywords

```
and        as         async      await      break      case
catch      chan       class      close      continue   copy
default    do         elif       else       enum       false
finally    for        from       fun        go         if
in         int        match      mut        nil        or
option     own        retry      return     unsafe     say
shared     slice      static     string     super      test
this       throw      true       try        use        var
view       while      quiet      bool       float      array
object     any        result
```

`&` and `*` are prefix operators that also form pointer type annotations
(`&T`, `&mut T`, `*const T`, `*mut T`); see
[Pointers and references](#pointers-and-references).

### Operators and punctuation

```
+   -   *   /   %    ++   --   !   ~
<   <=  >   >=  ==   !=   &&   ||  !
<<  >>  &   |   ^
<|  |>
=   ?:  ?    ...  ?.  ->   =>   ( ) { } [ ] , . :
```

## Types

### Primitive types

| Type | Values | Zero value |
| --- | --- | --- |
| `int` | 64-bit signed integers | `0` |
| `float` | 64-bit IEEE-754 | `0.0` |
| `bool` | `true`, `false` | `false` |
| `string` | owned UTF-8 string | `""` |
| `nil` | absence, used with `option[T]` | |

### Composite types

| Type | Meaning |
| --- | --- |
| `array[T]` | Owned heap array with element type `T`. |
| `option[T]` | `T` or `nil`. |
| `result[Ok, Err]` | Success value of type `Ok` or error value of type `Err`, built with `Ok(v)` and `Err(e)`. |
| `object` | An opaque dynamic value (used by `json.parse` results). |
| `any` | An untyped value; the element type of an untyped array literal. |
| `void` | The implicit return type of functions with no return; not written in annotations. |
| `T -> R` | Function type (not yet written in annotations). |
| `shared T` | Reference-counted handle to a heap value. |
| `view T` / `view mut T` | Borrow of a heap value. |
| `own T` | Owning allocation of `T` (see [Pointers and references](#pointers-and-references)). |
| `&T` / `&mut T` | Addressable reference / exclusive reference. |
| `*const T` / `*mut T` | Raw pointer, usable only inside `unsafe`. |
| `slice[T]` | Bounds-checked window over an `array[T]`. |
| `chan[T]` | Virtual-task channel carrying elements of type `T`; created with `chan.new(capacity)`. Send with `\|>` (moves the value in), receive with `<\|` (moves it out). |

| `(T, U, ...)` | Tuple type. |
| class name | Instance of that class. |

### Type annotations

Type annotations precede the declared name:

```
var int count = 42
var float ratio = 0.5
var string name = "ada"
var array[int] xs = [1, 2, 3]
var option[int] maybe = nil
```

`option[T]` is written as `option[T]`. A view annotation is `view T`:
`var view array[int] r = ...`. A shared annotation is `shared T`:
`shared array[int] a = ...`. Pointer annotations are `own T`, `&T`,
`&mut T`, `*const T`, and `*mut T`; see
[Pointers and references](#pointers-and-references).

The empty array literal `[]` requires an annotation. Other literals infer
their type. Without an annotation and without an initializer, a variable takes
its zero value.

### Generics and associated types

Generic functions, classes, enums, and aliases are specialized at compile time.
Trait bounds are checked for each concrete specialization. Traits may declare
an associated type, which each implementation binds with a simple type alias:

```ntsc
trait Producer {
    type Item
    fun item() -> Item
}

impl Producer for User {
    type Item = int
    fun item() -> int { return 7 }
}

fun read<T: Producer>(view T value) -> T::Item {
    return value.item()
}
```

`T::Item` is resolved during monomorphization. Neutron does not silently erase
an unresolved generic type into a dynamic value. Use `any` explicitly when a
dynamic value is intended; this keeps generic code statically checked by
default and makes the dynamic boundary visible in source.

## Pointers and references

Neutron keeps checked ownership separate from raw addresses. See the
[Unsafe and raw pointers](../guide/unsafe-and-pointers.md) guide for
the full story.

| Form | Meaning |
| --- | --- |
| `own T` | An owning allocation of `T`, created with `alloc(value)`. |
| `&T` | An immutable addressable reference. |
| `&mut T` | An exclusive mutable reference. |
| `*const T` | A raw immutable pointer. |
| `*mut T` | A raw mutable pointer. |

```ntsc
var own Packet packet = alloc(Packet(7))   // owning allocation
var &Packet read = &packet                 // immutable reference
var &mut Packet write = &mut packet        // exclusive reference
say("id: " + read.id)                      // fields reach through a reference
```

`alloc(value)` moves `value` into an owning allocation. A class instance is
already heap-allocated and is adopted rather than copied; any other value is
boxed into a fresh cell. An `own T` frees its allocation when its owner leaves
scope, after reclaiming the pointee's owned contents, and it is moved (not
copied) by assignment. `copy(p)` deep-copies the pointee into an independent
allocation.

References are created with `&value` and `&mut value`, and may borrow a
variable or a field. Borrowing a place that already holds an address (an `own`,
another reference, a `shared` handle, or a view) yields a reference to the
pointee, not to the handle. A reference borrows and never frees.

A reference is borrow-checked like a view, with non-lexical lifetimes:

- A borrow lives from its declaration to the reference's final use.
- Either one live `&mut` or any number of live `&`, never both.
- The referent cannot be moved, reassigned, or destroyed while borrowed.
- A reference cannot be returned, stored in an array, object, class field, or
  destructured target, or cross a thread boundary.
- A reference cannot borrow a temporary.

```ntsc
var xs = [1, 2]
var &array[int] r = &xs
say("" + r[0])              // borrow ends at this final use
var &mut array[int] w = &mut xs   // accepted: the shared borrow is dead
```

Raw pointers are created only from a reference with
`memory.raw_address(reference)`, which preserves the pointee type and
mutability: `&T` yields `*const T` and `&mut T` yields `*mut T`. They can be
dereferenced only inside `unsafe`:

```ntsc
unsafe {
    var *mut int raw = memory.raw_address(&mut packet.id)
    *raw = 42
}
```

- `*raw` reads through a raw pointer; `*raw = v` writes through one.
- Raw dereference and `memory.raw_address` require `unsafe { }`.
- Writing through `*const` is rejected.
- Raw pointers keep their pointee type: `memory.raw_address(&mut packet)` on an
  `own Packet` is `*mut Packet`, and assigning it to a `*mut int` is a type
  error.

For a bounds-checked buffer that exposes no addresses at all, use the
[memory capability API](stdlib.md#memory).

## Variables

### Local variables

```
var [type] name [= initializer]
```

Declaration and assignment are separate; reassignment preserves the declared
type. `var` also starts the declaration of a `static var`:

```
static var int calls = 0
```

### Compile-time constants

```
static const [type] name = constant-expression
```

`static const` declares a value folded at compile time. The initializer must
be a constant expression: literals, arithmetic on constants, references to
earlier constants, and calls to pure functions (single-`return` bodies):

```
static const var int SIZE = 1024
static const var int HALF = SIZE / 2       // folded to 512
static const var bool OK = 3 < 5           // folded to true
```

Pure functions called from constant initializers are evaluated at build time:

```ntsc
fun double(int x) -> int {
    return x * 2
}
static const var int VAL = double(21)   // folded to 42
```

Constants can reference earlier constants but not later ones. Circular
references are rejected with an error.

### Destructuring

Arrays, objects, and tuples can be unpacked:

```
var [a, b] = [1, 2]
var {name, age} = obj
var (x, y) = (10, 20)
```

Each name becomes a variable in the current scope.

### Views

```
view var name = expr            // immutable borrow
view mut var name = expr        // exclusive mutable borrow
var view T name = expr          // annotation form
```

`view` declarations borrow a heap value; the view owns nothing. A view's owner
must live at least as long as the view variable. Borrows end after the view's
final use, and assignments plus control-flow joins track every possible owner.
Views cannot borrow temporaries or inner-scope locals, be returned, be stored
in owned containers or closures, or cross thread boundaries. Exclusivity and
move-while-viewed violations use ownership diagnostic code `NTSC-E0501`.

### Shared

```
shared T name = expr            // reference-counted alias to a heap value
```

`shared` requires a heap type. Assignment to a shared variable copies the
reference; the value is never moved.

### Scope

Blocks create scopes. A variable declared in a block is visible from its
declaration to the end of the block.

## Functions

### Declarations

```
fun name(type param, ...) [-> return-type] { ... }
fun name(type param, ...) [-> return-type] => expression
```

Parameters are written type-first (`fun add(int a, int b)`) and always carry
a type. The return type follows `->`; when it is omitted the function
returns nothing (`void`). Functions can reference later declarations
and declarations in imported files.

### Async functions

```
async fun name(param, ...) [-> return-type] { ... }
```

An async function can suspend at `await` expressions, including `await`
inside control flow (`if`, loops, `try`/`catch`) and nested `return`
statements, which complete the future through its result slot. `await`
is only legal as a statement-level call, a variable initializer, an
assignment right-hand side, or a return value.

### Lambdas

```
fun (param, ...) [-> return-type] { ... }
fun (param, ...) [-> return-type] => expression
```

Lambdas are first-class values stored in variables and passed to functions.

### Returns

`return expr` returns a value; `return` alone returns `void`. Heap values are
moved out of the function. The return type is checked at the call site.

## Results and error propagation

`result[Ok, Err]` is a built-in generic enum for functions that can fail. The
global constructors `Ok(value)` and `Err(value)` build one:

```ntsc
fun half(int x) -> result[int, string] {
    if (x % 2 == 0) {
        return Ok(x / 2)
    }
    return Err("odd input")
}
```

### The `?` operator

Postfix `?` on a result propagates an `Err` out of the enclosing function
immediately; an `Ok` unwraps to its payload. It requires the enclosing
function to return a result whose Ok side matches the payload:

```ntsc
fun caller() -> result[int, string] {
    var n = parse_num(true)?
    return Ok(n)          // runs only when parse_num succeeded
}
```

When the function's error type is `string`, any non-string error payload is
converted with the standard stringify rules, so `Err(7)?` in a function
returning `result[_, string]` propagates `"7"`.

A `throw` inside a result-returning function is caught at the function
boundary and returned as `Err(message)` instead of escaping.

### Result combinators

Results expose `unwrap_or(default)`, `map(f)`, `and_then(f)`, and
`or_else(f)`. Options expose `ok_or(default)` and `ok_or_else(f)` to turn an
option into a result:

```ntsc
var n = Ok(21).map(fun(int x) -> int { return x * 2 })
say(fmt.i64_to_str(n.unwrap_or(0)))     // 42

var option[int] maybe = nil
var r = maybe.ok_or(-1)                 // result[int, int]
```

`map` transforms an `Ok` payload; `and_then` chains a function returning
another result; `or_else` recovers from an `Err` by calling a function that
returns a replacement result. `unwrap_or` returns the default for an `Err`.
Payloads passed to combinators follow ordinary move/copy rules: fresh
constructor results are consumed, stored values are deep-copied.

## Classes

```
class Name [extends Parent] {
    var type field
    ...
    fun name(type param, ...) [-> ret] { ... }
    ...
}
```

- A field may declare an initializer (`var name = "bag"`). It is applied at
  construction, before `init` runs, so a constructor can overwrite it. Only
  fields declared without one start at their zero value.
- `init` is the constructor. A class without `init` is still instantiated: its
  fields hold their initializers, or their zero values where none was declared.
- `this` refers to the current instance.
- Instances have reference semantics: assignment aliases the same object.
- `extends` provides single inheritance. `super` is reserved but not usable.
- Non-escaping classes without `init` are stack-allocated by escape analysis.

Instantiation: `Name(args)`.

### Operator overloading

A class can overload operators by defining methods named after the operator
symbol. Binary operators take the right operand as a parameter; unary operators
take no parameters. Comparison operators return `bool`.

```ntsc
class Vec {
    var float x
    var float y

    fun init(float x, float y) {
        this.x = x
        this.y = y
    }

    fun +(view Vec other) -> Vec {
        return Vec(this.x + other.x, this.y + other.y)
    }

    fun -() -> Vec {
        return Vec(-this.x, -this.y)
    }

    fun ==(view Vec other) -> bool {
        return this.x == other.x && this.y == other.y
    }
}
```

Supported operator methods: `+`, `-`, `*`, `/`, `%`, `==`, `!=`, `<`, `<=`,
`>`, `>=`, unary `-`, unary `!`.

## Enums

```
enum Name {
    Member,
    Other = value
}
```

Members may carry an explicit value.

## Statements

### Expression statement

An expression followed by a terminator. `say(expr)` prints a value with a
trailing newline.

### Blocks

```
{ statements }
```

### If

```
if (cond) { ... } [elif (cond) { ... }]* [else { ... }]
```

`elif` chains and the trailing `else` are optional.

### While and do-while

```
while (cond) { ... }
do { ... } while (cond)
```

### For

```
for ([init]; [cond]; [step]) { ... }
```

Each part is optional. `init` is typically `var i = 0`; `step` is typically
`i = i + 1` or `i++`.

### For-in

```
for (var name in iterable) { ... }
```

`iterable` must be an array or a class implementing the iterator protocol.
Arrays iterate through the runtime's element accessors; a class iterates
through its `length`/`get` methods. Other kinds of values (strings, slices,
tuples, objects) are rejected at compile time; split a string with
`strings.split(s, "")` to loop over its characters. The loop variable must be
declared with `var`; its type comes from the array's element type or the
class's `get` return type.

### For-await

```
for await name in producer { ... }
```

Evaluates `producer` and iterates over its elements. Currently equivalent to
`for (var name in producer)` but reserved for future streaming iteration where
`producer` is an async iterable.

### Go

```
go worker(args)
go { ... }
```

`go` spawns a goroutine: a cheap stackless task on the work-stealing
scheduler (one worker thread per CPU core). The call form runs an `async fun`
on the scheduler; the block form compiles the block to an anonymous future.
`go` never blocks the caller and returns nothing; when the process `main`
returns, outstanding goroutines are abandoned (as in Go) — the runtime
reclaims their futures and owned values at shutdown, so abandonment leaks
nothing. There is no goroutine `join` in the language yet: coordinate
completion with channels or `async.sleep`.

Arguments and captured variables cross a thread boundary and are classified
by the ownership checker: scalars are copied, safe handles (channels, files,
sockets) are shared, owned heap values (`string`, `array`, `object`, class
instances) are moved into the goroutine and can no longer be used by the
caller, and `view`/`shared` values are rejected (`NTSC-E0501`). See
[Ownership](#ownership) and the
[concurrency guide](../guide/concurrency.md).

### Channels

`chan[T]` is a typed channel between goroutines, created with
`chan.new(capacity)`:

```ntsc
var chan[int] jobs = chan.new(4)

7 |> jobs        // send: moves 7 into `jobs` (parks while full)
x <| jobs        // receive: binds a fresh `x` from `jobs` (parks while empty)
close(jobs)      // no more sends; receivers drain what is queued
```

- `|>` sends: the arrow points where the data flows — the value is *moved
  into* the channel on its right; the sender cannot use it afterwards
  (`NTSC-E0501` on later use).
- `<|` receives: the channel on the right feeds the variable on its left.
  Legal only as a statement at the top level of an `async fun` body; it binds
  a fresh variable that owns the received value.
- `close(expr)` closes the channel. Receivers drain the values already queued,
  then a receive completes with the zero value.
- `for name in channel { ... }` receives one value per iteration until the
  channel is closed and drained; also legal only at the top level of an
  `async fun` body.

Channel sends/receives are suspension points: a blocked operation parks the
goroutine without tying up a scheduler thread.

### Break and continue

Exit the innermost loop; skip to the next iteration.

### Match

```
match (expr) {
    case pattern [if guard] => statement
    ...
    default => statement
}
```

Patterns:

- Literal: `case 0 => ...`
- Wildcard: `case _ => ...`
- Variable: `case x => ...` (binds the matched value)
- Array destructure: `case [a, b, ...rest] => ...`
- Object destructure: `case {name, age} => ...`
- Result variants: `case Ok(v) => ...` / `case Err(e) => ...` bind the
  active payload; the binder is scoped to the arm body and owns an
  independent copy. Use `_` to ignore a payload (`case Err(_) => ...`),
  and combine with guards: `case Err(e) if e == "timeout" => ...`. A
  result scrutinee requires variant patterns; matching it against plain
  values never succeeds.

The first matching case executes; the match value is consumed once. Guards
are boolean expressions after `if`; in a variant arm the guard runs after
the binder is in scope, so it can read the payload.

### Try / catch / finally

```
try { ... } [catch (name) { ... }] [finally { ... }]
```

### Throw

```
throw expr
```

### Retry

```
retry count { ... } [catch (name) { ... }]
```

Runs the block up to `count` times, retrying while a run throws and attempts
remain; the optional `catch` runs when attempts are exhausted.

### Unsafe

```
unsafe { ... }
```

Marks a region where raw-pointer operations are allowed. Raw pointer
dereference (`*raw`, `*raw = v`) and `memory.raw_address(...)` are rejected
outside an `unsafe` block. The block itself lowers to a plain block; the
`unsafe` marker is enforced at type-check time.

### Quiet

```
quiet [name, ...] { ... }
```

Suppresses lint warnings in the block. With no names, suppresses all
suppresable lints. `quiet [unused_variable]` suppresses the unused-variable
warning.

### Use

```
use module [as alias]
use (a, b) = from module
use "file.nt" [as alias]
use (a, b) = from "file.nt"
```

An identifier after `use` names a standard library module (all module names
are predeclared, so module `use` is optional). A quoted path loads a source
file relative to the including file; the `.nt` extension is inferred when
omitted and the path must stay within the project root.
See [Modules](../guide/modules.md).

### Test

```
test name { statements }
```

A test body. Run by `ntsc test`; a thrown exception fails the test. See
[Testing](../guide/testing.md).

## Expressions

### Literals

`42`, `0.5`, `"hi"`, `'hi'`, `r"raw"`, `true`, `false`, `nil`.

### Variables and `this`

A bare name refers to a variable, parameter, or function. `this` refers to
the enclosing instance.

### Grouping

`(expr)`.

### Unary

| Operator | Meaning |
| --- | --- |
| `-x` | negation |
| `!x` | logical not |
| `~x` | bitwise not |
| `...x` | spread (in destructuring and argument lists) |

### Postfix

| Operator | Meaning |
| --- | --- |
| `x++` | increment in place |
| `x--` | decrement in place |

### Binary

Arithmetic, comparison, bitwise, and logical operators per the precedence
table in the guide. `+` also concatenates strings and arrays. Heap equality
`==` compares identity, not contents. `&&` and `||` short-circuit: the right
operand is evaluated only when the left one does not already decide the
result, so a bounds-checked read on that side cannot trap behind a false (or
true, for `||`) guard.

### Numeric faults

Integer arithmetic is checked. An operation whose mathematical result does not
fit in a 64-bit signed integer throws a catchable exception instead of wrapping
or producing a build-mode-dependent value, so a debug build and an optimized
build always agree:

| Operation | Message |
| --- | --- |
| `a + b` out of range | `integer addition overflow` |
| `a - b` out of range, including `-x` of the minimum value | `integer subtraction overflow` |
| `a * b` out of range | `integer multiplication overflow` |
| `a / 0`, `a % 0` | `division by zero` |
| minimum value `/ -1`, `% -1` | `integer division overflow` |
| `a << b`, `a >> b` with `b` negative or above 63 | `shift amount out of range` |

`x++` and `x--` are checked like the `+` and `-` they stand for. Float
arithmetic is IEEE-754 and never throws: it yields infinity or NaN.

Indexing is checked. A negative or out-of-range index throws
`array index out of bounds`, on writes (`a[i] = v`) as well as reads.

Numeric conversions are explicit. `int` widens to `float` implicitly, but a
`float` is not assignable to an `int` — the type checker rejects it. Where a
conversion to `int` is requested, out-of-range values saturate to the minimum or
maximum and NaN converts to `0`, rather than being left undefined.

String and array operations clamp instead of faulting: byte offsets snap to a
UTF-8 character boundary, ranges clamp to the value's length, and lengths and
capacities are bounded so no allocation-size calculation can wrap.

### Member access

- `obj.field` — read or assign a field or method.
- `obj?.field` — optional chain; yields `nil` when the receiver is `nil`.
- `arr[i]` — index read.
- `arr[i] = v` — index assignment.

### Calls

`f(args)` with evaluated arguments. Arguments are moved for owned parameters
and borrowed for `view` parameters.

### Assignment

`name = expr`, `obj.field = expr`, `arr[i] = expr`. Assigning an owned value
moves it.

### Lambda literals

As under Functions.

### Ternary

`cond ? then : else`.

### Array literal

`[e1, e2, ...]`. Homogeneous; the empty literal needs an annotation.

### Tuple literal

`(e1, e2, ...)` produces a tuple. Elements may have different types. The
length is part of the type:

```ntsc
var t = (10, "hello")       // (int, string)
var t2: (int, int) = (1, 2)
```

Tuple elements are accessed by zero-based dot index: `t.0`, `t.1`. Indexing
is bounds-checked.

### Object literal

`{ key: value, ... }` produces an `object`. Used with `json` operations and
destructuring.

### Await

`await callee(args)` suspends the enclosing async function until the callee
future completes, then yields its result. Only legal inside `async fun`.

The callee can be a named async function or an inline async block:

```
var x = await compute()
var y = await async { await async.sleep(10); return 42 }
```

An inline `async { ... }` block compiles to an anonymous future. It cannot
take parameters; its return type is inferred from `return` statements.

### View

`view expr` in an expression position is not valid; views are declarations
and parameter types.

### Copy

`copy(expr)` deep-copies a heap value (owned, shared, or borrowed) to a new
owned value. Copying a borrow (such as an array element) materializes an
independent copy; the source stays alive. Scalar and class copies are
handled by the type checker.

## Ownership

Heap values (strings, arrays, class instances, shared handles) have a single
owner at a time. Moves transfer ownership; views borrow; `copy` duplicates.
Scalars are copied on assignment.

An array owns its string elements: `arrays.push` and index-assignment store an
independent copy, and `arrays.pop` hands ownership of the removed element to
the caller. A string used as the source of a push or index-assignment can be
reassigned or dropped immediately after, and the array still reads back the
original bytes.

Because the container keeps ownership, reading a heap element or field out of
one produces a view rather than a value: `var s = names[0]` on an
`array[string]` is rejected, while `copy(names[0])`, `view var s = names[0]`,
and using `names[0]` inline without storing it are all accepted. Scalar
elements and fields are copied out and stay owned.

A move is only counted on paths that reach the code after a branch. A branch
that ends in `return`, `throw`, `break`, or `continue` never reaches the join,
so a move inside it does not invalidate the value afterwards. A loop body is
analysed as running more than once, so a value moved in the body must be
reassigned before the next iteration.

A returned expression moves every local it hands over, not just a bare
variable: `return (arr, text)` transfers both, so the callee's exit-time drop
does not release what the caller now owns. A local that only appears inside a
borrowed read (an element or field access), under `copy(...)`, or as an operand
of a concatenation is *not* moved — that value stays owned here.

Moves are only diagnosed for values whose type is statically known to own heap
memory — a declared `string`/`array`/class, or a parameter of that type. A
variable initialized from a call whose return type the checker cannot pin down
is treated as non-owning, so moving it and then reading it again is accepted
silently. Write `copy(x)` when the intent is to keep using the source; that is
a move-free operation regardless of how the type was inferred.

A class instance is reached through its handle, so passing one to a `view` or
`view mut` parameter borrows it: the caller keeps using the same instance
afterwards, and a `view mut` parameter's writes are visible to the caller.

A view may borrow the pointee of a `shared` value. The borrow refers to the
value inside the handle, not the handle, and the exclusivity rules apply to it
like any other borrow.

### Destruction

Every initialized owned value is destroyed exactly once, on whichever path
leaves its scope: the end of the scope, `return`, `throw`, a rethrow, each
`retry` attempt, `break`, and `continue`. Temporaries are destroyed once
consumed: array and object literals, concatenated and compared strings,
constructor arguments, and the value a destructuring statement unpacked.

An `init` that throws destroys the fields it had already written; the fields it
had not reached hold their zero value and need no cleanup. The instance itself is
never handed to the caller, so no name observes the partially built value.

An exception message is moved into its catch binding, which owns it for the
handler; `throw e` inside a handler transfers it on instead of copying it.

Assigning to a field or an element destroys the value that place held. The value
expression is evaluated first, so it may read the place being written
(`b.items = b.items`, `xs[0] = xs[1] + 1`).

An `object` is an owned value like a string: assigning one moves it, and it is
destroyed with its owner.

Fields of an instance that may be reachable through a second name are not
destroyed, because destroying them once per name would destroy them twice; debug
builds report them as leaks at exit.

The ownership checker (code `NTSC-E0501`) reports:

- use after move,
- move of a viewed source,
- conflicting views (`view mut` while an existing view is live),
- writing to or reassigning a source while it is viewed mutably,
- writing to a field of an instance while a view borrows one of its fields,
- views of temporaries,
- views escaping their scope,
- storing a borrowed element or field in an owned variable.

The type checker (code `NTSC-E0201`) rejects storing a view anywhere that can
outlive the borrow: declaring or assigning an owned variable from one, an array
element, an object property, and a class field. `copy(...)` produces an owned
value that any of them accept.

## Diagnostics

Diagnostics carry a code and annotated source spans:

| Code | Stage |
| --- | --- |
| `NTSC-E0001` | parse |
| `NTSC-E0101` | name resolution |
| `NTSC-E0201` | type checking |
| `NTSC-E0301` | code generation and linking |
| `NTSC-E0401` | module loading and building |
| `NTSC-E0501` | ownership |
| `NTSC-W0001` | lint warning |

Lint warnings are non-fatal; the build continues and reports them. The
unused-variable lint can be suppressed with `quiet [unused_variable]`.

With `--json`, diagnostics are emitted as structured JSON on stdout.

## Rust bridge typing

A call into a Rust adaptor is typed `Any` by the checker, so the checker
cannot catch a mistake in how its result is used; codegen re-reads the
adaptor's export manifest and types the value for real. Codegen's answer is
the one that reaches LLVM, and the two can disagree:

- A `bool` result is an i1, not a handle. Returning one from a function
  declared `-> int` fails module verification rather than type checking.
- An `option[T]` result is a cell handle, so `if (result)` is a *nullness*
  test on the cell and its payload still needs an unwrap. Compare against
  `nil` first when the value matters.

Declare classes before the functions that read their fields: a class's
struct type is created when its first use is emitted, so a function emitted
earlier that reads a field of a later-declared class fails with
`struct type ... not found`.

Do not return a class instance with string fields inside a tuple. The
strings read back empty (integers survive), in both destructured and
indexed reads, while the same class returned directly is fine. Prefer an
out-parameter: the caller creates the value and the callee fills it through
a `view mut` parameter.

## Known limitations

- Raw pointers cannot be cast between pointee types, constructed as null, or
  offset arithmetically; they exist only to reach a referent's storage.
- A raw pointer is not borrow-checked. Inside `unsafe`, keeping one past its
  referent's lifetime is the caller's responsibility.
- `unsafe` does not change behavior at runtime; it gates raw-pointer
  operations at type-check time.
- `super` is lexed but cannot be used in expressions.
- Integer literals are decimal only.
- String escapes (`\n`, `\t`, `\r`, `\0`, `\\`, `\"`, `\'`, `\$`) are processed
  in quoted literals; `r"..."` strings are stored verbatim.
- Only scalars and stdlib handles cross a thread boundary; views, `shared`
  values, and owned heap payloads are rejected. See
  [Threading rules](../guide/concurrency.md#threading-rules).
- `try`/`throw`/`retry` are allowed inside async functions. A `throw`
  propagates to the caller after `wait_any`/`wait_all` returns.
- A `chan[T]` parameter whose element is a class breaks that one function: the
  class name stops resolving inside it, so `T()` reports an undefined function
  and a field read on a value received from the channel miscompiles. Carry
  `string` or `int` messages and encode structured ones, or move the work into a
  synchronous function that takes the channel as a local.
