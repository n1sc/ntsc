//! Minimal runtime + standard library support for NTSC-compiled binaries.
//!
//! All functions here use `extern "C"` ABI so they can be called from
//! LLVM-generated code. No raw pointers cross the boundary except through
//! the cross-adaptor shared-object API (`ntsc_shared_object_*`), where adaptor
//! crates exchange heap values as opaque pointers with creator-side drop
//! glue; every other owned heap value (string, array, shared box, future,
//! opaque module value) lives in the handle registry under an `i64` key,
//! and generated code passes only those keys. `unsafe` appears only in the
//! shared-object boundary functions, where the caller contracts are
//! documented per function.
//!
//! ## Ownership rules
//!
//! * A handle is registered exactly once and removed exactly once. Copying a
//!   handle in generated code is a *borrow* and performs no registry
//!   operation; deep copies (`copy(...)`, string clone, array deep clone)
//!   register a fresh entry.
//! * Handle `0` is the null handle: every API treats it as "no value" and is
//!   a no-op for it.
//! * Reader functions (`ntsc_say`, `ntsc_string_equals`, `ntsc_array_get`,
//!   ...) borrow their arguments. Functions named `*_drop` remove their
//!   argument unconditionally. `ntsc_throw`, `ntsc_panic`, `ntsc_assert`, and
//!   `ntsc_string_from_words` *consume* their message/word-array argument.
//!
//! ## Exceptions
//!
//! Exceptions use a return-check model. `ntsc_throw` stores the message in a
//! thread-local pending slot and returns `0`; generated code checks
//! `ntsc_exception_pending()` after every call and branches to its
//! exception-return path when it is set. `ntsc_exception_take_message`
//! transfers the message to a catch binding and clears the pending slot;
//! `ntsc_rethrow` re-arms it (used after a `finally` that ran with a clean
//! flag).

mod idmap;
pub mod modules;
mod ntask;
mod registry;

use std::io::{self, Write};

use crate::registry::NULL;

// ══════════════════════════════════════════════════════════════════════════
// Entry / exit
// ══════════════════════════════════════════════════════════════════════════

/// Entry point stub — linked against generated `main` symbol.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_runtime_init() {}

/// Enter a blocking section on the calling thread. When the caller is a
/// scheduler worker and every regular worker is already blocked, a
/// temporary coverage worker is spawned so ready goroutines still find a
/// driver (see `ntask::scheduler::worker_will_block`). Always paired with
/// [`ntsc_worker_did_unblock`]; a no-op off scheduler workers.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_worker_will_block() {
    ntask::scheduler::worker_will_block();
}

/// Leave a blocking section entered with [`ntsc_worker_will_block`].
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_worker_did_unblock() {
    ntask::scheduler::worker_did_unblock();
}

/// RAII guard for blocking sections inside the runtime: enters on
/// construction, leaves on drop, so every return path — including a caught
/// panic unwinding through generated-code frames — stays balanced.
pub(crate) struct BlockGuard;

impl BlockGuard {
    pub(crate) fn enter() -> Self {
        ntask::scheduler::worker_will_block();
        BlockGuard
    }
}

impl Drop for BlockGuard {
    fn drop(&mut self) {
        ntask::scheduler::worker_did_unblock();
    }
}

/// Exit point — called after `main` returns. Reports leaked registry
/// entries to stderr (only when `report != 0`, i.e. debug builds) and
/// aborts on an uncaught pending exception.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_runtime_shutdown(report: i8) {
    if ntsc_exception_pending() != 0 {
        ntsc_uncaught_exception();
    }
    // Drain workers before reporting so the final goroutine drives can
    // reclaim their registry entries.
    ntask::reactor::shutdown();
    ntask::scheduler::shutdown();
    // Capability loads keep an `Arc` in a thread-local fast-path cache. It is
    // not a registry entry, so clear it explicitly before process shutdown to
    // release the final backing allocation even when no later memory access
    // invalidates that cache.
    registry::clear_memory_cache();
    let leaks = registry::live_count();
    if report != 0 && leaks > 0 {
        let entries = registry::live_entries();
        let stderr = io::stderr();
        let mut handle = stderr.lock();
        let _ = writeln!(
            handle,
            "warning[NTSC-W0002]: memory leak detected: {} registry object(s) leaked",
            leaks
        );
        for entry in entries {
            let detail = entry
                .detail
                .as_deref()
                .map(|value| format!(" containing \"{value}\""))
                .unwrap_or_default();
            if let Some((line, column)) = entry.site {
                let _ = writeln!(handle, "  --> <source>:{line}:{column}");
                let _ = writeln!(
                    handle,
                    "   = note: {} handle {}{} was allocated here",
                    entry.kind, entry.id, detail
                );
            } else {
                let _ = writeln!(
                    handle,
                    "   = note: leaked {} handle {}{} (runtime allocation)",
                    entry.kind, entry.id, detail
                );
            }
        }
        let _ = handle.flush();
    }
}

/// Attach an NTSC source location to a registry allocation for debug leak
/// diagnostics. Values created entirely inside the runtime remain unmarked.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_leak_mark(id: i64, line: i64, column: i64) {
    registry::mark_leak_site(id, line, column);
}

// ══════════════════════════════════════════════════════════════════════════
// Strings
// ══════════════════════════════════════════════════════════════════════════

/// Print the string behind `msg` to stdout, followed by a newline. Borrows
/// the string; the null handle is a no-op. Guarded: a slow pipe reader
/// blocks the writing thread.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_say(msg: i64) {
    let _block = BlockGuard::enter();
    let Some(text) = registry::with_string(msg, str::to_string) else {
        return;
    };
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(text.as_bytes());
    let _ = handle.write_all(b"\n");
    let _ = handle.flush();
}

/// Print a 64-bit signed integer to stdout as a string, followed by
/// newline.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_print_i64(val: i64) {
    let _block = BlockGuard::enter();
    println!("{val}");
}

/// Print a 64-bit float to stdout as a string, followed by newline.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_print_f64(val: f64) {
    let _block = BlockGuard::enter();
    println!("{val}");
}

/// Convert an integer (0/1) to the string "true" or "false". The returned
/// handle is owned by the caller.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_bool_to_string(val: i8) -> i64 {
    let s = if val != 0 { "true" } else { "false" };
    registry::put_string(s.to_string())
}

/// Convert an i64 to a string. The returned handle is owned by the caller.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_i64_to_string(val: i64) -> i64 {
    registry::put_string(format!("{val}"))
}

/// Convert an f64 to a string. The returned handle is owned by the caller.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_f64_to_string(val: f64) -> i64 {
    registry::put_string(format!("{val}"))
}

/// Concatenate two strings. Both arguments are borrowed; the returned
/// handle is a fresh owned string. The null handle reads as an empty
/// string.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_string_concat(a: i64, b: i64) -> i64 {
    registry::string_concat(a, b).unwrap_or(NULL)
}

/// Duplicate a string, registering a fresh owned copy. This is the runtime
/// support for the `copy("...")` expression. The null handle copies to
/// null.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_string_clone(s: i64) -> i64 {
    registry::clone_string(s).unwrap_or(NULL)
}

/// Compare two strings for equality; returns 1 when equal, 0 otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_string_equals(a: i64, b: i64) -> i8 {
    i8::from(registry::string_equals(a, b))
}

/// Drop an owned string (removes it from the registry). The null handle is
/// a no-op, so moved-from or never-initialized slots can be dropped
/// unconditionally.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_string_drop(id: i64) {
    let _ = registry::take_string(id);
}

/// Upper bound for `ntsc_string_from_words` output, in bytes: keeps word
/// loops and `Vec` growth bounded even for hostile byte counts.
const MAX_BUILTIN_OUTPUT: usize = 1 << 24;

fn string_from_words_impl(words: i64, byte_count: i64, permanent: bool) -> i64 {
    let mut bytes = Vec::new();
    if words != NULL {
        // Clamp the byte count: a hostile count could otherwise force an
        // effectively unbounded word loop (or a `Vec` capacity panic).
        let byte_count = (byte_count.max(0) as usize).min(MAX_BUILTIN_OUTPUT);
        let word_count = byte_count / 8 + usize::from(byte_count > 0);
        for i in 0..word_count {
            let word = registry::array_get(words, i as i64).unwrap_or(0) as u64;
            bytes.extend_from_slice(&word.to_ne_bytes());
        }
        bytes.truncate(byte_count);
        // The word array is consumed, not leaked.
        registry::array_drop(words);
    }
    if permanent {
        registry::put_string_permanent(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        registry::put_string(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Reassemble a UTF-8 string from the `i64` words of `words`, consuming
/// the word array. The returned handle is an owned string.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_string_from_words(words: i64, byte_count: i64) -> i64 {
    string_from_words_impl(words, byte_count, false)
}

/// Like `ntsc_string_from_words`, but registers the string as permanent:
/// it is a compile-time constant (string literal), owned for the program
/// lifetime, never removed, and excluded from leak reporting.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_string_from_words_permanent(words: i64, byte_count: i64) -> i64 {
    string_from_words_impl(words, byte_count, true)
}

/// Byte length of the string behind `s` (0 for null or non-string).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_string_len(s: i64) -> i64 {
    registry::with_string(s, |t| t.len() as i64).unwrap_or(0)
}

/// Byte at `index` in the string behind `s` (0-255), or -1 for out of
/// bounds, null, or non-string.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_string_byte_at(s: i64, index: i64) -> i64 {
    if index < 0 {
        return -1;
    }
    registry::with_string(s, |t| {
        t.as_bytes()
            .get(index as usize)
            .copied()
            .map(|b| b as i64)
            .unwrap_or(-1)
    })
    .unwrap_or(-1)
}

/// Create a new string handle from a raw byte buffer. `ptr` must point to
/// at least `len` readable bytes. The bytes are interpreted as UTF-8 with
/// lossy replacement. Returns the null handle for null ptr or zero length.
///
/// # Safety
///
/// `ptr` must be non-null and point to at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ntsc_string_from_bytes(ptr: *const u8, len: i64) -> i64 {
    // An empty string is a real string: the poll-family bridges hand back
    // an empty handle for "no event", and Neutron compares it against "".
    // The pointer is never dereferenced for len == 0, so a null pointer is
    // only rejected when bytes would actually be read.
    if len < 0 || (ptr.is_null() && len != 0) {
        return NULL;
    }
    if len == 0 {
        return registry::put_string(String::new());
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    registry::put_string(String::from_utf8_lossy(bytes).into_owned())
}

/// Overwrite the string behind `handle` with the given bytes (interpreted
/// as UTF-8 with lossy replacement), so callers observe mutated out-buffers
/// in place. Returns whether the handle referred to a live string.
///
/// # Safety
///
/// `ptr` must be non-null and point to at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ntsc_string_assign_bytes(handle: i64, ptr: *const u8, len: i64) -> i8 {
    if handle == NULL || ptr.is_null() || len < 0 {
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    i8::from(registry::assign_string(
        handle,
        String::from_utf8_lossy(bytes).into_owned(),
    ))
}

/// Copy the bytes of the string behind `handle` into `out_buf`, writing
/// at most `max_len` bytes. Returns the number of bytes actually written
/// (0 for null/unknown handle or zero-length buffer).
///
/// # Safety
///
/// `out_buf` must be non-null and point to at least `max_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ntsc_string_to_bytes(handle: i64, out_buf: *mut u8, max_len: i64) -> i64 {
    if handle == NULL || out_buf.is_null() || max_len <= 0 {
        return 0;
    }
    registry::with_string(handle, |s| {
        let bytes = s.as_bytes();
        let to_copy = bytes.len().min(max_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, to_copy);
        }
        to_copy as i64
    })
    .unwrap_or(0)
}

// ══════════════════════════════════════════════════════════════════════════
// Panic / error handling
// ══════════════════════════════════════════════════════════════════════════

/// Panic with a message — prints an error and aborts. Consumes the message
/// handle.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_panic(msg: i64) {
    let text = registry::get_string(msg).unwrap_or_else(|| "panic".to_string());
    let _ = registry::take_string(msg);
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    let _ = writeln!(handle, "NTSC PANIC: {text}");
    let _ = handle.flush();
    std::process::abort();
}

/// Assert — if `condition` is 0, panic with the given message. Consumes
/// the message handle either way so the generated caller never leaks it.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_assert(condition: i8, msg: i64) {
    if condition == 0 {
        ntsc_panic(msg);
    } else {
        let _ = registry::take_string(msg);
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Dynamic arrays
// ══════════════════════════════════════════════════════════════════════════

/// Create a new dynamic array of raw scalar elements.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_new(elem_size: i64, initial_capacity: i64) -> i64 {
    ntsc_array_new_typed(elem_size, initial_capacity, 0)
}

/// Create a new dynamic array. `string_elements != 0` marks an array whose
/// elements are owned strings: the runtime deep-copies strings on insert,
/// deep-copies them in every array-producing operation, and reclaims them
/// when the container is dropped. All other arrays store raw `i64` slots.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_new_typed(
    elem_size: i64,
    initial_capacity: i64,
    string_elements: i8,
) -> i64 {
    registry::array_new(elem_size, initial_capacity, string_elements)
}

/// Set the string-elements flag of an existing array. Empty array literals
/// do not know their element representation at creation time, so codegen
/// calls this once the destination type is known. Only meaningful before
/// any element has been inserted.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_set_string_elements(id: i64, string_elements: i8) {
    registry::array_set_string_elements(id, string_elements);
}

/// Get the length of a dynamic array (0 for the null handle).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_len(id: i64) -> i64 {
    registry::array_len(id)
}

/// Whether `id` names a live dynamic array (0 for the null handle or any
/// other value kind). Generated Rust bridges use this to reject wrong-kind
/// handles with a precise error instead of decoding them as empty.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_is_array(id: i64) -> i8 {
    i8::from(registry::is_array(id))
}

/// Read the element at `index`. Returns the raw element value for scalar
/// arrays and the stored handle for string/nested-array elements (a
/// *borrow*: the array keeps ownership). An out-of-bounds index throws
/// instead of reading garbage.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_get(id: i64, index: i64) -> i64 {
    match registry::array_get(id, index) {
        Some(value) => value,
        None => throw_string("array index out of bounds"),
    }
}

/// Push an element into a dynamic array. String elements are deep-copied
/// into a fresh handle owned by the array; all other elements are stored
/// by value. Returns the array handle on success, 0 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_push(id: i64, elem: i64) -> i64 {
    if registry::array_push(id, elem) {
        id
    } else {
        NULL
    }
}

/// Replace the element at `index`. String elements are deep-copied into a
/// fresh handle owned by the array and the old element is reclaimed; all
/// other elements are replaced by value. A negative or out-of-bounds index
/// throws (mirroring `ntsc_array_get`); an unknown or non-array handle is
/// a safe failure returning 0. Returns the array handle on success.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_set(id: i64, index: i64, elem: i64) -> i64 {
    if !registry::is_array(id) {
        return NULL;
    }
    if index < 0 || index >= registry::array_len(id) {
        return throw_string("array index out of bounds");
    }
    if registry::array_set(id, index, elem) {
        id
    } else {
        NULL
    }
}

/// Remove and return the last element of an owned array. For a string
/// array the returned handle is *transferred* to the caller; for scalar
/// arrays the raw value is returned. Returns 0 for an empty array.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_pop(id: i64) -> i64 {
    registry::array_pop(id).unwrap_or(NULL)
}

/// Drop an owned array, reclaiming any owned string elements. The null
/// handle is a no-op, so moved-from slots can be dropped unconditionally.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_drop(id: i64) {
    registry::array_drop(id);
}

/// Cross-adaptor shared objects: one process-wide handle space letting
/// generated adaptor crates exchange opaque values (see
/// `registry::shared_object_put`). Unlike strings and arrays, shared values
/// cross as pointers with creator-side drop glue: the runtime stores
/// both opaquely and only hands the pointer back after checking the
/// entry's type tag.
///
/// Safety on every function below: `ptr`/`len`/`out` arguments must be
/// live, readable/writable for their lengths, and pointers handed to
/// `ntsc_shared_object_put` must be live `Box` allocations whose layout matches
/// `drop_fn`; ownership of a put pointer transfers to the registry.
/// Pointers returned by borrow/take are valid until the matching release
/// (borrow) or until the caller frees them (take).
///
/// # Safety
///
/// The caller must ensure `ptr` is readable for `len` bytes and that the
/// pointer range is valid for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ntsc_shared_object_tag(ptr: *const u8, len: i64) -> u64 {
    if ptr.is_null() || len <= 0 {
        return 0;
    }
    // SAFETY: caller guarantees `ptr..ptr+len` is readable (adaptor-side
    // constant tag strings).
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return 0;
    };
    registry::shared_object_tag(text)
}

/// Copy the text of one interned tag into the caller's buffer (up to
/// `cap` bytes) and return the bytes written. Unknown ids and
/// non-positive caps write nothing.
///
/// # Safety
///
/// The caller must ensure `out` is writable for `cap` bytes when `cap` is
/// positive and that the pointer remains valid for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ntsc_shared_object_tag_text(id: u64, out: *mut u8, cap: i64) -> i64 {
    if out.is_null() || cap <= 0 {
        return 0;
    }
    let Some(name) = registry::shared_object_tag_name(id) else {
        return 0;
    };
    let bytes = name.as_bytes();
    let count = bytes.len().min(cap as usize);
    // SAFETY: caller guarantees `out..out+cap` is writable.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, count);
    }
    count as i64
}

/// Register one heap value shared across adaptors (`refs == 1`). Null
/// pointers, null drop glue, and the zero tag fail closed to the null
/// handle.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_object_put(
    ptr: *mut std::ffi::c_void,
    drop_fn: Option<extern "C" fn(*mut std::ffi::c_void)>,
    tag: u64,
) -> i64 {
    registry::shared_object_put(ptr, drop_fn, tag)
}

/// Borrow the shared value behind `id` after checking `tag`: retains and
/// returns its pointer (null on mismatch). The caller must call
/// `ntsc_shared_object_release` after use.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_object_borrow(id: i64, tag: u64) -> *mut std::ffi::c_void {
    registry::shared_object_borrow(id, tag)
}

/// Borrow an exclusively owned object mutably. The pointer stays valid until
/// `ntsc_shared_object_release_mut`; other borrows and takes fail meanwhile.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_object_borrow_mut(id: i64, tag: u64) -> *mut std::ffi::c_void {
    registry::shared_object_borrow_mut(id, tag)
}

/// End a mutable borrow without consuming the object's owning handle.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_object_release_mut(id: i64) {
    registry::shared_object_release_mut(id);
}

/// Take ownership of the shared value behind `id` after checking `tag`:
/// removes the entry without running drop glue and returns its pointer
/// (null on mismatch or while a borrow is outstanding).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_object_take(id: i64, tag: u64) -> *mut std::ffi::c_void {
    registry::shared_object_take(id, tag)
}

/// Release one shared-object reference; at zero the entry is removed and
/// its drop glue runs once. Null, unknown, and wrong-kind handles are
/// no-ops.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_object_release(id: i64) {
    registry::shared_object_release(id);
}

/// Record another live Neutron-side share. Unknown handles are no-ops
/// returning the input unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_object_retain(id: i64) -> i64 {
    registry::shared_object_retain(id)
}

/// The tag of the shared object behind `id`, or 0 for null, unknown, and
/// wrong-kind handles.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_object_tag_of(id: i64) -> u64 {
    registry::shared_object_tag_of(id)
}

/// Return a new array with the element at `index` removed (negative = from
/// the end). The input array is never mutated. The result is a copy even
/// when the index is out of bounds (a no-op), so every returned handle is
/// fresh.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_remove_at(id: i64, index: i64) -> i64 {
    registry::array_remove_at(id, index)
}

/// Return a new array that is a copy of the given array. String elements
/// are deep-copied so the two arrays never share ownership; all other
/// elements are copied by value.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_clone(id: i64) -> i64 {
    registry::array_clone(id, 0)
}

/// Return a new array that is a deep copy of the given array. Elements
/// that are themselves arrays (at `elem_array_levels` nested depths) are
/// recursively deep-cloned; string elements are deep-copied as well, so no
/// subtree or string is shared with the source.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_deep_clone(id: i64, elem_array_levels: i64) -> i64 {
    registry::array_clone(id, elem_array_levels)
}

/// Return a new array containing a sub-range of the given array. `end < 0`
/// means "to the end". Out-of-range indices are clamped.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_slice(id: i64, start: i64, end: i64) -> i64 {
    registry::array_slice(id, start, end)
}

/// Return a new array with the elements in reverse order.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_reverse(id: i64) -> i64 {
    registry::array_reverse(id)
}

/// Return a new array containing `count` copies of `val`. When
/// `string_elements != 0`, `val` is a string handle and every element is
/// an independent deep copy of it; otherwise `val` is the raw element
/// value.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_fill(
    val: i64,
    count: i64,
    elem_size: i64,
    string_elements: i8,
) -> i64 {
    registry::array_fill(val, count, elem_size, string_elements)
}

/// Return a new array of `i64` elements `start..end` (empty when `end <=
/// start`).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_range(start: i64, end: i64) -> i64 {
    registry::array_range(start, end)
}

/// Return a new empty array with the same element representation as the
/// given array. The input array is never mutated.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_clear(id: i64) -> i64 {
    registry::array_clear(id)
}

/// Return a new array with the elements shuffled (Fisher-Yates using OS
/// randomness). The input array is never mutated.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_shuffle(id: i64) -> i64 {
    registry::array_shuffle(id)
}

/// Return a new array with the elements sorted. `mode` selects the
/// comparison: 0 = `i64` values, 1 = `f64` values (via raw bits), 2 =
/// strings. The input array is never mutated.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_array_sort(id: i64, mode: i8) -> i64 {
    registry::array_sort(id, mode)
}

// ══════════════════════════════════════════════════════════════════════════
// Shared boxes
// ══════════════════════════════════════════════════════════════════════════

/// Register a shared box adopting a single owned reference to `inner`.
/// Returns the box handle; `inner` (the wrapped value's handle) is now
/// owned by the box and reclaimed when the last reference is released.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_new(inner: i64) -> i64 {
    registry::shared_new(inner)
}

/// Record another live copy of the shared box. Returns the box handle.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_retain(id: i64) -> i64 {
    registry::shared_retain(id)
}

/// Release one copy of the shared box. When the last copy is released the
/// box is removed and ownership of the wrapped value's handle is returned
/// to the caller (0 is returned while copies remain). Generated code must
/// drop the returned handle like any owned value.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_release(id: i64) -> i64 {
    registry::shared_release(id)
}

/// Borrow the handle of the value wrapped by a shared box (used by `view
/// of shared`). Returns 0 for an unknown box.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_shared_inner(id: i64) -> i64 {
    registry::shared_inner(id)
}

use std::cell::RefCell;

// ══════════════════════════════════════════════════════════════════════════
// Exception state (return-check model)
// ══════════════════════════════════════════════════════════════════════════

thread_local! {
    /// The handle of the pending exception message, or `None` when no
    /// exception is active. The message stays registered for the whole
    /// propagation; it is removed by `ntsc_exception_take_message` (catch
    /// binding) or `ntsc_exception_clear`.
    static PENDING_EXCEPTION: RefCell<Option<i64>> = const { RefCell::new(None) };
}

/// Replace the current async task stack with a scheduler-owned stack.
pub(crate) fn install_async_tasks(tasks: Vec<(AsyncPollFn, i64)>) {
    ASYNC_TASKS.with(|current| *current.borrow_mut() = tasks);
}

/// Poll the current scheduler task once, retaining a child pushed by the
/// poller for the next scheduler turn.
pub(crate) fn poll_async_tasks_once() {
    ASYNC_TASKS.with(|tasks| {
        let (poll, future) = match tasks.borrow().last().copied() {
            Some(value) => value,
            None => return,
        };
        let depth = tasks.borrow().len();
        let done = poll(future) == 1;
        let mut stack = tasks.borrow_mut();
        if done && stack.len() == depth {
            stack.pop();
        }
    });
}

pub(crate) fn take_async_tasks() -> Vec<(AsyncPollFn, i64)> {
    ASYNC_TASKS.with(|tasks| std::mem::take(&mut *tasks.borrow_mut()))
}

/// Transfer the current thread's pending exception message out of its TLS
/// slot, so the sender can move it to another thread. Returns `NULL` when
/// none is pending. Ownership of the returned handle passes to the caller.
pub(crate) fn take_pending_exception_message() -> i64 {
    ntsc_exception_take_message()
}

/// Re-arm `msg` as the pending exception on the current thread. `msg` must
/// be a valid message handle (or `NULL`). Used to re-raise an exception
/// captured from another thread's goroutine.
pub(crate) fn rearm_pending_exception(msg: i64) {
    if msg != NULL {
        ntsc_throw(msg);
    }
}

/// Throw an exception with the given message. *Consumes* the message
/// handle and returns 0; the pending flag is observed via
/// `ntsc_exception_pending`. The caller must not use `msg` afterwards.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_throw(msg: i64) -> i64 {
    let message = if msg == NULL {
        registry::put_string(String::new())
    } else {
        msg
    };
    let previous = PENDING_EXCEPTION.with(|pending| pending.borrow_mut().replace(message));
    if let Some(previous) = previous
        && previous != message
    {
        // A nested runtime failure may replace an exception before generated
        // code gets a chance to take it. The pending slot owns the old
        // message, so replacing it must release that ownership immediately.
        let _ = registry::take_string(previous);
    }
    NULL
}

/// Re-arm a previously taken exception so it keeps propagating. Identical
/// to `ntsc_throw`; used after a `finally` block that ran with a clean
/// pending flag. Consumes the message handle.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_rethrow(msg: i64) -> i64 {
    ntsc_throw(msg)
}

/// Return 1 when an exception is pending, 0 otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_exception_pending() -> i8 {
    PENDING_EXCEPTION.with(|pending| i8::from(pending.borrow().is_some()))
}

/// Alias of [`ntsc_exception_pending`].
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_exception_is_active() -> i8 {
    ntsc_exception_pending()
}

/// Borrow the message of the pending exception. The handle stays valid
/// until the exception is taken or cleared; returns 0 when none is
/// pending.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_exception_get_message() -> i64 {
    PENDING_EXCEPTION.with(|pending| pending.borrow().unwrap_or(NULL))
}

/// Transfer ownership of the pending exception message to the caller and
/// clear the pending slot. Returns 0 when none is pending. The caller must
/// drop the returned string like any owned value.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_exception_take_message() -> i64 {
    PENDING_EXCEPTION.with(|pending| pending.borrow_mut().take().unwrap_or(NULL))
}

/// Clear the pending exception, reclaiming its message.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_exception_clear() {
    let message = ntsc_exception_take_message();
    let _ = registry::take_string(message);
}

/// Report an uncaught exception (print the message to stderr) and exit with a
/// failure code. The pending slot is left empty afterwards (the process is
/// ending anyway): `abort` would also be legitimate, but a thrown-value error
/// is a normal user-visible failure in a 0.1 language, and aborting with
/// SIGABRT turns every user's unhandled error into a core dump.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_uncaught_exception() -> ! {
    let message = ntsc_exception_take_message();
    let text = registry::get_string(message).unwrap_or_else(|| "exception".to_string());
    let _ = registry::take_string(message);
    let text = if text.is_empty() {
        "uncaught exception".to_string()
    } else {
        text
    };
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    let _ = writeln!(handle, "uncaught exception: {text}");
    let _ = handle.flush();
    std::process::exit(1);
}

/// Internal helper: throw a string literal owned by the runtime itself.
fn throw_string(message: &str) -> i64 {
    ntsc_throw(registry::put_string(message.to_string()))
}

// ══════════════════════════════════════════════════════════════════════════
// Async executor (see docs/async-rfc.md §8.3)
// ══════════════════════════════════════════════════════════════════════════

/// A poll function for one async future. Returns `1` when the future has
/// completed (its result is stored wherever the generated poll logic puts
/// it), `0` when it is still pending. The future is referenced by its
/// registry handle.
pub type AsyncPollFn = extern "C" fn(i64) -> i8;

/// Reclaims one heap-allocated future: runs its drop, then frees it. A
/// goroutine keeps its root future's cleanup so the scheduler can reclaim a
/// future that never completed; the trampoline does the same on completion.
pub type AsyncCleanupFn = extern "C" fn(i64);

thread_local! {
    /// Per-context cooperative task stack: each entry is the poll function
    /// and future handle of an in-progress async function.
    static ASYNC_TASKS: RefCell<Vec<(AsyncPollFn, i64)>> = const { RefCell::new(Vec::new()) };

    /// Stack of async contexts for `wait_any`/`wait_all`. Each entry is a
    /// saved `(ASYNC_TASKS, result)` pair. The active context is always in
    /// `ASYNC_TASKS`; pushing/saving/restoring swaps the TLS.
    static ASYNC_STACKS: RefCell<Vec<AsyncStackEntry>> = const { RefCell::new(Vec::new()) };
}

/// A saved async context: the task list and result of the enclosing scope.
type AsyncStackEntry = (Vec<(AsyncPollFn, i64)>, i64);

/// Register a new `async.sleep(ms)` future and return its handle.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_sleep_new(ms: i64) -> i64 {
    registry::async_sleep_new(ms)
}

/// Drop an `async.sleep` future handle. A handle of any other kind, an
/// already dropped one, and the null handle are all no-ops.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_sleep_drop(id: i64) {
    registry::async_sleep_drop(id);
}

/// Poll an `async.sleep(ms)` future: arm it on the first poll, then return
/// `1` once the deadline has passed and `0` otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_sleep_poll(id: i64) -> i8 {
    registry::async_sleep_poll(id)
}

/// Register an offloaded-blocking future: `work(arg)` runs on the worker pool
/// and its result handle is yielded once the job completes. Returns the
/// future handle.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_op_new(work: extern "C" fn(i64) -> i64, arg: i64) -> i64 {
    registry::async_op_new(Box::new(move || work(arg)))
}

/// Poll an offloaded-blocking future. The first poll starts the job on the
/// pool and parks the goroutine; returns `1` once the job is done and its
/// result is available via [`ntsc_async_op_result`].
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_op_poll(id: i64) -> i8 {
    registry::async_op_poll(id)
}

/// Reap the result handle of a completed offloaded future. A negative
/// value marks an exception the bridge threw on its worker thread (the
/// negated message handle): it is re-raised on the awaiting thread here,
/// so the resumed goroutine unwinds exactly like a synchronous call.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_op_result(id: i64) -> i64 {
    let result = registry::async_op_result(id);
    if result < 0 {
        ntsc_throw(-result);
        return NULL;
    }
    result
}

/// Drop an offloaded-blocking future handle.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_op_drop(id: i64) {
    registry::async_op_drop(id);
}

// ══════════════════════════════════════════════════════════════════════════
// Virtual-task scheduler ABI
// ══════════════════════════════════════════════════════════════════════════

#[unsafe(no_mangle)]
pub extern "C" fn ntask_go(poll_fn: AsyncPollFn, future: i64) -> i64 {
    spawn_goroutine(poll_fn, future, None)
}

/// Spawn a goroutine that owns a heap-allocated future: `cleanup` runs the
/// future's drop and frees it if the goroutine never completes, so a `go`
/// still parked when the program ends does not leak the handles its future
/// holds.
#[unsafe(no_mangle)]
pub extern "C" fn ntask_go_owned(
    poll_fn: AsyncPollFn,
    future: i64,
    cleanup: AsyncCleanupFn,
) -> i64 {
    spawn_goroutine(poll_fn, future, Some(cleanup))
}

/// Spawn a goroutine whose handle the caller discards (`go f(x)`). No registry
/// wrapper is created: nothing can join or drop it, so the id would only be
/// allocated to be reclaimed. `cleanup` reclaims the future if it never
/// completes.
#[unsafe(no_mangle)]
pub extern "C" fn ntask_go_detached(poll_fn: AsyncPollFn, future: i64, cleanup: AsyncCleanupFn) {
    ntask::scheduler::spawn_runnable(poll_fn, future, Some(cleanup), NULL);
}

fn spawn_goroutine(poll_fn: AsyncPollFn, future: i64, cleanup: Option<AsyncCleanupFn>) -> i64 {
    // Reserve the wrapper id first so the goroutine can record it: a child
    // that finishes on its first poll must find its wrapper.
    let handle = registry::reserve_id();
    let core = ntask::scheduler::register(poll_fn, future, cleanup, handle);
    registry::insert_reserved(handle, registry::Handle::Goroutine { core });
    ntask::scheduler::make_runnable(core);
    handle
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_join(goroutine: i64) -> i64 {
    let Some(core) = registry::task_core(goroutine) else {
        return NULL;
    };
    ntask::scheduler::join_blocking(core)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_join_park(goroutine: i64) -> i8 {
    let Some(core) = registry::task_core(goroutine) else {
        return 1;
    };
    ntask::scheduler::park_join(core);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_goroutine_drop(goroutine: i64) {
    registry::goroutine_drop(goroutine);
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_chan_new(capacity: i64, owns_elements: i8) -> i64 {
    let core = ntask::scheduler::register_chan(capacity.max(0) as usize, owns_elements != 0);
    registry::insert(registry::Handle::Chan { core, count: 1 })
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_chan_retain(channel: i64) -> i64 {
    registry::chan_retain(channel)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_chan_send(channel: i64, value: i64) -> i8 {
    let Some(core) = registry::task_core(channel) else {
        return 1;
    };
    ntask::scheduler::park_chan_send(core, value);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_chan_recv(channel: i64) -> i8 {
    let Some(core) = registry::task_core(channel) else {
        return 1;
    };
    ntask::scheduler::park_chan_recv(core);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_chan_recv_result() -> i64 {
    ntask::scheduler::recv_result()
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_chan_recv_ok() -> i8 {
    ntask::scheduler::recv_ok().into()
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_chan_close(channel: i64) {
    if let Some(core) = registry::task_core(channel) {
        ntask::scheduler::chan_close(core);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_chan_drop(channel: i64) {
    registry::chan_drop(channel);
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_timer_new() -> i64 {
    let core = ntask::scheduler::register_io();
    registry::insert(registry::Handle::ReactorReg { core })
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_timer_park(timer: i64, deadline_ms: i64) -> i8 {
    let Some(core) = registry::task_core(timer) else {
        return 1;
    };
    ntask::scheduler::park_timer(deadline_ms);
    let _ = core;
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_reactor_drop(registration: i64) {
    registry::reactor_reg_drop(registration);
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_io_new() -> i64 {
    let core = ntask::scheduler::register_io();
    registry::insert(registry::Handle::AsyncIo { core })
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_io_attach(registration: i64, fd: i64, read: i8) {
    if let Some(core) = registry::task_core(registration) {
        ntask::reactor::attach_fd(core, fd, read != 0);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_io_park(registration: i64, read: i8) -> i8 {
    let Some(core) = registry::task_core(registration) else {
        return 1;
    };
    ntask::scheduler::park_fd(core, read != 0);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_io_ready(registration: i64) -> i8 {
    registry::task_core(registration)
        .is_some_and(ntask::scheduler::io_ready)
        .into()
}

#[unsafe(no_mangle)]
pub extern "C" fn ntask_io_drop(registration: i64) {
    registry::async_io_drop(registration);
}

/// Save the current async context and start a fresh one.
///
/// Pushes the current `(ASYNC_TASKS, result)` onto `ASYNC_STACKS` and
/// replaces `ASYNC_TASKS` with an empty vec so `ntsc_async_push` and
/// `ntsc_async_run` operate on a local context. Returns the previous
/// result handle (NULL if this is the first nesting level).
fn save_async_context() -> i64 {
    ASYNC_STACKS.with(|stacks| {
        ASYNC_TASKS.with(|tasks| {
            let prev_tasks = tasks.borrow().clone();
            let prev_result =
                ASYNC_STACKS.with(|s| s.borrow().last().map(|(_, r)| *r).unwrap_or(NULL));
            stacks.borrow_mut().push((prev_tasks, prev_result));
            tasks.borrow_mut().clear();
            prev_result
        })
    })
}

/// Restore a previously saved async context, returning the current
/// result handle.
fn restore_async_context(result: i64) -> i64 {
    ASYNC_STACKS.with(|stacks| {
        ASYNC_TASKS.with(|tasks| {
            if let Some((saved, _)) = stacks.borrow_mut().pop() {
                *tasks.borrow_mut() = saved;
            }
            let current_result = stacks.borrow().last().map(|(_, r)| *r).unwrap_or(NULL);
            let _ = result;
            current_result
        })
    })
}

/// Drive the root future to completion in a fresh async context.
///
/// The root's poll function and handle are pushed onto the thread-local
/// task stack; the driver repeatedly polls the topmost task. A task that
/// returns `0` without pushing a sub-future is waiting on time alone
/// (e.g. `sleep`); the driver waits a 1 ms quantum before re-polling it.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_run(poll_fn: AsyncPollFn, root: i64) {
    if root == NULL {
        return;
    }
    if std::env::var_os("NTSC_LEGACY_ASYNC").is_none() {
        let goroutine = ntask_go(poll_fn, root);
        let _ = ntask_join(goroutine);
        ntask_goroutine_drop(goroutine);
        return;
    }
    save_async_context();
    ASYNC_TASKS.with(|tasks| {
        tasks.borrow_mut().push((poll_fn, root));
        loop {
            let (poll, future, depth_before) = {
                let tasks = tasks.borrow();
                let Some(&(poll, future)) = tasks.last() else {
                    return;
                };
                (poll, future, tasks.len())
            };
            let done = poll(future) == 1;
            let pushed_child = {
                let mut tasks = tasks.borrow_mut();
                let pushed_child = tasks.len() > depth_before;
                if done {
                    tasks.pop();
                }
                pushed_child
            };
            if done {
                continue;
            }
            if !pushed_child {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    });
}

/// Schedule a sub-future: push `(poll_fn, future)` onto the current task
/// stack. Called by a poll function right before it returns `0` at a
/// suspension point, so the driver polls the sub-future next.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_push(poll_fn: AsyncPollFn, future: i64) {
    if future == NULL {
        return;
    }
    ASYNC_TASKS.with(|tasks| {
        tasks.borrow_mut().push((poll_fn, future));
    });
}

/// Run two async branches concurrently; return the result of whichever
/// finishes first. The losing branch is dropped automatically.
///
/// Called from within a poll function. Each branch is pushed onto its own
/// local task stack and polled round-robin until one completes. Returns
/// the winning future handle (can be used to read the result slot).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_wait_any(
    poll_a: AsyncPollFn,
    future_a: i64,
    poll_b: AsyncPollFn,
    future_b: i64,
) -> i64 {
    if future_a == NULL && future_b == NULL {
        return NULL;
    }
    if future_a == NULL || future_b == NULL {
        let (poll, fut) = if future_a != NULL {
            (poll_a, future_a)
        } else {
            (poll_b, future_b)
        };
        ASYNC_TASKS.with(|tasks| {
            tasks.borrow_mut().push((poll, fut));
        });
        return fut;
    }
    let _ = save_async_context();
    ASYNC_TASKS.with(|tasks| {
        tasks.borrow_mut().push((poll_a, future_a));
    });
    ASYNC_STACKS.with(|stacks| {
        stacks.borrow_mut().push((vec![(poll_b, future_b)], NULL));
    });
    let result = loop {
        let (poll, fut, len_a) = ASYNC_TASKS.with(|tasks| {
            let t = tasks.borrow();
            let Some(&(p, f)) = t.last() else {
                return (None, NULL, 0);
            };
            (Some(p), f, t.len())
        });
        let Some(poll_fn) = poll else {
            break NULL;
        };
        let done_a = poll_fn(fut) == 1;
        let (pushed_a, done_a_final) = ASYNC_TASKS.with(|tasks| {
            let mut t = tasks.borrow_mut();
            let pushed = t.len() > len_a;
            if done_a {
                t.pop();
            }
            (pushed, done_a)
        });
        if done_a_final {
            break future_a;
        }
        let (poll_b, fut_b, len_b) = ASYNC_STACKS.with(|stacks| {
            let mut s = stacks.borrow_mut();
            let Some(last) = s.last_mut() else {
                return (None, NULL, 0);
            };
            let Some(&(p, f)) = last.0.last() else {
                return (None, NULL, 0);
            };
            let len = last.0.len();
            (Some(p), f, len)
        });
        let Some(poll_b_fn) = poll_b else {
            break NULL;
        };
        let done_b = poll_b_fn(fut_b) == 1;
        let pushed_b = ASYNC_STACKS.with(|stacks| {
            let mut s = stacks.borrow_mut();
            let Some(last) = s.last_mut() else {
                return false;
            };
            let pushed = last.0.len() > len_b;
            if done_b {
                last.0.pop();
            }
            pushed
        });
        if done_b {
            break future_b;
        }
        if !pushed_a && !pushed_b {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let _ = restore_async_context(NULL);
    result
}

/// Run two async branches concurrently; wait for both to finish and
/// return the handle of the second branch.
///
/// Called from within a poll function. Each branch is pushed onto its own
/// local task stack and polled round-robin until both complete.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_wait_all(
    poll_a: AsyncPollFn,
    future_a: i64,
    poll_b: AsyncPollFn,
    future_b: i64,
) -> i64 {
    if future_a == NULL && future_b == NULL {
        return NULL;
    }
    let _ = save_async_context();
    if future_a != NULL {
        ASYNC_TASKS.with(|tasks| {
            tasks.borrow_mut().push((poll_a, future_a));
        });
    }
    if future_b != NULL {
        ASYNC_STACKS.with(|stacks| {
            stacks.borrow_mut().push((vec![(poll_b, future_b)], NULL));
        });
    }
    let mut done_a = future_a == NULL;
    let mut done_b = future_b == NULL;
    while !done_a || !done_b {
        if !done_a {
            let (poll, fut, len_a) = ASYNC_TASKS.with(|tasks| {
                let t = tasks.borrow();
                let Some(&(p, f)) = t.last() else {
                    return (None, NULL, 0);
                };
                (Some(p), f, t.len())
            });
            if let Some(poll_fn) = poll {
                let d = poll_fn(fut) == 1;
                let pushed = ASYNC_TASKS.with(|tasks| {
                    let mut t = tasks.borrow_mut();
                    let pushed = t.len() > len_a;
                    if d {
                        t.pop();
                    }
                    pushed
                });
                done_a = d && !pushed;
            } else {
                done_a = true;
            }
        }
        if !done_b {
            let (poll_b, fut_b, len_b) = ASYNC_STACKS.with(|stacks| {
                let mut s = stacks.borrow_mut();
                let Some(last) = s.last_mut() else {
                    return (None, NULL, 0);
                };
                let Some(&(p, f)) = last.0.last() else {
                    return (None, NULL, 0);
                };
                let len = last.0.len();
                (Some(p), f, len)
            });
            if let Some(poll_b_fn) = poll_b {
                let d = poll_b_fn(fut_b) == 1;
                let pushed = ASYNC_STACKS.with(|stacks| {
                    let mut s = stacks.borrow_mut();
                    let Some(last) = s.last_mut() else {
                        return false;
                    };
                    let pushed = last.0.len() > len_b;
                    if d {
                        last.0.pop();
                    }
                    pushed
                });
                if d && !pushed {
                    done_b = true;
                }
            } else {
                done_b = true;
            }
        }
        if done_a && done_b {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let _ = restore_async_context(NULL);
    future_b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_op_result_reraises_worker_exceptions_on_this_thread() {
        // The work fn mimics the generated async-bridge wrapper: the sync
        // call throws (parking its message in this thread's pending slot),
        // the wrapper takes it and completes the op with the negated
        // handle.
        extern "C" fn work(id: i64) -> i64 {
            let message = registry::put_string(format!("boom-{id}"));
            ntsc_throw(message);
            let pending = ntsc_exception_pending();
            assert_eq!(pending, 1);
            let taken = ntsc_exception_take_message();
            -taken
        }

        let id = ntsc_async_op_new(work, 7);
        // The first poll starts the work on the offload pool.
        let _ = ntsc_async_op_poll(id);
        let mut ready = 0;
        for _ in 0..5_000 {
            if ntsc_async_op_poll(id) == 1 {
                ready = 1;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(ready, 1, "offloaded work must complete");

        // Reaping re-raises the exception on the calling thread instead of
        // returning the encoded value.
        assert_eq!(ntsc_exception_pending(), 0);
        let value = ntsc_async_op_result(id);
        assert_eq!(value, NULL);
        assert_eq!(ntsc_exception_pending(), 1);
        let message = ntsc_exception_take_message();
        assert_eq!(
            registry::with_string(message, str::to_string),
            Some("boom-7".to_string())
        );
        registry::take_string(message);
        ntsc_async_op_drop(id);
    }

    #[test]
    fn string_handles_balance_and_null_is_a_noop() {
        let s = ntsc_i64_to_string(42);
        assert_ne!(s, NULL);
        assert_eq!(
            registry::with_string(s, str::to_string),
            Some("42".to_string())
        );
        let cloned = ntsc_string_clone(s);
        assert_eq!(
            registry::with_string(cloned, str::to_string),
            Some("42".to_string())
        );
        ntsc_string_drop(cloned);
        ntsc_string_drop(NULL);
        ntsc_say(NULL);
        ntsc_string_clone(NULL);
        ntsc_string_drop(s);
        assert_eq!(registry::with_string(s, str::to_string), None);
        assert!(registry::clone_string(s).is_none());
    }

    #[test]
    fn string_from_words_reassembles_bytes_and_consumes_words() {
        let words = ntsc_array_new(8, 2);
        ntsc_array_push(words, u64::from_ne_bytes(*b"Hello\0\0\0") as i64);
        ntsc_array_push(words, 0);
        let s = ntsc_string_from_words(words, 5);
        assert_eq!(
            registry::with_string(s, str::to_string),
            Some("Hello".to_string())
        );

        assert_eq!(registry::with_array(words, |_| true), None);
        ntsc_string_drop(s);
    }

    #[test]
    fn concat_equals_join_results() {
        let a = ntsc_i64_to_string(1);
        let b = ntsc_i64_to_string(23);
        let joined = ntsc_string_concat(a, b);
        let expected = ntsc_string_concat(ntsc_i64_to_string(12), ntsc_i64_to_string(3));
        assert!(registry::string_equals(joined, expected));
        assert_eq!(ntsc_string_equals(joined, expected), 1);
        ntsc_string_drop(a);
        ntsc_string_drop(b);
        ntsc_string_drop(joined);
        ntsc_string_drop(expected);
    }

    #[test]
    fn array_ops_round_trip_scalars_and_strings() {
        let arr = ntsc_array_new(8, 0);
        assert_eq!(ntsc_array_len(arr), 0);
        ntsc_array_push(arr, 10);
        ntsc_array_push(arr, 20);
        assert_eq!(ntsc_array_len(arr), 2);
        assert_eq!(ntsc_array_get(arr, 0), 10);
        assert_eq!(ntsc_array_get(arr, 1), 20);
        assert_eq!(ntsc_array_pop(arr), 20);
        assert_eq!(ntsc_array_len(arr), 1);

        let strings = ntsc_array_new_typed(8, 0, 1);
        let s1 = ntsc_i64_to_string(7);
        let s2 = ntsc_i64_to_string(8);
        ntsc_array_push(strings, s1);
        ntsc_array_push(strings, s2);

        assert_eq!(
            registry::with_string(s1, str::to_string),
            Some("7".to_string())
        );
        let taken = ntsc_array_pop(strings);
        assert_eq!(
            registry::with_string(taken, str::to_string),
            Some("8".to_string())
        );
        ntsc_string_drop(taken);
        ntsc_array_drop(strings);

        assert_eq!(ntsc_array_len(strings), 0);
        ntsc_array_drop(arr);
        ntsc_string_drop(s1);
        ntsc_string_drop(s2);
    }

    #[test]
    fn array_is_array_distinguishes_kinds() {
        // Generated collection bridges guard on this before decoding, so a
        // wrong-kind handle throws a precise error instead of reading as
        // an empty collection.
        assert_eq!(ntsc_array_is_array(NULL), 0);
        let s = ntsc_i64_to_string(1);
        assert_eq!(ntsc_array_is_array(s), 0);
        let arr = ntsc_array_new(8, 0);
        assert_eq!(ntsc_array_is_array(arr), 1);
        ntsc_array_drop(arr);
        assert_eq!(ntsc_array_is_array(arr), 0);
        ntsc_string_drop(s);
    }

    #[test]
    fn array_get_out_of_bounds_throws() {
        let arr = ntsc_array_new(8, 0);
        ntsc_array_push(arr, 1);
        let value = ntsc_array_get(arr, 99);
        assert_eq!(value, NULL);
        assert_eq!(ntsc_exception_pending(), 1);
        let message = ntsc_exception_take_message();
        assert_eq!(
            registry::with_string(message, str::to_string),
            Some("array index out of bounds".to_string())
        );
        ntsc_string_drop(message);
        assert_eq!(ntsc_exception_pending(), 0);
        ntsc_array_drop(arr);
    }

    #[test]
    fn shared_box_retains_and_releases() {
        let s = ntsc_i64_to_string(5);
        let boxed = ntsc_shared_new(s);
        assert_ne!(boxed, NULL);

        assert_eq!(ntsc_shared_retain(boxed), boxed);
        assert_eq!(ntsc_shared_release(boxed), NULL);
        let inner = ntsc_shared_release(boxed);
        assert_ne!(inner, NULL);
        assert_eq!(
            registry::with_string(inner, str::to_string),
            Some("5".to_string())
        );
        ntsc_string_drop(inner);
    }

    #[test]
    fn shared_inner_borrows_the_wrapped_value() {
        let s = ntsc_i64_to_string(9);
        let boxed = ntsc_shared_new(s);
        let inner = ntsc_shared_inner(boxed);
        assert_eq!(
            registry::with_string(inner, str::to_string),
            Some("9".to_string())
        );

        let released = ntsc_shared_release(boxed);
        assert_eq!(released, inner);
        ntsc_string_drop(released);
    }

    #[test]
    fn throw_then_take_round_trips() {
        assert_eq!(ntsc_exception_pending(), 0);
        let msg = ntsc_i64_to_string(123);
        ntsc_throw(msg);
        assert_eq!(ntsc_exception_pending(), 1);
        assert_eq!(ntsc_exception_is_active(), 1);
        let borrowed = ntsc_exception_get_message();
        assert_eq!(
            registry::with_string(borrowed, str::to_string),
            Some("123".to_string())
        );
        let taken = ntsc_exception_take_message();
        assert_eq!(taken, borrowed);
        assert_eq!(ntsc_exception_pending(), 0);
        ntsc_string_drop(taken);
    }

    #[test]
    fn replacing_a_pending_exception_reclaims_the_old_message() {
        ntsc_exception_clear();
        let first = ntsc_i64_to_string(1);
        let second = ntsc_i64_to_string(2);
        ntsc_throw(first);
        ntsc_throw(second);
        assert_eq!(registry::get_string(first), None);
        assert_eq!(registry::get_string(second), Some("2".to_string()));
        ntsc_exception_clear();
        assert_eq!(registry::get_string(second), None);
    }

    #[test]
    fn rethrow_and_clear_keep_balance() {
        ntsc_throw(ntsc_i64_to_string(1));
        let taken = ntsc_exception_take_message();
        ntsc_rethrow(taken);
        assert_eq!(ntsc_exception_pending(), 1);
        ntsc_exception_clear();
        assert_eq!(ntsc_exception_pending(), 0);
    }

    #[test]
    /// The executor drives a parent future that awaits a 20 ms sleep
    /// child: first poll arms the child and pushes it; once the child
    /// completes the parent is re-polled and finishes. All state lives in
    /// the registry.
    fn executor_drives_a_sleeping_parent_to_completion() {
        struct Parent {
            state: i32,
            child: i64,
            result: i32,
        }
        extern "C" fn parent_poll(id: i64) -> i8 {
            let state = registry::with_opaque(id, |parent: &Parent| parent.state).unwrap_or(2);
            match state {
                0 => {
                    let child = ntsc_async_sleep_new(20);
                    ntsc_async_push(ntsc_async_sleep_poll, child);

                    // Registry calls must not run inside an opaque closure,
                    // so the child handle is stored only after the lock is
                    // free.
                    registry::with_opaque_mut(id, |parent: &mut Parent| {
                        parent.state = 1;
                        parent.child = child;
                    });
                    0
                }
                1 => {
                    let child =
                        registry::with_opaque(id, |parent: &Parent| parent.child).unwrap_or(NULL);
                    ntsc_async_sleep_drop(child);
                    registry::with_opaque_mut(id, |parent: &mut Parent| {
                        parent.result = 42;
                    });
                    1
                }
                _ => 1,
            }
        }
        let started = std::time::Instant::now();
        let root = registry::put_opaque(Parent {
            state: 0,
            child: NULL,
            result: 0,
        });
        ntsc_async_run(parent_poll, root);
        assert!(started.elapsed().as_millis() >= 15);
        let parent = registry::take_opaque::<Parent>(root).expect("parent future");
        assert_eq!(parent.state, 1);
        assert_eq!(parent.result, 42);
    }

    #[test]
    fn executor_runs_an_immediately_completing_future() {
        extern "C" fn done_poll(id: i64) -> i8 {
            registry::with_opaque_mut(id, |state: &mut i32| {
                *state = 7;
                1
            })
            .unwrap_or(1)
        }
        let root = registry::put_opaque(0i32);
        ntsc_async_run(done_poll, root);
        let state = registry::take_opaque::<i32>(root).expect("state");
        assert_eq!(state, 7);
    }

    #[test]
    fn sleep_future_transitions_through_its_states() {
        let sleep = ntsc_async_sleep_new(5);
        assert_eq!(ntsc_async_sleep_poll(sleep), 0);
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(ntsc_async_sleep_poll(sleep), 1);
        assert_eq!(ntsc_async_sleep_poll(sleep), 1);
        ntsc_async_sleep_drop(sleep);
    }

    #[test]
    fn virtual_goroutine_runs_and_kind_checked_drop() {
        extern "C" fn mark_done(id: i64) -> i8 {
            registry::with_opaque_mut(id, |value: &mut i64| *value = 42)
                .map(|_| 1)
                .unwrap_or(1)
        }
        let state = registry::put_opaque(0i64);
        let task = ntask_go(mark_done, state);
        assert_ne!(task, NULL);
        // Completion may race the caller, but a joinable handle must remain
        // addressable until the explicit drop below.
        std::thread::yield_now();
        assert!(registry::task_core(task).is_some());
        let _ = ntask_join(task);
        assert!(registry::task_core(task).is_some());
        assert_eq!(registry::with_opaque(state, |value: &i64| *value), Some(42));
        ntask_goroutine_drop(task);
        ntask_goroutine_drop(task);
        let _ = registry::take_opaque::<i64>(state);
    }

    #[test]
    fn channel_handle_lifecycle_is_kind_checked() {
        let channel = ntask_chan_new(2, 0);
        assert_ne!(channel, NULL);
        ntask_chan_close(channel);
        ntask_chan_drop(channel);
        ntask_chan_drop(channel);
    }

    #[test]
    fn offload_future_parks_goroutine_until_pool_finishes() {
        // Foreign-future await pattern: the goroutine polls the offloaded
        // future; while the job runs on the pool its worker is parked and
        // gives way to other work, then resumes when the pool finishes.
        use super::modules::process::{
            ntsc_async_process_exec, ntsc_async_process_exec_drop, ntsc_async_process_exec_poll,
            ntsc_async_process_exec_result,
        };

        extern "C" fn drive_offload(id: i64) -> i8 {
            let fut = registry::with_opaque(id, |s: &(i64, i64)| s.0).unwrap();
            if ntsc_async_process_exec_poll(fut) == 1 {
                let code = ntsc_async_process_exec_result(fut);
                registry::with_opaque_mut(id, |s: &mut (i64, i64)| *s = (fut, code));
                ntsc_async_process_exec_drop(fut);
                1
            } else {
                0
            }
        }

        let fut = ntsc_async_process_exec(registry::put_string("exit 0".to_string()));
        let state = registry::put_opaque((fut, 0i64));
        let task = ntask_go(drive_offload, state);
        assert_ne!(task, NULL);
        let _ = ntask_join(task);
        let (_, code) = registry::with_opaque(state, |s: &(i64, i64)| *s).unwrap();
        assert_eq!(code, 0);
        ntask_goroutine_drop(task);
        let _ = registry::take_opaque::<(i64, i64)>(state);
    }

    #[test]
    fn reactor_handle_lifecycle_is_kind_checked() {
        let timer = ntask_timer_new();
        let io = ntask_io_new();
        assert_ne!(timer, NULL);
        assert_ne!(io, NULL);
        ntask_io_attach(io, -1, 1);
        ntask_reactor_drop(timer);
        ntask_io_drop(io);
        ntask_reactor_drop(timer);
        ntask_io_drop(io);
    }

    #[test]
    fn blocked_channel_sender_and_receiver_are_unparked() {
        #[derive(Clone, Copy)]
        struct State {
            channel: i64,
            value: i64,
            state: i8,
        }

        extern "C" fn receive(id: i64) -> i8 {
            let Some((channel, state)) =
                registry::with_opaque(id, |s: &State| (s.channel, s.state))
            else {
                return 1;
            };
            if state == 0 {
                ntask_chan_recv(channel);
                registry::with_opaque_mut(id, |s: &mut State| s.state = 1);
                return 0;
            }
            let value = ntask_chan_recv_result();
            registry::with_opaque_mut(id, |s: &mut State| {
                s.value = value;
                s.state = 2;
            });
            1
        }

        extern "C" fn send(id: i64) -> i8 {
            let Some((channel, value, state)) =
                registry::with_opaque(id, |s: &State| (s.channel, s.value, s.state))
            else {
                return 1;
            };
            if state == 0 {
                ntask_chan_send(channel, value);
                registry::with_opaque_mut(id, |s: &mut State| s.state = 1);
                return 0;
            }
            1
        }

        let channel = ntask_chan_new(0, 0);
        let receiver_state = registry::put_opaque(State {
            channel,
            value: 0,
            state: 0,
        });
        let sender_state = registry::put_opaque(State {
            channel,
            value: 77,
            state: 0,
        });
        let receiver = ntask_go(receive, receiver_state);
        let sender = ntask_go(send, sender_state);
        let _ = ntask_join(receiver);
        let _ = ntask_join(sender);
        let receiver_state = registry::take_opaque::<State>(receiver_state).unwrap_or(State {
            channel,
            value: 0,
            state: 0,
        });
        assert_eq!(receiver_state.value, 77);
        ntask_goroutine_drop(receiver);
        ntask_goroutine_drop(sender);
        let _ = registry::take_opaque::<State>(sender_state);
        ntask_chan_drop(channel);
    }

    #[test]
    fn scheduler_stress_handles_concurrent_channel_handoffs() {
        #[derive(Clone, Copy)]
        struct State {
            channel: i64,
            value: i64,
            received: i64,
            phase: i8,
        }

        extern "C" fn receive(id: i64) -> i8 {
            let Some((channel, phase)) =
                registry::with_opaque(id, |state: &State| (state.channel, state.phase))
            else {
                return 1;
            };
            if phase == 0 {
                ntask_chan_recv(channel);
                let _ = registry::with_opaque_mut(id, |state: &mut State| state.phase = 1);
                return 0;
            }
            let value = ntask_chan_recv_result();
            let _ = registry::with_opaque_mut(id, |state: &mut State| {
                state.received = value;
                state.phase = 2;
            });
            1
        }

        extern "C" fn send(id: i64) -> i8 {
            let Some((channel, value, phase)) = registry::with_opaque(id, |state: &State| {
                (state.channel, state.value, state.phase)
            }) else {
                return 1;
            };
            if phase == 0 {
                ntask_chan_send(channel, value);
                let _ = registry::with_opaque_mut(id, |state: &mut State| state.phase = 1);
                return 0;
            }
            1
        }

        const PAIRS: usize = 128;
        const ROUNDS: usize = 4;
        for round in 0..ROUNDS {
            let mut tasks = Vec::with_capacity(PAIRS * 2);
            let mut states = Vec::with_capacity(PAIRS);
            for pair in 0..PAIRS {
                let channel = ntask_chan_new(0, 0);
                let receiver_state = registry::put_opaque(State {
                    channel,
                    value: 0,
                    received: 0,
                    phase: 0,
                });
                let sender_state = registry::put_opaque(State {
                    channel,
                    value: (round * PAIRS + pair) as i64,
                    received: 0,
                    phase: 0,
                });
                states.push((channel, receiver_state, sender_state));
                tasks.push((ntask_go(receive, receiver_state), receiver_state));
                tasks.push((ntask_go(send, sender_state), sender_state));
            }

            for (task, _) in &tasks {
                let _ = ntask_join(*task);
            }
            for (task, _) in tasks {
                ntask_goroutine_drop(task);
            }
            for (channel, receiver_state, sender_state) in states {
                let received =
                    registry::with_opaque(receiver_state, |state: &State| state.received)
                        .expect("receiver state");
                let expected = registry::with_opaque(sender_state, |state: &State| state.value)
                    .expect("sender state");
                assert_eq!(received, expected);
                let _ = registry::take_opaque::<State>(receiver_state);
                let _ = registry::take_opaque::<State>(sender_state);
                ntask_chan_drop(channel);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn reactor_wakes_a_goroutine_on_loopback_readiness() {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;

        struct State {
            io: i64,
            polls: i8,
        }

        extern "C" fn wait_readable(id: i64) -> i8 {
            let Some((io, polls)) =
                registry::with_opaque(id, |state: &State| (state.io, state.polls))
            else {
                return 1;
            };
            if polls == 0 {
                ntask_io_park(io, 1);
                registry::with_opaque_mut(id, |state: &mut State| state.polls = 1);
                return 0;
            }
            if ntask_io_ready(io) != 0 {
                registry::with_opaque_mut(id, |state: &mut State| state.polls = 2);
                return 1;
            }
            ntask_io_park(io, 1);
            0
        }

        let (mut reader, mut writer) = UnixStream::pair().unwrap_or_else(|_| panic!("unix pair"));
        let io = ntask_io_new();
        ntask_io_attach(io, reader.as_raw_fd() as i64, 1);
        let state = registry::put_opaque(State { io, polls: 0 });
        let task = ntask_go(wait_readable, state);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let _ = writer.write_all(b"x");
        });
        let _ = ntask_join(task);
        let state = registry::take_opaque::<State>(state).unwrap_or(State { io, polls: 0 });
        assert_eq!(state.polls, 2);
        let mut byte = [0u8; 1];
        let _ = std::io::Read::read_exact(&mut reader, &mut byte);
        ntask_goroutine_drop(task);
        ntask_io_drop(io);
    }

    #[test]
    fn a_goroutine_that_throws_propagates_its_exception_to_the_joiner() {
        // Regression: an uncaught throw raised by an async poll runs on a
        // scheduler worker, so its pending exception lived only on that
        // worker's TLS. `ntask_join` must re-raise it on the joining thread,
        // or the caller's `ntsc_runtime_shutdown` would never see it and the
        // throw would be silently swallowed.
        extern "C" fn throw_boom(_id: i64) -> i8 {
            let message = registry::put_string("boom".to_string());
            ntsc_throw(message);
            1
        }
        assert_eq!(ntsc_exception_pending(), 0);
        let task = ntask_go(throw_boom, NULL);
        let _ = ntask_join(task);
        assert_eq!(
            ntsc_exception_pending(),
            1,
            "join must re-raise on the caller"
        );
        let message = ntsc_exception_take_message();
        let text = registry::get_string(message).unwrap_or_default();
        ntsc_string_drop(message);
        assert_eq!(text, "boom");
        ntask_goroutine_drop(task);
    }

    #[test]
    fn array_set_replaces_elements_and_reclaims_strings() {
        // Scalar arrays replace raw values; the caller's value is
        // untouched. Out-of-bounds index is a no-op.
        let ints = ntsc_array_new(8, 3);
        ntsc_array_push(ints, 1);
        ntsc_array_push(ints, 2);
        ntsc_array_push(ints, 3);
        assert_eq!(ntsc_array_set(ints, 1, 9), ints);
        assert_eq!(registry::with_array(ints, |a| a.elements[1]), Some(9));

        assert_eq!(ntsc_array_set(ints, 7, 0), NULL);
        assert_eq!(ntsc_array_set(ints, -1, 0), NULL);
        ntsc_array_drop(ints);

        // String arrays deep-copy the new value and reclaim the old
        // element without deadlocking or freeing the caller's handle.
        let strs = ntsc_array_new_typed(8, 2, 1);
        let src = ntsc_i64_to_string(5);
        ntsc_array_push(strs, src);
        ntsc_array_push(strs, ntsc_i64_to_string(7));
        let fresh = ntsc_i64_to_string(9);
        assert_eq!(ntsc_array_set(strs, 0, fresh), strs);

        assert_eq!(
            registry::with_string(src, str::to_string),
            Some("5".to_string())
        );
        assert_eq!(
            registry::with_string(fresh, str::to_string),
            Some("9".to_string())
        );
        // The source handles remain owned by the caller.
        let elements = registry::with_array(strs, |a| a.elements.clone()).unwrap();
        assert_ne!(elements[0], fresh);
        assert_eq!(
            registry::with_string(elements[0], str::to_string),
            Some("9".to_string())
        );
        assert_eq!(
            registry::with_string(elements[1], str::to_string),
            Some("7".to_string())
        );
        ntsc_string_drop(src);
        ntsc_string_drop(fresh);
        ntsc_array_drop(strs);
    }
}
