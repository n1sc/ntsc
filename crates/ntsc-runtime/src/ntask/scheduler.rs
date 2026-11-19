//! M:N task scheduler: a fixed OS-thread pool multiplexing cheap stackless
//! goroutines onto `num_cpus` workers.
//!
//! Each worker owns a lock-free run queue (see [`super::runqueue`]); a spawn
//! from a worker lands on that worker's queue, a full ring overflows to the
//! shared queue under [`core::GLOBAL`], and idle workers steal from peers. A
//! worker takes a goroutine's stack from a task shard and polls without a
//! lock held, then stores its state and requeues it or parks it on a
//! channel/timer/descriptor, or finish it. Because a goroutine is a single
//! stackless future (no worker thread-local async stack), a torn-out
//! goroutine can be picked up by any worker — that is what lets CPU work
//! spread across the pool.
//!
//! Blocking coordination (channel buffers, parked waiters, timers, fd
//! interests) remains inside the [`GLOBAL`] mutex with the shared queue, so
//! a block/unblock decision is one atomic critical section: there is no lost
//! wakeup. Idle workers sleep on a condition variable; any spawn, channel
//! handoff, timer expiry, or fd-ready event re-notifies them.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock, mpsc};
use std::thread::JoinHandle;

use super::core::{self, ChanOp, Goroutine, Park};
use super::runqueue::RunQueue;
use super::task_stack::TaskStack;
use crate::registry::NULL;

/// Idle-worker sleep signal. Workers wait on this when the ready queue is
/// empty; every path that makes a goroutine runnable notifies it.
pub(crate) static SIGNAL: LazyLock<(Mutex<()>, Condvar)> =
    LazyLock::new(|| (Mutex::new(()), Condvar::new()));

/// A blocking job handed to the bounded offload pool: run to completion on a
/// standalone thread so a scheduler worker is never blocked on I/O or a
/// child process.
type OffloadJob = Box<dyn FnOnce() + Send>;

static OFFLOADING: AtomicBool = AtomicBool::new(false);
static OFFLOAD_STOP: AtomicBool = AtomicBool::new(false);
static OFFLOAD_SEND: LazyLock<Mutex<Option<mpsc::Sender<OffloadJob>>>> =
    LazyLock::new(|| Mutex::new(None));
static OFFLOAD_THREADS: LazyLock<Mutex<Vec<JoinHandle<()>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
const CHANNEL_POLL_BUDGET: u32 = 8;

thread_local! {
    static CURRENT_GID: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
    static CHANNEL_POLLS_LEFT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };

    /// Number of consecutive local-queue spawns on this worker. Large spawn
    /// bursts already have runnable work in the owner's queue, so waking an
    /// idle worker for every child only adds futex traffic and steal races.
    static LOCAL_SPAWN_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };

    /// Detached tasks spawned by the current worker but not yet published to
    /// the global goroutine table. Publishing in batches amortizes the global
    /// lock and run-queue operations during large spawn loops.
    static DETACHED_BATCH: RefCell<Vec<(i64, Goroutine)>> = const { RefCell::new(Vec::new()) };
    static DETACHED_ID_RANGE: std::cell::Cell<(i64, u32)> = const {
        std::cell::Cell::new((0, 0))
    };

    /// The running goroutine's suspension state, held on the worker for the
    /// duration of one poll. `park_self`, `park_chan_send` and the `recv_*`
    /// readers are called from generated code on every suspension and every
    /// channel receive; going through the global lock for each made the poll
    /// path take it three or four times instead of twice. [`drive`] loads this
    /// when it picks the goroutine up and flushes it back when the poll returns,
    /// so the shared table still sees exactly the same transitions.
    static CURRENT_STATE: std::cell::Cell<TaskState> = const {
        std::cell::Cell::new(TaskState::IDLE)
    };

    /// Spin-watchdog state. Always active: the last driven goroutine id
    /// and its consecutive instant-drive streak on this worker. Healthy
    /// goroutines park, complete, or yield to others between drives;
    /// thousands of consecutive drives of one id mean an instant
    /// poll→requeue loop that never suspends.
    static WATCH_LAST_GID: std::cell::Cell<i64> = const { std::cell::Cell::new(-1) };
    static WATCH_STREAK: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static WATCH_DRIVES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static WATCH_QUIET_UNTIL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Consecutive instant drives after which a requeue yields to the shared
/// queue instead of the worker-local LIFO slot. The local slot is the fast
/// path for a parent that suspends one turn before its child parks, but an
/// instant poll→requeue loop that never parks would otherwise bounce on the
/// same core at 100% and starve every other goroutine there. Yielding moves
/// the spinner to the shared queue where any worker can interleave, and the
/// OS yield lets a timer or offload completion run.
const SPIN_YIELD_THRESHOLD: u32 = 10_000;

/// Consecutive instant drives that trip the spin watchdog. Hundreds of
/// thousands of polls returning "not done, no park" in a row is seconds of
/// a core at 100% with zero forward progress by definition (no suspension
/// point was reached); legitimate ping-ponging alternates goroutine ids
/// and resets the streak.
const SPIN_WATCH_THRESHOLD: u32 = 200_000;

/// Cooldown between watchdog reports on one worker, in millis of wall time.
const SPIN_WATCH_COOLDOWN_MS: u64 = 30_000;

/// Note one drive of `gid` for the spin watchdog. Callers: every path in
/// [`drive`] that requeues instantly without parking or completing.
/// Always active; the per-drive cost is two thread-local comparisons.
/// Returns the consecutive-drive streak so callers can yield a spinner to
/// the shared queue instead of hot-looping the local slot.
fn watchdog_note_drive(gid: i64) -> u32 {
    let streak = if WATCH_LAST_GID.with(|last| last.get()) == gid {
        let next = WATCH_STREAK.with(|streak| streak.get().saturating_add(1));
        WATCH_STREAK.with(|streak| streak.set(next));
        next
    } else {
        WATCH_LAST_GID.with(|last| last.set(gid));
        WATCH_STREAK.with(|streak| streak.set(1));
        1
    };
    if streak >= SPIN_WATCH_THRESHOLD {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let quiet_until = WATCH_QUIET_UNTIL.with(|quiet| quiet.get());
        if now_ms >= quiet_until {
            WATCH_QUIET_UNTIL.with(|quiet| quiet.set(now_ms + SPIN_WATCH_COOLDOWN_MS));
            let worker = WORKER_INDEX.with(|index| {
                index
                    .get()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "?".into())
            });
            eprintln!(
                "[sched] HOT SPIN: goroutine {gid} requeued without parking {streak} times in a row on worker {worker} (no suspension point reached; check for a future that never parks)"
            );
        }
        WATCH_STREAK.with(|streak| streak.set(0));
        return SPIN_WATCH_THRESHOLD;
    }
    streak
}

/// Requeue `gid` after an instant drive. Healthy one-turn suspends stay on
/// the worker-local LIFO slot; a goroutine past [`SPIN_YIELD_THRESHOLD`]
/// consecutive instant drives goes to the shared queue with an OS yield so
/// timer, channel, and offload completions can interleave.
fn requeue_instant(gid: i64) {
    let streak = watchdog_note_drive(gid);
    if streak >= SPIN_YIELD_THRESHOLD || note_instant_drive() {
        core::push_ready(gid);
        notify_one();
        std::thread::yield_now();
        return;
    }
    make_runnable(gid);
}

fn note_instant_drive() -> bool {
    WATCH_DRIVES.with(|count| {
        let next = count.get().saturating_add(1);
        count.set(if next >= SPIN_YIELD_THRESHOLD {
            0
        } else {
            next
        });
        next >= SPIN_YIELD_THRESHOLD
    })
}

/// Note that `gid` settled (parked with a waiter, or completed): a settled
/// goroutine is by definition not spinning, so any streak ends here.
fn watchdog_note_settled(gid: i64) {
    WATCH_DRIVES.with(|count| count.set(0));
    if WATCH_LAST_GID.with(|last| last.get()) == gid {
        WATCH_STREAK.with(|streak| streak.set(0));
    }
}

/// Per-poll suspension state mirrored onto the worker thread. Every field is a
/// scalar, so this is `Copy` and lives in a `Cell` with no borrow bookkeeping.
#[derive(Clone, Copy)]
struct TaskState {
    park: Park,
    pending_send: i64,
    pending_send_owned: bool,
    recv_result: i64,
    recv_result_owned: bool,
    recv_ok: bool,
}

impl TaskState {
    const IDLE: TaskState = TaskState {
        park: Park::None,
        pending_send: NULL,
        pending_send_owned: false,
        recv_result: NULL,
        recv_result_owned: false,
        recv_ok: false,
    };

    fn store(self, goroutine: &mut Goroutine) {
        goroutine.pending_send = self.pending_send;
        goroutine.pending_send_owned = self.pending_send_owned;
        goroutine.recv_result = self.recv_result;
        goroutine.recv_result_owned = self.recv_result_owned;
        goroutine.recv_ok = self.recv_ok;
    }
}

/// Workers launched by [`start`]; kept so [`shutdown`] can join them.
static WORKERS: LazyLock<Mutex<Vec<JoinHandle<()>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static REACTOR: LazyLock<Mutex<Option<JoinHandle<()>>>> = LazyLock::new(|| Mutex::new(None));

/// Per-worker lock-free run queues, allocated once for the process. A `OnceLock`
/// rather than a lock: the hot paths (spawn, pop, steal, emptiness check) only
/// index it, so putting a lock in front would reintroduce the contention the
/// queues exist to avoid. `shutdown` drains the queues instead of freeing them,
/// so a later `start` reuses the same allocation.
static QUEUES: OnceLock<Box<[RunQueue]>> = OnceLock::new();

fn queues() -> &'static [RunQueue] {
    QUEUES.get().map(|queues| &**queues).unwrap_or(&[])
}

/// Whether the worker pool is running, so a spawn need not lock [`WORKERS`].
static STARTED: AtomicBool = AtomicBool::new(false);

/// Searching and unparked worker counts, packed into one word so both are read
/// with a single atomic operation. Go (`sched.nmspinning`) and Tokio
/// (`idle::State`) both require this: a wake is only worth its futex syscall
/// when no worker is already hunting for work and one is actually parked.
static IDLE_STATE: AtomicUsize = AtomicUsize::new(0);
const SEARCHING_MASK: usize = 0xffff;
const UNPARKED_SHIFT: u32 = 16;

thread_local! {
    static WORKER_INDEX: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Extra run-queue slots preallocated for temporary coverage workers (see
/// [`worker_will_block`]). 2 KiB per queue; idle temps return their slot.
const EXTRA_SLOTS: usize = 64;

/// Idle 10 ms park rounds after which a temporary worker exits and returns
/// its queue slot (about one second of finding nothing to drive).
const TEMP_IDLE_ROUNDS: u32 = 100;

/// Grace period at shutdown for workers stuck in blocking calls to finish
/// before their threads are abandoned to die with the process (Tokio's
/// `shutdown_timeout` precedent). Overridable via `NTS_SHUTDOWN_GRACE_MS`.
fn shutdown_grace_ms() -> u64 {
    static GRACE: LazyLock<u64> = LazyLock::new(|| {
        std::env::var("NTS_SHUTDOWN_GRACE_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(500)
    });
    *GRACE
}

/// Cap on live temporary workers. Overridable via
/// `NTS_MAX_BLOCKING_THREADS` (default 64, hard-capped by EXTRA_SLOTS).
fn max_extra_workers() -> usize {
    static MAX: LazyLock<usize> = LazyLock::new(|| {
        std::env::var("NTS_MAX_BLOCKING_THREADS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(64)
            .clamp(0, EXTRA_SLOTS)
    });
    *MAX
}

/// Regular (non-temporary) worker count, fixed at pool startup.
static REGULAR_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Workers currently inside a blocking section (see [`worker_will_block`]).
/// Incremented before the blocking call, decremented after, so a worker
/// wedged forever (an infinite blocking loop) keeps its count — coverage
/// then over-provisions slightly instead of starving, and idle temps exit
/// on their own.
static BLOCKED_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Live temporary coverage workers.
static EXTRA_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Free extra queue-slot indices for temporary workers to claim.
static EXTRA_FREE: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Temporary workers spawned over the process lifetime (diagnostic only).
static TEMP_SPAWNS: AtomicUsize = AtomicUsize::new(0);

/// Start the worker pool. Idempotent. Call once (or let the first spawn
/// trigger it).
pub(crate) fn start() {
    if SHUTDOWN.load(Ordering::Relaxed) || STARTED.load(Ordering::Acquire) {
        return;
    }
    let mut workers = WORKERS.lock().unwrap_or_else(|p| p.into_inner());
    if !workers.is_empty() {
        return;
    }
    QUEUES.get_or_init(|| {
        let count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);
        REGULAR_WORKERS.store(count, Ordering::Release);
        (0..count + EXTRA_SLOTS).map(|_| RunQueue::new()).collect()
    });
    let regular = REGULAR_WORKERS.load(Ordering::Acquire).max(1);
    // Replenish claimable temp slots (a restart after abandoned temps may
    // find some still checked out; those leak boundedly until process end).
    {
        let mut free = EXTRA_FREE.lock().unwrap_or_else(|p| p.into_inner());
        for index in (regular..regular + EXTRA_SLOTS).rev() {
            if !free.contains(&index) {
                free.push(index);
            }
        }
    }
    for index in 0..regular {
        if let Ok(worker) = std::thread::Builder::new()
            .name(format!("ntask-worker-{index}"))
            .spawn(move || worker_loop(index))
        {
            workers.push(worker);
        }
    }
    STARTED.store(true, Ordering::Release);
}

/// Enter a blocking section on a scheduler worker (Tokio's `block_in_place`
/// doctrine: never block a worker without cover).
///
/// Called by blocking runtime entries (`time.sleep`, synchronous socket and
/// subprocess operations) and by every generated Rust bridge around the
/// dependency call, whose duration the compiler cannot know. When every
/// regular worker is already blocked, a temporary coverage worker is
/// spawned (bounded by [`max_extra_workers`]) so newly-ready goroutines —
/// timer wakeups, channel handoffs, event arrivals — still find a driver.
/// A no-op off scheduler workers (offload-pool, Tokio, and main threads).
pub(crate) fn worker_will_block() {
    if WORKER_INDEX.with(|index| index.get()).is_none() {
        return;
    }
    if SHUTDOWN.load(Ordering::Relaxed) {
        return;
    }
    BLOCKED_WORKERS.fetch_add(1, Ordering::SeqCst);
    let regular = REGULAR_WORKERS.load(Ordering::Acquire);
    let extra = EXTRA_WORKERS.load(Ordering::Acquire);
    let blocked = BLOCKED_WORKERS.load(Ordering::Acquire);
    // Unblocked drivers left after this worker parks: regular + extra -
    // blocked. Below one, bring cover. The counts are racy by nature; an
    // overshoot spawns a thread that idle-exits within about a second.
    if regular > 0 && regular + extra <= blocked {
        spawn_temp_worker();
    }
}

/// Leave a blocking section. Balances [`worker_will_block`]; safe to call
/// off scheduler workers (a no-op there, mirroring the entry).
pub(crate) fn worker_did_unblock() {
    if WORKER_INDEX.with(|index| index.get()).is_none() {
        return;
    }
    BLOCKED_WORKERS.fetch_sub(1, Ordering::SeqCst);
}

/// Spawn one temporary coverage worker on a free extra queue slot. Racy
/// overshoots are bounded by the freelist and the live cap; surplus temps
/// idle-exit on their own.
fn spawn_temp_worker() {
    let index = match EXTRA_FREE.lock().unwrap_or_else(|p| p.into_inner()).pop() {
        Some(index) => index,
        None => return,
    };
    if EXTRA_WORKERS.fetch_add(1, Ordering::SeqCst) >= max_extra_workers() {
        EXTRA_WORKERS.fetch_sub(1, Ordering::SeqCst);
        EXTRA_FREE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(index);
        return;
    }
    TEMP_SPAWNS.fetch_add(1, Ordering::Relaxed);
    match std::thread::Builder::new()
        .name(format!("ntask-worker-temp-{index}"))
        .spawn(move || temp_worker_loop(index))
    {
        Ok(handle) => {
            WORKERS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(handle);
            notify_one();
        }
        Err(_) => {
            EXTRA_WORKERS.fetch_sub(1, Ordering::SeqCst);
            EXTRA_FREE
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(index);
        }
    }
}

/// A temporary coverage worker: the regular [`worker_loop`] plus an idle
/// exit that returns its queue slot. Leftover queued work (only possible
/// via a race with a concurrent spawn onto this queue) is returned to the
/// shared queue rather than dropped.
fn temp_worker_loop(index: usize) {
    WORKER_INDEX.with(|worker| worker.set(Some(index)));
    IDLE_STATE.fetch_add(1 << UNPARKED_SHIFT, Ordering::SeqCst);
    let mut tick: u32 = 0;
    let mut idle_rounds: u32 = 0;
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }
        tick = tick.wrapping_add(1);
        match pop_ready(index, tick) {
            Some(gid) => {
                idle_rounds = 0;
                drive_tracked(gid, index);
            }
            None => {
                idle_rounds += 1;
                if idle_rounds >= TEMP_IDLE_ROUNDS {
                    break;
                }
                wait_for_work_budget(index, 1);
            }
        }
    }
    for gid in queues()[index].drain() {
        core::push_ready(gid);
    }
    IDLE_STATE.fetch_sub(1 << UNPARKED_SHIFT, Ordering::SeqCst);
    EXTRA_WORKERS.fetch_sub(1, Ordering::SeqCst);
    EXTRA_FREE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(index);
}

/// Start the bounded offload pool. Idempotent. A small, fixed number of
/// standalone threads run blocking jobs (`process.exec*`, socket transfers)
/// so a scheduler worker is never blocked on I/O or a child process. Bound
/// the count so runaway children cannot exhaust threads.
fn start_offload_pool() {
    let mut guard = OFFLOAD_SEND.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_some() {
        // The sender is the single initialization marker: another thread has
        // already run the one-time setup (or is about to, under this lock).
        return;
    }
    OFFLOAD_STOP.store(false, Ordering::Relaxed);
    let count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 8);
    let (tx, rx) = mpsc::channel::<OffloadJob>();
    // Publish the sender under the lock *before* spawning workers, so any
    // concurrent `run_offload` that locks `OFFLOAD_SEND` after this returns
    // is guaranteed to see it (no job can be dropped racing the startup).
    *guard = Some(tx);
    OFFLOADING.store(true, Ordering::SeqCst);
    drop(guard);
    let rx = Arc::new(Mutex::new(rx));
    let mut threads = OFFLOAD_THREADS.lock().unwrap_or_else(|p| p.into_inner());
    for index in 0..count {
        let rx = Arc::clone(&rx);
        if let Ok(handle) = std::thread::Builder::new()
            .name(format!("ntask-offload-{index}"))
            .spawn(move || offload_worker(rx))
        {
            threads.push(handle);
        }
    }
}

/// A single offload-pool worker: pull jobs off the queue and run them to
/// completion.
fn offload_worker(rx: Arc<Mutex<mpsc::Receiver<OffloadJob>>>) {
    loop {
        let job = {
            let Ok(rx) = rx.lock() else {
                return;
            };
            match rx.recv() {
                Ok(job) => job,
                Err(_) => return,
            }
        };
        job();
        if OFFLOAD_STOP.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// Enqueue a blocking job onto the offload pool. It runs on a standalone
/// thread; the caller registers an op, submits a job that completes it, and
/// parks the current goroutine on it.
pub(crate) fn run_offload(job: impl FnOnce() + Send + 'static) {
    start_offload_pool();
    let send = OFFLOAD_SEND.lock().unwrap_or_else(|p| p.into_inner());
    // `start_offload_pool` publishes the sender under this same lock before
    // returning, so it is always `Some` here.
    if let Some(tx) = send.as_ref() {
        let _ = tx.send(Box::new(job));
    }
}

/// Park the current goroutine on an already-registered offloaded job
/// (used by the reactive future poll path, which registers the op itself so a
/// worker can complete it with the future's result).
pub(crate) fn park_op(core_id: i64) {
    park_self(Park::Job { core: core_id });
}

/// Register a goroutine running `poll` over `future`; returns its id without
/// scheduling it. `handle` is its registry wrapper id, reserved by the caller.
/// `cleanup` reclaims the future if the goroutine never completes.
pub(crate) fn register(
    poll: core::PollFn,
    future: i64,
    cleanup: Option<core::CleanupFn>,
    handle: i64,
) -> i64 {
    start();
    core::register_goroutine(new_goroutine(poll, future, cleanup, handle))
}

/// Register a goroutine and queue it as runnable. A spawn from outside the
/// worker pool takes the global lock once instead of twice.
pub(crate) fn spawn_runnable(
    poll: core::PollFn,
    future: i64,
    cleanup: Option<core::CleanupFn>,
    handle: i64,
) -> i64 {
    start();
    let goroutine = new_goroutine(poll, future, cleanup, handle);
    if WORKER_INDEX.with(|index| index.get()).is_some() {
        if handle == NULL {
            return spawn_detached_batched(goroutine);
        }
        let gid = core::register_goroutine(goroutine);
        make_runnable(gid);
        return gid;
    }
    let gid = core::register_goroutine_runnable(goroutine);
    notify_one();
    gid
}

const DETACHED_BATCH_CAP: usize = 64;

fn spawn_detached_batched(goroutine: Goroutine) -> i64 {
    let gid = DETACHED_ID_RANGE.with(|range| {
        let (next, remaining) = range.get();
        if remaining > 0 {
            range.set((next + 1, remaining - 1));
            next
        } else {
            let start = core::reserve_core_ids(DETACHED_BATCH_CAP);
            range.set((start + 1, DETACHED_BATCH_CAP as u32 - 1));
            start
        }
    });
    // Publish immediately. Batching stranded the child in this worker's
    // thread-local batch whenever the spawning goroutine never yields (a
    // hot `while (true)` poll loop), because the flush only runs after a
    // poll returns. `notify_one` already skips its futex when no worker is
    // parked, so the amortization bought nothing here.
    core::TASKS.insert(gid, goroutine);
    if let Some(queue) = local_queue() {
        if let Some(displaced) = queue.push_next(gid)
            && !queue.push(displaced)
        {
            overflow_to_shared(queue, displaced);
        }
        notify_spawn_burst();
    } else {
        core::push_ready(gid);
        notify_one();
    }
    gid
}

/// Publish the current worker's detached tasks in the task table and then
/// enqueue them locally without issuing one wakeup per child.
fn flush_detached_batch() {
    let pending = DETACHED_BATCH.with(|batch| std::mem::take(&mut *batch.borrow_mut()));
    if pending.is_empty() {
        return;
    }
    let ids: Vec<i64> = pending.iter().map(|(gid, _)| *gid).collect();
    for (gid, goroutine) in pending {
        core::TASKS.insert(gid, goroutine);
    }
    if let Some(queue) = local_queue() {
        for gid in ids {
            if let Some(displaced) = queue.push_next(gid)
                && !queue.push(displaced)
            {
                overflow_to_shared(queue, displaced);
            }
        }
        notify_one();
    } else {
        core::push_ready_batch(ids);
        notify_one();
    }
}

fn new_goroutine(
    poll: core::PollFn,
    future: i64,
    cleanup: Option<core::CleanupFn>,
    handle: i64,
) -> Goroutine {
    Goroutine {
        tasks: TaskStack::single(poll, future),
        handle,
        cleanup: cleanup.map(|f| (f, future)),
        pending_send: NULL,
        pending_send_owned: false,
        recv_result: NULL,
        recv_result_owned: false,
        recv_ok: false,
        done: false,
        result: NULL,
        pending_exception: NULL,
        joiners: Vec::new(),
    }
}

/// Push a goroutine onto the ready queue and wake a worker if one is asleep.
/// From a worker, the goroutine goes into that worker's LIFO slot: a channel
/// handoff or an immediate requeue then stays on the core that produced it, with
/// its state still in cache.
pub(crate) fn make_runnable(gid: i64) {
    if let Some(queue) = local_queue() {
        if let Some(displaced) = queue.push_next(gid)
            && !queue.push(displaced)
        {
            overflow_to_shared(queue, displaced);
        }
        notify_spawn_burst();
        return;
    }
    core::push_ready(gid);
    notify_one();
}

/// Wake workers for a local spawn burst without taking the condition-variable
/// path on every child. The first few children get immediate wakeups so a
/// small CPU fan-out starts in parallel; after that, periodic notifications
/// keep progress if the spawning goroutine later parks itself.
fn notify_spawn_burst() {
    const INITIAL: u32 = 32;
    const INTERVAL: u32 = 64;
    LOCAL_SPAWN_COUNT.with(|count| {
        let next = count.get().saturating_add(1);
        count.set(next);
        if next <= INITIAL || next.is_multiple_of(INTERVAL) {
            notify_one();
        }
    });
}

/// This worker's run queue, or `None` when called from outside the pool.
fn local_queue() -> Option<&'static RunQueue> {
    let index = WORKER_INDEX.with(|index| index.get())?;
    queues().get(index)
}

/// Move half of a full local ring to the shared queue along with `gid`, so the
/// per-spawn cost of a full queue is one shared-lock acquisition per 128
/// goroutines rather than one per goroutine (Go's `runqputslow`).
fn overflow_to_shared(queue: &RunQueue, gid: i64) {
    let mut batch = queue.take_half();
    batch.push(gid);
    core::push_ready_batch(batch);
}

/// Whether a wake is worth its futex syscall: only when no worker is already
/// hunting for work and at least one is parked. `fetch_add(0, SeqCst)` is the
/// store-load barrier this check requires — `Acquire` would let the queue push
/// and this load both read stale, losing the wakeup. Go's `wakep` and Tokio's
/// `notify_should_wakeup` use exactly this pattern.
fn should_wake() -> bool {
    // Temporary workers decrement the unparked count before the live count.
    // Read the live count first so an exit cannot hide an idle worker.
    let workers = live_worker_count();
    let state = IDLE_STATE.fetch_add(0, Ordering::SeqCst);
    needs_idle_worker_wakeup(state, workers)
}

fn live_worker_count() -> usize {
    REGULAR_WORKERS.load(Ordering::Acquire) + EXTRA_WORKERS.load(Ordering::SeqCst)
}

fn needs_idle_worker_wakeup(state: usize, workers: usize) -> bool {
    let searching = state & SEARCHING_MASK;
    let unparked = state >> UNPARKED_SHIFT;
    searching == 0 && unparked < workers
}

/// Notify a single idle worker, if waking one can help.
fn notify_one() {
    if !should_wake() {
        return;
    }
    let (lock, cvar) = &*SIGNAL;
    let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
    cvar.notify_one();
}

/// Notify all workers (used when a burst of goroutines became runnable).
pub(crate) fn notify_all() {
    if !should_wake() {
        return;
    }
    notify_all_unconditional();
}

/// Notify all workers even when none is counted idle: a worker between its
/// queue check and its wait is not yet parked but must see the stop flag.
fn notify_all_unconditional() {
    let (lock, cvar) = &*SIGNAL;
    let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
    cvar.notify_all();
}

/// Per-worker scheduler statistics, reported at shutdown when the environment
/// variable `NTS_SCHED_STATS` is set. Diagnostic only: the counters are plain
/// relaxed atomics on the worker's own slot, so they cost one increment per
/// poll and never synchronize between workers.
const MAX_TRACKED_WORKERS: usize = 64;
static STAT_POLLS: [AtomicUsize; MAX_TRACKED_WORKERS] =
    [const { AtomicUsize::new(0) }; MAX_TRACKED_WORKERS];
static STAT_STEALS: [AtomicUsize; MAX_TRACKED_WORKERS] =
    [const { AtomicUsize::new(0) }; MAX_TRACKED_WORKERS];
static STAT_SPIN_NS: [AtomicUsize; MAX_TRACKED_WORKERS] =
    [const { AtomicUsize::new(0) }; MAX_TRACKED_WORKERS];
static STAT_PARK_NS: [AtomicUsize; MAX_TRACKED_WORKERS] =
    [const { AtomicUsize::new(0) }; MAX_TRACKED_WORKERS];
static STAT_BUSY_NS: [AtomicUsize; MAX_TRACKED_WORKERS] =
    [const { AtomicUsize::new(0) }; MAX_TRACKED_WORKERS];

fn stats_enabled() -> bool {
    static ENABLED: LazyLock<bool> =
        LazyLock::new(|| std::env::var_os("NTS_SCHED_STATS").is_some());
    *ENABLED
}

/// Print per-worker distribution and idle time. Called from [`shutdown`].
fn report_stats() {
    if !stats_enabled() {
        return;
    }
    // Temporary workers share untracked queue slots past the regular
    // range: report the regular rows plus aggregate temp activity.
    let workers = REGULAR_WORKERS
        .load(Ordering::Relaxed)
        .min(MAX_TRACKED_WORKERS);
    let total_polls: usize = (0..workers)
        .map(|i| STAT_POLLS[i].load(Ordering::Relaxed))
        .sum();
    eprintln!("[sched] worker  polls    share   steals   busy_ms  spin_ms  park_ms  idle%");
    for index in 0..workers {
        let polls = STAT_POLLS[index].load(Ordering::Relaxed);
        let steals = STAT_STEALS[index].load(Ordering::Relaxed);
        let busy_ms = STAT_BUSY_NS[index].load(Ordering::Relaxed) / 1_000_000;
        let spin_ms = STAT_SPIN_NS[index].load(Ordering::Relaxed) / 1_000_000;
        let park_ms = STAT_PARK_NS[index].load(Ordering::Relaxed) / 1_000_000;
        let share = if total_polls > 0 {
            polls as f64 * 100.0 / total_polls as f64
        } else {
            0.0
        };
        let live = busy_ms + spin_ms + park_ms;
        let idle_pct = if live > 0 {
            (spin_ms + park_ms) as f64 * 100.0 / live as f64
        } else {
            0.0
        };
        eprintln!(
            "[sched] {index:>6} {polls:>7} {share:>6.1}% {steals:>8} {busy_ms:>9} {spin_ms:>8} {park_ms:>8} {idle_pct:>5.1}%"
        );
    }
    eprintln!("[sched] total polls {total_polls}");
    eprintln!(
        "[sched] workers in blocking sections now: {} (temp workers live: {}, spawned total: {}, extra cap: {})",
        BLOCKED_WORKERS.load(Ordering::Relaxed),
        EXTRA_WORKERS.load(Ordering::Relaxed),
        TEMP_SPAWNS.load(Ordering::Relaxed),
        max_extra_workers()
    );
}

/// Drive one goroutine, tracking per-worker polls and busy time when
/// scheduler statistics are enabled and the index is tracked.
fn drive_tracked(gid: i64, index: usize) {
    if stats_enabled() && index < MAX_TRACKED_WORKERS {
        STAT_POLLS[index].fetch_add(1, Ordering::Relaxed);
        let started = std::time::Instant::now();
        drive(gid);
        STAT_BUSY_NS[index].fetch_add(started.elapsed().as_nanos() as usize, Ordering::Relaxed);
    } else {
        drive(gid);
    }
}

/// The worker loop: pull a runnable goroutine and drive it, sleeping when
/// there is nothing to do.
fn worker_loop(index: usize) {
    WORKER_INDEX.with(|worker| worker.set(Some(index)));
    // Workers start unparked; parking decrements this.
    IDLE_STATE.fetch_add(1 << UNPARKED_SHIFT, Ordering::SeqCst);
    let mut tick: u32 = 0;
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            IDLE_STATE.fetch_sub(1 << UNPARKED_SHIFT, Ordering::SeqCst);
            return;
        }
        tick = tick.wrapping_add(1);
        match pop_ready(index, tick) {
            Some(gid) => drive_tracked(gid, index),
            None => wait_for_work(index),
        }
    }
}

/// Polls between forced checks of the shared queue. Without it, two goroutines
/// that keep re-readying each other on one worker starve every goroutine in the
/// shared queue. Go uses 61 for the same reason.
const GLOBAL_CHECK_INTERVAL: u32 = 61;

/// Pop a runnable goroutine: the local queue first, the shared queue when it is
/// dry (or every `GLOBAL_CHECK_INTERVAL` polls, for fairness), then steal.
fn pop_ready(index: usize, tick: u32) -> Option<i64> {
    let all = queues();
    let Some(queue) = all.get(index) else {
        return core::pop_ready();
    };
    if tick.is_multiple_of(GLOBAL_CHECK_INTERVAL)
        && let Some(gid) = core::pop_ready()
    {
        return Some(gid);
    }
    if let Some(gid) = queue.pop() {
        return Some(gid);
    }
    if let Some(gid) = refill_from_shared(queue, live_worker_count()) {
        return Some(gid);
    }
    steal_from_peers(queue, index, all)
}

/// Move a share of the shared queue into `queue`, returning one id to run now.
/// The share is bounded by the worker count so one worker cannot swallow a burst
/// that the others could be running in parallel.
fn refill_from_shared(queue: &RunQueue, workers: usize) -> Option<i64> {
    let mut batch = core::pop_ready_batch(workers);
    let gid = batch.pop()?;
    for id in batch {
        if !queue.push(id) {
            core::push_ready(id);
        }
    }
    Some(gid)
}

/// Take half of a peer's queue. The scan starts at a rotating offset so every
/// thief does not hammer worker 0, as in Go's randomized `stealOrder`.
fn steal_from_peers(queue: &RunQueue, index: usize, all: &'static [RunQueue]) -> Option<i64> {
    let count = all.len();
    for offset in 1..count {
        let victim = (index + offset) % count;
        if let Some(gid) = all[victim].steal_into(queue) {
            if stats_enabled() && index < MAX_TRACKED_WORKERS {
                STAT_STEALS[index].fetch_add(1, Ordering::Relaxed);
            }
            return Some(gid);
        }
    }
    None
}

/// Queue checks a worker makes before it parks. A spawn loop hands out work
/// faster than a syscall round trip, so a worker that sleeps the moment its
/// queue runs dry makes every later spawn pay a futex wake.
const SPIN_BEFORE_PARK: u32 = 64;

/// Sleep until a goroutine becomes runnable or we are asked to stop.
fn wait_for_work(index: usize) {
    wait_for_work_budget(index, u32::MAX);
}

/// Park up to `rounds` 10 ms slices waiting for work. Returns `false` when
/// the budget expired with no work and no shutdown — the temporary-worker
/// idle exit. A full budget behaves exactly like [`wait_for_work`].
fn wait_for_work_budget(index: usize, mut rounds: u32) -> bool {
    let track = stats_enabled() && index < MAX_TRACKED_WORKERS;
    let spin_started = track.then(std::time::Instant::now);
    // Counted as searching while spinning, so a concurrent spawn skips its wake:
    // this worker will see the work itself within the spin budget.
    IDLE_STATE.fetch_add(1, Ordering::SeqCst);
    let mut found = false;
    for _ in 0..SPIN_BEFORE_PARK {
        if SHUTDOWN.load(Ordering::Relaxed) || has_work() {
            found = true;
            break;
        }
        std::hint::spin_loop();
    }
    let was_last_searcher = IDLE_STATE.fetch_sub(1, Ordering::SeqCst) & SEARCHING_MASK == 1;
    if let Some(started) = spin_started {
        STAT_SPIN_NS[index].fetch_add(started.elapsed().as_nanos() as usize, Ordering::Relaxed);
    }
    if found {
        return true;
    }
    // The last searcher to give up must re-check every queue before sleeping,
    // and wake a peer if it finds anything: a spawn that ran concurrently with
    // the decrement above may have skipped its notification.
    if was_last_searcher && has_work() {
        notify_one();
        return true;
    }

    let (lock, cvar) = &*SIGNAL;
    let park_started = track.then(std::time::Instant::now);
    IDLE_STATE.fetch_sub(1 << UNPARKED_SHIFT, Ordering::SeqCst);
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) || has_work() {
            break;
        }
        if rounds == 0 {
            IDLE_STATE.fetch_add(1 << UNPARKED_SHIFT, Ordering::SeqCst);
            if let Some(started) = park_started {
                STAT_PARK_NS[index]
                    .fetch_add(started.elapsed().as_nanos() as usize, Ordering::Relaxed);
            }
            return false;
        }
        rounds -= 1;
        let guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let _ = cvar
            .wait_timeout(guard, std::time::Duration::from_millis(10))
            .unwrap_or_else(|p| p.into_inner());
    }
    IDLE_STATE.fetch_add(1 << UNPARKED_SHIFT, Ordering::SeqCst);
    if let Some(started) = park_started {
        STAT_PARK_NS[index].fetch_add(started.elapsed().as_nanos() as usize, Ordering::Relaxed);
    }
    true
}

/// Whether any queue holds a runnable goroutine. Every check is an atomic load,
/// so the pre-park spin can run it thousands of times without contending with
/// the spawner it is waiting on.
fn has_work() -> bool {
    queues().iter().any(|queue| !queue.is_empty()) || core::has_ready()
}

/// Drive one goroutine to a suspension/completion point. The poll function
/// runs without the global lock held, so it may call back into the runtime.
fn drive(gid: i64) {
    LOCAL_SPAWN_COUNT.with(|count| count.set(0));
    CHANNEL_POLLS_LEFT.with(|budget| budget.set(CHANNEL_POLL_BUDGET));
    let Some(tasks) = core::TASKS.with_mut(gid, |gr| {
        // Take the suspension state onto this worker for the poll, so
        // the runtime calls the poll makes stay lock-free.
        CURRENT_STATE.with(|state| {
            state.set(TaskState {
                park: Park::None,
                pending_send: gr.pending_send,
                pending_send_owned: gr.pending_send_owned,
                recv_result: gr.recv_result,
                recv_result_owned: gr.recv_result_owned,
                recv_ok: gr.recv_ok,
            })
        });
        std::mem::take(&mut gr.tasks)
    }) else {
        return;
    };
    CURRENT_GID.with(|cur| cur.set(Some(gid)));
    let poll_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::install_async_tasks(tasks);
        crate::poll_async_tasks_once();
        flush_detached_batch();
        crate::take_async_tasks()
    }));
    let tasks = match poll_outcome {
        Ok(tasks) => tasks,
        Err(_) => {
            let _ = crate::take_async_tasks();
            CURRENT_GID.with(|cur| cur.set(None));
            CURRENT_STATE.with(|state| state.set(TaskState::IDLE));
            let message = crate::registry::put_string("goroutine panicked".to_string());
            let mut g = core::GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
            let found = core::TASKS.contains(gid);
            if !found {
                drop(g);
                let _ = crate::registry::remove(message);
                return;
            }
            let joiners = core::TASKS
                .with_mut(gid, |gr| {
                    gr.tasks = TaskStack::new();
                    gr.done = true;
                    gr.cleanup = None;
                    gr.pending_exception = message;
                    std::mem::take(&mut gr.joiners)
                })
                .unwrap_or_default();
            for j in &joiners {
                g.ready.push_back(*j);
            }
            core::sync_ready_len(&g);
            drop(g);
            eprintln!(
                "[sched] goroutine {gid} panicked; parked as failed (never silently dropped)"
            );
            core::TASKS.notify_completion(gid);
            notify_all();
            return;
        }
    };
    let done = tasks.is_empty();
    CURRENT_GID.with(|cur| cur.set(None));
    let state = CURRENT_STATE.with(|state| state.replace(TaskState::IDLE));

    // A goroutine that yielded without a wait target needs no shared state at
    // all: requeue it straight onto this worker's LIFO slot. That is the
    // spawn-heavy path (a `go` body that runs to completion in one poll never
    // reaches here, but a multi-poll body hits it every turn), so keeping the
    // global lock out of it removes one acquisition per poll.
    if !done && matches!(state.park, Park::None) {
        if core::TASKS
            .with_mut(gid, |gr| {
                gr.tasks = tasks;
                state.store(gr);
            })
            .is_none()
        {
            return;
        }
        requeue_instant(gid);
        return;
    }
    if done {
        watchdog_note_settled(gid);
        let Some((joiners, reclaim)) = core::TASKS.with_mut(gid, |gr| {
            gr.tasks = tasks;
            gr.done = true;
            // The trampoline already reclaimed the future on this path.
            gr.cleanup = None;
            // The poll ran on this worker, so an exception it raised (a throw
            // with no handler in an async body) is parked on this thread's
            // TLS. Capture it so the thread that joins the goroutine can
            // re-raise it; otherwise the uncaught exception would vanish with
            // the worker thread.
            let thrown = crate::take_pending_exception_message();
            if thrown != NULL {
                gr.pending_exception = thrown;
            }
            let joiners = std::mem::take(&mut gr.joiners);
            // A joinable goroutine must remain addressable even when it
            // completes before its caller reaches `ntask_join`. Detached
            // goroutines have a null wrapper and can be reclaimed eagerly.
            let reclaim = gr.handle == NULL && gr.pending_exception == NULL && joiners.is_empty();
            (joiners, reclaim)
        }) else {
            return;
        };
        if reclaim {
            core::TASKS.reclaim_detached(gid);
            // Detached completion has no joiners and made no work runnable.
            return;
        }
        if !joiners.is_empty() {
            let mut g = core::GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
            g.ready.extend(joiners.iter().copied());
            core::sync_ready_len(&g);
        }
        core::TASKS.notify_completion(gid);
        if !joiners.is_empty() {
            notify_all();
        }
        return;
    }
    let mut g = core::GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(park) = core::TASKS.with_mut(gid, |gr| {
        gr.tasks = tasks;
        // Flush the state the poll produced; `park` is consumed here, so the
        // stored copy always reads `Park::None` between polls.
        state.store(gr);
        state.park
    }) else {
        return;
    };
    let mut woke = false;
    let mut requeue_local = false;
    match park {
        Park::None => {
            requeue_local = true;
        }
        Park::Chan { core, op } => {
            // Handoff or park: either way this drive settled (a handoff
            // completed its operation; a park waits).
            watchdog_note_settled(gid);
            let mut channel_state = state;
            let progress = chan_op(&mut g, gid, core, op, &mut channel_state);
            core::TASKS.with_mut(gid, |gr| channel_state.store(gr));
            if progress != ChannelProgress::Blocked {
                g.ready.push_back(gid);
            }
            woke = progress == ChannelProgress::Handoff;
        }
        Park::Timer { at } => {
            watchdog_note_settled(gid);
            g.timers.entry(at).or_default().push(gid);
            core::timers_pending_offset(1);
            let pending = !g.ready.is_empty();
            drop(g);
            super::reactor::wake_reactor();
            if pending {
                notify_all();
            }
            return;
        }
        Park::Fd { io, read } => {
            watchdog_note_settled(gid);
            let fd = g.ios.get_mut(&io).map(|slot| {
                slot.parked = true;
                // `ready` stays set; consumed via `io_ready` when it re-polls.
                slot.waiters.push(gid);
                slot.fd
            });
            let fd = fd.unwrap_or(0);
            drop(g);
            super::reactor::register_fd(io, fd, read);
            return;
        }
        Park::Join { target } => {
            // A completed join target hands off (progress); a pending one
            // parks. Either way this drive settled.
            watchdog_note_settled(gid);
            woke = join_park(&mut g, gid, target);
        }
        Park::Job { core } => {
            let done = g.ops.get_mut(&core).map(|op| op.done).unwrap_or(false);
            if done {
                // The offload pool already finished it; the goroutine resumes
                // to reap the result on its next poll.
                requeue_local = true;
            } else if let Some(op) = g.ops.get_mut(&core) {
                op.waiter = Some(gid);
                watchdog_note_settled(gid);
            } else {
                // Unknown job id: re-parking on it would spin forever
                // (requeue → re-park with no waiter ever completing it).
                // Park a 1 ms timer instead so the goroutine retries on a
                // bounded schedule and other work interleaves.
                watchdog_note_settled(gid);
                let at = core::now_ms().saturating_add(1);
                g.timers.entry(at).or_default().push(gid);
                core::timers_pending_offset(1);
                let pending = !g.ready.is_empty();
                drop(g);
                super::reactor::wake_reactor();
                if pending {
                    notify_all();
                }
                return;
            }
        }
    }
    core::sync_ready_len(&g);
    drop(g);
    if requeue_local {
        requeue_instant(gid);
        return;
    }
    // A bare park made nothing runnable: whoever later completes it (offload
    // job, timer, channel partner) notifies then. Only a real handoff or
    // requeue needs a worker.
    if woke {
        notify_all();
    }
}

/// Park on a sibling goroutine's completion. Returns `true` if the target
/// already finished and this goroutine was requeued (so a wake must be sent).
fn join_park(g: &mut core::Global, gid: i64, target: i64) -> bool {
    let result = core::TASKS.with_mut(target, |task| {
        if task.done {
            Some(task.result)
        } else {
            task.joiners.push(gid);
            None
        }
    });
    match result {
        Some(None) => false,
        Some(Some(result)) => {
            core::TASKS.with_mut(gid, |joiner| joiner.result = result);
            g.ready.push_back(gid);
            true
        }
        None => {
            g.ready.push_back(gid);
            true
        }
    }
}

/// Perform a channel send/recv for a goroutine that just parked on it. Returns
/// `true` if an opposite waiter was woken (so a wake must be sent).
#[derive(PartialEq, Eq)]
enum ChannelProgress {
    Blocked,
    Completed,
    Handoff,
}

fn chan_op(
    g: &mut core::Global,
    gid: i64,
    core_id: i64,
    op: ChanOp,
    caller: &mut TaskState,
) -> ChannelProgress {
    match op {
        ChanOp::Send => chan_send(g, gid, core_id, caller),
        ChanOp::Recv => chan_recv(g, gid, core_id, caller),
    }
}

fn chan_send(
    g: &mut core::Global,
    gid: i64,
    core_id: i64,
    caller: &mut TaskState,
) -> ChannelProgress {
    let value = caller.pending_send;
    let Some(chan) = g.chans.get_mut(&core_id) else {
        // Channel gone: treat as a no-op send (value released by caller).
        caller.pending_send = NULL;
        caller.pending_send_owned = false;
        return ChannelProgress::Completed;
    };
    if chan.closed {
        if chan.owns_elements {
            let _ = crate::registry::remove(value);
        }
        caller.pending_send = NULL;
        caller.pending_send_owned = false;
        return ChannelProgress::Completed;
    }
    let cap = chan.cap;
    if let Some(receiver) = chan.receivers.pop_front() {
        core::TASKS.with_mut(receiver, |gr| {
            gr.recv_result = value;
            gr.recv_result_owned = chan.owns_elements;
            gr.recv_ok = true;
        });
        caller.pending_send = NULL;
        caller.pending_send_owned = false;
        g.ready.push_back(receiver);
        return ChannelProgress::Handoff;
    }
    if cap > 0 && chan.buf.len() < cap {
        chan.buf.push_back(value);
        caller.pending_send = NULL;
        caller.pending_send_owned = false;
        return ChannelProgress::Completed;
    }
    caller.pending_send_owned = chan.owns_elements;
    chan.senders.push_back(gid);
    ChannelProgress::Blocked
}

fn chan_recv(
    g: &mut core::Global,
    gid: i64,
    core_id: i64,
    caller: &mut TaskState,
) -> ChannelProgress {
    let Some(chan) = g.chans.get_mut(&core_id) else {
        // Channel gone: receive returns the zero value.
        caller.recv_result = NULL;
        caller.recv_result_owned = false;
        caller.recv_ok = false;
        return ChannelProgress::Completed;
    };
    let mut woke = false;
    if let Some(v) = chan.buf.pop_front() {
        caller.recv_result = v;
        caller.recv_result_owned = chan.owns_elements;
        caller.recv_ok = true;
        if let Some(s) = chan.senders.pop_front() {
            let sv = core::TASKS
                .with_mut(s, |sg| {
                    sg.pending_send_owned = false;
                    std::mem::replace(&mut sg.pending_send, NULL)
                })
                .unwrap_or(NULL);
            chan.buf.push_back(sv);
            g.ready.push_back(s);
            woke = true;
        }
        return if woke {
            ChannelProgress::Handoff
        } else {
            ChannelProgress::Completed
        };
    }
    if let Some(s) = chan.senders.pop_front() {
        // Unbuffered: take the parked sender's value directly.
        let sv = core::TASKS
            .with_mut(s, |sg| {
                sg.pending_send_owned = false;
                std::mem::replace(&mut sg.pending_send, NULL)
            })
            .unwrap_or(NULL);
        caller.recv_result = sv;
        caller.recv_result_owned = chan.owns_elements;
        caller.recv_ok = true;
        g.ready.push_back(s);
        return ChannelProgress::Handoff;
    }
    if chan.closed {
        caller.recv_result = NULL;
        caller.recv_result_owned = false;
        caller.recv_ok = false;
        return ChannelProgress::Completed;
    }
    // Empty and open — park as a receiver.
    chan.receivers.push_back(gid);
    ChannelProgress::Blocked
}

/// Close a channel: no more sends; parked receivers are woken to drain or see
/// the zero value; parked senders are released.
pub(crate) fn chan_close(core_id: i64) {
    let mut g = core::GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let (drained, senders, owns_elements, count) = {
        let Some(chan) = g.chans.get_mut(&core_id) else {
            return;
        };
        if chan.closed {
            return;
        }
        chan.closed = true;
        let receivers: Vec<i64> = chan.receivers.drain(..).collect();
        let senders: Vec<i64> = chan.senders.drain(..).collect();
        let count = receivers.len() + senders.len();
        // Parked receivers drain whatever is buffered; the rest stays in the
        // buffer for future receives (close forbids sends, it does not
        // discard queued values — they are released when the channel itself
        // drops).
        let drained: Vec<(i64, Option<i64>)> = receivers
            .into_iter()
            .map(|r| (r, chan.buf.pop_front()))
            .collect();
        (drained, senders, chan.owns_elements, count)
    };
    let mut abandoned = Vec::new();
    for (r, got) in drained {
        let has_gr = core::TASKS.contains(r);
        match (got, has_gr, owns_elements) {
            (Some(value), true, _) => {
                core::TASKS.with_mut(r, |gr| {
                    gr.recv_result = value;
                    gr.recv_result_owned = owns_elements;
                    gr.recv_ok = true;
                });
            }
            (Some(value), false, true) => abandoned.push(value),
            (None, true, _) => {
                core::TASKS.with_mut(r, |gr| {
                    gr.recv_result = NULL;
                    gr.recv_result_owned = false;
                    gr.recv_ok = false;
                });
            }
            (None, false, _) => {}
            (Some(_), false, false) => {}
        }
        g.ready.push_back(r);
    }
    for s in senders {
        core::TASKS.with_mut(s, |gr| {
            if gr.pending_send != NULL {
                abandoned.push(gr.pending_send);
            }
            gr.pending_send = NULL;
            gr.pending_send_owned = false;
        });
        g.ready.push_back(s);
    }
    core::sync_ready_len(&g);
    drop(g);
    if owns_elements {
        for value in abandoned {
            let _ = crate::registry::remove(value);
        }
    }
    if count > 0 {
        notify_all();
    }
}

/// The current goroutine id on this worker, if any.
pub(crate) fn current_gid() -> Option<i64> {
    CURRENT_GID.with(|cur| cur.get())
}

/// Park the current goroutine with the given wait target (does not requeue).
/// Writes the worker-local state; [`drive`] flushes it after the poll returns.
pub(crate) fn park_self(park: Park) {
    if current_gid().is_none() {
        return;
    }
    CURRENT_STATE.with(|state| {
        let mut current = state.get();
        current.park = park;
        state.set(current);
    });
}

/// Park the current goroutine on a channel send with the given value.
pub(crate) fn park_chan_send(core_id: i64, value: i64) {
    if current_gid().is_none() {
        return;
    }
    CURRENT_STATE.with(|state| {
        let mut current = state.get();
        current.pending_send = value;
        current.pending_send_owned = false;
        current.park = Park::Chan {
            core: core_id,
            op: ChanOp::Send,
        };
        state.set(current);
    });
}

/// Park the current goroutine on a channel receive.
pub(crate) fn park_chan_recv(core_id: i64) {
    if current_gid().is_none() {
        return;
    }
    CURRENT_STATE.with(|state| {
        let mut current = state.get();
        current.recv_result = NULL;
        current.recv_result_owned = false;
        current.recv_ok = false;
        current.park = Park::Chan {
            core: core_id,
            op: ChanOp::Recv,
        };
        state.set(current);
    });
}

#[repr(i8)]
pub(crate) enum ChannelPoll {
    Pending = 0,
    Ready = 1,
    Invalid = 2,
}

pub(crate) fn poll_chan_send(core_id: i64, value: i64) -> ChannelPoll {
    park_chan_send(core_id, value);
    poll_channel(core_id, ChanOp::Send)
}

pub(crate) fn poll_chan_recv(core_id: i64) -> ChannelPoll {
    park_chan_recv(core_id);
    poll_channel(core_id, ChanOp::Recv)
}

fn poll_channel(core_id: i64, op: ChanOp) -> ChannelPoll {
    if CHANNEL_POLLS_LEFT.with(|budget| budget.get()) == 0 {
        return ChannelPoll::Pending;
    }
    let Some(gid) = current_gid() else {
        return ChannelPoll::Pending;
    };
    let mut g = core::GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(chan) = g.chans.get(&core_id) else {
        return ChannelPoll::Pending;
    };
    let ready = chan.closed
        || match op {
            ChanOp::Send => !chan.receivers.is_empty() || chan.buf.len() < chan.cap,
            ChanOp::Recv => !chan.senders.is_empty() || !chan.buf.is_empty(),
        };
    if !ready {
        return ChannelPoll::Pending;
    }
    if !core::TASKS.contains(gid) {
        return ChannelPoll::Pending;
    }
    let mut state = CURRENT_STATE.with(|state| state.get());

    // Readiness and completion share GLOBAL. Only an immediately completed
    // operation runs here; a blocked caller is not published until its poll ends.
    CHANNEL_POLLS_LEFT.with(|budget| budget.set(budget.get() - 1));
    let progress = chan_op(&mut g, gid, core_id, op, &mut state);
    state.park = Park::None;
    if core::TASKS.with_mut(gid, |gr| state.store(gr)).is_none() {
        return ChannelPoll::Pending;
    }
    CURRENT_STATE.with(|current| current.set(state));
    if progress == ChannelProgress::Handoff {
        core::sync_ready_len(&g);
    }
    drop(g);
    if progress == ChannelProgress::Handoff {
        notify_all();
    }
    ChannelPoll::Ready
}

/// Park the current goroutine until `deadline_ms` (wall clock).
pub(crate) fn park_timer(deadline_ms: i64) {
    park_self(Park::Timer { at: deadline_ms });
}

/// Park the current goroutine on fd readiness.
pub(crate) fn park_fd(io: i64, read: bool) {
    park_self(Park::Fd { io, read });
}

/// Park the current goroutine until `target` completes.
pub(crate) fn park_join(target: i64) {
    park_self(Park::Join { target });
}

/// The result handle recorded on the current goroutine by its last receive.
pub(crate) fn recv_result() -> i64 {
    if current_gid().is_none() {
        return NULL;
    }
    CURRENT_STATE.with(|state| {
        let mut current = state.get();
        let result = current.recv_result;
        current.recv_result = NULL;
        current.recv_result_owned = false;
        state.set(current);
        result
    })
}

/// Whether the last receive on this goroutine delivered a real value.
pub(crate) fn recv_ok() -> bool {
    if current_gid().is_none() {
        return false;
    }
    CURRENT_STATE.with(|state| state.get().recv_ok)
}

/// Block the calling *OS thread* until the goroutine completes, then return its
/// result handle. Used by the synchronous runtime bridge when a caller wants to
/// wait on a spawned goroutine.
pub(crate) fn join_blocking(gid: i64) -> i64 {
    let Some((result, exception)) = core::TASKS.join(gid) else {
        return NULL;
    };
    if exception != NULL {
        crate::rearm_pending_exception(exception);
    }
    result
}

/// Join worker threads, abandoning (detaching) any still blocked past the
/// grace period so shutdown cannot hang on a wedged worker. Finished
/// threads are joined normally; only the stuck remainder is abandoned.
fn join_with_grace(workers: Vec<std::thread::JoinHandle<()>>) {
    let mut pending = workers;
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(shutdown_grace_ms());
    let mut index = 0;
    while index < pending.len() {
        if pending[index].is_finished() {
            let handle = pending.remove(index);
            let _ = handle.join();
            continue;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        index += 1;
        if index >= pending.len() {
            index = 0;
        }
    }
    // Dropping the surviving handles detaches their threads.
    drop(pending);
}

/// Stop the worker pool and reactor, joining their threads. Idempotent, and
/// resets global state so it can be reused across tests in one binary.
///
/// Workers wedged in never-returning blocking sections (an infinite blocking
/// loop, with no suspension point for the stop flag to reach) are abandoned
/// after the grace period and die with the process instead of hanging
/// shutdown forever (Tokio's `shutdown_timeout` precedent). Dropping a
/// `JoinHandle` detaches its thread; wedged workers hold no scheduler locks
/// while blocked, so abandoning them cannot deadlock the teardown below.
pub(crate) fn shutdown() {
    if !SHUTDOWN.swap(true, Ordering::Relaxed) {
        // Unconditional: a worker that is about to park has not yet been
        // counted in `IDLE`, and it must still observe the stop flag.
        notify_all_unconditional();
    }
    let workers: Vec<_> = std::mem::take(&mut *WORKERS.lock().unwrap_or_else(|p| p.into_inner()));
    join_with_grace(workers);
    let reactor = REACTOR.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(r) = reactor {
        let _ = r.join();
    }
    super::reactor::reset();
    // Stop and join the offload pool: flag the workers and wake them with a
    // sentinel job each so they exit their blocking `recv`. A job that never
    // returns (a hung fetch or gateway `block_on`) must not hang shutdown
    // forever, so apply the same grace period as scheduler workers and
    // detach the stragglers to die with the process.
    if OFFLOADING.load(Ordering::SeqCst) {
        OFFLOAD_STOP.store(true, Ordering::Relaxed);
        let count = OFFLOAD_THREADS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len();
        let send = OFFLOAD_SEND.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(tx) = send.as_ref() {
            for _ in 0..count {
                let _ = tx.send(Box::new(|| {}));
            }
        }
        drop(send);
        let offload: Vec<_> =
            std::mem::take(&mut *OFFLOAD_THREADS.lock().unwrap_or_else(|p| p.into_inner()));
        join_with_grace(offload);
        OFFLOAD_SEND
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        OFFLOADING.store(false, Ordering::SeqCst);
        OFFLOAD_STOP.store(false, Ordering::Relaxed);
    }
    // Reclaim goroutines that were never joined or driven, releasing their
    // handles now that the workers have stopped.
    struct Abandoned {
        handle: i64,
        exception: i64,
        result: i64,
        cleanup: Option<(core::CleanupFn, i64)>,
    }
    let mut g = core::GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let pending: Vec<Abandoned> = core::TASKS.drain(|gr| Abandoned {
        handle: gr.handle,
        exception: gr.pending_exception,
        result: if gr.done { gr.result } else { NULL },
        cleanup: gr.cleanup,
    });
    core::clear_ready(&mut g);
    g.timers.clear();
    let abandoned_ops: Vec<i64> = g
        .ops
        .drain()
        .filter_map(|(_, op)| (op.done && op.result != NULL).then_some(op.result))
        .collect();
    core::timers_reset();
    drop(g);
    for result in abandoned_ops {
        let _ = crate::registry::remove(result);
    }
    for abandoned in pending {
        crate::registry::remove_goroutine_handle(abandoned.handle);
        if abandoned.exception != NULL {
            let _ = crate::registry::remove(abandoned.exception);
        }
        if abandoned.result != NULL {
            let _ = crate::registry::remove(abandoned.result);
        }
        // A goroutine that never completed still owns its future; its drop
        // reaches back into the scheduler, so the global lock is released.
        if let Some((cleanup, future)) = abandoned.cleanup {
            cleanup(future);
        }
    }
    // The queues outlive the pool (a later `start` reuses them), so drain any
    // goroutine still queued rather than freeing the rings.
    for queue in queues() {
        let _ = queue.drain();
    }
    report_stats();
    IDLE_STATE.store(0, Ordering::SeqCst);
    STARTED.store(false, Ordering::Release);
    SHUTDOWN.store(false, Ordering::Relaxed);
}

/// Records the reactor thread's join handle for [`shutdown`].
pub(crate) fn register_reactor_handle(handle: JoinHandle<()>) {
    *REACTOR.lock().unwrap_or_else(|p| p.into_inner()) = Some(handle);
}

// Re-exported for the ABI layer.
pub(crate) use core::complete_op;
pub(crate) use core::drop_chan;
pub(crate) use core::drop_goroutine;
pub(crate) use core::drop_io;
pub(crate) use core::drop_op;
pub(crate) use core::io_ready;
pub(crate) use core::op_done;
pub(crate) use core::op_result;
pub(crate) use core::register_chan;
pub(crate) use core::register_io;
pub(crate) use core::register_op;

/// Wake up to `count` idle workers.
pub(crate) fn wake_workers(count: usize) {
    if count == 0 || !should_wake() {
        return;
    }
    let (lock, cvar) = &*SIGNAL;
    let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
    for _ in 0..count {
        cvar.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_poll_and_completion_do_not_need_the_parking_lock() {
        extern "C" fn completed(_: i64) -> i8 {
            1
        }
        let handle = crate::registry::reserve_id();
        let gid = core::register_goroutine(new_goroutine(completed, NULL, None, handle));
        crate::registry::insert_reserved(handle, crate::registry::Handle::Goroutine { core: gid });
        let parking = core::GLOBAL.lock().unwrap();
        let (send, recv) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            drive(gid);
            send.send(()).unwrap();
        });
        recv.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(join_blocking(gid), NULL);
        drop(parking);
        worker.join().unwrap();
        crate::registry::goroutine_drop(handle);
    }

    #[test]
    fn async_join_registration_cannot_miss_concurrent_completion_on_the_same_shard() {
        extern "C" fn completed(_: i64) -> i8 {
            1
        }
        for _ in 0..64 {
            let offset = super::super::task_table::SHARD_COUNT as i64;
            let target = core::reserve_core_ids(offset as usize + 1);
            let joiner = target + offset;
            core::TASKS.insert(target, new_goroutine(completed, NULL, None, NULL));
            core::TASKS.insert(joiner, new_goroutine(completed, NULL, None, NULL));
            let mut parking = core::Global {
                next_core: 1,
                ready: Default::default(),
                chans: Default::default(),
                ios: Default::default(),
                ops: Default::default(),
                timers: Default::default(),
            };
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let other = Arc::clone(&barrier);
            let worker = std::thread::spawn(move || {
                other.wait();
                core::TASKS
                    .with_mut(target, |task| {
                        task.done = true;
                        std::mem::take(&mut task.joiners)
                    })
                    .unwrap()
            });
            barrier.wait();
            let already_done = join_park(&mut parking, joiner, target);
            let woken = worker.join().unwrap();
            if already_done {
                assert!(parking.ready.contains(&joiner));
                assert!(woken.is_empty());
            } else {
                assert_eq!(woken, [joiner]);
            }
            core::TASKS.remove(target);
            core::TASKS.remove(joiner);
        }
    }

    #[test]
    fn immediate_channel_polls_have_a_budget_and_do_not_publish_the_caller() {
        extern "C" fn completed(_: i64) -> i8 {
            1
        }

        let channel = register_chan(CHANNEL_POLL_BUDGET as usize + 1, false);
        let gid = core::register_goroutine(new_goroutine(completed, NULL, None, NULL));
        let previous_gid = CURRENT_GID.with(|current| current.replace(Some(gid)));
        let previous_state = CURRENT_STATE.with(|state| state.replace(TaskState::IDLE));
        let previous_budget = CHANNEL_POLLS_LEFT.with(|budget| budget.replace(CHANNEL_POLL_BUDGET));
        for value in 0..CHANNEL_POLL_BUDGET {
            assert!(matches!(
                poll_chan_send(channel, i64::from(value)),
                ChannelPoll::Ready
            ));
        }
        assert!(matches!(poll_chan_send(channel, 99), ChannelPoll::Pending));
        {
            let g = core::GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
            let chan = &g.chans[&channel];
            assert_eq!(chan.buf.len(), CHANNEL_POLL_BUDGET as usize);
            assert!(chan.senders.is_empty());
            assert!(!g.ready.contains(&gid));
        }

        CURRENT_STATE.with(|state| state.set(TaskState::IDLE));
        CHANNEL_POLLS_LEFT.with(|budget| budget.set(CHANNEL_POLL_BUDGET));
        for value in 0..CHANNEL_POLL_BUDGET {
            assert!(matches!(poll_chan_recv(channel), ChannelPoll::Ready));
            assert!(recv_ok());
            assert_eq!(recv_result(), i64::from(value));
        }
        assert!(matches!(poll_chan_recv(channel), ChannelPoll::Pending));
        {
            let g = core::GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
            assert!(g.chans[&channel].receivers.is_empty());
            assert!(!g.ready.contains(&gid));
        }
        CHANNEL_POLLS_LEFT.with(|budget| budget.set(1));
        chan_close(channel);
        assert!(matches!(poll_chan_recv(channel), ChannelPoll::Ready));
        assert!(!recv_ok());
        assert_eq!(recv_result(), NULL);

        CURRENT_GID.with(|current| current.set(previous_gid));
        CURRENT_STATE.with(|state| state.set(previous_state));
        CHANNEL_POLLS_LEFT.with(|budget| budget.set(previous_budget));
        core::drop_goroutine(gid);
        core::drop_chan(channel);
        assert_eq!(
            crate::ntask_chan_send_poll(NULL, NULL),
            ChannelPoll::Invalid as i8
        );
        assert_eq!(
            crate::ntask_chan_recv_poll(NULL),
            ChannelPoll::Invalid as i8
        );
        assert_eq!(crate::ntask_chan_send(NULL, NULL), 1);
        assert_eq!(crate::ntask_chan_recv(NULL), 1);
    }

    #[test]
    fn wakeups_count_workers_not_reserved_queue_slots() {
        let four_running = 4 << UNPARKED_SHIFT;
        assert!(!needs_idle_worker_wakeup(four_running, 4));
        assert!(needs_idle_worker_wakeup(four_running, 5));
        assert!(needs_idle_worker_wakeup(3 << UNPARKED_SHIFT, 4));
        assert!(!needs_idle_worker_wakeup((3 << UNPARKED_SHIFT) | 1, 4));
        assert!(!needs_idle_worker_wakeup(0, 0));
    }

    #[test]
    fn completion_only_reclaims_successful_detached_tasks() {
        extern "C" fn completed(_: i64) -> i8 {
            1
        }

        let detached = core::register_goroutine(new_goroutine(completed, NULL, None, NULL));
        drive(detached);
        assert!(!core::TASKS.contains(detached));

        let handle = crate::registry::reserve_id();
        let joinable = core::register_goroutine(new_goroutine(completed, NULL, None, handle));
        crate::registry::insert_reserved(
            handle,
            crate::registry::Handle::Goroutine { core: joinable },
        );
        drive(joinable);
        assert!(
            core::TASKS
                .with(joinable, |goroutine| goroutine.done)
                .unwrap_or(false)
        );
        assert_eq!(join_blocking(joinable), NULL);
        crate::registry::goroutine_drop(handle);

        let failed = core::register_goroutine(new_goroutine(completed, NULL, None, NULL));
        let exception = crate::registry::put_string("detached failure".to_string());
        crate::rearm_pending_exception(exception);
        drive(failed);
        assert!(
            core::TASKS
                .with(failed, |goroutine| goroutine.done
                    && goroutine.pending_exception == exception)
                .unwrap_or(false)
        );
        assert_eq!(join_blocking(failed), NULL);
        assert_eq!(crate::take_pending_exception_message(), exception);
        core::drop_goroutine(failed);
        assert!(crate::registry::get_string(exception).is_some());
        assert!(crate::registry::remove(exception).is_some());
        assert!(crate::registry::get_string(exception).is_none());
    }

    #[test]
    fn blocking_joiners_do_not_depend_on_worker_idle_notifications() {
        extern "C" fn completed(_: i64) -> i8 {
            1
        }

        let handle = crate::registry::reserve_id();
        let gid = core::register_goroutine(new_goroutine(completed, NULL, None, handle));
        crate::registry::insert_reserved(handle, crate::registry::Handle::Goroutine { core: gid });

        // Completion must reach blocking joiners even while worker signals
        // cannot be delivered, including multiple simultaneous joiners.
        let worker_signal = SIGNAL.0.lock().unwrap_or_else(|p| p.into_inner());
        let (started_send, started_recv) = mpsc::channel();
        let (finished_send, finished_recv) = mpsc::channel();
        let joiners: Vec<_> = (0..8)
            .map(|_| {
                let started = started_send.clone();
                let finished = finished_send.clone();
                std::thread::spawn(move || {
                    started.send(()).unwrap();
                    finished.send(join_blocking(gid)).unwrap();
                })
            })
            .collect();
        for _ in &joiners {
            started_recv
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        let driver = std::thread::spawn(move || drive(gid));
        for _ in &joiners {
            assert_eq!(
                finished_recv
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap(),
                NULL
            );
        }
        drop(worker_signal);
        driver.join().unwrap();
        for joiner in joiners {
            joiner.join().unwrap();
        }
        crate::registry::goroutine_drop(handle);
    }

    #[test]
    fn block_guard_is_a_noop_off_scheduler_workers() {
        // Unit tests run on plain threads (no WORKER_INDEX), so entering
        // and leaving blocking sections must not touch the counters.
        assert_eq!(BLOCKED_WORKERS.load(Ordering::SeqCst), 0);
        worker_will_block();
        worker_did_unblock();
        {
            let _guard = crate::BlockGuard::enter();
        }
        assert_eq!(BLOCKED_WORKERS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn watchdog_helpers_are_safe_to_call_repeatedly() {
        // The watchdog is always active; alternate ids so the streak never
        // reaches the report threshold (no stderr noise), then verify the
        // streak/yield math and the settled reset without overflow.
        for i in 0..300_000u32 {
            watchdog_note_drive((i % 2) as i64);
        }
        watchdog_note_settled(0);
        watchdog_note_settled(1);
        watchdog_note_settled(12345);
        assert!(watchdog_note_drive(9) < SPIN_YIELD_THRESHOLD);
    }

    #[test]
    fn alternating_instant_requeues_still_get_a_scheduler_yield_budget() {
        WATCH_DRIVES.with(|count| count.set(0));
        for i in 0..SPIN_YIELD_THRESHOLD {
            watchdog_note_drive((i % 3) as i64);
            assert_eq!(note_instant_drive(), i + 1 == SPIN_YIELD_THRESHOLD);
        }
        assert_eq!(WATCH_DRIVES.with(|count| count.get()), 0);
        watchdog_note_settled(0);
        assert_eq!(WATCH_DRIVES.with(|count| count.get()), 0);
    }

    #[test]
    fn temp_worker_exits_idle_and_returns_its_slot() {
        // No shutdown() here: like the offload pool, the started worker
        // pool persists for the rest of the test binary (shutting down
        // would clear scheduler-global state other tests may be using).
        start();
        let baseline_extra = EXTRA_WORKERS.load(Ordering::SeqCst);
        let baseline_spawns = TEMP_SPAWNS.load(Ordering::Relaxed);
        spawn_temp_worker();
        assert_eq!(EXTRA_WORKERS.load(Ordering::SeqCst), baseline_extra + 1);
        // An idle temp exits after about a second and releases its slot;
        // allow generous wall-clock slack for loaded CI hosts.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while EXTRA_WORKERS.load(Ordering::SeqCst) != baseline_extra {
            assert!(
                std::time::Instant::now() < deadline,
                "temp worker never exited"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(TEMP_SPAWNS.load(Ordering::Relaxed) > baseline_spawns);
        // The slot came back: a second spawn succeeds immediately.
        spawn_temp_worker();
        assert_eq!(EXTRA_WORKERS.load(Ordering::SeqCst), baseline_extra + 1);
    }
}
