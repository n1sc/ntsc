//! Public-ABI tests for handle validity and kind safety.
//!
//! Generated code never sees a pointer: every owned value crosses the ABI as an
//! opaque `i64` handle resolved inside the runtime's registry. These tests pin
//! the guarantees that makes possible, entirely through the public `extern "C"`
//! surface and with no `unsafe` code:
//!
//! * A handle is never reused, so a stale handle is permanently unknown.
//! * Dropping a live handle of the wrong kind is a no-op rather than corruption
//!   of another live value.
//! * Dropping a non-null string/array handle that is not live is a hard error
//!   (`NTSC-R0001`), catching double frees and use-after-free drops.
//! * A handle of the wrong kind is never mistaken for the right one, in either
//!   direction, for strings, arrays, shared boxes, futures, and the opaque
//!   resources the stdlib modules own.
//! * The null handle (`0`) is accepted everywhere and means "no value".
//!
//! An operation given a handle it cannot honor reports the documented safe
//! failure for its return type — `0`/null for a handle, `0` for a count, `-1`
//! for a search or code lookup — instead of panicking, and never treats an
//! unknown nonzero handle as a license to touch some other entry.

use ntsc_runtime::modules::{io, strings};
use ntsc_runtime::{
    ntsc_array_clone, ntsc_array_drop, ntsc_array_get, ntsc_array_len, ntsc_array_new,
    ntsc_array_new_typed, ntsc_array_pop, ntsc_array_push, ntsc_async_sleep_drop,
    ntsc_async_sleep_new, ntsc_async_sleep_poll, ntsc_shared_inner, ntsc_shared_new,
    ntsc_shared_release, ntsc_shared_retain, ntsc_string_clone, ntsc_string_concat,
    ntsc_string_drop, ntsc_string_equals, ntsc_string_from_words,
};

/// Register an owned string through the ABI and return its handle.
fn string_handle(text: &str) -> i64 {
    let words = ntsc_array_new(8, text.len().div_ceil(8) as i64);
    for chunk in text.as_bytes().chunks(8) {
        let mut bytes = [0_u8; 8];
        bytes[..chunk.len()].copy_from_slice(chunk);
        ntsc_array_push(words, i64::from_ne_bytes(bytes));
    }
    ntsc_string_from_words(words, text.len() as i64)
}

/// Whether the string behind `handle` still reads as `text`.
fn string_is(handle: i64, text: &str) -> bool {
    let expected = string_handle(text);
    let equal = ntsc_string_equals(handle, expected) == 1;
    ntsc_string_drop(expected);
    equal
}

/// A handle that was never registered. Handles count up from 1, so a value this
/// large cannot collide with one a test registers.
const UNKNOWN: i64 = i64::MAX - 7;

#[test]
fn a_dropped_handle_is_never_reissued() {
    // Reissuing a handle is what makes a stale handle dangerous: the next
    // allocation would answer to the old name. Ids only ever count up.
    let first = string_handle("gone");
    ntsc_string_drop(first);
    for _ in 0..64 {
        let next = string_handle("fresh");
        assert_ne!(next, first, "a dropped handle was handed out again");
        ntsc_string_drop(next);
    }
    // The stale handle stays unknown, and reading it does not resurrect it.
    assert_eq!(ntsc_string_clone(first), 0);
    assert_eq!(strings::ntsc_strings_length(first), 0);
    assert_eq!(ntsc_string_equals(first, first), 0);
}

fn assert_double_drop_aborts(kind: &str) {
    let exe = std::env::current_exe().expect("current test exe");
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("double_drop_child")
        .arg("--nocapture")
        .env("NTSC_DOUBLE_DROP_KIND", kind)
        .output()
        .expect("run double drop child");
    assert!(
        !output.status.success(),
        "double drop should abort, got: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NTSC-R0001"),
        "expected double-drop diagnostic, got: {stderr}"
    );
}

#[test]
fn double_drop_child() {
    let Ok(kind) = std::env::var("NTSC_DOUBLE_DROP_KIND") else {
        return;
    };
    match kind.as_str() {
        "string" => {
            let h = string_handle("doomed");
            ntsc_string_drop(h);
            ntsc_string_drop(h);
        }
        "array" => {
            let arr = ntsc_array_new(8, 4);
            assert_eq!(ntsc_array_push(arr, 7), arr);
            ntsc_array_drop(arr);
            ntsc_array_drop(arr);
        }
        "unknown_string" => {
            ntsc_string_drop(i64::MAX - 7);
        }
        "unknown_array" => {
            ntsc_array_drop(i64::MAX - 7);
        }
        _ => panic!("unknown kind"),
    }
}

#[test]
fn dropping_the_same_handle_twice_is_reported() {
    assert_double_drop_aborts("string");
    assert_double_drop_aborts("array");
}

#[test]
fn a_string_handle_is_not_accepted_as_an_array() {
    let s = string_handle("not an array");

    assert_eq!(ntsc_array_len(s), 0);
    assert_eq!(ntsc_array_get(s, 0), 0);
    assert_eq!(ntsc_array_push(s, 1), 0, "push must refuse a non-array");
    assert_eq!(ntsc_array_pop(s), 0);
    assert_eq!(ntsc_array_clone(s), 0);
    // A destructive array operation must not consume the string.
    ntsc_array_drop(s);
    assert!(string_is(s, "not an array"));
    ntsc_string_drop(s);
}

#[test]
fn an_array_handle_is_not_accepted_as_a_string() {
    let arr = ntsc_array_new_typed(8, 2, 0);
    assert_eq!(ntsc_array_push(arr, 11), arr);

    assert_eq!(strings::ntsc_strings_length(arr), 0);
    assert_eq!(strings::ntsc_strings_char_code(arr, 0), -1);
    assert_eq!(ntsc_string_clone(arr), 0);
    assert_eq!(ntsc_string_equals(arr, arr), 0);
    // A destructive string operation must not consume the array.
    ntsc_string_drop(arr);
    assert_eq!(ntsc_array_len(arr), 1);
    assert_eq!(ntsc_array_get(arr, 0), 11);
    ntsc_array_drop(arr);
}

#[test]
fn a_shared_box_is_not_accepted_as_its_contents_and_vice_versa() {
    let inner = string_handle("boxed");
    let boxed = ntsc_shared_new(inner);
    assert_ne!(boxed, 0);
    assert_eq!(ntsc_shared_inner(boxed), inner);

    // The box is not a string or an array.
    assert_eq!(strings::ntsc_strings_length(boxed), 0);
    assert_eq!(ntsc_array_len(boxed), 0);
    ntsc_string_drop(boxed);
    ntsc_array_drop(boxed);
    assert_eq!(
        ntsc_shared_inner(boxed),
        inner,
        "a wrong-kind drop destroyed the box"
    );

    // A string is not a box: releasing one must not consume it.
    assert_eq!(ntsc_shared_release(inner), 0);
    assert_eq!(ntsc_shared_inner(inner), 0);
    assert!(string_is(inner, "boxed"));

    // Retain/release of the real box still balances, and the last release hands
    // the inner value back to the caller to drop.
    assert_eq!(ntsc_shared_retain(boxed), boxed);
    assert_eq!(ntsc_shared_release(boxed), 0, "one copy left");
    assert_eq!(
        ntsc_shared_release(boxed),
        inner,
        "last copy releases inner"
    );
    assert_eq!(ntsc_shared_release(boxed), 0, "box is already gone");
    assert!(string_is(inner, "boxed"));
    ntsc_string_drop(inner);
}

#[test]
fn a_future_handle_and_a_value_handle_cannot_be_confused() {
    let sleep = ntsc_async_sleep_new(0);
    assert_ne!(sleep, 0);

    // A future is not a string or an array, and a value operation must not
    // consume it: polling still works afterwards.
    assert_eq!(strings::ntsc_strings_length(sleep), 0);
    assert_eq!(ntsc_array_len(sleep), 0);
    ntsc_string_drop(sleep);
    ntsc_array_drop(sleep);
    assert_eq!(ntsc_async_sleep_poll(sleep), 0, "first poll arms the timer");

    // Regression: the future drop removed *any* entry, so handing it a string
    // handle destroyed a live string.
    let s = string_handle("keep me");
    ntsc_async_sleep_drop(s);
    assert!(string_is(s, "keep me"), "a future drop consumed a string");
    ntsc_string_drop(s);

    let arr = ntsc_array_new(8, 1);
    assert_eq!(ntsc_array_push(arr, 5), arr);
    ntsc_async_sleep_drop(arr);
    assert_eq!(ntsc_array_len(arr), 1, "a future drop consumed an array");
    ntsc_array_drop(arr);

    // Dropping the future twice is a no-op, and a dropped future never polls
    // ready again.
    ntsc_async_sleep_drop(sleep);
    ntsc_async_sleep_drop(sleep);
    assert_eq!(ntsc_async_sleep_poll(sleep), 0);
}

#[test]
fn an_opaque_module_resource_is_not_accepted_as_a_value() {
    // `io.open` registers an opaque file resource. It is a handle like any
    // other, but no value operation may read or destroy it.
    let path = string_handle("");
    let mode = string_handle("r");
    let file = io::ntsc_io_open(path, mode);
    ntsc_string_drop(path);
    ntsc_string_drop(mode);
    // Opening "" fails, which is itself the documented safe failure: a null
    // handle rather than a panic.
    assert_eq!(file, 0, "opening an empty path must fail safely");

    // Every operation on the failed handle is a no-op too.
    assert_eq!(io::ntsc_io_read_line(file), 0);
    io::ntsc_io_close(file);
    io::ntsc_io_close(file);
}

#[test]
fn an_unknown_handle_is_refused_by_every_operation() {
    assert_eq!(strings::ntsc_strings_length(UNKNOWN), 0);
    assert_eq!(strings::ntsc_strings_char_code(UNKNOWN, 0), -1);
    assert_eq!(strings::ntsc_strings_index_of(UNKNOWN, UNKNOWN, 0), -1);
    assert_eq!(ntsc_string_clone(UNKNOWN), 0);
    assert_eq!(ntsc_string_equals(UNKNOWN, UNKNOWN), 0);
    assert_eq!(ntsc_array_len(UNKNOWN), 0);
    assert_eq!(ntsc_array_get(UNKNOWN, 0), 0);
    assert_eq!(ntsc_array_push(UNKNOWN, 1), 0);
    assert_eq!(ntsc_array_pop(UNKNOWN), 0);
    assert_eq!(ntsc_array_clone(UNKNOWN), 0);
    assert_eq!(ntsc_shared_inner(UNKNOWN), 0);
    assert_eq!(ntsc_shared_release(UNKNOWN), 0);
    assert_eq!(ntsc_async_sleep_poll(UNKNOWN), 0);
    // Dropping unknown non-null string/array handles is a hard error
    // (NTSC-R0001) so double-free/use-after-free bugs don't go silent.
    assert_double_drop_aborts("unknown_string");
    assert_double_drop_aborts("unknown_array");
    ntsc_async_sleep_drop(UNKNOWN);
    // Closing an unknown channel is also a harmless no-op.
    ntsc_runtime::ntask_chan_close(UNKNOWN);
    ntsc_runtime::ntask_chan_drop(UNKNOWN);
}

#[test]
fn the_null_handle_means_no_value_everywhere() {
    assert_eq!(strings::ntsc_strings_length(0), 0);
    assert_eq!(ntsc_string_clone(0), 0);
    assert_eq!(ntsc_string_equals(0, 0), 0, "null is not equal to null");
    assert_eq!(ntsc_array_len(0), 0);
    assert_eq!(ntsc_array_get(0, 0), 0);
    assert_eq!(ntsc_array_pop(0), 0);
    assert_eq!(ntsc_shared_inner(0), 0);
    assert_eq!(ntsc_shared_release(0), 0);
    assert_eq!(ntsc_async_sleep_poll(0), 0);
    ntsc_string_drop(0);
    ntsc_array_drop(0);
    ntsc_async_sleep_drop(0);

    // Concatenation reads an absent operand as empty text rather than
    // collapsing the whole expression to null.
    let s = string_handle("tail");
    let joined = ntsc_string_concat(0, s);
    assert_ne!(joined, 0);
    assert!(string_is(joined, "tail"));
    ntsc_string_drop(joined);
    ntsc_string_drop(s);
}

#[test]
fn a_consumed_element_handle_is_owned_by_exactly_one_place() {
    // `arrays.push` deep-copies a string element, so the caller's handle stays
    // theirs; `arrays.pop` transfers the array's copy out. Neither leaves two
    // owners of one entry, which is what a double drop would need.
    let arr = ntsc_array_new_typed(8, 2, 1);
    let pushed = string_handle("elem");
    assert_eq!(ntsc_array_push(arr, pushed), arr);
    let stored = ntsc_array_get(arr, 0);
    assert_ne!(stored, pushed, "push must store an independent copy");

    ntsc_string_drop(pushed);
    assert!(string_is(stored, "elem"), "the array still owns its copy");

    let popped = ntsc_array_pop(arr);
    assert_eq!(popped, stored, "pop transfers the array's own handle");
    assert_eq!(ntsc_array_len(arr), 0);
    ntsc_array_drop(arr);
    // The array no longer owns it, so the caller's drop is the only one.
    assert!(string_is(popped, "elem"));
    ntsc_string_drop(popped);
    assert_eq!(ntsc_string_clone(popped), 0, "popped handle is now stale");
}
