# Ownership

You have used functions, arrays, and [Tuples](tuples.md). Now learn how the
compiler decides who keeps a value alive. Start with moves, views, and copies;
the cleanup and escape-analysis details are useful when you later work with
classes and error handling.

For examples that call `arrays.*`, add `use arrays` at the top of your file.

An **owner** is the variable or container responsible for a value. A **scope**
is a block enclosed in braces. Owned strings, arrays, and class instances use
heap storage: storage whose lifetime follows the owner rather than just one
expression. Leaving the owner's scope releases the value unless ownership was
transferred elsewhere.

NTSC uses an Own - Move - View model: every heap value has exactly one owner
at a time. The compiler enforces this statically, so programs cannot
double-free or use a value after it has been freed. Runtime leak detection in
debug builds confirms that every owned value is eventually dropped.

Scalars (`int`, `float`, `bool`) are copied on assignment and never moved.
Heap values (strings, arrays, class instances, and shared handles) are owned;
assignment, parameter passing, and returns move them. Views are temporary
borrows.

## Moves

Assigning an owned value to another variable moves it:

```ntsc
var a = [1, 2, 3]
var b = a        // a is moved into b
say("" + b[0])   // 1
say("" + a[0])   // compile error: use after move
```

The source becomes invalid and cannot be used until it is reassigned. The same
rule applies to passing a value into a function (the caller loses it) and to
returning a value from a function:

```ntsc
fun total(array[int] xs) -> int {
    var s = 0
    for (var x in xs) { s = s + x }
    return s
}

fun main() {
    var a = [1, 2, 3]
    say("" + total(a))   // a is moved here
    var b = a       // compile error: a was already moved
}
```

Returning a parameter moves it out of the function:

```ntsc
fun make() -> array[int] {
    var a = [5, 6, 7]
    return a        // moved out; no copy
}
```

Reassigning a moved-from variable makes it usable again:

```ntsc
var a = [9]
var b = a           // a moved into b
a = [10]            // a owns a new value now
say("" + b[0])     // 9
say("" + a[0])     // 10
```

### Moves and control flow

A value is dead after a branch if it was moved on *any* path that reaches the
code after the branch:

```ntsc
var xs = [1, 2]
if (flag) { var a = xs }         // moved on one path
say("" + arrays.length(xs))      // compile error: xs may be moved here
```

A branch that always leaves — `return`, `throw`, `break`, or `continue` — never
reaches that join, so a move inside it does not affect what follows:

```ntsc
fun f(int n) -> int {
    var xs = [1, 2]
    if (n > 0) {
        var a = xs
        return arrays.length(a)      // this path leaves here
    }
    return arrays.length(xs)         // ok: xs cannot have been moved
}
```

A loop body is analyzed as if it runs more than once, so a value moved in the
body is dead on the next iteration:

```ntsc
var xs = [1, 2]
for (var i = 0; i < 3; i = i + 1) {
    var taken = xs               // compile error: use of moved value on iteration 2
}
```

Reassigning inside the body keeps it alive:

```ntsc
var xs = [1, 2]
for (var i = 0; i < 3; i = i + 1) {
    var taken = xs
    xs = [3, 4]                  // ok: reinitialized before the next iteration
}
```

## Views

A `view` borrows a heap value without taking ownership. Views never free
anything, and the compiler guarantees the borrow does not outlive its source.

```ntsc
fun main() {
    var matrix = [[1, 2], [3, 4]]
    view var r = matrix[1]
    say("" + r[0])       // 3
    say("" + matrix[1][0])   // source still owned and readable
}
```

A view may be a function parameter, allowing a function to read a value the
caller still owns:

```ntsc
fun read(view array[int] v) {
    say("" + v[0])
}

fun main() {
    var xs = [10, 20]
    read(xs)        // xs is not consumed
    say("" + xs[1]) // still valid
}
```

### Mutating through a view

`view mut` borrows exclusively, and writes through it are visible on the
source after the view's scope ends:

```ntsc
var xs = [1, 2, 3]
{
    view mut var m = xs
    m[0] = 99
}
say("" + xs[0])   // 99
```

While a `view mut` is alive, no other view of the same source may exist, and
the source may not be moved.

### View rules

The compiler enforces these rules:

- A viewed value cannot be moved while the view is alive.
- A value cannot be borrowed by a view and mutated or re-borrowed in a
  conflicting way (`view mut` conflicts with any existing view).
- A view cannot borrow a temporary value or an owner declared in a shorter-lived inner scope.
- Reassigning a view updates its borrow; after a branch join, every possible owner remains borrowed until the view's final use.
- Views cannot be returned, stored in an owned container, captured by a closure, or passed across threads.
- No destination that outlives the borrow may hold a view. That covers
  declaring an owned variable from one and assigning one into an owned
  variable, an array element, an object property, a class field, or a `shared`
  value. Use `copy(...)` when the destination must keep the value:

  ```ntsc
  class Bag { var array items }
  var b = Bag()
  {
      var xs = [1, 2]
      view var v = xs
      b.items = v          // error: the instance owns its fields
      b.items = copy(v)    // ok: an owned copy
  }
  ```

- A view may borrow the pointee of a `shared` value; the borrow refers to the
  value inside the box, not the box itself, and the exclusivity rules apply to
  it like any other borrow.
- Reading a heap element or field out of a container yields a view, not a
  value, so it cannot be stored in an owned `var` (see
  [Reading out of a container](#reading-out-of-a-container)).

Violating a rule is a compile error with a message that names the conflict, for
example `cannot move `a` while it is viewed`.

### View syntax

The `view` keyword marks the declaration:

```ntsc
view var r = matrix[1]
view mut var m = xs
```

The same effect is available as a type annotation:

```ntsc
var view array[int] r = matrix[1]
```

and as a parameter type:

```ntsc
fun read(view array[int] v) { ... }
```

### View of shared

A view can borrow a `shared` handle's pointee. Because a shared handle keeps
the value alive for as long as any handle exists, the view never dangles:

```ntsc
shared array[int] s = [10, 20]
var view array[int] v = s
arrays.push(s, 30)
say("" + v[2])   // 30: the view sees mutations through the shared handle
```

### Reading out of a container

A container owns what it holds. An array owns its elements; an instance owns its
fields. So reading a *heap* element or field out of one hands out a borrow of a
value the container still owns — not a copy of it. Storing that borrow in an
owned `var` would give the value a second owner, and the container's next write
would free it out from under the borrow:

```ntsc
var array[string] names = ["ada", "bob"]
var s = names[0]        // error: cannot store a borrowed element in `s`
names[0] = "zoe"        // would free the string `s` points at
say(s)
```

Two fixes, depending on what you want:

```ntsc
var s = copy(names[0])  // an independent string you own
view var s = names[0]   // a block-scoped borrow; `names` stays the owner
```

Reading without storing needs neither, because the borrow only has to live for
the statement:

```ntsc
say(names[0])
for (var n in names) { say(n) }
```

*Scalar* elements and fields are copied out, so they are plain owned values:

```ntsc
var xs = [1, 2]
var n = xs[0]           // fine: ints are copied
```

The same rule covers class fields, including fields inherited through
`extends`:

```ntsc
class Bag { var array items }
var b = Bag()
var taken = b.items     // error: `b` owns the array
```

A field read from a temporary class instance is different: the temporary has
no other owner that can keep the field alive, so the field's ownership is
transferred to the expression result. This makes it safe to retain a field
from a factory result or to pass it to a function:

```ntsc
fun make_bag() -> Bag {
    var b = Bag()
    b.items = [1, 2, 3]
    return b
}

var items = make_bag().items   // moves the field out of the temporary bag
say("" + items[0])
```

The compiler drops the temporary receiver after the field is extracted. A
field read from a live variable remains a borrow and still requires `copy(...)`
or an explicit `view`, as shown above.

## Copies

`copy(expr)` deep-copies a heap value, producing an independent owned value:

```ntsc
var a = [1, 2]
var b = copy(a)     // deep copy; a and b are independent
arrays.push(b, 99)
say("" + arrays.length(a))   // 2
say("" + arrays.length(b))   // 3
```

`copy` of a `shared` handle copies the pointee to a new owned value:

```ntsc
shared array[int] s = [1, 2]
var c = copy(s)
arrays.push(c, 99)
say("" + arrays.length(s))   // 2, unchanged
```

Copying is explicit; NTSC never copies heap values implicitly.

`copy` also materializes a borrow, so copying out of a container is safe:

```ntsc
var names = ["ada", "grace"]
var first = copy(names[0])   // an owned string; `names` still owns its own
say(first)                   // ada
say(names[0])                // ada, not a released handle
```

## Cleanup and exceptions

Every initialized owned value is reclaimed exactly once, on whichever path
leaves its scope: falling off the end, `return`, `throw`, a rethrow from a
handler, a `retry` attempt, `break`, and `continue` all run the same drops.

```ntsc
for (var i = 0; i < 5; i = i + 1) {
    var xs = [i]
    if (i == 1) { continue }     // xs is reclaimed here
    if (i == 3) { break }        // and here
    say("" + xs[0])
}
```

Temporaries are reclaimed too — an array literal passed to a constructor, a
concatenated string compared with `==`, the object a `{...}` literal builds and
the pieces it was built from, a field extracted from a temporary class
instance, and the value a destructuring statement unpacked.

A constructor that throws part-way is cleaned up as well. Its `init` has already
moved some arguments into fields, and the instance never reaches the caller's
variable, so the constructor reclaims exactly the fields `init` had written:

```ntsc
class Pair {
    var string left
    var string right

    fun init(string l, string r) {
        this.left = l
        if (r == "bad") { throw "mid" }   // `left` is reclaimed on this edge
        this.right = r
    }
}

try {
    var p = Pair("a", "bad")
} catch (e) { say("caught " + e) }
```

An exception message is *moved* into the catch binding, which owns it for the
handler and drops it at the end. Rethrowing with `throw e` hands the same value
on rather than copying it, so a message that travels through several handlers is
still freed once.

Assignment to a field or an element drops the value the place held, including
the empty array an `init`-less class default-initializes a container field with.
Reading the same place in the value expression is safe, because the value is
computed before the store happens:

```ntsc
b.items = b.items          // no-op, not a free-then-read
xs[0] = xs[1] + 1
```

An instance whose fields the compiler cannot prove are unaliased is the one
exception: when a second name can still reach the instance, freeing its fields
per name would free them twice, so they are deliberately left to the process
exit instead. Debug builds report those as leaks.

## Escape analysis

Non-escaping class instances are stack-allocated instead of heap-allocated.
Escape analysis is conservative: an instance escapes if it is returned, stored
somewhere the analysis cannot track, or has a constructor (a constructor
observes `this`, so the instance is kept on the heap). The decision is
invisible to the program's behavior: reference semantics hold in both cases.

## Leak detection

In debug builds, the runtime tracks owned heap allocations. When a program
exits, any value that was never dropped is reported as a `NTSC WARNING`
message on stderr. Clean programs produce no warnings. Leak detection is
disabled in release builds.

For a practical review checklist, return to
[Leak-free conventions](conventions.md) after the concurrency chapter.

## Try it

Write a `total(view array[int] values) -> int` function using a `for-in` loop.
Call it from `main`, then print `arrays.length(values)` there to confirm the
caller still owns its array. Make a `copy(values)`, append to the copy, and
check that the original length did not change.

Next: [Classes and enums](classes.md) uses these ownership rules with types
you define yourself.
