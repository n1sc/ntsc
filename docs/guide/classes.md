# Classes and enums

Build on [Ownership](ownership.md) to define your own types. A **class**
groups data (fields) with operations on that data (methods). An **enum** names
a fixed set of possibilities. Start with a counter; inheritance, custom
iterators, and operator overloading are optional extensions later in the page.

## Classes

A class bundles fields and methods:

```ntsc
class Counter {
    var int n

    fun init(int start) {
        this.n = start
    }

    fun bump() {
        this.n = this.n + 1
    }
}

fun main() {
    var counter = Counter(40)
    counter.bump()
    say("counter: ${counter.n}") // counter: 41
}
```

This is a complete program. `Counter(40)` creates an instance and calls
`init`; `counter.bump()` calls a method on that instance.

Field declarations use the same `var` syntax as local variables and may carry a
type annotation. Methods use `fun`. A method named `init` is the constructor;
the constructor is what allows a class to be instantiated with arguments.
Classes without an `init` are still instantiated, with each field at its zero
value:

```ntsc
class Point {
    var int x
    var int y
}

fun main() {
    var p = Point()
    p.x = 3
    p.y = 4
}
```

A field can declare its own starting value, which is applied every time an
instance is constructed — before `init` runs, so a constructor is free to
overwrite it:

```ntsc
class Bag {
    var name = "bag"
    var xs = [1, 2, 3]
    fun init(string label) {
        this.name = label
    }
}

fun main() {
    var b = Bag("crate")
    say(b.name + " " + arrays.length(b.xs))  // crate 3
}
```

### Instantiation and field access

Instances are created by calling the class name like a function. Fields are
read and assigned with `.`:

```ntsc
var c = Counter(40)
c.bump()
say("" + c.n)   // 41
```

### `this`

`this` refers to the current instance. It is required to disambiguate field
access from local variables inside methods:

```ntsc
fun init(int start) {
    this.n = start
}
```

### Reference semantics

Class instances refer to an object, rather than embedding an independent copy
in each variable. For temporary access, make the borrow explicit. For an
independent object, use `copy`:

```ntsc
var p = Point()
{
    view mut var q = p
    q.x = 10
}
say("" + p.x)   // 10

var independent = copy(p)
independent.x = 20
say("" + p.x)   // still 10
```

Avoid creating multiple owned names for the same instance. Aliasing that the
compiler cannot prove safe can prevent field cleanup and produce debug leak
warnings. See [Leak-free conventions](conventions.md#6-design-classes-with-one-owner-for-each-field).

Escape analysis stack-allocates instances that neither escape the function nor
declare an `init`; classes with a constructor are always heap-allocated. The
behavioral contract (reference semantics, correct lifetime) is identical in
both cases.

### Inheritance

A class may extend a parent:

```ntsc
class Base {
    var int id

    fun init(int id) {
        this.id = id
    }
}

class Derived extends Base {
    // inherits fields and methods from Base
}
```

`extends` provides single inheritance: the child inherits the parent's fields
and methods. The `super` keyword is reserved for accessing the parent and is
not yet usable in expressions.

### Classes as values

Class instances are first-class values: they can be passed to functions,
returned from functions, stored in arrays, and used as the element type of
`for-in` when they implement the iterator protocol.

```ntsc
fun make() -> Counter {
    var c = Counter(40)
    c.bump()
    return c
}
```

A class instance can also be one element of an owning tuple. Returning and
then destructuring that tuple transfers the class instance and its owned
fields to the new binding; the class fields are reclaimed with that binding.
A class field read is still a borrow of the containing instance, even when it
is placed in a tuple.

## Enums

An enum declares a fixed set of named cases:

```ntsc
enum Direction {
    North,
    South,
    East,
    West
}
```

Enum cases are matched with `match`:

```ntsc
match (dir) {
    case North => say("up")
    case South => say("down")
    case _ => say("sideways")
}
```

See [Syntax and control flow](syntax-and-control-flow.md) for the full pattern
syntax available in `case` arms.

## The iterator protocol

Any class with a `length() -> int` method and a `get(int) -> T` method can be
used in `for-in`. The loop variable is typed from `get`'s return type:

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

The loop is equivalent to iterating indices `0 .. length()` and calling
`get(i)` on each. See [Arrays and iterators](arrays-and-iterators.md).

## Operator overloading

Classes can overload operators by defining methods named after the operator
symbol. When an operator is applied to a value of that class, the
corresponding method is called.

### Binary operators

Define a method named after the operator. The right operand is a parameter:

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

    fun -(view Vec other) -> Vec {
        return Vec(this.x - other.x, this.y - other.y)
    }

    fun *(float scalar) -> Vec {
        return Vec(this.x * scalar, this.y * scalar)
    }
}

fun main() {
    var a = Vec(1.0, 2.0)
    var b = Vec(3.0, 4.0)
    var c = a + b
    say("" + c.x)   // 4.0
}
```

The right-hand parameter is typically `view` for class types to avoid
consuming the operand. Scalar parameters (like `float scalar`) are passed
by value.

### Comparison operators

Comparison operators return `bool` and work the same way:

```ntsc
class Point {
    var int x
    var int y

    fun init(int x, int y) {
        this.x = x
        this.y = y
    }

    fun ==(view Point other) -> bool {
        return this.x == other.x && this.y == other.y
    }

    fun !=(view Point other) -> bool {
        return this.x != other.x || this.y != other.y
    }

    fun <(view Point other) -> bool {
        return this.x < other.x
    }

    fun >(view Point other) -> bool {
        return this.x > other.x
    }
}
```

With `<` defined, instances of `Point` can be used with `sort.sort_by` and
other stdlib functions that accept a comparison function.

### Unary operators

Unary operators take no parameters:

```ntsc
class Vec {
    var float x
    var float y

    fun init(float x, float y) {
        this.x = x
        this.y = y
    }

    fun -() -> Vec {
        return Vec(-this.x, -this.y)
    }
}

fun main() {
    var a = Vec(3.0, -4.0)
    var b = -a
    say("" + b.x)   // -3.0
}
```

### Supported operators

| Operator | Method name | Example |
| --- | --- | --- |
| `+` | `+` | `a + b` |
| `-` | `-` | `a - b` |
| `*` | `*` | `a * b` |
| `/` | `/` | `a / b` |
| `%` | `%` | `a % b` |
| `==` | `==` | `a == b` |
| `!=` | `!=` | `a != b` |
| `<` | `<` | `a < b` |
| `<=` | `<=` | `a <= b` |
| `>` | `>` | `a > b` |
| `>=` | `>=` | `a >= b` |
| unary `-` | `-` | `-a` |
| unary `!` | `!` | `!a` |

Operators can be chained:

```ntsc
var c = a + b * 2.0   // calls * then +
```

## Try it

Add a `reset()` method to the complete `Counter` program that sets `this.n`
to `0`. Call `bump()` twice, print the value, call `reset()`, and print it
again. You should see `42` followed by `0`.

Next: [Modules and imports](modules.md) moves your reusable code into another
source file.
