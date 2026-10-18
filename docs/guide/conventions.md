# Leak-free Neutron conventions

Use this chapter after [Concurrency](concurrency.md), with moves, views,
classes, exceptions, and tests already familiar. It is a practical review of
how those features work together, including explicit cleanup for files,
sockets, and channels.

Neutron is designed for deterministic ownership, not garbage collection. The
compiler and runtime can reclaim owned values on every normal and exceptional
exit, but they can only do that when the program expresses one clear owner for
each heap value.

This guide is the practical style guide for writing Neutron that stays quiet
under debug leak detection. It complements the detailed [ownership
guide](ownership.md). The short rule is:

> Every heap value has one obvious owner, every borrow has a short scope, and
every external resource has an explicit close operation.

The examples below use the normal debug build. A clean program must produce no
`NTSC-W0002` memory-leak warning on stderr. Do not use release mode, leak
suppression, `quiet`, or an intentional process exit to hide an ownership bug.

## 1. Make ownership visible in names and signatures

Declare heap types in public function signatures. Use `view` when a function
only borrows and use an owned parameter when it consumes the value.

```neutron
// Borrow: the caller keeps the array.
fun total(view array[int] values) -> int {
    var int result = 0
    for (var value in values) {
        result = result + value
    }
    return result
}

// Consume and return: ownership moves through the function.
fun append(array[int] values, int value) -> array[int] {
    arrays.push(values, value)
    return values
}
```

Conventions:

- Put `view` on read-only parameters.
- Put `view mut` on parameters that mutate the caller's value without taking
  ownership.
- Use an owned parameter when the function stores, returns, or otherwise takes
  the value.
- Return an owned value when the caller should receive it.
- Prefer one ownership direction per function. A function that both borrows
  and conditionally consumes the same value is difficult to reason about and
  usually needs to be split.
- Give heap values descriptive names such as `request_body`, `lines`, or
  `config`; avoid aliases such as `a` and `b` that obscure which name owns the
  value.

The compiler rejects many ambiguous patterns, but clear signatures make the
correct pattern obvious before the checker has to explain it.

## 2. Move deliberately; copy deliberately

Assignment, owned parameter passing, and returning a heap value move it. A
moved-from name is dead until it is assigned a new value.

```neutron
var array[int] original = [1, 2]
var array[int] consumed = original
// original is no longer usable here.
```

If both places need ownership, say so with `copy`:

```neutron
var array[int] original = [1, 2]
var array[int] duplicate = copy(original)
arrays.push(duplicate, 3)
// original is still [1, 2].
```

Do not create aliases as a substitute for `copy`:

```neutron
var array[int] first = [1, 2]
var array[int] second = first       // move, not an aliasing copy
```

For classes, `copy(instance)` deep-copies owned fields. Use it only when two
independent owners are required; unnecessary copies add work and make the
ownership story harder to follow.

## 3. Keep borrows short and never store them as owners

Reading a heap field or element from a live owner produces a borrow. Use it
inline, bind it as a `view`, or materialize an owned copy.

```neutron
var array[string] names = ["Ada", "Grace"]

say(names[0])                    // short, statement-scoped borrow
view var first = names[0]        // borrow remains valid in this block
var string saved = copy(names[0]) // independent owner
```

This is not safe:

```neutron
var string saved = names[0]      // compile error: borrowed value cannot escape
```

Prefer this order:

1. Use the expression inline if it is needed once.
2. Use `view var` if several reads are needed while the source stays live.
3. Use `copy(...)` only when the value must outlive the source or be stored.

A `view mut` is exclusive. End its scope before moving, replacing, or
reassigning the source:

```neutron
var array[int] values = [1, 2]
{
    view mut var current = values
    current[0] = 9
}
// The mutable borrow ended; values can be moved or replaced now.
```

Do not return views, put them in arrays/classes, capture them in lambdas, or
send them to another thread.

## 4. Treat every temporary as a value with a lifetime

Neutron drops temporaries, but code is easier to verify when temporary values
are either used inline or immediately assigned to a clearly owned local.

Good:

```neutron
var array[int] result = arrays.range(0, 10)
say("count=" + arrays.length(result))
```

Also good when the temporary is only needed for one operation:

```neutron
say("first=" + arrays.range(10, 12)[0])
```

Be especially careful with:

- array and object literals passed to functions;
- concatenation and string conversion results;
- class instances returned from functions;
- field extraction from temporary class instances;
- tuple elements and destructured values;
- results of `arrays.clone`, `slice`, `sort`, `shuffle`, and similar
  functional operations.

If a temporary needs to be used more than once, name it and let the scope own
it. Do not manufacture a second owner by assigning a borrowed member into an
owned variable; use `copy` when necessary.

## 5. Reassign owners only with intentional replacement

Assignment to an owned variable, field, or array element releases what that
place owned before storing the new value. This is safe and useful, but it
means replacement should be explicit and easy to audit.

```neutron
class State {
    var string status
}

fun reset(view mut State state) {
    state.status = "ready"
}
```

The value expression is evaluated before the destination is replaced, so
self-assignment and same-container reads are supported:

```neutron
state.status = state.status
values[0] = values[1]
```

Avoid keeping another owner or long-lived view of the destination while
reassigning it. A live borrow and a replacement fight over the same lifetime
and will be rejected or become difficult to maintain.

## 6. Design classes with one owner for each field

A class owns its heap fields. Initialize fields in `init`, use `view` methods
for observers, and avoid storing aliases to the same class instance.

```neutron
class Document {
    var string title
    var array[string] lines

    fun init(string title) {
        this.title = title
        this.lines = []
    }

    fun line_count() -> int {
        return arrays.length(this.lines)
    }
}
```

Conventions:

- Initialize every heap field on every construction path, including fields
  that may otherwise receive a default empty value.
- Let the class instance own its fields; do not copy a field out unless the
  caller genuinely needs independent ownership.
- Use `view` receivers/parameters for read-only methods.
- Use `view mut` for methods that mutate an existing instance without moving
  it.
- Return a field from a temporary instance only when ownership is intentionally
  transferred:

  ```neutron
  var array[string] lines = make_document().lines
  ```

- Avoid assigning the same class instance to multiple owned names. If the
  object must be shared, use an explicit `shared` value or redesign the API.

An instance that is aliased in a way escape analysis cannot prove safe may be
left alive and reported as a leak rather than risk a double free. Treat that
warning as a design problem: remove the alias, use `copy`, use `view`, or use
`shared` explicitly.

## 7. Use arrays according to their ownership contract

Arrays own their heap elements. In particular, a string array stores its own
copy when pushed or assigned, and `arrays.pop` transfers the removed string to
the caller.

```neutron
var string text = "one"
var array[string] values = []
arrays.push(values, text)
// text remains independently owned; values owns its own string copy.

var string removed = arrays.pop(values)
```

Conventions:

- Drop or move the array normally; do not try to manually drop its elements.
- Use `copy` when copying an array that must own independent nested heap
  values.
- Remember that functional operations return fresh owned arrays. Bind or pass
  those results, and do not retain a borrowed element from the source as if it
  were a new owner.
- Prefer `arrays.clear` when reusing an array in place. It releases the old
  string elements while preserving the array owner.
- Keep loops ownership-stable. A value moved in one iteration must be
  reinitialized before the next iteration, or the compiler will reject it.

For a loop that creates heap values, keep the value inside the loop body so it
is dropped on `continue`, `break`, `return`, and exceptions:

```neutron
for (var path in paths) {
    var string contents = sys.read(path)
    if (strings.is_empty(contents)) { continue }
    say(contents)
}
```

## 8. Handle exceptions as ownership edges

Every `return`, `throw`, `break`, `continue`, `retry`, and caught exception is
a cleanup edge. Write code so locals have one owner on each edge.

```neutron
fun read_required(string path) -> string {
    var string text = sys.read(path)
    if (strings.is_empty(text)) {
        throw "empty file"
    }
    return text
}
```

The owned `text` is reclaimed when the throw path leaves the function, or is
moved to the caller on the return path. Catch bindings own their exception
message for the duration of the handler:

```neutron
try {
    do_work()
} catch (err) {
    say(err)
}
```

Use `throw err` to transfer the same message while rethrowing. Do not create
an unnecessary copied message at every handler. Keep `finally` blocks focused
on non-owning bookkeeping; moving or replacing values there makes every exit
edge harder to audit.

When adding a new error path, test all of these separately:

- normal fall-through;
- `return` before the end;
- `throw` before the end;
- caught throw and handler completion;
- rethrow from a handler;
- `break` and `continue` inside loops;
- a constructor that throws after initializing only some fields.

## 9. Be explicit with shared values and concurrency

`shared T` is an explicit aliasing boundary. It is not a replacement for
ordinary ownership and it does not make every operation free.

```neutron
shared array[int] queue = [1, 2]
var view array[int] snapshot = queue
```

Conventions:

- Use `shared` only when multiple owners really need the same pointee.
- Keep the shared handle alive while any `view` of its pointee is used.
- A `go` call moves owned heap arguments into its goroutine. Pass `copy(...)`
  if the caller needs an independent value afterwards.
- Send scalars and channel handles freely. Views and `shared` values cannot
  cross a thread boundary.
- `process.spawn_thread` does not accept owned heap payloads, including
  `copy(...)` results. Pass a channel handle and send data with
  `collections.channel_send` instead; that API copies string messages.
- Close channels explicitly with the standard-library close operation when
  the protocol is complete.
- Do not leave detached goroutines waiting on resources that the main program
  has abandoned. Prefer a completion signal or cancellation protocol.

For every async operation, decide who owns the future and who owns its
result. Await it, explicitly drop/cancel it through the API, or make sure the
runtime's shutdown path can reclaim it. Do not keep a future in a discarded
local.

## 10. Close external resources explicitly

Registry leak detection tracks Neutron handles, but operating-system resources
also need deterministic cleanup. Always close what you open:

```neutron
var file = io.open(path, "r")
try {
    say(io.read_all(file))
} finally {
    io.close(file)
}

var socket = net.tcp_connect(host, port)
try {
    net.send(socket, "ping")
} finally {
    net.close(socket)
}
```

The same convention applies to TCP listeners, accepted streams, UDP sockets,
file locks, and other opaque module resources. Do not rely on process exit to
close them, especially in long-running programs or loops.

For resource-producing helper functions, document the ownership transfer in
the function name or signature and return the resource only after successful
construction. On failure, do not return a partially initialized resource.

## 11. Rust adaptors and unsafe boundaries

Generated Rust adaptors follow the same ownership rule. A Rust return that is
mapped to a Neutron string, array, or object becomes an owned Neutron value;
callers must consume, store, return, or `copy` it. Borrowed Rust parameters are
call-local views and must not be retained by the Rust function.

When using a Rust adaptor:

- read the generated export manifest with `ntsc cargo doc <crate>`;
- check whether a return is an owned object, an option, or a result that throws
  on error;
- do not use a borrowed Rust value after the call unless the generated API
  exposes it as a valid Neutron view;
- do not add a wrapper merely to hide an ownership mismatch;
- keep unsupported unsafe/lifetime-sensitive signatures unsupported until a
  sound shared bridge exists.

When implementing an adaptor or runtime function in Rust, prefer safe Rust
ownership (`String`, `Vec`, `Box`, `Arc`, `Option`, and `Result`) and keep any
raw-pointer block tiny, documented, and paired with an explicit release path.
A result handle must have exactly one destination on success, error, and
cancellation paths.

## 12. Test the ownership contract, not just the output

A program that prints the right output can still leak. During development:

```bash
ntsc build                 # debug build; leak warnings enabled
ntsc test                  # run project tests in debug mode
cargo test -p ntsc-codegen --test leak_detection_e2e
cargo test -p ntsc-runtime
```

For every new heap-producing feature, add at least one regression that:

1. runs the normal path;
2. repeats the operation in a loop;
3. discards or replaces the result;
4. exercises an error/exception path;
5. checks stderr for the absence of `NTSC-W0002`.

For resources, also test early `return`, `throw`, `break`, `continue`, and
cancellation. For arrays and classes, test field replacement and nested owned
values. For async operations, test cancellation before completion and dropping
a completed-but-unread result.

Use this checklist during review:

- [ ] Every heap value has one obvious owner.
- [ ] Every `view` has a shorter or equal lifetime than its source.
- [ ] Every required second owner uses `copy` or `shared` explicitly.
- [ ] Every loop iteration drops or transfers its temporary values.
- [ ] Every error and early-exit path has the same ownership balance as success.
- [ ] Every opened file/socket/channel/resource is explicitly closed.
- [ ] Every future is awaited, cancelled, or reclaimed at shutdown.
- [ ] Debug execution produces no `NTSC-W0002` warning.
- [ ] Release success is not used as evidence that debug ownership is correct.

If a convention conflicts with the compiler's ownership diagnostic, fix the
ownership design rather than suppressing the warning. Leak reports are there
to expose a lifetime bug before it becomes a use-after-free or a permanently
retained resource.

## Try it

Take a program from an earlier chapter and run it in debug mode with
`ntsc run`. Identify the owner of each heap value and add a test for an early
return or an error path. If you open a file or socket, check that the resource
is explicitly closed on both success and failure.

Next: [Unsafe and raw pointers](unsafe-and-pointers.md) explains the boundary
used for low-level code and C interop.
