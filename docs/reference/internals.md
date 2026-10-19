# Internals

This document describes how the compiler lowers NTSC to native code. It is
aimed at contributors and at users who need to understand the ABI.

## Pipeline

Compilation proceeds through a fixed set of stages, each owned by one crate:

```
source (.nt)
  -> ntsc-lexer     tokenize with automatic semicolon insertion
  -> ntsc-parser    build the AST
  -> ntsc-typeck    name resolution, type checking, ownership, lints
  -> ntsc-codegen   LLVM IR, optimization, object file
  -> link against libntsc_runtime.a  (via the system C compiler)
```

For a project, `ntsc-build` first loads the entry file and the transitive
closure of file imports, merging them into one program and recording per
span the file each token came from (for diagnostics). Modules are parsed in
parallel.

Name resolution runs before code generation so that undefined-name errors get
the `NTSC-E0101` diagnostic and "did you mean" suggestions. Lint warnings are
reported but do not fail the build.

## LLVM lowering

### Object model

No pointer crosses the ABI. Every owned heap value lives in the runtime's
handle registry under an opaque `i64` key, and generated code passes only those
keys. The registry is a map of `i64 -> Handle` sharded 64 ways by id, so
concurrent handle traffic from different workers does not serialize on one
mutex; every operation takes exactly one shard and never holds it while
calling back into the registry. The runtime crate contains no `unsafe` code at
all. `Ty::View(T)` has the same representation as `T` — a view is a borrowed
handle, not a distinct value.

The handle kinds are:

- **Strings**: an owned `String`; `ntsc_string_drop` removes it.
- **Arrays**: an element vector plus its element size and a `string_elements`
  flag.

```
ArrayData
  elem_size        : bytes per element
  string_elements  : elements are owned strings
  elements         : Vec<i64> — raw bits for scalars, handles for strings
```

A handle names the entry, not its storage, so it stays valid across
`arrays.push` however the element vector reallocates, and remains valid when
passed between functions.

`string_elements` marks an array whose elements are owned strings. Such arrays
deep-copy a string on insert (`arrays.push`, index-assignment), deep-copy every
element in array-producing operations (`clone`, `deep_clone`, `slice`,
`reverse`, `fill`, `clear`), free each element exactly once when the array is
dropped or an element is removed, and hand ownership of the removed element to
the caller on `pop`. All other arrays store plain bytes and none of this
applies. Codegen sets the flag from the element type: string and `Any` element
arrays own their strings; an empty `[]` literal adopts the representation of
its declared destination type (`var array[int] x = []` stores raw scalars).

- **Class instances**: heap structures with one field slot per field, or stack
  slots when escape analysis decides the instance cannot escape.
- **Shared boxes**: a counted entry holding a single inner handle.
  `ntsc_shared_new` adopts an owned handle; `ntsc_shared_retain`/
  `ntsc_shared_release` adjust the count, and the last release removes the box
  and returns the inner handle for the caller to drop.
- **Futures**: the state machine of an `async.sleep(ms)` future.
- **Goroutines**: a `go`-spawned task; the wrapper carries the scheduler's
  core id so `join`/drop can reach the goroutine.
- **Channels**: a `chan[T]` core, reference counted like `Shared` so copying
  the handle (`var b = a`, `go f(ch)`) keeps the channel alive for peers; the
  last handle drop reclaims the core and its buffered elements.
- **Reactor registrations**: a timer or descriptor-readiness interest
  (`ReactorReg`/`AsyncIo`), dropped with the future that armed it.
- **Offloaded futures**: an `http.*_async`/`process.*` call's state machine,
  holding the blocking closure and the op id of the pool job.
- **Opaque module resources**: values a stdlib module owns (files, sockets)
  stored as `Box<dyn Any + Send>` and readable only at the type the
  owning module registered.

### Handle validity and kind safety

Handles are issued by a monotonic counter and are **never reused**: once a
handle is dropped its id stays unknown for the life of the process, so a stale
handle can never name a value some later allocation registered. This is what
makes the guarantees below decidable rather than best-effort.

Every operation resolves its handle before doing anything, and every
*destructive* operation additionally checks the kind:

| Handle it is given | What happens |
| --- | --- |
| `0` (null) | "no value" — the operation is a no-op and reports its safe failure |
| unknown, or already consumed | same as null; no other entry is touched |
| a live handle of the wrong kind | left untouched; the operation reports its safe failure |
| a live handle of the right kind | the operation runs |

So `ntsc_string_drop` on an array handle, `ntsc_array_drop` on a string handle,
`ntsc_shared_release` on a plain string, and `ntsc_async_sleep_drop` on any
value handle are all no-ops that leave the value they were given intact. A
second drop of the same handle is a no-op for the same reason — the first drop
removed the entry, and nothing else was ever issued that id. Reads work the same
way: an array handle has no string length, a shared box is not its own contents,
and an opaque file resource cannot be read as either.

Because a refused operation cannot return an error out-of-band, each returns the
documented safe failure for its result type:

| Result type | Safe failure |
| --- | --- |
| a handle | `0` (null) |
| a count, length, or boolean | `0` |
| a search index or character code | `-1` |

Nothing panics and nothing treats an unknown nonzero handle as a license to
touch another entry. Faults that *are* the program's error rather than an
invalid handle still throw a catchable exception: indexing a real array out of
bounds throws `array index out of bounds` on reads and writes alike.

Two counters, `LIVE` and `PERMANENT`, track live and permanently-registered
(compile-time constant) entries; `LIVE + PERMANENT == map.len()` is asserted in
the registry's own tests, which is how a wrong-kind or duplicate drop silently
losing an entry would be caught. `crates/ntsc-runtime/tests/handle_validity.rs`
pins all of the above through the public ABI with no `unsafe` code.

### Moves and drops

The generated code decides at compile time who owns each value:

- A bare-variable argument to an owned parameter is a move: the source slot is
  nulled so it is not dropped at its own exit, and the callee drops it.
- A view parameter borrows; the caller keeps ownership. Taking a view of an
  instance, or of one of its fields, therefore leaves the instance's owned
  fields to be reclaimed at the owner's scope exit.
- A field's declared initializer is stored at construction under the same
  ownership rules as `this.field = value`, so the instance owns it and the
  class drop thunk reclaims it.
- `copy(expr)` emits a deep copy and the result is a fresh owned value. A
  borrow (`view`) is materialized from its inner type first, so the copy never
  aliases the value it was copied from.
- "Fresh" results (literals, `copy`, string concatenation, `arrays.*` calls)
  are dropped by the statement that produced them; owned values stored into a
  variable are dropped when the variable's scope exits.
- `emit_drop_all_owned` runs the drop thunk for every owned slot at function
  exit (on the normal path and on the exception path).
- A block that ends inside a loop body releases the owned values it bound
  immediately instead of deferring them to function exit: the block
  re-executes every iteration, so deferring would free only the last
  iteration's values and leak the rest. The slots are nulled, so a later
  `break`/`continue` through `drop_loop_locals` and the exit-time drop are
  safe no-ops. The same rule applies on each loop back edge, restricted to
  names the loop itself declared: a name visible before the loop keeps its
  final value for the code after the loop and is never touched there.

### Exceptions

Exceptions use a return-check model, not stack unwinding. `ntsc_throw` stores
the message handle in a thread-local pending slot and returns `0`; generated
code checks `ntsc_exception_pending()` after every call that can throw and
branches to its exception-return path when the flag is set, running the drops
for everything live on that path on the way out. A `catch` handler takes the
message with `ntsc_exception_take_message`, which clears the flag;
`ntsc_rethrow` re-arms it (used after a `finally` that ran with a clean flag).
An exception still pending when `main` returns is reported by
`ntsc_runtime_shutdown` through the same `ntsc_uncaught_exception` path,
which prints the message to stderr and exits with a failure code.

Standard library functions report failures by calling `throw_str`, which
constructs a `module.func: detail` message and calls `ntsc_throw`.

### Async lowering

An `async fun` is lowered to a poll-based state machine. The function body is
split into segments at each `await`; each suspension splits the body and
stores the continuation state plus live locals in a future struct. The runtime
executor (`ntsc_async_run`) polls the root future; a poll that reports
"pending" yields the thread and is re-polled later. Child futures are
scheduled with `ntsc_async_push`. `async.sleep(ms)` is implemented as a future
that arms a deadline on its first poll and completes once the clock passes it.

### Exception and async interaction

`try`, `throw`, and `retry` work inside async bodies. A `throw` sets the
thread-local pending-exception flag and propagates to the caller after
`wait_any`/`wait_all` returns.

## The task runtime

Goroutines, `chan[T]`, and the awaited I/O forms all run on one substrate in
`crates/ntsc-runtime/src/ntask`: an M:N scheduler that multiplexes stackless
goroutines onto a fixed pool of OS threads (one per CPU), plus a reactor
thread for timers and descriptor readiness. There is no per-goroutine stack —
a goroutine is a poll-based future, so it can be torn off one worker and
picked up by another, which is what spreads CPU work across the pool.

### Scheduler

Each worker owns a lock-free run queue (`ntask/runqueue.rs`), modeled on Go's
`runq` and Tokio's `multi_thread::queue`: a 256-slot ring with an atomic head
and tail plus a single-slot LIFO cell (`next`, Go's `runnext`). The owner is
the only producer, so a push is two loads and a release store; owners and
thieves consume the head by compare-exchange. The LIFO slot hands the most
recently readied goroutine straight back to the same worker, keeping a channel
ping-pong on one core with its state in cache.

A full ring overflows half its contents to the shared ready queue (one global
lock acquisition per 128 goroutines, Go's `runqputslow`), and an idle worker
steals half of a peer's ring, scanning victims from a rotating offset. Workers
refill from the shared queue in batches bounded by the worker count, so one
worker cannot swallow a burst the others could run in parallel; the shared
queue is also polled every 61st tick for fairness (Go uses the same interval).

A worker parks when its queue runs dry: it spins briefly (counted as
"searching" so concurrent spawns skip their wakeup), re-checks every queue as
the last searcher, then sleeps on a condition variable. Whether a goroutine
became runnable is one packed atomic (`searching` + `unparked` counts), so the
wake decision is a single load; a spawn wakes a peer only when nobody is
already hunting and someone is actually parked.

`go f(x)` from a worker pushes straight onto that worker's queue. Detached
spawns (`go` with the handle discarded) are batched: up to 64 goroutines are
registered under one global lock acquisition and enqueued together, so a
fan-out loop costs a fraction of the per-child lock traffic. Spawns from
outside the pool (the main thread before `await`) register and queue in one
critical section.

A worker drives a goroutine by loading its suspension state into thread-local
storage, running the poll function without the global lock, then re-locking
once to flush the state and apply the wait target. Suspension targets
(`Park`): cooperative yield (requeue on the LIFO slot), channel send/receive,
timer deadline, descriptor readiness, sibling join, or offloaded job.

### Channels

`chan[T]` is a bounded ring of `i64` slots (raw scalars or owned handles for
heap element types) plus parked-sender and parked-receiver queues, all under
the global mutex — so a send that finds room, a handoff to a parked receiver,
or a close that releases waiters is one atomic critical section and cannot
lose a wakeup. An unbuffered send parks the sender with its value; a receive
takes it directly. `close` wakes parked receivers to drain what is buffered
(then see the zero value) and releases parked senders.

Channel element ownership is tracked end to end: a buffered owned element
belongs to the channel, transfers to the receiver on receive, and returns to
the runtime if never received — `close` releases un-received owned elements,
a dropped channel releases its buffer, and a goroutine dropped while parked
with an owned send/receive value in flight releases that too. A leaked
goroutine therefore cannot leak the channel values it held.

### Offload pool

Blocking work that cannot be parked on the reactor — the `http.*_async`
requests — runs on a small pool of standalone threads (2–8). The calling
goroutine parks on an op id; the pool thread completes the op with the result
handle and the scheduler requeues the waiter. A blocked HTTPS request
therefore costs no scheduler thread. The same op machinery backs the
`ntsc_async_process_*` ABI (awaited child processes), which the runtime
implements but the `await` front-end does not lower yet.

### Reactor

One background thread fires timers and watches descriptor readiness so
workers never block on I/O. Timers live in a deadline-ordered map; the
reactor computes its wait timeout from the next deadline. Readiness uses
`poll(2)` on Unix and `WSAPoll` on Windows (sockets only, so the wake channel
is a loopback TCP socket pair rather than Unix's self-pipe; a wake write from
any thread unblocks the wait). Interests are registered when a goroutine
parks on a socket and detached when its future drops, so the reactor never
polls a closed descriptor; a registered readiness wakes every goroutine
parked on that io core.

`net.accept_async` and `net.recv_line_async` use this directly: each poll
tries a non-blocking syscall first (a ready socket costs one syscall with no
scheduler involvement) and parks on readiness only when it reports
`WouldBlock`. Sockets are created with `TCP_NODELAY`.

### Join and abandonment

A joinable goroutine stays addressable until its wrapper handle is dropped,
even if it completes first; a detached goroutine (handle discarded) is
reclaimed as soon as it finishes. Joining — the synchronous `ntask_join`
bridge, used by embedders and tests rather than by generated code — transfers
the goroutine's result and, if its body threw, the pending exception to the
joining thread, which re-raises it; an uncaught throw in a goroutine is
therefore observed instead of vanishing with the worker thread. The language
surface has no goroutine `join` yet: NTSC programs coordinate completion with
channels.

At shutdown (`ntsc_runtime_shutdown`), goroutines that were never joined or
driven are reclaimed: their futures run their drop (releasing armed sleeps,
captured arrays and strings, channel handles), and their in-flight owned
values are released. Abandoning background goroutines at `main` exit is
therefore leak-free, matching Go's `go` semantics without Go's leak on the
debug report.

### Scheduler statistics

Setting `NTS_SCHED_STATS` in the environment prints per-worker counters at
shutdown — polls, steals, busy/spin/park time, and idle share — to diagnose
load imbalance. The counters are relaxed atomics on the worker's own slot and
cost nothing when the variable is unset.

### Validation limits

The scheduler's functional tests cover handoff, parking, reactor wakeups, and
abandonment, and `scheduler_stress_handles_concurrent_channel_handoffs` adds a
high fan-out channel stress case. Some end-to-end suites intentionally use
wall-clock sleeps for slow clients and server-start windows, so they remain the
most likely source of a transient CI failure. The normal CI gate does not run
ThreadSanitizer or loom; the opt-in `.github/workflows/tsan.yml` workflow runs
the scheduler stress test repeatedly under ThreadSanitizer on Linux.

## Optimization

Debug builds skip optimization and emit no optimization passes. Release builds
run the module through LLVM's `opt` with a fixed pass pipeline:

```
mem2reg, instcombine, simplifycfg, sccp, dce, gvn
```

Verification is enabled for each pass. Loop unrolling and vectorization are
disabled to keep IR generation deterministic. In particular, `mem2reg`
promotes the alloca-based variable slots into SSA registers; without the pass
every loop induction variable stays on the stack (a measured roughly 3% versus
70% relative speedup).

Every emitted module is verified before the object file is written, and the
compiler dumps the IR if verification fails.

## Escape analysis

Before emitting a class instantiation, the compiler analyzes whether the
instance can escape the function. If it cannot (and the class has no `init`,
since a constructor observes `this`), the instance is placed in an alloca
instead of calling the allocator. The escape test is conservative:

- returning the instance escapes it,
- storing it where the analysis cannot track it escapes it,
- a single member read or index through a bare base is safe; chaining further
  is only rejected for the stack-allocation question, since a deeper read still
  hands out no ownership,
- a grouped object base is treated as an escape.

The decision is invisible to program behavior.

The same walk answers a second question with the opposite default: whether the
scope owns an instance's fields and must run its drop thunk at exit. Refusing a
candidate there leaks, so a read that only borrows — a view, or a chained
member/index access — keeps the instance in the field-drop set.

## Test harness

`test name { ... }` compiles to a function `test_<name>`. In test mode the
user `main` is replaced by a generated harness that runs each `test_<name>`
inside an exception frame, prints `PASS name` or `FAIL name: message`, and
exits non-zero if any test fails.

## Linking

`link_binary` invokes the system C compiler on Unix hosts (`cc` with the
object file, the runtime archive, `-lm`, and on non-macOS hosts `-lpthread`
and `-ldl`). On Windows it prefers the `ld.lld` bundled next to the ntsc
executable (MSI installs; links via the MinGW emulation with the bundled
import libraries and the GNU-flavoured runtime), then falls back to MSVC
`link.exe` and MinGW `gcc`. The MSVC path names the Windows system libraries
and the `/MD` CRT set (`ucrt`, `vcruntime`, `msvcrt`) explicitly: a Rust
staticlib carries no CRT default-lib directives of its own, and rustc emits
them only when it drives the final link itself. Object and archive names are
host-dependent: `.o`/`.obj`, and `libntsc_runtime.a` versus
`ntsc_runtime.lib`, chosen to match the linker in use.

`host_triple()` and `with_executable_extension()` let the build produce
objects and binaries the local linker can consume, so tests and `ntsc init`
work on Linux, macOS, and Windows.

## Leak detection

Leak detection falls out of the registry: an entry that is never removed is a
leak. `ntsc_runtime_shutdown(report)` runs after `main` returns, and when
`report != 0` (debug builds) and any non-permanent entries are still live it
prints:

```
NTSC WARNING: Memory leak detected! N registry object(s) leaked.
```

String literals and other compile-time constants are registered as permanent
and excluded from the count. Release builds pass `report = 0` and stay silent.
Runtime assertions and panics write `NTSC PANIC: message` to stderr and abort.

## Every heap allocation and its free

Every heap allocation site is statically paired with exactly one owning drop
path. A leak can only happen when the runtime machinery frees an object it
borrowed instead of owned (a codegen bug, guarded against by regression
tests), or when the program design accepts the leak itself (the last rows).

| Allocation (site) | Freed by |
| --- | --- |
| Option cells — codegen `box_option_value`, adaptor `_ntsc_option_cell` for returns | `option_free` on every owned option-slot drop; `0` is the `none` handle and allocates nothing |
| Result cells — codegen `allocate_result_cell` | `free_result_cell` after payload extraction |
| `own` boxes (`alloc(value)`) — codegen `emit_box_value` | the `Ty::Own` drop arm (thunk + `free`) |
| Class instances — codegen `class_alloc`, escape analysis stack-allocates non-escaping ones | class drop thunks for provably non-escaping instances; **escaping or aliased instances are never freed by design** (freed-twice is worse than leaked) |
| Class field copies — codegen `class_copy` | the standard class drop; recursive copies are rejected statically |
| `dyn` headers — codegen `dyn_header_alloc` | the vtable slot-0 wrapper frees header + object |
| `go` futures — codegen `go_future_alloc` | the trampoline at completion; the scheduler cleanup thunk at shutdown for abandoned goroutines |
| Registry regions (`memory.alloc`, 16 MiB cap) | explicit `memory.drop`, or the owning pointer's scope drop |
| Bridge option cells for Rust→Neutron callback params | the Neutron callback slot's option drop is the single free (see [rust-autogen](rust-autogen.md)) |

Accepted process-lifetime allocations (never freed, excluded from leak
reporting):

- string literals and other compile-time constants (registered permanent);
- the scheduler's per-process run-queue statics (reused across restarts);
- the leak-site map itself (bounded by executed var stores).

Leak marks (`ntsc_leak_mark`) fire at owned variable stores, so the report
can print `--> <source>:line:col`. Only int-like registry ids are marked:
string/array/object/shared handles, registered memory regions, and slices.
Malloc'd pointer cells (own boxes, option/result cells, `dyn` headers) are
deliberately unmarked — they have no registry id, and their drops are
direct frees.

## Deterministic IR

The code generator is written to produce deterministic IR:

- stdlib calls are routed through fixed ABI function names
  (`ntsc_<module>_<func>`), forward-declared up front;
- module-specific operations that need type knowledge the runtime cannot infer
  (arrays, sort, testing, `random.shuffle`/`weighted`, `process.spawn_thread`)
  are emitted by codegen helpers instead of the runtime;
- a fixed optimization pipeline with verification keeps output predictable.

## Platform support

Supported hosts are Linux, macOS, and Windows on x86-64 and aarch64, on stable
Rust with LLVM 23. The CI matrix builds and tests all three platforms.
