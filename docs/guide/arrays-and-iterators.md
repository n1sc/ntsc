# Arrays and iterators

With [Functions](functions.md), you can calculate an answer for one input.
An **array** stores a list of values of the same type. Here you will index a
list, change it, and visit each value with a loop. Custom iterators and the
detailed ownership rules at the end can wait until after classes and ownership.

For snippets using `arrays.*`, put `use arrays` at the top of your source
file, outside `main`.

## A complete example

```ntsc
fun square(int n) -> int {
    return n * n
}

fun main() {
    var array[int] values = [1, 2, 3, 4]
    var int total = 0
    for (var value in values) {
        total = total + square(value)
    }
    say("sum of squares: ${total}") // sum of squares: 30
}
```

The `for-in` loop binds one element to `value` on each iteration. The scalar
elements (`int` here) are copied into the loop variable. The array remains the
owner of its stored values.

## Array types and literals

Arrays are heap values with a fixed element type. The type is written
`array[T]`; literals are written with brackets:

```ntsc
var array[int] xs = [1, 2, 3]
var floats = [1.5, 2.5]       // array[float]
var names = ["a", "b"]        // array[string]
```

The empty literal `[]` has no element type and must be annotated:

```ntsc
var array[int] empty = []
```

Untyped arrays can also be created with `arrays.new()`, and so can a typed one
by seeding it with a value: `var array[int] xs = arrays.fill(0, 0)`. Prefer the
typed form whenever the elements have a type — an untyped (`array[any]`)
element carries no static type, so reading one as a number or a `bool`
reinterprets the value instead of using it.

## Indexing

`a[i]` reads an element, `a[i] = v` writes one. Indices are zero-based and
bounds are checked at runtime; out-of-range access throws an exception.

```ntsc
var fs = [2.5, 1.5, 3.5]
say("" + fs[0])    // 2.5
fs[1] = 9.5
```

Arrays are resizable at the runtime level but the literal `[]` and fixed
indexing keep a homogeneous type; use `arrays.push` and `arrays.pop` to grow
or shrink.

## The `arrays` module

The `arrays` module provides the standard array operations. Import it with
`use arrays`. Mutating operations take the array in place and
return `void`; functional operations take a view, leave the input untouched,
and return a new owned array.

### Mutating in place

```ntsc
var a = [1]
arrays.push(a, 2)          // append; a is now [1, 2]
var last = arrays.pop(a)   // remove and return the last element
arrays.clear(a)            // remove all elements
```

`arrays.push` returns `void`, so the call site does not reassign `a`; the array
handle is mutated directly and is never moved.

Arrays of strings (including untyped `[]` arrays and `array[any]`) own their
elements: `arrays.push`, `arrays.pop`, and index-assignment hand values to and
from the container without aliasing, so a string pushed into an array is safe
to reassign or drop at the call site afterwards. `arrays.pop` transfers
ownership of the removed string to the caller.

### Returning new arrays

```ntsc
var nums = [3, 1, 2]
var sorted = arrays.sort(nums)   // [1, 2, 3]; nums is untouched
var reversed = arrays.reverse(nums) // [2, 1, 3]; nums is untouched
var shorter = arrays.remove_at(nums, 0) // [1, 2]; nums is untouched
var s = arrays.slice(nums, 1, 3) // elements 1..3
var cl = arrays.clone(nums)      // copy; string elements are deep-copied
```

Use the returned array. Calling `arrays.sort(nums)` without storing its
result does not sort `nums` in place. To replace the original, write
`nums = arrays.sort(nums)`.

### Query and element operations

```ntsc
say("" + arrays.length(a))    // int
say("" + arrays.isEmpty(a))   // bool
var i = arrays.index_of(a, 2)   // first index, or -1
say("" + arrays.contains(a, 5)) // bool
var e = arrays.get(a, 0)     // element at index (bounds-checked)
var f = arrays.flat(a)       // flatten nested arrays one level
```

`arrays.at` is an alias of `get`. `arrays.range(start, end)` builds an
`array[int]` of consecutive values, and `arrays.fill(value, count)` builds an
array with `count` copies of `value`.

## The iterator protocol

Read this section after [Classes and enums](classes.md). For the examples
above, arrays already provide everything the `for-in` loop needs.

`for-in` iterates any value that provides two methods:

- `length() -> int`
- `get(int) -> T`

Arrays implement these natively, so arrays always iterate. Custom classes
implement them to participate; values of any other kind (a `string`, a
`slice`, a tuple, an instance of a class without the two methods) are
rejected at compile time with a `for-in is not supported` error, rather
than running a loop that never executes:

```ntsc
class Range {
    var int count

    fun init(int n) {
        this.count = n
    }

    fun length() -> int {
        return this.count
    }

    fun get(int i) -> int {
        return i * 10
    }
}

fun main() {
    var total = 0
    for (var v in Range(3)) {
        total = total + v
    }
    say("" + total)   // 30
}
```

The loop variable type is inferred from `get`'s return type, so a
`Words` class whose `get` returns `string` iterates as strings. The iterator
may be a function call, a field, or any expression.

## Ownership of loop variables

This section builds on [Ownership](ownership.md). The loop variable is bound
from `get`'s result. Scalar elements are copied; reading heap elements from an
existing container borrows from that container. Iterating a stored array keeps
the array as the owner. An owned function parameter still consumes the caller's
array when the function is called:

```ntsc
fun total(array[int] xs) -> int {
    var s = 0
    for (var x in xs) { s = s + x }
    return s
}
```

Iterating an array through a `view` parameter leaves the caller's array
owned and untouched. A fresh iterable expression is temporary-owned by the
loop and is reclaimed when the loop finishes or when control leaves an
iteration, including `return`, `throw`, `break`, and `continue`; an iterable
stored in a variable remains owned by that variable:

```ntsc
fun words() -> array[string] {
    return ["first", "second"]
}

fun find_second() -> bool {
    for (var word in words()) {
        if (word == "second") { return true }
    }
    return false
}

var source = words()
for (var word in source) { say(word) }  // source remains the owner
```

## Try it

Change the complete example to use `[2, 4, 6]`. Predict the sum of squares,
then run it. Next, use `arrays.length(values)` to print how many inputs were
processed.

Next: [Tuples](tuples.md) groups a few values that do not all need the same
type.
