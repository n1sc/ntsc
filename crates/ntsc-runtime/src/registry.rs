//! Thread-safe handle registry backing the whole runtime ABI.
//!
//! Every owned heap value that crosses the FFI boundary is stored here under
//! an opaque `i64` key (a handle). Generated code never passes pointers to the
//! runtime: it passes handles, and the runtime resolves them inside this map.
//!
//! Ownership rules mirror the pointer-based ABI this replaces:
//!
//! * A handle is registered exactly once and removed exactly once. Copying a
//!   handle in generated code is a *borrow* and performs no registry
//!   operation; deep copies (`copy(...)`, string clone, array deep clone)
//!   register a fresh entry.
//! * Handle `0` is the null handle: every API treats it as "no value" and is
//!   a no-op for it.
//!
//! ## Locking
//!
//! Every operation here acquires the registry lock once, extracts the owned
//! data it needs (cloning strings, copying element vectors), and releases the
//! lock before registering new entries. Nested acquisitions (calling a
//! registry function from inside a [`borrow`]/[`borrow_mut`] closure) would
//! deadlock and must never happen.

use std::any::Any;
use std::collections::HashSet;

use crate::idmap::IdMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::sync::{LazyLock, Mutex};

/// The null handle: no value.
pub(crate) const NULL: i64 = 0;

/// The byte size of a pointer slot (string/array handles, raw i64/f64
/// slots).
pub(crate) const PTR_SIZE: usize = std::mem::size_of::<i64>();

/// The handle table, sharded by id: a hot request touches several handles
/// (strings, sockets, futures) across workers, and one global mutex
/// serializes exactly that traffic.
const REGISTRY_SHARDS: usize = 64;
static REGISTRY: LazyLock<Box<[Mutex<IdMap<Handle>>]>> = LazyLock::new(|| {
    (0..REGISTRY_SHARDS)
        .map(|_| Mutex::new(IdMap::default()))
        .collect()
});

/// The shard holding `id`. Every registry operation takes exactly one shard
/// and never holds it while calling back into the registry, so there is no
/// lock ordering to violate.
#[inline]
fn shard_lock(id: i64) -> std::sync::MutexGuard<'static, IdMap<Handle>> {
    REGISTRY[(id.unsigned_abs() as usize) & (REGISTRY_SHARDS - 1)]
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Lock every shard in index order, for whole-table reads.
fn all_shards() -> Vec<std::sync::MutexGuard<'static, IdMap<Handle>>> {
    REGISTRY
        .iter()
        .map(|shard| shard.lock().unwrap_or_else(|p| p.into_inner()))
        .collect()
}
static NEXT_ID: AtomicI64 = AtomicI64::new(1);
static LIVE: AtomicI64 = AtomicI64::new(0);

/// Handles registered as permanent (compile-time constants such as string
/// literals). They are owned by the program for its whole lifetime, are
/// never removed, and are excluded from leak reporting.
static PERMANENT: AtomicI64 = AtomicI64::new(0);
static PERMANENT_IDS: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static LEAK_SITES: LazyLock<Mutex<IdMap<(i64, i64)>>> =
    LazyLock::new(|| Mutex::new(IdMap::default()));

/// Bumped whenever an entry is removed. A cached capability is only reused
/// while this is unchanged, so a dropped handle can never be served from a
/// cache: any removal invalidates every cache at once.
static REGISTRY_GEN: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Last capability resolved on this thread. A load or store in a loop
    /// touches the same handle repeatedly, and resolving it through the
    /// registry would take the global lock every time; the cached weak
    /// reference avoids prolonging
    /// the allocation's lifetime after its last capability is dropped.
    static MEMORY_CACHE: std::cell::RefCell<Option<CachedRegion>> =
        const { std::cell::RefCell::new(None) };
}

struct CachedRegion {
    id: i64,
    generation: u64,
    bytes: Weak<Mutex<Vec<u8>>>,
    offset: usize,
}

/// Resolve a capability to its region, using the per-thread cache when it is
/// still valid. Falls back to one registry lookup, which then populates the
/// cache.
fn memory_region(id: i64) -> Option<(Arc<Mutex<Vec<u8>>>, usize)> {
    let generation = REGISTRY_GEN.load(Ordering::Acquire);
    let cached = MEMORY_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache.as_ref().and_then(|entry| {
            (entry.id == id && entry.generation == generation)
                .then(|| entry.bytes.upgrade().map(|bytes| (bytes, entry.offset)))
                .flatten()
        })
    });
    if let Some(region) = cached {
        return Some(region);
    }
    let region = borrow(id, |handle| match handle {
        Handle::Memory(memory) => Some((Arc::clone(&memory.bytes), memory.offset)),
        _ => None,
    })??;
    MEMORY_CACHE.with(|cache| {
        *cache.borrow_mut() = Some(CachedRegion {
            id,
            generation,
            bytes: Arc::downgrade(&region.0),
            offset: region.1,
        });
    });
    Some(region)
}

/// Clear the thread-local capability cache before shutdown. The cache uses a
/// weak reference, so it cannot keep the backing allocation alive, but
/// clearing it avoids retaining stale capability metadata on long-lived
/// embedding threads.
pub(crate) fn clear_memory_cache() {
    MEMORY_CACHE.with(|cache| {
        *cache.borrow_mut() = None;
    });
}

/// An owned value stored in the registry.
pub(crate) enum Handle {
    /// An owned string.
    String(String),

    /// An owned dynamic array.
    Array(ArrayData),

    /// A reference-counted shared box wrapping an owned inner handle.
    Shared(SharedData),

    /// The state machine of an `async.sleep(ms)` future.
    AsyncSleep { state: i32, ms: i64, deadline: i64 },

    /// A scheduled virtual goroutine owned by the task scheduler.
    Goroutine { core: i64 },

    /// A channel core owned by the task scheduler, reference counted
    /// like `Shared` so `go` and assignments can borrow the handle
    /// without destroying the channel for peers. `core` is the
    /// scheduler's `Chan` id, `count` is the number of live `Handle`
    /// copies.
    Chan { core: i64, count: u64 },

    /// A reactor registration for timers or descriptor readiness.
    ReactorReg { core: i64 },

    /// An asynchronous I/O future associated with a reactor core.
    AsyncIo { core: i64 },

    /// An offloaded blocking future (sync `http.*`/`process.*` run on the
    /// worker pool): `state` 0 = not started, 1 = awaiting an offloaded job,
    /// 2 = done, holding the reaped result handle. The `work` closure runs
    /// the blocking operation on the pool and completes the op it started.
    AsyncOp {
        state: i32,
        work: Option<Box<dyn FnOnce() -> i64 + Send + 'static>>,
        op: i64,
        result: i64,
    },

    /// An opaque value owned by a stdlib module (files, sockets,
    /// channels...).
    Opaque(Box<dyn Any + Send>),

    /// A cross-adaptor shared object: a heap value owned by one generated
    /// adaptor crate but referenceable from any other through the
    /// process-wide handle space (see the shared-object section below).
    SharedObject(SharedObjectData),

    /// A safe pointer capability into a shared byte allocation.
    Memory(MemoryData),

    /// A bounds-checked window over an owned array.
    Slice(SliceData),
}

/// A slice borrows a window of an array: it stores the source handle plus the
/// window, never a pointer, so every access re-validates that the source is
/// still alive and that the index is inside the window.
pub(crate) struct SliceData {
    pub(crate) array: i64,
    pub(crate) start: usize,
    pub(crate) len: usize,
}

pub(crate) struct MemoryData {
    pub(crate) bytes: Arc<Mutex<Vec<u8>>>,
    pub(crate) offset: usize,
}

/// The window of `id`, or `None` when it is not a slice.
fn slice_window(id: i64) -> Option<(i64, usize, usize)> {
    borrow(id, |handle| match handle {
        Handle::Slice(slice) => Some((slice.array, slice.start, slice.len)),
        _ => None,
    })?
}

/// Create a window over `array`. Rejects an inverted or out-of-range range,
/// and an array that is not registered.
pub(crate) fn slice_of(array: i64, start: i64, end: i64) -> Option<i64> {
    if start < 0 || end < start {
        return None;
    }
    let len = with_array(array, |a| a.len())?;
    let (start, end) = (start as usize, end as usize);
    if end > len {
        return None;
    }
    Some(insert(Handle::Slice(SliceData {
        array,
        start,
        len: end - start,
    })))
}

/// Narrow an existing window. Bounds are relative to the window, so a
/// subslice can never widen it or escape the original array.
pub(crate) fn slice_sub(id: i64, start: i64, end: i64) -> Option<i64> {
    if start < 0 || end < start {
        return None;
    }
    let (array, base, len) = slice_window(id)?;
    let (start, end) = (start as usize, end as usize);
    if end > len {
        return None;
    }
    Some(insert(Handle::Slice(SliceData {
        array,
        start: base + start,
        len: end - start,
    })))
}

pub(crate) fn slice_len(id: i64) -> Option<i64> {
    slice_window(id).map(|(_, _, len)| len as i64)
}

/// Read through the window. The registry lock is released before touching
/// the array, because the array lookup takes it again.
pub(crate) fn slice_get(id: i64, index: i64) -> Option<i64> {
    if index < 0 {
        return None;
    }
    let (array, start, len) = slice_window(id)?;
    if index as usize >= len {
        return None;
    }
    array_get(array, (start + index as usize) as i64)
}

pub(crate) fn slice_set(id: i64, index: i64, value: i64) -> bool {
    if index < 0 {
        return false;
    }
    let Some((array, start, len)) = slice_window(id) else {
        return false;
    };
    if index as usize >= len {
        return false;
    }
    array_set(array, (start + index as usize) as i64, value)
}

/// Copy the window into a fresh owned array.
pub(crate) fn slice_to_array(id: i64) -> Option<i64> {
    let (array, start, len) = slice_window(id)?;
    let (elem_size, string_elements) = with_array(array, |a| (a.elem_size, a.string_elements))?;
    let out = array_new(elem_size as i64, len as i64, i8::from(string_elements));
    for offset in 0..len {
        let Some(element) = array_get(array, (start + offset) as i64) else {
            array_drop(out);
            return None;
        };
        if !array_push(out, element) {
            array_drop(out);
            return None;
        }
    }
    Some(out)
}

pub(crate) fn slice_fill(id: i64, value: i64) -> bool {
    let Some((array, start, len)) = slice_window(id) else {
        return false;
    };
    (0..len).all(|offset| array_set(array, (start + offset) as i64, value))
}

/// Copy `src` into `dst` element-wise. Both windows must have the same
/// length, so a copy can never write past either one.
pub(crate) fn slice_copy_from(dst: i64, src: i64) -> bool {
    let (Some((dst_array, dst_start, dst_len)), Some((src_array, src_start, src_len))) =
        (slice_window(dst), slice_window(src))
    else {
        return false;
    };
    if dst_len != src_len {
        return false;
    }
    for offset in 0..dst_len {
        let Some(element) = array_get(src_array, (src_start + offset) as i64) else {
            return false;
        };
        if !array_set(dst_array, (dst_start + offset) as i64, element) {
            return false;
        }
    }
    true
}

/// Whether two windows hold equal element bits, element by element.
pub(crate) fn slice_equal(a: i64, b: i64) -> Option<bool> {
    let (a_array, a_start, a_len) = slice_window(a)?;
    let (b_array, b_start, b_len) = slice_window(b)?;
    if a_len != b_len {
        return Some(false);
    }
    for offset in 0..a_len {
        let left = array_get(a_array, (a_start + offset) as i64)?;
        let right = array_get(b_array, (b_start + offset) as i64)?;
        if left != right {
            return Some(false);
        }
    }
    Some(true)
}

/// Drop a window. The array it borrowed is untouched: a slice owns nothing
/// but its own registry entry.
pub(crate) fn slice_drop(id: i64) {
    if id == NULL {
        return;
    }
    let mut guard = shard_lock(id);
    if matches!(guard.get(&id), Some(Handle::Slice(_))) {
        guard.remove(&id);
        LIVE.fetch_sub(1, Ordering::Relaxed);
        REGISTRY_GEN.fetch_add(1, Ordering::Release);
    }
}

pub(crate) fn memory_alloc(size: usize) -> i64 {
    insert(Handle::Memory(MemoryData {
        bytes: Arc::new(Mutex::new(vec![0; size])),
        offset: 0,
    }))
}

pub(crate) fn memory_offset(id: i64, delta: i64) -> Option<i64> {
    let (bytes, offset) = borrow(id, |handle| match handle {
        Handle::Memory(memory) => Some((Arc::clone(&memory.bytes), memory.offset)),
        _ => None,
    })??;
    let new_offset = if delta >= 0 {
        offset.checked_add(delta as usize)?
    } else {
        offset.checked_sub(delta.unsigned_abs() as usize)?
    };
    let len = bytes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    (new_offset <= len).then(|| {
        insert(Handle::Memory(MemoryData {
            bytes,
            offset: new_offset,
        }))
    })
}

pub(crate) fn memory_clone(id: i64) -> Option<i64> {
    let (bytes, offset) = borrow(id, |handle| match handle {
        Handle::Memory(memory) => Some((Arc::clone(&memory.bytes), memory.offset)),
        _ => None,
    })??;
    Some(insert(Handle::Memory(MemoryData { bytes, offset })))
}

pub(crate) fn memory_drop(id: i64) {
    if id == NULL {
        return;
    }
    let mut guard = shard_lock(id);
    if matches!(guard.get(&id), Some(Handle::Memory(_))) {
        guard.remove(&id);
        LIVE.fetch_sub(1, Ordering::Relaxed);
        REGISTRY_GEN.fetch_add(1, Ordering::Release);
    }
}

pub(crate) fn memory_load(id: i64, width: usize) -> Option<i64> {
    let (bytes, offset) = memory_region(id)?;
    let bytes = bytes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let end = offset.checked_add(width)?;
    let slice = bytes.get(offset..end)?;
    let mut raw = [0_u8; 8];
    raw[..width].copy_from_slice(slice);
    Some(i64::from_le_bytes(raw))
}

pub(crate) fn memory_store(id: i64, width: usize, value: i64) -> bool {
    let Some((bytes, offset)) = memory_region(id) else {
        return false;
    };
    let mut bytes = bytes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(end) = offset.checked_add(width) else {
        return false;
    };
    let Some(slice) = bytes.get_mut(offset..end) else {
        return false;
    };
    slice.copy_from_slice(&value.to_le_bytes()[..width]);
    true
}

/// A dynamic array: typed element storage. Elements are stored as `i64`
/// raw bits for scalars (`elem_size` low bytes are significant) and as
/// handles for string and nested-array elements.
pub(crate) struct ArrayData {
    pub(crate) elem_size: usize,
    pub(crate) string_elements: bool,
    pub(crate) elements: Vec<i64>,
}

impl ArrayData {
    /// Create an empty array with the given element size and initial
    /// capacity.
    pub(crate) fn new(elem_size: usize, capacity: usize, string_elements: bool) -> Self {
        ArrayData {
            elem_size,
            string_elements,
            elements: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.elements.len()
    }

    /// The element value at `index`, or `None` when out of bounds.
    pub(crate) fn get(&self, index: usize) -> Option<i64> {
        self.elements.get(index).copied()
    }

    /// Push a raw element value (raw bits or a handle for string
    /// elements).
    pub(crate) fn push(&mut self, elem: i64) {
        self.elements.push(elem);
    }

    /// Remove and return the last element, if any.
    pub(crate) fn pop(&mut self) -> Option<i64> {
        self.elements.pop()
    }
}

/// A reference-counted shared box. `count` tracks how many copies of the
/// handle are live; when it reaches zero the box is removed and ownership
/// of `inner` is returned to the caller.
pub(crate) struct SharedData {
    pub(crate) inner: i64,
    pub(crate) count: u64,
}

fn next_id() -> i64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn insert_locked(
    guard: &mut std::sync::MutexGuard<'_, IdMap<Handle>>,
    id: i64,
    handle: Handle,
) -> i64 {
    guard.insert(id, handle);
    // Bumped under the lock, like every other counter mutation, so the two
    // can never be observed out of step (see `counters_agree_with_map`).
    LIVE.fetch_add(1, Ordering::Relaxed);
    id
}

/// Register an owned value and return its fresh handle.
pub(crate) fn insert(handle: Handle) -> i64 {
    let id = next_id();
    let mut guard = shard_lock(id);
    insert_locked(&mut guard, id, handle)
}

/// Reserve a handle id before its value exists, for a caller that must know the
/// id up front (a goroutine records its own wrapper id).
pub(crate) fn reserve_id() -> i64 {
    next_id()
}

/// Register `handle` under an id previously taken from [`reserve_id`].
pub(crate) fn insert_reserved(id: i64, handle: Handle) {
    let mut guard = shard_lock(id);
    if guard.insert(id, handle).is_none() {
        LIVE.fetch_add(1, Ordering::Relaxed);
    }
}

/// Register an owned value as permanent: it lives for the whole program
/// and is never removed, so it is excluded from leak reporting.
pub(crate) fn insert_permanent(handle: Handle) -> i64 {
    let id = next_id();
    let mut guard = shard_lock(id);
    guard.insert(id, handle);

    PERMANENT.fetch_add(1, Ordering::Relaxed);
    PERMANENT_IDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id);
    drop(guard);
    id
}

/// Remove and return the value behind `id`, if any.
pub(crate) fn remove(id: i64) -> Option<Handle> {
    if id == NULL {
        return None;
    }
    let mut guard = shard_lock(id);
    let taken = guard.remove(&id);
    if taken.is_some() {
        LIVE.fetch_sub(1, Ordering::Relaxed);
        REGISTRY_GEN.fetch_add(1, Ordering::Release);
    }
    drop(guard);
    taken
}

/// Borrow the value behind `id`. Returns `None` for the null handle or an
/// unknown handle. The closure must not call back into the registry.
pub(crate) fn borrow<R>(id: i64, f: impl FnOnce(&Handle) -> R) -> Option<R> {
    if id == NULL {
        return None;
    }
    let guard = shard_lock(id);
    let handle = guard.get(&id)?;
    Some(f(handle))
}

/// Mutably borrow the value behind `id`. Returns `None` for the null
/// handle or an unknown handle. The closure must not call back into the
/// registry.
pub(crate) fn borrow_mut<R>(id: i64, f: impl FnOnce(&mut Handle) -> R) -> Option<R> {
    if id == NULL {
        return None;
    }
    let mut guard = shard_lock(id);
    let handle = guard.get_mut(&id)?;
    Some(f(handle))
}

/// The number of live registry entries (used for leak reporting).
pub(crate) fn live_count() -> i64 {
    LIVE.load(Ordering::Relaxed)
}

/// Whether `id` currently has an entry in the registry.
#[cfg(test)]
pub(crate) fn is_registered(id: i64) -> bool {
    shard_lock(id).contains_key(&id)
}

/// `LIVE + PERMANENT` must always equal the number of entries in the map.
///
/// Every counter mutation happens while the registry lock is held, so the
/// two can only be observed in agreement — including while sibling tests
/// in this binary register and drop handles of their own. That is why the
/// counter tests assert this invariant instead of an absolute snapshot of
/// `LIVE`: a snapshot races with every other test in the process.
#[cfg(test)]
pub(crate) fn assert_counters_agree_with_map(step: &str) {
    let shards = all_shards();
    let live = LIVE.load(Ordering::Relaxed);
    let permanent = PERMANENT.load(Ordering::Relaxed);
    let entries = shards.iter().map(|g| g.len() as i64).sum::<i64>();
    drop(shards);
    assert_eq!(
        live + permanent,
        entries,
        "counters drifted from the map after {step}: live {live} + permanent {permanent} != {entries} entries",
    );
}

/// Return a stable snapshot of every non-permanent live handle and its kind.
/// The shutdown reporter uses this instead of only printing the aggregate
/// count, which makes the first debugging pass identify the surviving object.
pub(crate) fn mark_leak_site(id: i64, line: i64, column: i64) {
    if id == NULL || !shard_lock(id).contains_key(&id) {
        return;
    }
    LEAK_SITES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id, (line, column));
}

pub(crate) struct LeakEntry {
    pub(crate) id: i64,
    pub(crate) kind: &'static str,
    pub(crate) detail: Option<String>,
    pub(crate) site: Option<(i64, i64)>,
}

pub(crate) fn live_entries() -> Vec<LeakEntry> {
    let shards = all_shards();
    let permanent = PERMANENT_IDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let sites = LEAK_SITES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut entries: Vec<_> = shards
        .iter()
        .flat_map(|guard| guard.iter())
        .filter_map(|(&id, handle)| {
            if permanent.contains(&id) {
                return None;
            }
            let kind = match handle {
                Handle::String(_) => "string",
                Handle::Array(_) => "array",
                Handle::Shared(_) => "shared",
                Handle::AsyncSleep { .. } => "async future",
                Handle::Goroutine { .. } => "goroutine",
                Handle::Chan { .. } => "channel",
                Handle::ReactorReg { .. } => "reactor registration",
                Handle::AsyncIo { .. } => "async io future",
                Handle::AsyncOp { .. } => "offloaded future",
                Handle::Opaque(_) => "opaque",
                Handle::SharedObject(_) => "shared object",
                Handle::Memory(_) => "pointer capability",
                Handle::Slice(_) => "slice",
            };
            let detail = match handle {
                Handle::String(value) => {
                    let mut preview: String = value.escape_debug().take(80).collect();
                    if value.escape_debug().count() > 80 {
                        preview.push_str("...");
                    }
                    Some(preview)
                }
                Handle::SharedObject(object) => shared_object_tag_name(object.tag).map(|name| {
                    let mut preview: String = name.chars().take(80).collect();
                    if name.chars().count() > 80 {
                        preview.push_str("...");
                    }
                    preview
                }),
                _ => None,
            };
            Some(LeakEntry {
                id,
                kind,
                detail,
                site: sites.get(&id).copied(),
            })
        })
        .collect();
    entries.sort_unstable_by_key(|entry| entry.id);
    entries
}

// ── Strings ────────────────────────────────────────────────────────────────

/// Register an owned string and return its handle.
pub fn put_string(s: String) -> i64 {
    insert(Handle::String(s))
}

/// Register an owned string as permanent (a compile-time constant such as
/// a string literal). It is never removed and excluded from leak
/// reporting.
pub fn put_string_permanent(s: String) -> i64 {
    insert_permanent(Handle::String(s))
}

/// Take ownership of the string behind `id`, removing it from the
/// registry.
///
/// A non-string handle (array, shared box, opaque resource, ...) is left
/// untouched and reads as `None`: destructive takes must never destroy the
/// wrong kind of value, so a misplaced drop cannot corrupt the registry.
pub fn take_string(id: i64) -> Option<String> {
    if id == NULL {
        return None;
    }
    // Compile-time literals are shared by every use site.  A generated drop
    // for an async local may pass one of these handles here, but it must not
    // remove the registry entry or later uses of the literal will decode as
    // an empty string.
    if PERMANENT_IDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&id)
    {
        return borrow(id, |handle| match handle {
            Handle::String(value) => Some(value.clone()),
            _ => None,
        })
        .flatten();
    }
    let mut guard = shard_lock(id);
    if !matches!(guard.get(&id), Some(Handle::String(_))) {
        return None;
    }
    match guard.remove(&id) {
        Some(Handle::String(s)) => {
            LIVE.fetch_sub(1, Ordering::Relaxed);
            Some(s)
        }
        _ => None,
    }
}

/// Clone the string behind `id` (a borrow), registering a fresh owned
/// copy.
pub fn clone_string(id: i64) -> Option<i64> {
    if id == NULL {
        return None;
    }
    let text = {
        let guard = shard_lock(id);
        match guard.get(&id) {
            Some(Handle::String(s)) => s.clone(),
            _ => return None,
        }
    };
    Some(put_string(text))
}

/// Read the string behind `id` (a borrow) without removing it.
pub fn get_string(id: i64) -> Option<String> {
    if id == NULL {
        return None;
    }
    let guard = shard_lock(id);
    match guard.get(&id) {
        Some(Handle::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Run `f` on the string behind `id`. The closure must not call back into
/// the registry.
pub fn with_string<R>(id: i64, f: impl FnOnce(&str) -> R) -> Option<R> {
    if id == NULL {
        return None;
    }
    let guard = shard_lock(id);
    match guard.get(&id) {
        Some(Handle::String(s)) => Some(f(s)),
        _ => None,
    }
}

/// Replace the string behind `id` with `s`. Returns whether the handle
/// referred to a live string.
pub fn assign_string(id: i64, s: String) -> bool {
    if id == NULL {
        return false;
    }
    let mut guard = shard_lock(id);
    match guard.get_mut(&id) {
        Some(Handle::String(existing)) => {
            *existing = s;
            true
        }
        _ => false,
    }
}

/// Concatenate two strings, registering the result.
pub(crate) fn string_concat(a: i64, b: i64) -> Option<i64> {
    let sa = match shard_lock(a).get(&a) {
        Some(Handle::String(s)) => s.clone(),
        // The null handle (and any unknown handle) reads as an empty
        // string, so concatenating a value that failed to produce a
        // handle still yields the surviving text instead of collapsing
        // the whole expression to null.
        _ => String::new(),
    };
    let sb = match shard_lock(b).get(&b) {
        Some(Handle::String(s)) => s.clone(),
        _ => String::new(),
    };
    Some(put_string(format!("{sa}{sb}")))
}

/// Compare two strings for equality.
pub(crate) fn string_equals(a: i64, b: i64) -> bool {
    if a == NULL || b == NULL {
        return false;
    }
    let sa = match shard_lock(a).get(&a) {
        Some(Handle::String(s)) => s.clone(),
        _ => return false,
    };
    let guard = shard_lock(b);
    let sb = match guard.get(&b) {
        Some(Handle::String(s)) => s.as_str(),
        _ => return false,
    };
    sa == sb
}

// ── Arrays ─────────────────────────────────────────────────────────────────

/// Create a new array handle. The requested capacity is clamped to
/// [`MAX_INITIAL_CAPACITY`] elements: `Vec::with_capacity` aborts the
/// process when the allocator refuses (past `isize::MAX`, or when the
/// request exceeds available memory), so a hostile capacity argument must
/// never reach it. Growth doubles from there, so the hint is still useful.
pub(crate) fn array_new(elem_size: i64, initial_capacity: i64, string_elements: i8) -> i64 {
    if elem_size <= 0 {
        return NULL;
    }
    let cap = if initial_capacity <= 0 {
        8
    } else {
        initial_capacity
    };
    let cap = (cap as usize).clamp(8, MAX_INITIAL_CAPACITY);
    insert(Handle::Array(ArrayData::new(
        elem_size as usize,
        cap,
        string_elements != 0,
    )))
}

/// Upper bound for a fresh array's initial capacity hint (elements): keeps
/// the pre-allocation small enough to always succeed; the array grows on
/// demand.
const MAX_INITIAL_CAPACITY: usize = 1 << 20;

/// Set the string-elements flag of an existing array. Only meaningful
/// before any element has been inserted.
pub(crate) fn array_set_string_elements(id: i64, string_elements: i8) {
    borrow_mut(id, |h| {
        if let Handle::Array(arr) = h {
            arr.string_elements = string_elements != 0;
        }
    });
}

pub(crate) fn array_len(id: i64) -> i64 {
    with_array(id, |a| a.len() as i64).unwrap_or(0)
}

/// The element value at `index`, or `None` when out of bounds.
pub(crate) fn array_get(id: i64, index: i64) -> Option<i64> {
    if index < 0 {
        return None;
    }
    with_array(id, |a| a.get(index as usize)).flatten()
}

/// Push an element into an owned array. String elements are deep-copied
/// into a fresh handle owned by the array; all other elements are stored
/// by value.
pub(crate) fn array_push(id: i64, elem: i64) -> bool {
    let mut value = elem;
    let string_elements = match shard_lock(id).get(&id) {
        Some(Handle::Array(arr)) => arr.string_elements,
        _ => return false,
    };
    if string_elements {
        let text = match shard_lock(elem).get(&elem) {
            Some(Handle::String(s)) => s.clone(),
            _ => return false,
        };
        value = insert(Handle::String(text));
    }
    let mut guard = shard_lock(id);
    let Some(Handle::Array(arr)) = guard.get_mut(&id) else {
        // The string copy was registered before reacquiring the array shard.
        // A concurrent removal or invalidated handle must not strand it.
        if string_elements {
            drop(guard);
            take_string(value);
        }
        return false;
    };
    arr.push(value);
    true
}

/// Replace the element at `index`. String elements are deep-copied into a
/// fresh handle owned by the array and the old element is reclaimed; all
/// other elements are replaced by value. A no-op for an out-of-bounds
/// index or an unknown array.
pub(crate) fn array_set(id: i64, index: i64, elem: i64) -> bool {
    if id == NULL || index < 0 {
        return false;
    }
    let mut value = elem;
    let string_elements = match shard_lock(id).get(&id) {
        Some(Handle::Array(arr)) => arr.string_elements,
        _ => return false,
    };
    if string_elements {
        let text = match shard_lock(elem).get(&elem) {
            Some(Handle::String(s)) => s.clone(),
            _ => return false,
        };
        value = insert(Handle::String(text));
    }
    let mut guard = shard_lock(id);
    let Some(Handle::Array(arr)) = guard.get_mut(&id) else {
        if string_elements {
            drop(guard);
            take_string(value);
        }
        return false;
    };
    if index as usize >= arr.elements.len() {
        if string_elements {
            drop(guard);
            take_string(value);
        }
        return false;
    }
    let old = std::mem::replace(&mut arr.elements[index as usize], value);
    drop(guard);

    // The new string handle is always freshly registered, so it can never
    // alias `old`; reclaim the replaced string element through `take_string`,
    // which locks the handle's own shard and decrements `LIVE`.
    if string_elements {
        take_string(old);
    }
    true
}

/// Remove the array behind `id`, reclaiming any owned string elements. A
/// non-array handle (or a stale handle) is a safe no-op.
pub(crate) fn array_drop(id: i64) {
    if id == NULL {
        return;
    }
    let is_array = {
        let guard = shard_lock(id);
        matches!(guard.get(&id), Some(Handle::Array(_)))
    };
    if !is_array {
        return;
    }
    let Some(Handle::Array(data)) = remove(id) else {
        return;
    };
    if data.string_elements {
        for &handle in &data.elements {
            take_string(handle);
        }
    }
}

/// Remove and return the last element of an owned array. For string arrays
/// the returned handle is transferred to the caller (no longer owned by
/// the array).
pub(crate) fn array_pop(id: i64) -> Option<i64> {
    if id == NULL {
        return None;
    }
    let mut guard = shard_lock(id);
    let arr = match guard.get_mut(&id) {
        Some(Handle::Array(arr)) => arr,
        _ => return None,
    };
    arr.pop()
}

/// Run `f` on the array behind `id`. The closure must not call back into
/// the registry.
pub(crate) fn with_array<R>(id: i64, f: impl FnOnce(&ArrayData) -> R) -> Option<R> {
    borrow(id, |h| match h {
        Handle::Array(a) => Some(f(a)),
        _ => None,
    })
    .flatten()
}

/// Read every element of an array as raw bits, without holding the
/// registry lock afterwards.
pub(crate) fn array_to_vec(id: i64) -> Vec<i64> {
    with_array(id, |a| a.elements.clone()).unwrap_or_default()
}

/// Replace the element vector of an existing array (used by
/// `sort.sort_by` to write the rearranged element bits back into a fresh
/// clone).
pub(crate) fn array_write_elements(id: i64, elements: Vec<i64>) -> bool {
    borrow_mut(id, |h| match h {
        Handle::Array(a) => {
            a.elements = elements;
            true
        }
        _ => false,
    })
    .unwrap_or(false)
}

/// Register a fully constructed array.
pub(crate) fn put_array(data: ArrayData) -> i64 {
    insert(Handle::Array(data))
}

/// Whether `id` names a registered array.
pub(crate) fn is_array(id: i64) -> bool {
    with_array(id, |_| true).unwrap_or(false)
}

/// Deep-copy the array behind `id`, recursing into nested arrays up to
/// `levels` deep. String elements are always deep-copied; every other
/// element is copied by value (nested arrays are shared at depths beyond
/// `levels`, matching the pointer ABI).
pub(crate) fn array_clone(id: i64, levels: i64) -> i64 {
    if id == NULL {
        return NULL;
    }
    let src = match with_array(id, |a| ArrayData {
        elem_size: a.elem_size,
        string_elements: a.string_elements,
        elements: a.elements.clone(),
    }) {
        Some(src) => src,
        None => return NULL,
    };
    let mut out = ArrayData::new(
        src.elem_size,
        src.elements.len().max(1),
        src.string_elements,
    );
    for &elem in &src.elements {
        let copied = if src.string_elements {
            clone_string(elem).unwrap_or(NULL)
        } else if levels > 0 && src.elem_size == PTR_SIZE {
            if is_array(elem) {
                array_clone(elem, levels - 1)
            } else {
                elem
            }
        } else {
            elem
        };
        out.push(copied);
    }
    put_array(out)
}

/// Replicate an element `count` times into a fresh array. String elements
/// are deep-copied from `val` (a string handle) per element.
pub(crate) fn array_fill(val: i64, count: i64, elem_size: i64, string_elements: i8) -> i64 {
    if count < 0 || elem_size <= 0 {
        return NULL;
    }
    let string_elements = string_elements != 0;
    let Ok(count) = usize::try_from(count) else {
        return NULL;
    };
    if count > MAX_INITIAL_CAPACITY {
        return NULL;
    }
    let mut data = ArrayData::new(elem_size as usize, count, string_elements);
    for _ in 0..count {
        let elem = if string_elements {
            match clone_string(val) {
                Some(handle) => handle,
                None => NULL,
            }
        } else {
            val
        };
        data.push(elem);
    }
    put_array(data)
}

/// Build a fresh `i64` array holding `start..end`.
pub(crate) fn array_range(start: i64, end: i64) -> i64 {
    let count = if end <= start {
        0
    } else {
        let Some(count) = end
            .checked_sub(start)
            .and_then(|count| usize::try_from(count).ok())
        else {
            return NULL;
        };
        count
    };
    if count > MAX_INITIAL_CAPACITY {
        return NULL;
    }
    let mut data = ArrayData::new(PTR_SIZE, count, false);
    for value in start..end {
        data.push(value);
    }
    put_array(data)
}

/// A new array holding a clamped sub-range of the array behind `id`, with
/// string elements deep-copied. The source array is never mutated.
fn array_slice_inner(id: i64, start: i64, end: i64) -> i64 {
    if id == NULL {
        return NULL;
    }
    let (elem_size, string_elements, elements) = {
        let guard = shard_lock(id);
        let arr = match guard.get(&id) {
            Some(Handle::Array(a)) => a,
            _ => return NULL,
        };
        (arr.elem_size, arr.string_elements, arr.elements.clone())
    };
    let len = elements.len();
    let s = start.max(0).min(len as i64) as usize;
    let e = if end < 0 {
        len
    } else {
        end.min(len as i64).max(0) as usize
    };
    let range_start = s.min(e);
    let mut data = ArrayData::new(
        elem_size,
        e.saturating_sub(range_start).max(1),
        string_elements,
    );
    for &elem in &elements[range_start..e] {
        let copied = if string_elements {
            clone_string(elem).unwrap_or(NULL)
        } else {
            elem
        };
        data.push(copied);
    }
    put_array(data)
}

/// A new array with the elements of the array behind `id` in reverse
/// order.
fn array_reverse_inner(id: i64) -> i64 {
    if id == NULL {
        return NULL;
    }
    let (elem_size, string_elements, elements) = {
        let guard = shard_lock(id);
        let arr = match guard.get(&id) {
            Some(Handle::Array(a)) => a,
            _ => return NULL,
        };
        (arr.elem_size, arr.string_elements, arr.elements.clone())
    };
    let mut data = ArrayData::new(elem_size, elements.len().max(1), string_elements);
    for &elem in elements.iter().rev() {
        let copied = if string_elements {
            clone_string(elem).unwrap_or(NULL)
        } else {
            elem
        };
        data.push(copied);
    }
    put_array(data)
}

/// A new empty array with the same element representation as the array
/// behind `id`.
/// Empty the array behind `id` in place, reclaiming any owned string
/// elements, and return the same handle. `arrays.clear` is documented as an
/// in-place `void` operation like `arrays.push`; returning a fresh array
/// instead left the caller's array untouched, so `arrays.clear(a)` appeared
/// to do nothing and everything after it kept the old elements.
fn array_clear_inner(id: i64) -> i64 {
    if id == NULL {
        return NULL;
    }
    let (string_elements, dropped) = {
        let mut guard = shard_lock(id);
        let Some(Handle::Array(arr)) = guard.get_mut(&id) else {
            return NULL;
        };
        let string_elements = arr.string_elements;
        let dropped = std::mem::take(&mut arr.elements);
        (string_elements, dropped)
    };
    // Reclaim outside the array's own shard: `take_string` locks the
    // element's shard, which may be this one.
    if string_elements {
        for handle in dropped {
            take_string(handle);
        }
    }
    id
}

/// A new array with the element at `index` (negative = from the end)
/// removed. The input array is never mutated. The result is a copy even
/// when the index is out of bounds (a no-op), so every returned handle is
/// fresh.
fn array_remove_at_inner(id: i64, index: i64) -> i64 {
    if id == NULL {
        return NULL;
    }
    let (elem_size, string_elements, elements) = {
        let guard = shard_lock(id);
        let arr = match guard.get(&id) {
            Some(Handle::Array(a)) => a,
            _ => return NULL,
        };
        (arr.elem_size, arr.string_elements, arr.elements.clone())
    };
    let len = elements.len() as i64;
    let idx = if index < 0 { len + index } else { index };
    let mut data = ArrayData::new(elem_size, len.max(1) as usize, string_elements);
    for (i, &elem) in elements.iter().enumerate() {
        if i as i64 == idx {
            continue;
        }
        let copied = if string_elements {
            clone_string(elem).unwrap_or(NULL)
        } else {
            elem
        };
        data.push(copied);
    }
    put_array(data)
}

/// A new array with the elements of the array behind `id` shuffled
/// (Fisher-Yates using OS randomness).
fn array_shuffle_inner(id: i64) -> i64 {
    let cloned = array_clone(id, 0);
    if cloned == NULL {
        return NULL;
    }
    let mut guard = shard_lock(cloned);
    let arr = match guard.get_mut(&cloned) {
        Some(Handle::Array(arr)) => arr,
        _ => return NULL,
    };
    let len = arr.elements.len();
    if len <= 1 {
        return cloned;
    }
    let mut random_bytes = vec![0u8; len * 4];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = file.read_exact(&mut random_bytes);
    }
    for i in (1..len).rev() {
        let j = if i * 4 < random_bytes.len() {
            (random_bytes[i * 4] as usize) % (i + 1)
        } else {
            i / 2
        };
        if i != j {
            arr.elements.swap(i, j);
        }
    }
    cloned
}

/// A new array with the elements of the array behind `id` sorted. `mode`
/// selects the comparison: 0 = `i64` values, 1 = `f64` values, 2 =
/// strings.
fn array_sort_inner(id: i64, mode: i8) -> i64 {
    let cloned = array_clone(id, 0);
    if cloned == NULL {
        return NULL;
    }

    // Pre-read string elements so the sort below only mutates the element
    // vector (no registry borrow is held across the comparison loop).
    let strings: Option<Vec<Option<String>>> = {
        let (string_elements, element_handles) = {
            let guard = shard_lock(cloned);
            let arr = match guard.get(&cloned) {
                Some(Handle::Array(a)) => a,
                _ => return NULL,
            };
            (arr.string_elements, arr.elements.clone())
        };
        if !string_elements {
            None
        } else {
            Some(
                element_handles
                    .iter()
                    .map(|&h| match shard_lock(h).get(&h) {
                        Some(Handle::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
            )
        }
    };
    let mut guard = shard_lock(cloned);
    let arr = match guard.get_mut(&cloned) {
        Some(Handle::Array(arr)) => arr,
        _ => return NULL,
    };
    let len = arr.elements.len();
    for i in 1..len {
        let mut j = i;
        while j > 0 {
            let a = arr.elements[j - 1];
            let b = arr.elements[j];
            let less = match mode {
                1 => f64::from_bits(a as u64) > f64::from_bits(b as u64),
                2 => {
                    if let Some(strings) = &strings {
                        let sa = strings[j - 1].as_deref().unwrap_or("");
                        let sb = strings[j].as_deref().unwrap_or("");
                        sa > sb
                    } else {
                        a > b
                    }
                }
                _ => a > b,
            };
            if !less {
                break;
            }
            arr.elements.swap(j - 1, j);
            j -= 1;
        }
    }
    cloned
}

// ── Shared boxes ───────────────────────────────────────────────────────────

/// Register a shared box adopting a single owned reference to `inner`.
pub(crate) fn shared_new(inner: i64) -> i64 {
    insert(Handle::Shared(SharedData { inner, count: 1 }))
}

/// Record another live copy of the shared box.
pub(crate) fn shared_retain(id: i64) -> i64 {
    with_shared_mut(id, |s| s.count += 1);
    id
}

/// Release one copy of the shared box. When the last copy is released the
/// box is removed and ownership of the wrapped value is returned to the
/// caller (`NULL` is returned while copies remain).
pub(crate) fn shared_release(id: i64) -> i64 {
    if id == NULL {
        return NULL;
    }
    let mut guard = shard_lock(id);
    {
        let shared = match guard.get_mut(&id) {
            Some(Handle::Shared(shared)) => shared,
            _ => return NULL,
        };
        shared.count = shared.count.saturating_sub(1);
        if shared.count > 0 {
            return NULL;
        }
    }
    let inner = match guard.remove(&id) {
        Some(Handle::Shared(shared)) => shared.inner,
        _ => return NULL,
    };
    LIVE.fetch_sub(1, Ordering::Relaxed);
    inner
}

/// Run `f` on the shared box behind `id` mutably. The closure must not
/// call back into the registry.
pub(crate) fn with_shared_mut<R>(id: i64, f: impl FnOnce(&mut SharedData) -> R) -> Option<R> {
    borrow_mut(id, |h| match h {
        Handle::Shared(s) => Some(f(s)),
        _ => None,
    })
    .flatten()
}

/// Borrow the handle of the value wrapped by the shared box behind `id`
/// without removing it. Returns the null handle for an unknown box.
pub(crate) fn shared_inner(id: i64) -> i64 {
    with_shared_mut(id, |s| s.inner).unwrap_or(NULL)
}

// ── Async sleep futures ─────────────────────────────────────────────────────

/// Register a new `async.sleep(ms)` future and return its handle.
pub(crate) fn async_sleep_new(ms: i64) -> i64 {
    insert(Handle::AsyncSleep {
        state: 0,
        ms,
        deadline: 0,
    })
}

/// Poll an `async.sleep(ms)` future: arm it on the first poll, then
/// return `1` once the deadline has passed and `0` otherwise.
pub(crate) fn async_sleep_poll(id: i64) -> i8 {
    let mut guard = shard_lock(id);
    let sleep = match guard.get_mut(&id) {
        Some(Handle::AsyncSleep {
            state,
            ms,
            deadline,
        }) => (state, ms, deadline),
        _ => return 0,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let now_ms = now.as_millis() as i64;
    let (result, park_until) = match *sleep.0 {
        0 => {
            *sleep.2 = now_ms.saturating_add(*sleep.1);
            *sleep.0 = 1;
            (0, Some(*sleep.2))
        }
        1 => {
            if now_ms >= *sleep.2 {
                *sleep.0 = 2;
                (1, None)
            } else {
                (0, Some(*sleep.2))
            }
        }
        _ => (1, None),
    };
    drop(guard);
    if let Some(deadline) = park_until {
        crate::ntask::scheduler::park_timer(deadline);
    }
    result
}

/// Drop an `async.sleep(ms)` future.
///
/// Kind-checked like every other destructive operation: a handle naming a
/// string, array, shared box, or opaque resource is left untouched, so
/// dropping a future twice — or dropping the wrong handle — cannot destroy
/// a live value of another kind.
pub(crate) fn async_sleep_drop(id: i64) {
    if id == NULL {
        return;
    }
    let mut guard = shard_lock(id);
    if !matches!(guard.get(&id), Some(Handle::AsyncSleep { .. })) {
        return;
    }
    if guard.remove(&id).is_some() {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

fn remove_kind(id: i64, predicate: impl FnOnce(&Handle) -> bool) -> Option<Handle> {
    if id == NULL {
        return None;
    }
    if borrow(id, predicate).unwrap_or(false) {
        remove(id)
    } else {
        None
    }
}

// ── Offloaded blocking futures ──────────────────────────────────────────

/// Register an offloaded-blocking future that runs `work` on the worker pool
/// and yields its result handle. Returns the future handle.
pub(crate) fn async_op_new(work: Box<dyn FnOnce() -> i64 + Send + 'static>) -> i64 {
    insert(Handle::AsyncOp {
        state: 0,
        work: Some(work),
        op: 0,
        result: NULL,
    })
}

/// Poll an offloaded future: on the first poll it starts the job on the pool
/// and parks the goroutine; once the pool completes the job it reaps the
/// result and reports readiness. All scheduler calls happen outside the
/// registry lock to avoid a lock-ordering inversion with the task core.
pub(crate) fn async_op_poll(id: i64) -> i8 {
    let start_work: Option<Box<dyn FnOnce() -> i64 + Send + 'static>> =
        shard_lock(id).get_mut(&id).and_then(|handle| match handle {
            Handle::AsyncOp { state: 0, work, .. } => work.take(),
            _ => None,
        });
    if let Some(work) = start_work {
        let op_id = crate::ntask::scheduler::register_op();
        // Publish the scheduler operation before enqueueing work. If the
        // future was dropped after `work` was taken but before this point,
        // cancellation can still remove the operation and the completion
        // path will reclaim the eventual result handle.
        let installed = {
            let mut guard = shard_lock(id);
            match guard.get_mut(&id) {
                Some(Handle::AsyncOp { state, op, .. }) if *state == 0 => {
                    *state = 1;
                    *op = op_id;
                    true
                }
                _ => false,
            }
        };
        crate::ntask::scheduler::run_offload(move || {
            let value = work();
            crate::ntask::scheduler::complete_op(op_id, value);
        });
        if !installed {
            // The future was cancelled before publication. No waiter can
            // consume this operation, so remove it now; a later completion
            // will dispose of the produced registry handle as an orphan.
            crate::ntask::scheduler::drop_op(op_id);
            return 0;
        }
        // Cancellation can win immediately after publication. Avoid marking
        // the current scheduler task as waiting on an operation that has
        // already been removed; a plain-thread poll simply skips this park.
        let still_active = shard_lock(id).get(&id).is_some_and(|handle| {
            matches!(
                handle,
                Handle::AsyncOp {
                    state: 1,
                    op,
                    ..
                } if *op == op_id
            )
        });
        if still_active {
            crate::ntask::scheduler::park_op(op_id);
        }
        return 0;
    }

    let op = shard_lock(id).get(&id).and_then(|handle| match handle {
        Handle::AsyncOp { state: 1, op, .. } => Some(*op),
        _ => None,
    });
    if let Some(op) = op {
        if !crate::ntask::scheduler::op_done(op) {
            let still_active = shard_lock(id).get(&id).is_some_and(|handle| {
                matches!(
                    handle,
                    Handle::AsyncOp {
                        state: 1,
                        op: active_op,
                        ..
                    } if *active_op == op
                )
            });
            if still_active {
                crate::ntask::scheduler::park_op(op);
            }
            return 0;
        }
        let value = crate::ntask::scheduler::op_result(op);
        let stored = {
            let mut guard = shard_lock(id);
            match guard.get_mut(&id) {
                Some(Handle::AsyncOp { state, result, .. }) => {
                    *state = 2;
                    *result = value;
                    true
                }
                _ => false,
            }
        };
        // Cancellation may remove the future after op_result transfers the
        // value but before it can be stored on the future. The poller then
        // owns that value and must reclaim it rather than lose it.
        if !stored && value != NULL {
            let _ = crate::registry::remove(value);
        }
    }
    1
}

/// Reap the result handle of a completed offloaded future. Reaping transfers
/// ownership exactly once; a second reap returns `NULL`.
pub(crate) fn async_op_result(id: i64) -> i64 {
    if id == NULL {
        return NULL;
    }
    let mut guard = shard_lock(id);
    match guard.get_mut(&id) {
        Some(Handle::AsyncOp {
            state: 2, result, ..
        }) => std::mem::replace(result, NULL),
        _ => NULL,
    }
}

/// Drop an offloaded future. A pending operation is cancelled first so its
/// worker either has its result reclaimed here or observes the missing op and
/// reclaims it on completion. A result already stored on the future is also
/// reclaimed; callers must reap it before dropping when they want to keep it.
pub(crate) fn async_op_drop(id: i64) {
    if id == NULL {
        return;
    }
    let pending_op = shard_lock(id).get(&id).and_then(|handle| match handle {
        Handle::AsyncOp { state: 1, op, .. } if *op != 0 => Some(*op),
        _ => None,
    });
    if let Some(op) = pending_op {
        crate::ntask::scheduler::drop_op(op);
    }
    let result = match remove_kind(id, |handle| matches!(handle, Handle::AsyncOp { .. })) {
        Some(Handle::AsyncOp { result, .. }) => result,
        _ => NULL,
    };
    if result != NULL {
        let _ = crate::registry::remove(result);
    }
}

// ── Goroutine handles ───────────────────────────────────────────────────

/// Remove the `Handle::Goroutine` wrapper `handle`, so a fire-and-forget spawn
/// does not leak its entry. A `NULL` or already-dropped handle is a no-op.
pub(crate) fn remove_goroutine_handle(handle: i64) {
    if handle == NULL {
        return;
    }
    let mut guard = shard_lock(handle);
    if !matches!(guard.get(&handle), Some(Handle::Goroutine { .. })) {
        return;
    }
    if guard.remove(&handle).is_some() {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) fn goroutine_drop(id: i64) {
    let Some(Handle::Goroutine { core }) =
        remove_kind(id, |handle| matches!(handle, Handle::Goroutine { .. }))
    else {
        return;
    };
    crate::ntask::scheduler::drop_goroutine(core);
}

pub(crate) fn chan_retain(id: i64) -> i64 {
    with_chan_mut(id, |handle| {
        if let Handle::Chan { count, .. } = handle {
            *count = count.saturating_add(1);
        }
    });
    id
}

fn with_chan_mut<R>(id: i64, f: impl FnOnce(&mut Handle) -> R) -> Option<R> {
    borrow_mut(id, |h| match h {
        Handle::Chan { .. } => Some(f(h)),
        _ => None,
    })
    .flatten()
}

/// Drop a channel handle. Like Go, dropping a `chan` variable does **not**
/// close the channel — `close(ch)` is explicit. Reference counted like
/// `Shared`: the `Chan` core stays alive until the last handle is gone.
/// Previously this did `chan_close` + `drop_chan`, which woke a parked
/// sender as a no-op after the peer's `ch` went out of scope and made
/// `go sender(ch)` appear to complete after `main` returned, unlike Go
/// where it stays blocked.
pub(crate) fn chan_drop(id: i64) {
    if id == NULL {
        return;
    }
    let mut guard = shard_lock(id);
    let count = match guard.get_mut(&id) {
        Some(Handle::Chan { count, .. }) => {
            *count = count.saturating_sub(1);
            *count
        }
        _ => return,
    };
    if count > 0 {
        return;
    }
    let taken = guard.remove(&id);
    if taken.is_some() {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
    drop(guard);
    if let Some(Handle::Chan { core, .. }) = taken {
        crate::ntask::scheduler::drop_chan(core);
    }
}

/// Drop a reactor registration handle.
pub(crate) fn reactor_reg_drop(id: i64) {
    let Some(Handle::ReactorReg { core }) =
        remove_kind(id, |handle| matches!(handle, Handle::ReactorReg { .. }))
    else {
        return;
    };
    // Unwatch the fd before the core is removed so the reactor never polls a
    // stale descriptor.
    crate::ntask::reactor::detach_fd(core);
    crate::ntask::scheduler::drop_io(core);
}

/// Drop an asynchronous I/O future handle.
pub(crate) fn async_io_drop(id: i64) {
    let Some(Handle::AsyncIo { core }) =
        remove_kind(id, |handle| matches!(handle, Handle::AsyncIo { .. }))
    else {
        return;
    };
    // Unwatch the fd before the core is removed so the reactor never polls a
    // stale descriptor.
    crate::ntask::reactor::detach_fd(core);
    crate::ntask::scheduler::drop_io(core);
}

pub(crate) fn task_core(id: i64) -> Option<i64> {
    borrow(id, |handle| match handle {
        Handle::Goroutine { core }
        | Handle::Chan { core, .. }
        | Handle::ReactorReg { core }
        | Handle::AsyncIo { core } => Some(*core),
        _ => None,
    })?
}

// ── Opaque module values ───────────────────────────────────────────────────

/// Register an opaque value owned by a stdlib module.
pub(crate) fn put_opaque<T: Any + Send>(value: T) -> i64 {
    insert(Handle::Opaque(Box::new(value)))
}

/// Run `f` on the opaque value behind `id` as `T`. Returns `None` when the
/// handle is null, unknown, or holds a different type. The closure must
/// not call back into the registry.
pub(crate) fn with_opaque<R, T: Any + Send>(id: i64, f: impl FnOnce(&T) -> R) -> Option<R> {
    borrow(id, |h| match h {
        Handle::Opaque(opaque) => opaque.downcast_ref::<T>().map(f),
        _ => None,
    })
    .flatten()
}

/// Run `f` on the opaque value behind `id` as `T`, mutably. Returns `None`
/// when the handle is null, unknown, or holds a different type. The
/// closure must not call back into the registry.
pub(crate) fn with_opaque_mut<R, T: Any + Send>(id: i64, f: impl FnOnce(&mut T) -> R) -> Option<R> {
    borrow_mut(id, |h| match h {
        Handle::Opaque(opaque) => opaque.downcast_mut::<T>().map(f),
        _ => None,
    })
    .flatten()
}

/// Take the opaque value behind `id`, returning it as `T`.
///
/// A non-opaque handle (or an opaque of a different type) is left
/// untouched and reads as `None`: destructive takes must never destroy the
/// wrong kind of value.
pub(crate) fn take_opaque<T: Any + Send>(id: i64) -> Option<T> {
    if id == NULL {
        return None;
    }
    let mut guard = shard_lock(id);
    if !matches!(
        guard.get(&id),
        Some(Handle::Opaque(opaque)) if opaque.is::<T>()
    ) {
        return None;
    }
    match guard.remove(&id) {
        Some(Handle::Opaque(opaque)) => {
            LIVE.fetch_sub(1, Ordering::Relaxed);
            opaque.downcast::<T>().ok().map(|boxed| *boxed)
        }
        _ => None,
    }
}

/// Run `f` over the opaque value behind `id` with the registry lock released,
/// then put the (possibly modified) value back at the same `id`. Blocking
/// socket operations must use this so they do not hold the registry lock for
/// the whole wait.
pub(crate) fn with_opaque_io<T: Any + Send, R>(id: i64, f: impl FnOnce(T) -> (T, R)) -> Option<R> {
    if id == NULL {
        return None;
    }
    let taken = {
        let mut guard = shard_lock(id);
        match guard.get(&id) {
            Some(Handle::Opaque(opaque)) if opaque.is::<T>() => guard.remove(&id),
            _ => return None,
        }
    };
    let (t, r) = match taken {
        Some(Handle::Opaque(opaque)) => match opaque.downcast::<T>() {
            Ok(boxed) => f(*boxed),
            Err(opaque) => {
                shard_lock(id).insert(id, Handle::Opaque(opaque));
                return None;
            }
        },
        _ => return None,
    };
    shard_lock(id).insert(id, Handle::Opaque(Box::new(t)));
    Some(r)
}

// ── Shared cross-adaptor objects ───────────────────────────────────────────
//
// Generated adaptor crates (`ntsc-adaptor-*`, one per Rust dependency)
// exchange opaque values through this process-wide handle space. Every
// adaptor links the same runtime, so unlike the old per-adaptor maps a
// handle created by one adaptor resolves in any other: a `ClientBuilder`
// built by the `serenity` adaptor can be consumed by the `songbird`
// adaptor's `register` bridge, with no per-crate support code anywhere.
//
// Each entry carries a type tag (an interned string naming the owning
// crate, its exact version and feature set, and the type path) chosen by
// the creating adaptor. Every lookup checks the tag: a handle of the
// wrong type — including the same type path built from a different crate
// version, which may differ in layout — fails closed (null) instead of
// reinterpreting memory. The tag text is available for diagnostics, so a
// mismatch reports both sides.
//
// Ownership mirrors `Box`: the entry owns one allocation plus its
// creator-side drop glue. `borrow` retains (bumps `refs`) and hands out
// the pointer; the borrower must `release` after use, so the allocation
// provably outlives every in-flight borrow. `take` transfers ownership
// to the caller but only when no borrow is outstanding (`refs == 1`);
// `release` drops to zero, runs the drop glue exactly once, and removes
// the entry. Unknown, null, and wrong-kind handles are no-ops throughout,
// like every other registry API.

/// Drop glue for one shared object: drops the box behind `ptr` in place
/// and deallocates it. Monomorphized for the concrete value type by the
/// creating adaptor, so the runtime never names adaptor types.
pub(crate) type SharedObjectDrop = extern "C" fn(*mut std::ffi::c_void);

/// One heap value shared across adaptor crates.
pub(crate) struct SharedObjectData {
    /// The allocation, from `Box::into_raw` on the creating side. Owned
    /// by this entry until `take` transfers it or `release` drops it.
    ptr: *mut std::ffi::c_void,
    /// Runs exactly once, with `ptr`, when the last reference is
    /// released. Recorded together with `ptr` at insertion and never
    /// separated from it.
    drop_fn: SharedObjectDrop,
    /// Interned type tag; every access must present the same tag.
    tag: u64,
    /// Live references: the entry itself (1) plus one per outstanding
    /// borrow or Neutron-side share.
    refs: usize,
    exclusive_borrow: bool,
}

// SAFETY: the entry owns `ptr` (a live heap allocation) and `drop_fn`
// (valid for exactly that allocation). The entry is only accessed under
// its shard lock, except for raw pointers handed out by `borrow`/`take`,
// which the refcount protocol keeps alive for the whole use. Sharing
// `&SharedObjectData` across threads therefore cannot race on the
// allocation itself. Callers creating `&mut T` aliases from one entry on
// two threads would race exactly as with any aliased `&mut`; Neutron
// bridges hold at most the borrows their single call needs, matching the
// previous per-adaptor maps' contract.
unsafe impl Send for SharedObjectData {}

/// Interned shared-object type tags: tag text to small id.
static SHARED_OBJECT_TAGS: LazyLock<Mutex<std::collections::HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
/// Tag id to text (`id == index + 1`), for mismatch diagnostics.
static SHARED_OBJECT_TAG_NAMES: LazyLock<Mutex<Vec<String>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Intern `name` as a shared-object type tag and return its id (0 is
/// never a valid id). Idempotent: the same text always yields the same id.
pub(crate) fn shared_object_tag(name: &str) -> u64 {
    let mut tags = SHARED_OBJECT_TAGS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&id) = tags.get(name) {
        return id;
    }
    let mut names = SHARED_OBJECT_TAG_NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    names.push(name.to_string());
    let id = names.len() as u64;
    tags.insert(name.to_string(), id);
    id
}

/// The text of one interned tag, if any.
pub(crate) fn shared_object_tag_name(id: u64) -> Option<String> {
    SHARED_OBJECT_TAG_NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(id.checked_sub(1)? as usize)
        .cloned()
}

/// Register one heap value shared across adaptors and return its handle
/// (`refs == 1`). Fail-closed: a null pointer or null drop glue could
/// never be freed, so such an entry must never exist.
pub(crate) fn shared_object_put(
    ptr: *mut std::ffi::c_void,
    drop_fn: Option<SharedObjectDrop>,
    tag: u64,
) -> i64 {
    let Some(drop_fn) = drop_fn else {
        return NULL;
    };
    if ptr.is_null() || tag == 0 {
        return NULL;
    }
    insert(Handle::SharedObject(SharedObjectData {
        ptr,
        drop_fn,
        tag,
        refs: 1,
        exclusive_borrow: false,
    }))
}

/// Borrow the value behind `id` after checking its tag: retains (bumps
/// `refs`) and returns the pointer. The caller must `shared_object_release`
/// after use. Returns null for the null handle, unknown or wrong-kind
/// handles, and tag mismatches.
pub(crate) fn shared_object_borrow(id: i64, tag: u64) -> *mut std::ffi::c_void {
    if id == NULL {
        return std::ptr::null_mut();
    }
    let mut guard = shard_lock(id);
    match guard.get_mut(&id) {
        Some(Handle::SharedObject(object)) if object.tag == tag && !object.exclusive_borrow => {
            object.refs = object.refs.saturating_add(1);
            object.ptr
        }
        _ => std::ptr::null_mut(),
    }
}

pub(crate) fn shared_object_borrow_mut(id: i64, tag: u64) -> *mut std::ffi::c_void {
    let mut guard = shard_lock(id);
    match guard.get_mut(&id) {
        Some(Handle::SharedObject(object)) if object.tag == tag && object.refs == 1 => {
            object.exclusive_borrow = true;
            object.refs += 1;
            object.ptr
        }
        _ => std::ptr::null_mut(),
    }
}

pub(crate) fn shared_object_release_mut(id: i64) {
    {
        let mut guard = shard_lock(id);
        match guard.get_mut(&id) {
            Some(Handle::SharedObject(object)) if object.exclusive_borrow => {
                object.exclusive_borrow = false;
            }
            _ => return,
        }
    }
    shared_object_release(id);
}

/// Take ownership of the value behind `id` after checking its tag:
/// removes the entry (without running the drop glue) and returns the
/// pointer. Requires unique ownership (`refs == 1`): an outstanding
/// borrow keeps raw pointers alive, so taking over them would free
/// memory still in use. Returns null for the null handle, unknown or
/// wrong-kind handles, tag mismatches, and busy entries.
pub(crate) fn shared_object_take(id: i64, tag: u64) -> *mut std::ffi::c_void {
    if id == NULL {
        return std::ptr::null_mut();
    }
    let mut guard = shard_lock(id);
    match guard.get(&id) {
        Some(Handle::SharedObject(object)) if object.tag == tag && object.refs == 1 => {}
        _ => return std::ptr::null_mut(),
    }
    match guard.remove(&id) {
        Some(Handle::SharedObject(object)) => {
            LIVE.fetch_sub(1, Ordering::Relaxed);
            REGISTRY_GEN.fetch_add(1, Ordering::Release);
            object.ptr
        }
        _ => std::ptr::null_mut(),
    }
}

/// Release one reference to the shared object behind `id`. At zero the
/// entry is removed and its drop glue runs exactly once. Null, unknown,
/// and wrong-kind handles are no-ops.
pub(crate) fn shared_object_release(id: i64) {
    if id == NULL {
        return;
    }
    let taken = {
        let mut guard = shard_lock(id);
        let object = match guard.get_mut(&id) {
            Some(Handle::SharedObject(object)) => object,
            _ => return,
        };
        object.refs = object.refs.saturating_sub(1);
        if object.refs > 0 {
            return;
        }
        guard.remove(&id)
    };
    if taken.is_some() {
        LIVE.fetch_sub(1, Ordering::Relaxed);
        REGISTRY_GEN.fetch_add(1, Ordering::Release);
    }
    if let Some(Handle::SharedObject(object)) = taken {
        (object.drop_fn)(object.ptr);
    }
}

/// Record another live Neutron-side share of the object behind `id`.
/// Unknown, null, and wrong-kind handles are no-ops returning the input
/// unchanged, matching the retain protocol everywhere else.
pub(crate) fn shared_object_retain(id: i64) -> i64 {
    if id == NULL {
        return id;
    }
    borrow_mut(id, |handle| {
        if let Handle::SharedObject(object) = handle {
            object.refs = object.refs.saturating_add(1);
        }
    });
    id
}

/// The tag of the shared object behind `id`, or 0 for null, unknown,
/// and wrong-kind handles.
pub(crate) fn shared_object_tag_of(id: i64) -> u64 {
    borrow(id, |handle| match handle {
        Handle::SharedObject(object) => object.tag,
        _ => 0,
    })
    .unwrap_or(0)
}

// ── High-level array operations used by lib.rs / modules ───────────────────

pub(crate) fn array_slice(id: i64, start: i64, end: i64) -> i64 {
    array_slice_inner(id, start, end)
}

pub(crate) fn array_reverse(id: i64) -> i64 {
    array_reverse_inner(id)
}

pub(crate) fn array_clear(id: i64) -> i64 {
    array_clear_inner(id)
}

pub(crate) fn array_remove_at(id: i64, index: i64) -> i64 {
    array_remove_at_inner(id, index)
}

pub(crate) fn array_shuffle(id: i64) -> i64 {
    array_shuffle_inner(id)
}

pub(crate) fn array_sort(id: i64, mode: i8) -> i64 {
    array_sort_inner(id, mode)
}

#[cfg(test)]
mod capability_benchmark {
    use super::*;
    use std::time::Instant;

    /// The reason the per-thread cache exists: resolving a capability through
    /// the registry on every access takes the global lock, which dominates the
    /// cost of the access itself. This compares the cached path against the
    /// same work with the cache defeated (a removal each round invalidates it)
    /// and against a plain `Vec<u8>` as the native baseline.
    #[test]
    fn cached_capability_access_beats_registry_lookup_per_access() {
        const ROUNDS: usize = 100_000;

        let region = memory_alloc(64);

        // Warm the cache so the first access is not counted as a miss.
        assert!(memory_store(region, 8, 1));

        let cached = Instant::now();
        for round in 0..ROUNDS {
            assert!(memory_store(region, 8, round as i64));
            assert_eq!(memory_load(region, 8), Some(round as i64));
        }
        let cached = cached.elapsed();

        let uncached = Instant::now();
        for round in 0..ROUNDS {
            // Removing an unrelated handle bumps the generation, which is
            // exactly the cache-miss path: one registry lookup per access.
            let scratch = insert(Handle::String(String::new()));
            remove(scratch);
            assert!(memory_store(region, 8, round as i64));
            assert_eq!(memory_load(region, 8), Some(round as i64));
        }
        let uncached = uncached.elapsed();

        let mut native = [0_u8; 64];
        let baseline = Instant::now();
        for round in 0..ROUNDS {
            native[8..16].copy_from_slice(&(round as i64).to_le_bytes());
            let mut raw = [0_u8; 8];
            raw.copy_from_slice(&native[8..16]);
            assert_eq!(i64::from_le_bytes(raw), round as i64);
        }
        let baseline = baseline.elapsed();

        memory_drop(region);

        println!(
            "capability {ROUNDS} store+load rounds: cached {cached:?}, uncached {uncached:?}, native Vec<u8> {baseline:?}"
        );
        assert!(
            cached < uncached,
            "the cache must beat a registry lookup per access: cached {cached:?} vs uncached {uncached:?}"
        );
    }

    /// A dropped capability must never be served from the cache.
    #[test]
    fn dropping_a_capability_invalidates_the_cache() {
        let region = memory_alloc(16);
        assert!(memory_store(region, 8, 7));
        assert_eq!(memory_load(region, 8), Some(7));

        memory_drop(region);

        assert_eq!(memory_load(region, 8), None);
        assert!(!memory_store(region, 8, 9));
    }

    /// Two capabilities used alternately must each resolve to their own
    /// region, not to whichever one the cache happens to hold.
    fn store_and_read(region: i64, value: i64) -> Option<i64> {
        assert!(memory_store(region, 8, value));
        memory_load(region, 8)
    }

    #[test]
    fn alternating_capabilities_do_not_alias_through_the_cache() {
        let first = memory_alloc(16);
        let second = memory_alloc(16);

        for round in 0..64 {
            assert_eq!(store_and_read(first, round), Some(round));
            assert_eq!(store_and_read(second, round + 1000), Some(round + 1000));
            assert_eq!(memory_load(first, 8), Some(round));
        }

        memory_drop(first);
        memory_drop(second);
    }

    /// A region shared by several capabilities stays coherent across threads:
    /// the cache is per thread, but the bytes behind it are shared.
    #[test]
    fn concurrent_readers_observe_a_shared_region() {
        let region = memory_alloc(16);
        assert!(memory_store(region, 8, 4242));

        let clones: Vec<i64> = (0..4)
            .map(|_| memory_clone(region).expect("clone a live capability"))
            .collect();

        let readers: Vec<_> = clones
            .iter()
            .copied()
            .map(|handle| {
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        assert_eq!(memory_load(handle, 8), Some(4242));
                    }
                })
            })
            .collect();
        for reader in readers {
            reader.join().expect("reader thread panicked");
        }

        for handle in clones {
            memory_drop(handle);
        }
        memory_drop(region);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clearing_then_pushing_reuses_the_same_array() {
        let arr = put_array(ArrayData::new(8, 1, false));
        assert!(array_push(arr, 1));
        assert!(array_push(arr, 2));
        assert_eq!(array_clear(arr), arr);
        assert!(array_push(arr, 9));
        assert_eq!(array_len(arr), 1);
    }
    use super::*;

    #[test]
    fn invalid_string_array_set_reclaims_its_temporary_copy() {
        let text = "invalid_string_array_set_copy";
        let array = array_new(8, 0, 1);
        let source = put_string(text.to_string());
        assert!(array_push(array, source));
        for index in [-1, 1, 99] {
            assert!(!array_set(array, index, source));
        }
        assert_eq!(array_len(array), 1);
        take_string(source);
        array_drop(array);
        assert!(
            !live_entries()
                .iter()
                .any(|entry| entry.detail.as_deref() == Some(text))
        );
    }

    #[test]
    fn capability_cache_does_not_keep_dropped_memory_alive() {
        let pointer = memory_alloc(8);
        let weak = borrow(pointer, |handle| match handle {
            Handle::Memory(memory) => Arc::downgrade(&memory.bytes),
            _ => panic!("expected memory"),
        })
        .unwrap();
        assert_eq!(memory_load(pointer, 8), Some(0));
        memory_drop(pointer);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn array_slice_with_reversed_or_out_of_range_bounds_is_empty() {
        let source = array_range(1, 4);

        let reversed = array_slice(source, 3, 1);
        let out_of_range = array_slice(source, 10, 20);

        assert_eq!(array_to_vec(reversed), Vec::<i64>::new());
        assert_eq!(array_to_vec(out_of_range), Vec::<i64>::new());
        assert_eq!(array_to_vec(source), vec![1, 2, 3]);

        array_drop(reversed);
        array_drop(out_of_range);
        array_drop(source);
    }

    #[test]
    fn drop_of_a_wrong_kind_or_stale_handle_is_a_no_op() {
        let s = put_string("hello".to_string());
        let a = array_new(8, 0, 1);
        let op = insert(Handle::Opaque(Box::new(42i64)));
        let stale = 999_999;

        array_drop(s);
        assert_eq!(get_string(s), Some("hello".to_string()));

        take_string(a);
        assert!(is_array(a));
        array_drop(a);

        take_string(op);
        assert_eq!(with_opaque::<i64, _>(op, |v| *v), Some(42));

        take_string(stale);
        array_drop(stale);
        take_opaque::<i64>(stale);
        assert_eq!(get_string(stale), None);
    }

    #[test]
    fn takes_leave_wrong_kind_and_stale_handles_untouched() {
        let s = put_string("keep".to_string());
        let a = array_new(8, 0, 0);

        assert_eq!(take_string(a), None);
        assert_eq!(take_string(s), Some("keep".to_string()));
        assert_eq!(take_string(s), None, "stale string handle");

        assert_eq!(take_opaque::<i64>(s), None);
        array_drop(a);
    }

    #[test]
    fn array_new_clamps_hostile_capacities() {
        let huge = array_new(8, i64::MAX, 0);
        assert_ne!(huge, NULL);
        let cap = with_array(huge, |data| data.elements.capacity());
        assert!(cap.is_some_and(|c| c <= MAX_INITIAL_CAPACITY));
        assert_eq!(array_len(huge), 0);
        array_drop(huge);

        assert_eq!(array_new(-1, 4, 0), NULL);
    }

    #[test]
    fn wrong_kind_take_keeps_the_opaque_handle_valid() {
        let op = insert(Handle::Opaque(Box::new(vec![1i64, 2, 3])));
        assert_eq!(take_string(op), None);
        assert_eq!(take_opaque::<String>(op), None);
        assert_eq!(
            with_opaque::<Vec<i64>, _>(op, Clone::clone),
            Some(vec![1, 2, 3])
        );
        let taken = take_opaque::<Vec<i64>>(op);
        assert_eq!(taken, Some(vec![1, 2, 3]));
    }

    /// Regression: `take_string`/`take_opaque` remove the entry directly
    /// and must keep the leak-reporting `LIVE` counter in sync with the
    /// map.
    #[test]
    fn takes_balance_the_live_counter() {
        let s = put_string("one".to_string());
        let op = insert(Handle::Opaque(Box::new(1i64)));
        let wrong = put_string("two".to_string());
        assert!(is_registered(s) && is_registered(op) && is_registered(wrong));
        assert_counters_agree_with_map("three inserts");

        // A wrong-kind take leaves the entry alone: the counter must not
        // move.
        assert_eq!(take_string(op), None);
        assert_eq!(take_opaque::<i64>(s), None);
        assert!(is_registered(s) && is_registered(op));
        assert_counters_agree_with_map("wrong-kind takes");

        assert_eq!(take_string(s), Some("one".to_string()));
        assert_eq!(take_opaque::<i64>(op), Some(1));
        assert_eq!(take_string(wrong), Some("two".to_string()));
        assert!(!is_registered(s) && !is_registered(op) && !is_registered(wrong));
        assert_counters_agree_with_map("takes");

        let a = array_new(8, 0, 1);
        assert!(is_registered(a));
        assert_counters_agree_with_map("array_new");
        array_drop(a);
        assert!(!is_registered(a));
        assert_counters_agree_with_map("array_drop");
    }

    /// Every other path that removes an entry outside `remove` must
    /// decrement the counter too: a string element replaced inside an
    /// array, and the last release of a shared box.
    #[test]
    fn every_removal_path_keeps_the_counters_in_sync() {
        let arr = array_new(4, 0, 1);
        assert!(array_push(arr, put_string("old".to_string())));

        assert!(array_set(arr, 0, put_string("new".to_string())));
        assert_counters_agree_with_map("array_set of a string element");
        array_drop(arr);
        assert_counters_agree_with_map("array_drop of a string array");

        let boxed = insert(Handle::Shared(SharedData {
            inner: put_string("inner".to_string()),
            count: 2,
        }));
        assert_eq!(shared_release(boxed), NULL);
        assert!(is_registered(boxed));
        assert_counters_agree_with_map("shared_release with copies left");
        let inner = shared_release(boxed);
        assert!(!is_registered(boxed));
        assert_counters_agree_with_map("final shared_release");
        assert_eq!(take_string(inner), Some("inner".to_string()));
        assert_counters_agree_with_map("take of the released inner value");
    }

    /// Contention probe: 50k put/get/remove cycles from N threads. If the
    /// 4-thread wall time is far worse than 4x a single thread's, the global
    /// registry lock is a bottleneck and sharding would pay.
    #[test]
    fn registry_contention_probe() {
        use std::time::Instant;

        let cycle = |base: i64| {
            for i in 0..50_000 {
                let s = put_string(format!("probe-{base}-{i}"));
                assert!(get_string(s).is_some());
                let _ = take_string(s);
            }
        };

        let single = Instant::now();
        cycle(1_000_000);
        let single = single.elapsed();

        let start = Instant::now();
        let handles: Vec<_> = (0..4)
            .map(|t| std::thread::spawn(move || cycle(2_000_000 + t * 1_000_000)))
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let four = start.elapsed();

        println!("registry probe: 1 thread {single:?}, 4 threads {four:?}");
        // Informational: the assert only guards against pathological collapse.
        assert!(
            four.as_millis() < single.as_millis() * 20,
            "registry contention collapse: 1 thread {single:?}, 4 threads {four:?}"
        );
    }

    /// Permanent entries are live for the whole program and must be
    /// excluded from the leak-reporting counter.
    #[test]
    fn permanent_handles_are_excluded_from_the_live_counter() {
        let id = put_string_permanent("literal".to_string());
        assert!(is_registered(id));
        assert_eq!(get_string(id), Some("literal".to_string()));
        assert_counters_agree_with_map("insert_permanent");
    }

    #[test]
    fn taking_a_permanent_string_keeps_it_available() {
        let id = put_string_permanent("shared literal".to_string());
        assert_eq!(take_string(id), Some("shared literal".to_string()));
        assert_eq!(get_string(id), Some("shared literal".to_string()));
        assert!(is_registered(id));
    }

    /// A channel handle is reference counted: `go f(ch)` and `var b = ch`
    /// retain it, so the core survives until the last holder drops it, and
    /// only the removing drop touches `LIVE`.
    #[test]
    fn chan_handles_are_reference_counted_and_balance_the_live_counter() {
        let core = crate::ntask::scheduler::register_chan(1, false);
        let id = insert(Handle::Chan { core, count: 1 });
        assert_counters_agree_with_map("chan insert");

        assert_eq!(chan_retain(id), id);
        assert_eq!(chan_retain(id), id);
        assert_counters_agree_with_map("two chan retains");

        chan_drop(id);
        assert!(is_registered(id));
        assert_counters_agree_with_map("chan drop with copies left");
        chan_drop(id);
        assert!(is_registered(id));
        assert_counters_agree_with_map("second chan drop with a copy left");

        chan_drop(id);
        assert!(!is_registered(id));
        assert_counters_agree_with_map("final chan drop");

        chan_drop(id);
        assert_counters_agree_with_map("chan drop of a stale handle");
    }

    /// Dropping a channel handle must not close the channel: `close(ch)` is
    /// explicit, so a peer parked on a send stays parked when the other side
    /// goes out of scope, as in Go.
    #[test]
    fn dropping_a_chan_handle_does_not_close_the_channel() {
        let core = crate::ntask::scheduler::register_chan(0, false);
        let id = insert(Handle::Chan { core, count: 1 });
        chan_retain(id);

        chan_drop(id);

        let closed = crate::ntask::core::GLOBAL
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .chans
            .get(&core)
            .map(|chan| chan.closed);
        assert_eq!(closed, Some(false));

        chan_drop(id);
        assert!(!is_registered(id));
    }

    /// Drop glue plus a drop counter for the shared-object tests: the box
    /// holds a `String`, and every run records one drop.
    static SHARED_OBJECT_TEST_DROPS: AtomicI64 = AtomicI64::new(0);

    extern "C" fn drop_test_string(ptr: *mut std::ffi::c_void) {
        SHARED_OBJECT_TEST_DROPS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: test-only glue paired with boxes made by `share_object_test`.
        unsafe {
            drop(Box::from_raw(ptr as *mut String));
        }
    }

    fn share_object_test(value: &str, tag: u64) -> i64 {
        let boxed = Box::new(value.to_string());
        shared_object_put(
            Box::into_raw(boxed) as *mut std::ffi::c_void,
            Some(drop_test_string),
            tag,
        )
    }

    fn read_test(ptr: *mut std::ffi::c_void) -> String {
        // SAFETY: test pointers come from live shared entries.
        unsafe { (*(ptr as *const String)).clone() }
    }

    #[test]
    fn shared_objects_borrow_take_and_release_exactly_once() {
        let tag = shared_object_tag("test-crate@1.0+feat/Type");
        let drops_before = SHARED_OBJECT_TEST_DROPS.load(Ordering::Relaxed);
        let id = share_object_test("hello", tag);
        assert!(is_registered(id));
        assert_counters_agree_with_map("shared put");

        // Borrow retains and reads back the value; release balances it.
        let ptr = shared_object_borrow(id, tag);
        assert!(!ptr.is_null());
        assert_eq!(read_test(ptr), "hello");
        shared_object_release(id);
        assert!(is_registered(id));

        // Take transfers ownership without running drop glue; the caller
        // frees the box itself.
        let taken = shared_object_take(id, tag);
        assert!(!taken.is_null());
        assert!(!is_registered(id));
        assert_eq!(read_test(taken), "hello");
        assert_eq!(
            SHARED_OBJECT_TEST_DROPS.load(Ordering::Relaxed),
            drops_before
        );
        drop_test_string(taken);
        assert_eq!(
            SHARED_OBJECT_TEST_DROPS.load(Ordering::Relaxed),
            drops_before + 1
        );
        assert_counters_agree_with_map("shared take and manual drop");
    }

    #[test]
    fn mutable_object_borrows_are_exclusive_and_preserve_the_handle() {
        let tag = shared_object_tag("mutable-fixture/String");
        let id = share_object_test("before", tag);
        let shared = shared_object_borrow(id, tag);
        assert!(!shared.is_null());
        assert!(shared_object_borrow_mut(id, tag).is_null());
        shared_object_release(id);
        let exclusive = shared_object_borrow_mut(id, tag);
        assert!(!exclusive.is_null());
        assert!(shared_object_borrow(id, tag).is_null());
        assert!(shared_object_borrow_mut(id, tag).is_null());
        assert!(shared_object_take(id, tag).is_null());
        // SAFETY: this pointer has the string tag and the sole mutable borrow.
        unsafe { (*(exclusive as *mut String)).push_str(" after") };
        shared_object_release_mut(id);
        let shared = shared_object_borrow(id, tag);
        assert_eq!(read_test(shared), "before after");
        shared_object_release(id);
        shared_object_release(id);
        assert!(!is_registered(id));
    }

    #[test]
    fn shared_object_release_runs_drop_glue_at_zero() {
        let tag = shared_object_tag("test-crate@1.0+feat/Other");
        let drops_before = SHARED_OBJECT_TEST_DROPS.load(Ordering::Relaxed);
        let id = share_object_test("bye", tag);
        assert_eq!(shared_object_retain(id), id);
        shared_object_release(id);
        assert!(is_registered(id));
        assert_eq!(
            SHARED_OBJECT_TEST_DROPS.load(Ordering::Relaxed),
            drops_before
        );
        shared_object_release(id);
        assert!(!is_registered(id));
        assert_eq!(
            SHARED_OBJECT_TEST_DROPS.load(Ordering::Relaxed),
            drops_before + 1
        );
        assert_counters_agree_with_map("shared release to zero");
    }

    #[test]
    fn shared_tag_mismatch_fails_closed() {
        let tag = shared_object_tag("test-crate@1.0+feat/Same");
        let other = shared_object_tag("test-crate@2.0+feat/Same");
        assert_ne!(tag, other);
        let drops_before = SHARED_OBJECT_TEST_DROPS.load(Ordering::Relaxed);
        let id = share_object_test("guarded", tag);

        assert!(shared_object_borrow(id, other).is_null());
        assert!(shared_object_take(id, other).is_null());
        // The entry is untouched: the right tag still borrows...
        let ptr = shared_object_borrow(id, tag);
        assert!(!ptr.is_null());
        shared_object_release(id);
        // ...and takes once unique ownership is restored.
        let taken = shared_object_take(id, tag);
        assert!(!taken.is_null());
        assert!(!is_registered(id));
        drop_test_string(taken);
        assert_eq!(
            SHARED_OBJECT_TEST_DROPS.load(Ordering::Relaxed),
            drops_before + 1
        );
    }

    #[test]
    fn shared_take_refuses_outstanding_borrows() {
        let tag = shared_object_tag("test-crate@1.0+feat/Busy");
        let id = share_object_test("busy", tag);
        let ptr = shared_object_borrow(id, tag);
        assert!(!ptr.is_null());
        // An in-flight borrow holds a raw pointer: taking over it would
        // free live memory, so the take fails and the entry survives.
        assert!(shared_object_take(id, tag).is_null());
        assert!(is_registered(id));
        shared_object_release(id);
        let taken = shared_object_take(id, tag);
        assert!(!taken.is_null());
        drop_test_string(taken);
    }

    #[test]
    fn shared_wrong_kind_and_stale_handles_are_no_ops() {
        let tag = shared_object_tag("test-crate@1.0+feat/Kind");
        let string = put_string("plain".to_string());
        assert!(shared_object_borrow(string, tag).is_null());
        assert!(shared_object_take(string, tag).is_null());
        assert_eq!(shared_object_tag_of(string), 0);
        shared_object_release(string);
        assert_eq!(get_string(string), Some("plain".to_string()));

        let stale = 987_654_321;
        assert!(shared_object_borrow(stale, tag).is_null());
        assert!(shared_object_take(stale, tag).is_null());
        assert_eq!(shared_object_retain(stale), stale);
        assert_eq!(shared_object_tag_of(stale), 0);
        shared_object_release(stale);
        shared_object_release(NULL);
        assert_eq!(take_string(string), Some("plain".to_string()));

        // Fail-closed puts never create entries.
        assert_eq!(
            shared_object_put(std::ptr::null_mut(), Some(drop_test_string), tag),
            NULL
        );
        let leaked = Box::into_raw(Box::new("leak-check".to_string()));
        assert_eq!(
            shared_object_put(leaked as *mut std::ffi::c_void, None, tag),
            NULL
        );
        // SAFETY: the rejected put transferred nothing; free it here.
        unsafe {
            drop(Box::from_raw(leaked));
        }
        assert_counters_agree_with_map("shared no-op paths");
    }

    #[test]
    fn shared_tags_intern_stably_and_report_text() {
        let first = shared_object_tag("some-crate@0.1+a,b/Type");
        assert_eq!(shared_object_tag("some-crate@0.1+a,b/Type"), first);
        assert_ne!(first, 0);
        assert_eq!(
            shared_object_tag_name(first).as_deref(),
            Some("some-crate@0.1+a,b/Type")
        );
        assert_eq!(shared_object_tag_name(u64::MAX), None);
        assert_eq!(shared_object_tag_of(987_654_321), 0);
    }
}
