# Shared values

This is the first advanced chapter. Finish the foundations through
[Testing](testing.md) and be comfortable with
[Ownership](ownership.md) before continuing. Here you will distinguish an
independent copy from two handles that deliberately refer to the same value.

The examples using `arrays.*` need `use arrays` at the top of the source file.

`shared T` is the escape hatch for explicit aliasing. Where moves transfer
ownership and views borrow within a scope, a shared value keeps a heap value
alive as long as any handle to it exists. The cost is reference-counted
ownership instead of compile-time single-ownership.

`shared` requires a heap type (`array[T]`, `string`, or a class). Wrapping a
scalar is rejected at compile time:

```ntsc
shared int x = 5    // compile error: shared requires a heap type
```

## Declaring shared values

```ntsc
shared array[int] a = [1, 2, 3]
shared string g = "hello"
```

## Aliasing

Assigning one shared variable to another copies the reference. Nothing is
moved, and both handles alias the same value:

```ntsc
shared array[int] a = [1, 2, 3]
shared array[int] b = a
arrays.push(b, 4)
say("" + a[3])   // 4: a sees the push through b
```

## Adoption

An owned value can be adopted into a shared slot. The value is boxed and, when
the source is a bare variable, moved:

```ntsc
var arr = [1, 2]
shared array[int] s = arr
```

A shared string initialized from a literal boxes an owned copy of the literal.

## Shared values in functions

Shared parameters and returns are reference-counted: a shared argument remains
usable after the call, and a shared return is safe to drop:

```ntsc
fun bump(shared array[int] a) {
    arrays.push(a, 7)
}

fun make() -> shared array[int] {
    return [1, 2]
}

fun main() {
    shared array[int] x = [0]
    bump(x)
    say("" + arrays.length(x))   // 2
    var fresh = make()
    bump(fresh)
    say("" + arrays.length(fresh))   // 3
}
```

## Views and copies of shared values

A view of a shared handle borrows the wrapped value. It observes mutations
made through other handles and owns nothing:

```ntsc
shared array[int] s = [10, 20]
var view array[int] v = s
arrays.push(s, 30)
say("" + v[2])   // 30
```

`copy(s)` deep-copies the wrapped value into a new owned value that is fully
independent of the shared handle:

```ntsc
var c = copy(s)
arrays.push(c, 99)
say("" + arrays.length(s))   // unchanged
```

## The standard library and shared arrays

The `arrays` module operates on shared arrays in place rather than boxing
them. Mutating functions such as `arrays.push` and `arrays.pop` modify the
wrapped array; functional functions such as `arrays.sort` and `arrays.slice`
read it without consuming it:

```ntsc
shared array[int] s = [5, 1, 3]
arrays.push(s, 9)
var sorted = arrays.sort(s)
say("" + sorted[0])          // 1
say("" + arrays.length(s))   // 4: s was not consumed
```

## When to use shared

Reach for `shared T` when a value must outlive the scope where it was created
without a single clear owner, or when several parts of one thread need to keep
their own copy alive. Prefer moves and views for the common single-owner case;
shared values pay a reference-counting overhead and push lifetime errors to
runtime rather than compile time.

A `shared` value cannot cross a thread boundary: the reference count is not
synchronized, so passing one to `process.spawn_thread` or
`collections.channel_send` is a compile error. To hand data to another thread,
send it through a channel — see
[Threading rules](concurrency.md#threading-rules).

## Try it

Inside `main`, declare a shared array and a second shared handle to it. Push
a value through the second handle and print the length through the first.
Then create `copy(...)`, push another value into the copy, and check that the
shared array's length did not change.

Next: [Concurrency](concurrency.md) coordinates independently running work.
Shared values and thread-safe channels are different tools; the next chapter
explains that distinction.
