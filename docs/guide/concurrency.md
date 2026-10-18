# Concurrency

Before continuing, finish [Shared values](shared-values.md) and be comfortable
with functions, ownership, and error handling. **Concurrency** lets multiple
pieces of work make progress without waiting for each one to finish before
starting the next.

There are three everyday tools:

- `async fun` declares work that can pause; `await` waits for its result.
- `go` starts an async function as a **goroutine**, independently of its caller.
- `chan[T]` carries typed messages between those pieces of work.

Start with the complete examples below. Scheduler details, OS threads, and
awaitable network I/O are later sections.

## Start with async and await

Replace `src/main.nt` with this program and run `ntsc run`:

```ntsc
async fun answer() -> int {
    await async.sleep(10)
    return 42
}

async fun main() -> int {
    var int value = await answer()
    say("answer: ${value}") // answer: 42
    return 0
}
```

`await` pauses the current coroutine until the operation finishes. Its result
becomes `value`. The two functions are marked `async` because they can pause.
This example waits for one result; it does not spawn independent work yet.

## Coordinate a worker with a channel

Use a channel to wait for a worker's result, rather than guessing how long the
worker will take:

```ntsc
async fun worker(chan[int] results) {
    await async.sleep(10)
    42 |> results
    close(results)
}

async fun main() -> int {
    var chan[int] results = chan.new(1)
    go worker(results)
    value <| results
    say("worker result: ${value}") // worker result: 42
    return 0
}
```

`go worker(results)` starts the worker. `42 |> results` sends its answer;
`value <| results` receives it and waits when needed. The worker closes the
channel after its last send. The root coroutine has received the answer before
it returns, so the example does not depend on a sleep in `main` for completion.

A blocked channel operation or an `await` parks the goroutine, not the thread:
the scheduler thread picks up other work, so tens of thousands of in-flight
I/O operations cost tens of thousands of cheap tasks — not OS threads.
CPU-bound goroutines spread across the whole pool.

## Goroutines

`go worker(args)` runs an async function on the scheduler; `go { ... }` runs an
inline block. Spawn is fire-and-forget: `go` returns nothing and never blocks
the caller. When `main` returns, outstanding goroutines are abandoned (as in
Go). Coordinate completion with a channel when the work must finish first;
a fixed sleep is not a reliable completion protocol. Abandonment is
leak-free: the runtime reclaims an abandoned goroutine's future at shutdown,
releasing every registry value it still owned (strings, arrays, armed sleep
deadlines, channel handles), so the debug leak report stays clean. There is
no goroutine `join` in the language yet — completion is coordinated with
channels; the runtime's join machinery exists for embedders, and it is what
keeps a goroutine's result and uncaught exception readable until its handle
is dropped.

Fan-out means starting one worker for each item. This snippet assumes an
existing `urls` array and an `async fun download(...)` declaration:

```ntsc
for url in urls {
    go download(url)
}
```

## Channels

`chan[T]` is a typed channel. The operators show the direction data moves — the arrow points where the
value goes: `|>` pipes the value on its left into the channel on its right;
`<|` feeds the variable on its left from the channel on its right.

```ntsc
var chan[int] jobs = chan.new(10)   // buffered channel, capacity 10

value |> jobs                       // send: moves value into jobs (parks when full)
x <| jobs                           // receive: binds fresh x from jobs (parks when empty)
close(jobs)                         // no more sends; receivers drain, then get zero

for v in jobs {                     // receive until the channel is closed and drained
    handle(v)
}
```

Rules:

- `chan.new(capacity)` creates the channel; the element type comes from the
  annotated variable (`var chan[string] jobs = ...`).
- A send **moves** the value into the channel: the sender cannot use it
  afterwards (using it is `NTSC-E0501`). There is no alias and no double free.
- A receive binds a fresh variable that owns the value and frees it at scope
  exit.
- Sends and receives are legal only as statements at the top level of an
  `async fun` body (they are suspension points, like `await`); a blocked
  operation parks the goroutine instead of the thread.
- `close(ch)` forbids further sends. Values already queued are still received;
  when the buffer is drained, receives complete with the zero value and
  `for v in ch` exits.
- A channel handle may be passed to `go` (and moved through channels) freely:
  the handle is shared and the channel state lives behind the runtime's lock.
- A `chan[T]` whose element is a **class** is currently limited: naming it in an
  `async fun` parameter breaks that function (the class name stops resolving
  inside it), and a class value received by `for v in ch` miscompiles a field
  read. Carry `string` or `int` messages and encode anything structured — for
  example a JSON object built with `json.escape_string` — or keep the class work
  in a synchronous function that receives the channel as a local.

## Memory safety across goroutines

The ownership checker classifies every value that crosses into a goroutine:
arguments to `go fn(...)` and free variables captured by `go { ... }`.
The table below describes `go`; the OS-thread API has an additional heap
payload restriction, described after the table.

| Value crossing into a goroutine | What happens | Why it is safe |
| --- | --- | --- |
| Scalars (`int`, `float`, `bool`) | Copied | The goroutine shares nothing with the spawner. |
| Safe handles (channels, files, sockets, threads) | Shared | The resource lives in the mutex-guarded runtime registry; the handle itself is an `int`. The spawner keeps ownership and stays free to use the channel. |
| Function references | Copied | Code is immutable; a lambda captures nothing. |
| Owned heap values (`string`, `array`, `object`, class instances) | **Moved if bare, copied if borrowed, transferred if fresh** | A bare variable argument is moved with its owning slot (`NTSC-E0501` afterwards, so exactly one owner runs the goroutine). A borrowed read (`names[0]`) is not the spawner's owner, so the goroutine gets an independent copy — its future's drop can never reclaim the source container's entry. A fresh construction has no other owner and moves as-is. |
| `view` / `view mut` | **Rejected** (`NTSC-E0501`) | A borrow may outlive the spawner's frame; a goroutine cannot hold one. Move or copy the value instead. |
| `shared` | **Rejected** (`NTSC-E0501`) | The reference count is not synchronized across threads; copies would race on both the value and its count. Send the data through a channel instead. |

For `value |> ch`, the value moves into the channel; for `value <| ch`, it
moves out to the receiver. A heap value crossing a typed channel has exactly
one owner at every instant.

Unlike `go`, `process.spawn_thread` rejects owned heap payloads: it does not
transfer ownership into a goroutine future. Passing `copy(value)` does not
change that restriction. Give the OS thread a channel handle and send string
data through `collections.channel_send`, which copies messages synchronously.
Both APIs reject views and unsynchronized `shared` values.

## OS threads

`process.spawn_thread(body, arg)` starts a real OS thread — the escape hatch
for long-running native work, FFI, or tight loops that should not be
preempted. The body is a lambda with a single parameter; the argument crosses
the thread boundary under the rules below. The returned handle is what
`process.thread_join` waits on:

```ntsc
var rx = collections.channel(4)
var tx = collections.channel_sender(rx)
var producer = process.spawn_thread(fun(int tx) {
    collections.channel_send(tx, "ping")
    collections.channel_send(tx, "pong")
    collections.channel_close(tx)
}, tx)

process.thread_join(producer)
```

A goroutine is not an OS thread: prefer `go` for concurrency, and reach for
`spawn_thread` only when something must genuinely block a core.

### The legacy `collections.channel` family

`collections.channel(capacity)`, `channel_sender`, `channel_send`,
`channel_recv`, `channel_try_recv`, and `channel_close` still work for
OS-thread code (they copy string messages and block the calling thread).
New goroutine code should use `chan[T]`, which is typed and parks instead of
blocking.

### Scaling benchmarks

`benchmarks/run_concurrency.py` measures the worker pool directly: 10k–100k
goroutines on a fixed pool, CPU fan-out across cores, blocked-on-`sleep`
goroutines freeing workers, and concurrent HTTP fan-out — each side by side
with the equivalent Go goroutine program. Results land in
`benchmarks/results_concurrency.md`.

### Threading rules

NTSC has no `Send`/`Sync` traits to implement. The compiler classifies every
value handed to another thread, and the classification is total — there is
nothing to opt into and no escape hatch. A value crosses a thread boundary
when it is an argument to `go`, a capture of a `go { ... }` block, an argument
to `process.spawn_thread` (the payload), or the message of
`collections.channel_send`. `go` transfers owned payloads; `spawn_thread`
rejects them; the legacy channel API copies string messages. Violations are
ownership errors (`NTSC-E0501`) reported at compile time:

```ntsc
fun main() {
    var xs = [1, 2]
    process.spawn_thread(worker, xs)
    // error[NTSC-E0501]: cannot pass `xs` to process.spawn_thread: an owned
    // heap value would cross as a raw handle that both threads then alias
    // without synchronization, ...
}
```

A user-defined type is thread-safe exactly when its values are: a class
instance is an owned heap value, so it follows that row of the table. There is
no way to declare otherwise, which is why the rules are exhaustive.

`await` is not a thread boundary between goroutines: a suspension parks the
current goroutine and the scheduler resumes it (possibly on a different
worker), but values live in the goroutine's own future the whole time. Values
that cannot cross into a `go` are still free to live across an `await` in the
goroutine that owns them.

## Async / await

An `async fun` is a coroutine that suspends at `await` points and resumes
later. Goroutines and async futures are the same machinery: `async fun main`
runs as the root goroutine on the scheduler, and `await` parks the current
goroutine until the awaited future completes.

```ntsc
async fun fetch(string url) -> string {
    await async.sleep(10)
    return "data for " + url
}

async fun main() -> int {
    var first = await fetch("a")
    var second = await fetch("b")
    say(first)
    say(second)
    return 0
}
```

### Rules

- `async fun main` is supported and drives the root future; its `int` result
  becomes the process exit code.
- Local variables survive suspension: values written before an `await` are
  intact afterwards.
- Await results flow back into the calling coroutine after the child future
  completes.
- `try`, `throw`, and `retry` work inside async bodies. A `throw` sets
  the thread-local pending-exception flag and propagates to the caller
  after `wait_any`/`wait_all` returns. This enables the timeout pattern:
  racing an operation against a sleep that throws a catchable error.

### The `async` module

`async.sleep(ms)` pauses the coroutine for approximately `ms` milliseconds:
the future reports "pending" until the deadline passes, and the scheduler
re-polls it on a millisecond quantum.

Never call the blocking `time.sleep(ms)` (or `sys.sleep(ms)`) inside an
`async fun`: it holds the driving worker thread for the whole duration
instead of parking the goroutine, and a `while (true)` poll loop around it
wedges that worker inside a single endless poll. Always
`await async.sleep(ms)` from async code; the goroutine parks on a timer and
the worker is free until the reactor fires the deadline.

### Awaitable I/O

Blocking stdlib calls have awaitable variants that free the scheduler thread
while the goroutine waits. They come in two flavors:

- **Reactor-backed** (`net.accept_async`, `net.recv_line_async`): the wait is
  an I/O readiness wait handled by the reactor. Each poll tries a
  non-blocking syscall first, and the goroutine parks on socket readiness
  until a client connects or bytes arrive. A server built this way serves any
  number of slow clients on a fixed thread pool.
- **Offloaded** (`http.*_async`): the blocking request runs on a bounded
  worker-thread pool, and the calling goroutine parks until the result is
  ready.

```ntsc
use net
use fmt

async fun handle_client(int sock) {
    var string line = await net.recv_line_async(sock)
    net.send_line(sock, "ok:" + fmt.i64_to_str(fmt.to_int(line) + 1))
    net.close(sock)
}

async fun accept_loop(int listener) {
    var int sock = await net.accept_async(listener)
    go handle_client(sock)
    go accept_loop(listener)   // keep accepting while this client is handled
}

async fun main() -> int {
    var int listener = net.tcp_listen(8080)
    say("listening on " + fmt.i64_to_str(net.local_port(listener)))
    go accept_loop(listener)
    await async.sleep(60000)   // serve for a while, then exit
    net.close(listener)
    return 0
}
```

`http.get_async` offloads the request to the pool the same way, so a
goroutine waiting on a response never holds a scheduler thread hostage:

```ntsc
var resp = await http.get_async("https://example.com/")
```

### Inline async blocks

Instead of writing a separate `async fun`, you can inline a block directly at
the `await` site:

```ntsc
async fun main() -> int {
    var result = await async {
        await async.sleep(50);
        return 42
    }
    say(result)  // 42
    return 0
}
```

An inline `async { ... }` block compiles to an anonymous future. It cannot
take parameters and must have a consistent return type across all paths.
A `go { ... }` block is the same machinery spawned onto the scheduler instead
of awaited; unlike an inline async block, it captures the local variables it
references (moved or shared per the table above).

### `for await`

`for await x in producer` iterates over `producer` (an array or other
iterable). It is syntactically equivalent to `for (var x in producer)` today
but signals intent to support streaming async iteration in the future.

```ntsc
async fun main() -> int {
    var items = ["a", "b", "c"]
    for await x in items {
        say(x)
    }
    return 0
}
```

## How it runs

NTSC multiplexes cheap stackless goroutines onto a multi-threaded,
work-stealing scheduler. A reactor handles readiness for supported I/O.
The thread pool can keep doing other work while a goroutine is parked:

```text
  Goroutines (go / async)   ── thousands, cheap, stackless
                 │
        ┌────────┴─────────┐
        ▼                  ▼
  Work-stealing        Reactor (non-blocking I/O)
  scheduler            sockets / timers / channels
  OS-thread pool       readiness loop; never blocks a thread
  (= CPU count)
```

OS threads remain an escape hatch for blocking native work. The older
`collections.channel` functions support that model; new goroutine code should
use typed `chan[T]` channels.

## Try it

Change the worker's result from `42` to another integer and run the complete
channel program. Then make the worker's delay longer. The receive in `main`
should still wait for the correct result without an extra sleep.

Next: [Leak-free conventions](conventions.md) reviews ownership and cleanup
across these features, including cancellation and external resources.
