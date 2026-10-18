# Unsafe and raw pointers

This is an advanced, optional chapter. Read
[Leak-free conventions](conventions.md) first and be comfortable with owners,
views, and resource cleanup. You will learn how safe references differ from
raw pointers and how to call a C function. Most application code can stay
with ordinary ownership and views.

For examples that call `memory.raw_address`, add `use memory` at the top of
your source file, outside `main`.

Neutron keeps checked ownership separate from raw addresses. The `unsafe`
block is the boundary where the compiler stops enforcing safety guarantees
and hands control to the programmer.

## The `unsafe` block

```
unsafe { ... }
```

An `unsafe` block marks a region where raw-pointer operations are allowed.
Outside `unsafe`, the compiler rejects raw pointer dereferences and
`memory.raw_address(...)` calls. Inside `unsafe`, these operations are
permitted but still type-checked.

The block itself lowers to a plain block at runtime — `unsafe` is a
compile-time gate, not a runtime mode:

```ntsc
var int x = 42
var &mut int w = &mut x
unsafe {
    var *mut int raw = memory.raw_address(w)
    *raw = 100
}
say("" + x)   // 100
```

### What `unsafe` gates

| Operation | Requires `unsafe` |
| --- | --- |
| `*raw` (read through raw pointer) | yes |
| `*raw = v` (write through raw pointer) | yes |
| `memory.raw_address(reference)` | yes |
| `&value` / `&mut value` (safe references) | no |
| `view` / `view mut` (views) | no |
| `alloc(value)` (owning allocation) | no |
| `copy(value)` (deep copy) | no |

### Nesting

`unsafe` blocks can be nested. The innermost `unsafe` scope determines
whether raw pointer operations are allowed:

```ntsc
unsafe {
    var *mut int p = memory.raw_address(&mut x)
    unsafe {
        *p = 42   // allowed: inside unsafe
    }
}
```

## Raw pointers

Raw pointers are unmanaged addresses. They do not borrow-check, do not
track lifetimes, and do not free their referent. They exist to reach
storage that the safe pointer system cannot express.

### Creating raw pointers

Raw pointers are created from references with `memory.raw_address()`:

```ntsc
var int value = 10
{
    var &int shared_ref = &value
    unsafe {
        var *const int raw = memory.raw_address(shared_ref)
        say("" + *raw)
    }
}
{
    var &mut int mut_ref = &mut value
    unsafe {
        var *mut int raw_mut = memory.raw_address(mut_ref)
        *raw_mut = 20
    }
}
```

`memory.raw_address` preserves the pointee type and mutability:

| Input | Output |
| --- | --- |
| `&T` | `*const T` |
| `&mut T` | `*mut T` |

### Dereferencing

`*raw` reads through a raw pointer; `*raw = v` writes through one. Both
require `unsafe`:

```ntsc
var int x = 0
var &mut int w = &mut x
unsafe {
    var *mut int p = memory.raw_address(w)
    *p = 42
    say("" + *p)   // 42
}
```

Writing through `*const` is rejected at type-check time — the compiler
prevents mutation through a shared pointer even inside `unsafe`.

### Type preservation

Raw pointers keep their pointee type. You cannot cast between pointee
types:

```ntsc
var own Packet packet = alloc(Packet(7))
var &mut Packet write = &mut packet
unsafe {
    var *mut Packet raw = memory.raw_address(write)
    // var *mut int bad = raw   // type error: cannot cast *mut Packet to *mut int
}
```

## Safe references vs. raw pointers

Neutron has three levels of indirection, each with different guarantees:

| Form | Borrow-checked | Freed automatically | Use case |
| --- | --- | --- | --- |
| `&T` / `&mut T` | yes | no | Temporary access within a scope |
| `view` / `view mut` | yes | no | Borrows that outlive a single expression |
| `*const T` / `*mut T` | no | no | FFI, low-level storage, escaping the borrow checker |
| `own T` | yes | yes | Owning allocations |

Safe references (`&T`, `&mut T`) and views are borrow-checked:
the compiler tracks their lifetime and prevents use-after-free, dangling
pointers, and data races. Raw pointers bypass all of this — correctness
is the programmer's responsibility.

## Safety rules

Inside `unsafe`, the programmer must uphold these invariants:

- A `*const T` must not be used to write.
- A `*mut T` must point to valid, aligned memory for the duration of use.
- The referent must not be moved or destroyed while a raw pointer to it
  is live.
- A raw pointer must not outlive its referent (the compiler cannot enforce
  this — it is UB if violated).

Violating these rules is undefined behavior. The compiler does not insert
runtime checks inside `unsafe`.

## Limitations

- Raw pointers cannot be constructed as null.
- Raw pointers cannot be offset arithmetically.
- Raw pointers cannot be cast between pointee types.
- `unsafe` does not change runtime behavior — it only gates compile-time
  checks.
- `super` is lexed but not usable in expressions (not related to `unsafe`,
  but listed here as a known limitation).

## When to use `unsafe`

Most Neutron code never needs `unsafe`. Use it when:

- Implementing FFI bindings to C libraries.
- Building data structures that need to bypass the borrow checker
  (e.g. intrusive linked lists).
- Working with hardware or memory-mapped I/O.

In all other cases, prefer safe references, views, and `shared` handles.

## C FFI declarations

External C functions are declared at module scope and may only be called from
an `unsafe` block. The Neutron spelling keeps the normal function syntax:

```ntsc
extern "C" fun abs(int value) -> int

fun main() -> int {
    unsafe {
        say("" + abs(-7))
    }
    return 0
}
```

C-style declarations are also accepted in an extern block. These use C ABI
widths for primitive types:

```ntsc
extern "C" {
    int abs(int value)
    size_t strlen(*const char)
}
```

Declare the native libraries in `neutron.toml`:

```toml
[ffi]
search_paths = ["native"]
libraries = ["m"]
```

Search paths are resolved relative to the project directory unless they are
absolute. `libraries = ["geometry"]` searches those directories for the
platform library (`libgeometry.a`, `libgeometry.so`, or the corresponding
platform form) and passes it to the linker. Each library is linked as part of
the project build.
FFI declarations do not provide ownership or validity guarantees; pointer
preconditions and the foreign function's error conventions remain the
responsibility of the `unsafe` call site.

## Try it

Put the first pointer example inside `fun main()` and run it. It prints `100`.
Move the raw-pointer write outside the `unsafe` block and read the compiler's
diagnostic, then restore the block. Compare the example with a `view mut`
version that needs no raw pointer.

Next: [Rust adaptors](rust-adaptors.md) uses generated bridges to call a Rust
dependency from Neutron.
