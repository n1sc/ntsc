//! Core state types of the ntroutine substrate: the global scheduling and
//! parking registry, channels, goroutines, and reactor interests.
//!
//! All blocking coordination (which goroutine is parked on which channel, the
//! channel buffers, timers, and file-descriptor interests) lives under a single
//! [`Mutex`] guarded [`Global`] state, so a block/unblock decision and the
//! registration of a waiter are one atomic critical section — there is no lost
//! wakeup. Runnable goroutines are handed to per-worker local queues (see
//! [`crate::ntask::scheduler`]), so CPU-bound work spreads across the OS-thread
//! pool while I/O-bound goroutines park without tying up a thread.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::idmap::IdMap;
use crate::registry::{self, NULL};

/// A poll function for one async future. Returns `1` when the future
/// completed, `0` when it is still pending. See [`crate::AsyncPollFn`].
pub(crate) type PollFn = crate::AsyncPollFn;

/// Reclaims one abandoned future. See [`crate::AsyncCleanupFn`].
pub(crate) type CleanupFn = crate::AsyncCleanupFn;

/// A per-goroutine wait target set by generated code before a poll returns
/// `0`. The worker's driver reads it after the poll to decide whether to
/// requeue, park on a channel, park on a timer, or park on a descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Park {
    /// No explicit wait: a cooperative yield. The goroutine is requeued.
    None,
    /// Wait on a channel core, performing the given operation.
    Chan { core: i64, op: ChanOp },
    /// Wait until the given wall-clock deadline.
    Timer { at: i64 },
    /// Wait for readiness on a descriptor, for the given operation.
    Fd { io: i64, read: bool },
    /// Wait until a sibling goroutine completes.
    Join { target: i64 },
    /// Wait until an external worker thread completes an offloaded blocking
    /// job (see [`AsyncOp`]). The result handle is stored on the goroutine.
    Job { core: i64 },
}

/// Direction of a channel operation a parked goroutine wants to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChanOp {
    Send,
    Recv,
}

/// A goroutine: one stackless future scheduled onto the worker pool. It does
/// not rely on any worker thread-local async stack, so a torn-out goroutine is
/// migratable between workers.
#[derive(Debug)]
pub(crate) struct Goroutine {
    /// Nested async task stack. It moves with the goroutine between workers.
    pub(crate) tasks: Vec<(PollFn, i64)>,
    /// Registry id of this goroutine's `Handle::Goroutine` wrapper, so it is
    /// reclaimed by one removal when the goroutine finishes.
    pub(crate) handle: i64,
    /// Reclaims the root future if this goroutine never completes, so an
    /// abandoned future's owned handles are released at shutdown. `None` for a
    /// goroutine whose future is not heap-allocated by generated code.
    pub(crate) cleanup: Option<(CleanupFn, i64)>,
    /// The value a blocked sender is handing off, or `NULL`.
    pub(crate) pending_send: i64,
    /// Whether `pending_send` is an owned channel element.
    pub(crate) pending_send_owned: bool,
    /// The value a receiver picked up (or the zero value on close), or `NULL`.
    pub(crate) recv_result: i64,
    /// Whether `recv_result` is an owned channel element not yet consumed by
    /// the generated future.
    pub(crate) recv_result_owned: bool,
    /// Whether `recv_result` carries a real value (`false` after a close-drain
    /// or a failed receive). `for v in chan` needs the distinction because a
    /// raw scalar `0` is a legal received value.
    pub(crate) recv_ok: bool,
    /// `true` once the future has completed (its result is in `result`).
    pub(crate) done: bool,
    /// The result handle of a completed goroutine.
    pub(crate) result: i64,
    /// Message of an uncaught exception that ended this goroutine's future,
    /// or `NULL`. Ownership transfers to the thread that joins it, which
    /// re-raises it so the caller observes the pending exception.
    pub(crate) pending_exception: i64,
    /// Sibling goroutines parked waiting for this one to complete.
    pub(crate) joiners: Vec<i64>,
}

/// A channel: an optional bounded ring buffer of `i64` slots plus the parked
/// senders and receivers. Slots store raw scalar values or owned handles
/// (for heap element types); ownership of a handle slot transfers to the
/// receiver when it is received out, and back to the runtime when the channel
/// is dropped.
#[derive(Debug)]
pub(crate) struct Chan {
    pub(crate) buf: VecDeque<i64>,
    pub(crate) cap: usize,
    pub(crate) owns_elements: bool,
    pub(crate) closed: bool,
    pub(crate) senders: VecDeque<i64>,
    pub(crate) receivers: VecDeque<i64>,
}

impl Chan {
    pub(crate) fn new(cap: usize, owns_elements: bool) -> Self {
        Chan {
            buf: VecDeque::with_capacity(cap),
            cap,
            owns_elements,
            closed: false,
            senders: VecDeque::new(),
            receivers: VecDeque::new(),
        }
    }
}

/// A reactor registration: a timer or a file-descriptor readiness interest
/// that can be polled from a goroutine.
#[derive(Debug)]
pub(crate) struct AsyncIo {
    /// `0` = timer, non-zero = the raw file descriptor interest.
    pub(crate) fd: i64,
    /// Poll the readiness without blocking.
    pub(crate) ready: bool,
    /// Whether a goroutine is currently parked on this interest.
    pub(crate) parked: bool,
    /// Goroutines parked waiting for this descriptor to become ready.
    pub(crate) waiters: Vec<i64>,
}

/// An offloaded blocking job. A goroutine parks on it while a worker-thread
/// (the bounded offload pool) runs the blocking work; that thread completes
/// the op, waking the parked goroutine. This is how a scheduler thread stays
/// free while a child process runs or a blocking socket transfer happens.
#[derive(Debug)]
pub(crate) struct AsyncOp {
    /// The goroutine parked waiting for completion, if any.
    pub(crate) waiter: Option<i64>,
    /// The result handle once the job finished, or `NULL`.
    pub(crate) result: i64,
    /// Whether the worker finished the job.
    pub(crate) done: bool,
}

/// The global scheduling + parking state, guarded by one mutex.
pub(crate) struct Global {
    pub(crate) next_core: i64,
    /// Ready goroutine ids, shared queue consumed by workers. A global queue
    /// keeps the block/unblock path deadlock-free and still lets many workers
    /// run CPU-bound goroutines in parallel.
    pub(crate) ready: VecDeque<i64>,
    pub(crate) goroutines: IdMap<Goroutine>,
    pub(crate) chans: IdMap<Chan>,
    pub(crate) ios: IdMap<AsyncIo>,
    /// Offloaded blocking jobs not yet completed (or awaiting a reader).
    pub(crate) ops: IdMap<AsyncOp>,
    /// wall-clock ms deadline -> woken goroutine ids.
    pub(crate) timers: std::collections::BTreeMap<i64, Vec<i64>>,
}

pub(crate) static GLOBAL: LazyLock<Mutex<Global>> = LazyLock::new(|| {
    Mutex::new(Global {
        next_core: 1,
        ready: VecDeque::new(),
        goroutines: IdMap::default(),
        chans: IdMap::default(),
        ios: IdMap::default(),
        ops: IdMap::default(),
        timers: std::collections::BTreeMap::new(),
    })
});

/// Number of goroutines currently parked on timers.
static TIMER_PENDING: AtomicI64 = AtomicI64::new(0);

/// Adjust the pending-timer count by `delta` (negative to decrement). The
/// global lock must be held whenever a timer is parked or fired.
pub(crate) fn timers_pending_offset(delta: i64) {
    let _ = TIMER_PENDING.fetch_add(delta, Ordering::Relaxed);
}

/// Whether any goroutine is parked on a timer. The reactor fast path uses this
/// to skip the global lock when there is nothing to scan.
pub(crate) fn has_pending_timers() -> bool {
    TIMER_PENDING.load(Ordering::Relaxed) > 0
}

/// Reset the pending-timer count.
pub(crate) fn timers_reset() {
    TIMER_PENDING.store(0, Ordering::Relaxed);
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Length of `Global::ready`, mirrored outside the lock so a parking worker can
/// test the shared queue with one atomic load instead of taking the mutex.
static READY_LEN: AtomicUsize = AtomicUsize::new(0);

/// Whether the shared ready queue holds anything.
pub(crate) fn has_ready() -> bool {
    READY_LEN.load(Ordering::Acquire) > 0
}

/// Push a goroutine onto the shared ready queue. The overflow path for a full
/// worker ring, and the wakeup path from outside the pool.
pub(crate) fn push_ready(gid: i64) {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    guard.ready.push_back(gid);
    READY_LEN.store(guard.ready.len(), Ordering::Release);
}

/// Push a batch onto the shared ready queue under one lock acquisition.
pub(crate) fn push_ready_batch(ids: Vec<i64>) {
    if ids.is_empty() {
        return;
    }
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    guard.ready.extend(ids);
    READY_LEN.store(guard.ready.len(), Ordering::Release);
}

/// Take one goroutine from the shared ready queue.
pub(crate) fn pop_ready() -> Option<i64> {
    if !has_ready() {
        return None;
    }
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let gid = guard.ready.pop_front();
    READY_LEN.store(guard.ready.len(), Ordering::Release);
    gid
}

/// Take a share of the shared ready queue: `len / workers + 1`, capped so one
/// worker cannot swallow a burst the others could run in parallel.
pub(crate) fn pop_ready_batch(workers: usize) -> Vec<i64> {
    if !has_ready() {
        return Vec::new();
    }
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let share = guard.ready.len() / workers.max(1) + 1;
    let take = share.min(guard.ready.len()).min(READY_BATCH_CAP);
    let batch: Vec<i64> = guard.ready.drain(..take).collect();
    READY_LEN.store(guard.ready.len(), Ordering::Release);
    batch
}

/// Upper bound on one refill, so a worker never holds the shared lock long.
const READY_BATCH_CAP: usize = 128;

/// Clear the shared ready queue, keeping [`READY_LEN`] in step.
pub(crate) fn clear_ready(guard: &mut Global) {
    guard.ready.clear();
    READY_LEN.store(0, Ordering::Release);
}

/// Republish [`READY_LEN`] after a direct mutation of `Global::ready` made
/// while the caller already held the lock (the channel and reactor wake paths
/// push onto it as part of a larger critical section).
pub(crate) fn sync_ready_len(guard: &Global) {
    READY_LEN.store(guard.ready.len(), Ordering::Release);
}

/// Register a new goroutine and return its core id. The goroutine is not
/// scheduled until [`crate::ntask::scheduler::make_runnable`] is called for it.
pub(crate) fn register_goroutine(g: Goroutine) -> i64 {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let id = guard.next_core;
    guard.next_core = guard.next_core.saturating_add(1);
    guard.goroutines.insert(id, g);
    id
}

/// Register a goroutine and queue it as runnable in one critical section.
/// A spawn from outside the worker pool (`main` spawning in a loop) would
/// otherwise take the global lock twice per goroutine.
pub(crate) fn register_goroutine_runnable(g: Goroutine) -> i64 {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let id = guard.next_core;
    guard.next_core = guard.next_core.saturating_add(1);
    guard.goroutines.insert(id, g);
    guard.ready.push_back(id);
    sync_ready_len(&guard);
    id
}

/// Reserve a contiguous range of scheduler core ids. Callers can then build
/// a batch of goroutines without taking the global lock once per child; the
/// ids are still globally unique because the range reservation is atomic with
/// respect to every other core registration.
pub(crate) fn reserve_core_ids(count: usize) -> i64 {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let start = guard.next_core;
    guard.next_core = guard.next_core.saturating_add(count.max(1) as i64);
    start
}

/// Register a new channel and return its core id.
pub(crate) fn register_chan(cap: usize, owns_elements: bool) -> i64 {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let id = guard.next_core;
    guard.next_core = guard.next_core.saturating_add(1);
    guard.chans.insert(id, Chan::new(cap, owns_elements));
    id
}

/// Register a new reactor interest and return its core id.
pub(crate) fn register_io() -> i64 {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let id = guard.next_core;
    guard.next_core = guard.next_core.saturating_add(1);
    guard.ios.insert(
        id,
        AsyncIo {
            fd: 0,
            ready: false,
            parked: false,
            waiters: Vec::new(),
        },
    );
    id
}

/// Drop a goroutine core and clean up any global wait queues it was
/// parked on (timers, channels, io, offloaded jobs). Otherwise a
/// `go` that was parked on `async.sleep` and then dropped (e.g. its
/// `Handle::Goroutine` was reaped after `main` returned) would keep its
/// deadline in `timers` and `TIMER_PENDING` would stay >0, keeping the
/// reactor spinning. The same applies to `chan`/`io`/`job` waiters.
pub(crate) fn drop_goroutine(core: i64) {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(removed) = guard.goroutines.remove(&core) else {
        return;
    };
    let mut abandoned = Vec::new();
    if removed.pending_send_owned && removed.pending_send != NULL {
        abandoned.push(removed.pending_send);
    }
    if removed.recv_result_owned && removed.recv_result != NULL {
        abandoned.push(removed.recv_result);
    }
    if removed.pending_exception != NULL {
        abandoned.push(removed.pending_exception);
    }
    if removed.done && removed.result != NULL {
        abandoned.push(removed.result);
    }
    let cleanup = (!removed.done).then_some(removed.cleanup);
    // Timers: remove this gid from any deadline bucket.
    let mut removed_timers = 0i64;
    guard.timers.retain(|_, gids| {
        let before = gids.len();
        gids.retain(|&gid| gid != core);
        removed_timers += (before - gids.len()) as i64;
        !gids.is_empty()
    });
    if removed_timers > 0 {
        timers_pending_offset(-removed_timers);
    }
    // Channels: remove from any senders/receivers queue.
    for chan in guard.chans.values_mut() {
        chan.senders.retain(|&gid| gid != core);
        chan.receivers.retain(|&gid| gid != core);
    }
    // Reactor io: remove from any waiters list.
    for io in guard.ios.values_mut() {
        io.waiters.retain(|&gid| gid != core);
        if io.waiters.is_empty() {
            io.parked = false;
        }
    }
    // Offloaded jobs: clear waiter if it was this gid.
    for op in guard.ops.values_mut() {
        if op.waiter == Some(core) {
            op.waiter = None;
        }
    }
    drop(guard);
    for value in abandoned {
        let _ = registry::remove(value);
    }
    if let Some(Some((cleanup, future))) = cleanup {
        cleanup(future);
    }
}

/// Drop a channel core, reclaiming any buffered element handles.
pub(crate) fn drop_chan(core: i64) {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(chan) = guard.chans.remove(&core) {
        for slot in chan.buf {
            if chan.owns_elements {
                let _ = registry::remove(slot);
            }
        }
    }
}

/// Drop a reactor-interest core.
pub(crate) fn drop_io(core: i64) {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    guard.ios.remove(&core);
}

pub(crate) fn io_ready(core: i64) -> bool {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(io) = guard.ios.get_mut(&core) else {
        return false;
    };
    std::mem::take(&mut io.ready)
}

/// Reserve a fresh offloaded-job id. The caller enqueues the blocking work
/// on the offload pool and parks the calling goroutine on it. See
/// [`crate::ntask::scheduler::offload_start`] and
/// [`crate::ntask::scheduler::complete_job`].
pub(crate) fn register_op() -> i64 {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let id = guard.next_core;
    guard.next_core = guard.next_core.saturating_add(1);
    guard.ops.insert(
        id,
        AsyncOp {
            waiter: None,
            result: NULL,
            done: false,
        },
    );
    id
}

/// Mark an offloaded job done with `result`. Callable from any thread
/// (typically a worker on the offload pool); wakes the parked goroutine, if
/// any.
pub(crate) fn complete_op(core: i64, result: i64) {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(op) = guard.ops.get_mut(&core) else {
        drop(guard);
        // A future can be dropped while its offload job is still running. The
        // job owns the produced registry handle until it is published here;
        // if cancellation removed the op first, reclaim that handle instead
        // of silently losing it.
        if result != NULL {
            let _ = registry::remove(result);
        }
        return;
    };
    op.result = result;
    op.done = true;
    let waiter = op.waiter.take();
    if let Some(gid) = waiter {
        guard.ready.push_back(gid);
        sync_ready_len(&guard);
    }
    drop(guard);
    if waiter.is_some() {
        crate::ntask::scheduler::notify_all();
    }
}

/// Take the result handle of a completed offloaded job, removing it from the
/// table. Ownership of the handle transfers to the caller. Returns `NULL`
/// for an unknown or not-yet-completed job.
pub(crate) fn op_result(core: i64) -> i64 {
    let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(op) = guard.ops.remove(&core) else {
        return NULL;
    };
    op.result
}

/// Whether the offloaded job is already done (`false` for an unknown id).
pub(crate) fn op_done(core: i64) -> bool {
    GLOBAL
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .ops
        .get(&core)
        .map(|op| op.done)
        .unwrap_or(false)
}

/// Drop an offloaded job that will never be reaped (e.g. its future was
/// dropped mid-flight), releasing the result handle if one arrived.
pub(crate) fn drop_op(core: i64) {
    let result = {
        let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .ops
            .remove(&core)
            .filter(|op| op.done)
            .map(|op| op.result)
    };
    // Drop the scheduler lock before running arbitrary registry destructors.
    // An opaque result may have a destructor that needs another runtime lock.
    if let Some(result) = result.filter(|&result| result != NULL) {
        let _ = registry::remove(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A goroutine dropped while parked on a timer must leave no deadline
    /// behind: a stale entry would keep `TIMER_PENDING` above zero and the
    /// reactor scanning forever.
    #[test]
    fn completing_a_dropped_offload_reclaims_its_result() {
        let core = register_op();
        drop_op(core);
        let result = registry::put_string("orphaned offload result".to_string());
        complete_op(core, result);
        assert!(registry::get_string(result).is_none());
    }

    #[test]
    fn dropping_a_completed_offload_reclaims_its_result() {
        let core = register_op();
        let result = registry::put_string("completed offload result".to_string());
        complete_op(core, result);
        drop_op(core);
        assert!(registry::get_string(result).is_none());
    }

    #[test]
    fn async_result_transfers_once_and_cancelling_reclaims_late_result() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        let payload = registry::put_string("transferred async payload".to_string());
        let future = registry::async_op_new(Box::new(move || payload));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while registry::async_op_poll(future) == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(registry::async_op_result(future), payload);
        assert_eq!(registry::async_op_result(future), NULL);
        registry::async_op_drop(future);
        assert_eq!(
            registry::take_string(payload),
            Some("transferred async payload".to_string())
        );

        let release = Arc::new(AtomicBool::new(false));
        let produced = Arc::new(Mutex::new(None));
        let worker_release = Arc::clone(&release);
        let worker_produced = Arc::clone(&produced);
        let cancelled = registry::async_op_new(Box::new(move || {
            while !worker_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            let payload = registry::put_string("cancelled async payload".to_string());
            *worker_produced.lock().unwrap_or_else(|p| p.into_inner()) = Some(payload);
            payload
        }));
        assert_eq!(registry::async_op_poll(cancelled), 0);
        registry::async_op_drop(cancelled);
        release.store(true, Ordering::Release);

        let payload = loop {
            if let Some(payload) = *produced.lock().unwrap_or_else(|p| p.into_inner()) {
                break payload;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        while registry::get_string(payload).is_some() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(registry::get_string(payload).is_none());
    }

    #[test]
    fn dropping_a_timer_parked_goroutine_clears_its_deadline() {
        extern "C" fn never_done(_: i64) -> i8 {
            0
        }
        let core = register_goroutine(Goroutine {
            tasks: vec![(never_done as PollFn, NULL)],
            handle: NULL,
            cleanup: None,
            pending_send: NULL,
            pending_send_owned: false,
            recv_result: NULL,
            recv_result_owned: false,
            recv_ok: false,
            done: false,
            result: NULL,
            pending_exception: NULL,
            joiners: Vec::new(),
        });
        let deadline = now_ms() + 10_000;
        {
            let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
            guard.timers.entry(deadline).or_default().push(core);
        }
        timers_pending_offset(1);
        assert!(has_pending_timers());

        drop_goroutine(core);

        let guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
        assert!(!guard.timers.contains_key(&deadline));
        assert!(!guard.goroutines.contains_key(&core));
        drop(guard);
        assert!(!has_pending_timers());
    }

    /// A goroutine dropped while parked on a channel must be removed from the
    /// channel's wait queues, or a later handoff would target a dead id.
    #[test]
    fn dropping_a_chan_parked_goroutine_clears_its_wait_queues() {
        extern "C" fn never_done(_: i64) -> i8 {
            0
        }
        let chan_core = register_chan(0, false);
        let core = register_goroutine(Goroutine {
            tasks: vec![(never_done as PollFn, NULL)],
            handle: NULL,
            cleanup: None,
            pending_send: NULL,
            pending_send_owned: false,
            recv_result: NULL,
            recv_result_owned: false,
            recv_ok: false,
            done: false,
            result: NULL,
            pending_exception: NULL,
            joiners: Vec::new(),
        });
        {
            let mut guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
            let chan = guard.chans.get_mut(&chan_core).expect("channel");
            chan.senders.push_back(core);
        }

        drop_goroutine(core);

        let guard = GLOBAL.lock().unwrap_or_else(|p| p.into_inner());
        let chan = guard.chans.get(&chan_core).expect("channel");
        assert!(chan.senders.is_empty());
        assert!(chan.receivers.is_empty());
        drop(guard);
        drop_chan(chan_core);
    }

    #[test]
    fn dropping_a_goroutine_reclaims_owned_values_and_exception() {
        extern "C" fn never_done(_: i64) -> i8 {
            0
        }
        let send_value = registry::put_string("send".to_string());
        let recv_value = registry::put_string("recv".to_string());
        let exception = registry::put_string("exception".to_string());
        let core = register_goroutine(Goroutine {
            tasks: vec![(never_done as PollFn, NULL)],
            handle: NULL,
            cleanup: None,
            pending_send: send_value,
            pending_send_owned: true,
            recv_result: recv_value,
            recv_result_owned: true,
            recv_ok: true,
            done: true,
            result: NULL,
            pending_exception: exception,
            joiners: Vec::new(),
        });

        drop_goroutine(core);

        assert!(registry::get_string(send_value).is_none());
        assert!(registry::get_string(recv_value).is_none());
        assert!(registry::get_string(exception).is_none());
    }
}
