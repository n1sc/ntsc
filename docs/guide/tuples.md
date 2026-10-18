# Tuples

After [Arrays and iterators](arrays-and-iterators.md), you know how to store a
list. A **tuple** groups a fixed number of related values, and each can have a
different type. Use an array for a list of similar items; use a tuple for a
small group such as a width and height, or a value and its label.

## Literal syntax

Tuples are written as a comma-separated list in parentheses:

```ntsc
var t = (10, 20)
var mixed = (42, "hello", true)
```

The type of a tuple is written with the element types in parentheses:

```ntsc
var t: (int, int) = (10, 20)
var mixed: (int, string, bool) = (42, "hello", true)
```

## Indexing

Elements are accessed by zero-based numeric index using dot notation:

```ntsc
var t = (10, 20)
say("" + t.0)   // 10
say("" + t.1)   // 20
```

Indexing is bounds-checked: an out-of-range index throws a catchable exception.

## Destructuring

Tuples can be unpacked into individual variables:

```ntsc
var t = (42, "hello")
var (a, b) = t
say("" + a)   // 42
say(b)        // hello
```

The number of binding names must match the tuple length.

## Returning tuples from functions

Functions can return tuples, which is useful for returning multiple values:

```ntsc
fun bounds() -> (int, int) {
    return (100, 200)
}

fun main() {
    var (w, h) = bounds()
    say("" + w)   // 100
    say("" + h)   // 200
}
```

A tuple that carries heap values *moves* them: returning a local inside a
tuple hands it to the caller, and the callee no longer drops it.

The following examples look ahead to [Ownership](ownership.md) and
[Classes and enums](classes.md). The integer-only `bounds` program above is
enough for a first pass through this chapter.

```ntsc
fun pair() -> (array[int], string) {
    var nums = [1, 2]
    var label = "two"
    return (nums, label)   // both move to the caller
}
```

Wrap an element in `copy(...)` to hand over an independent value and keep the
original; reading an element or a field inside the tuple borrows and does not
move it.

Class instances follow the same ownership rule. A class returned in an owning
tuple transfers the instance and its owned fields to the destructured binding,
including when the tuple is stored first:

```ntsc
class Record {
    var string label

    fun init(string label) {
        this.label = label
    }
}

fun record_pair() -> (Record, string) {
    var record = Record("left")
    return (record, "tag")
}

fun main() {
    var values = record_pair()
    var (record, tag) = values
    say(record.label + ":" + tag)
}
```

A class field read remains a borrow of the containing instance's field. It does
not create a second owner when placed in a tuple; use `copy(...)` when an
independent class value is required.

The returned tuple can also be stored and indexed later:

```ntsc
fun pair() -> (int, int) {
    return (7, 14)
}

fun main() {
    var t = pair()
    say("" + t.0)   // 7
    say("" + t.1)   // 14
}
```

## Tuples with different types

Tuples shine when grouping values of different types without defining a class:

```ntsc
fun main() {
    var t = ("alice", "bob")
    var (a, b) = t
    say(a + " and " + b)   // alice and bob
}
```

## Use cases

- **Multiple return values**: Return several results from a function without
  wrapping them in a class.
- **Swap**: `(a, b) = (b, a)` swaps two variables.
- **Grouping**: Pair related values that don't warrant a class definition.

## Try it

Change `bounds()` to return `(3, 4)`, destructure it into `width` and `height`,
and print `width * height` from `main`. You should see `12`.

Next: [Ownership](ownership.md) explains what happens when strings, arrays,
and other values are assigned or passed to a function.
