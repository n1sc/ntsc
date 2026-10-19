//! Declarations of every runtime and libc function the emitter calls into.
//!
//! The ABI is handle-based: strings, arrays, options, shared boxes, JSON
//! objects and `any` values all travel as opaque `i64` registry handles,
//! and scalars as their LLVM types. A few non-obvious contracts are noted
//! at their declarations; the sections below mirror the runtime's module
//! layout.

use super::*;

pub(crate) fn declare_runtime_functions(module: &Module<'_>) {
    let ctx = module.get_context();
    let void_ty = ctx.void_type();
    let i64_ty = ctx.i64_type();
    let f64_ty = ctx.f64_type();
    let i8_ty = ctx.i8_type();
    let async_poll_fn = ctx.ptr_type(AddressSpace::default());

    let extern_linkage = Some(inkwell::module::Linkage::External);

    macro_rules! declare {
        ($name:expr, $ret:expr $(, $param:expr)*) => {
            module.add_function(
                $name,
                $ret.fn_type(&[$($param.into()),*], false),
                extern_linkage,
            );
        };
    }

    // ── Core runtime ─────────────────────────────────────────────────────

    declare!("ntsc_runtime_init", void_ty);

    // Reports leaked allocations at exit when the flag is nonzero (debug
    // builds only).
    declare!("ntsc_runtime_shutdown", void_ty, i8_ty);

    declare!("ntsc_leak_mark", void_ty, i64_ty, i64_ty, i64_ty);

    declare!("ntsc_say", void_ty, i64_ty);

    declare!("ntsc_print_i64", void_ty, i64_ty);

    declare!("ntsc_print_f64", void_ty, f64_ty);

    declare!("ntsc_panic", void_ty, i64_ty);

    declare!("ntsc_assert", void_ty, i8_ty, i64_ty);

    declare!("ntsc_bool_to_string", i64_ty, i8_ty);

    declare!("ntsc_i64_to_string", i64_ty, i64_ty);

    declare!("ntsc_f64_to_string", i64_ty, f64_ty);

    // ── Emitted heap (libc) ──────────────────────────────────────────────
    // Class instances and option cells are allocated here; the runtime never
    // sees these pointers.

    declare!("malloc", ctx.ptr_type(AddressSpace::default()), i64_ty);

    declare!("free", void_ty, ctx.ptr_type(AddressSpace::default()));

    // ── Strings ──────────────────────────────────────────────────────────

    declare!("ntsc_string_concat", i64_ty, i64_ty, i64_ty);

    declare!("ntsc_string_clone", i64_ty, i64_ty);

    declare!("ntsc_string_drop", void_ty, i64_ty);

    declare!("ntsc_string_equals", i8_ty, i64_ty, i64_ty);

    // Consumes the word array (8 bytes per word, little-endian) and
    // truncates to byte_count. Used for string literals.
    declare!("ntsc_string_from_words", i64_ty, i64_ty, i64_ty);

    // Like the above, but returns a permanent handle (string literal) that
    // is never dropped and excluded from leak reporting.
    declare!("ntsc_string_from_words_permanent", i64_ty, i64_ty, i64_ty);

    // ── Exceptions ───────────────────────────────────────────────────────
    // `ntsc_throw` sets the pending flag and never returns a real value;
    // `get_message` borrows the pending message while `take_message`
    // consumes it.

    declare!("ntsc_throw", i64_ty, i64_ty);

    declare!("ntsc_rethrow", i64_ty, i64_ty);

    declare!("ntsc_exception_is_active", i8_ty);

    declare!("ntsc_exception_pending", i8_ty);

    declare!("ntsc_exception_get_message", i64_ty);

    declare!("ntsc_exception_take_message", i64_ty);

    declare!("ntsc_exception_clear", void_ty);

    // ── Heap arrays ──────────────────────────────────────────────────────
    // `get` borrows its element, `pop` transfers ownership of the removed
    // element, `set` replaces an element (reclaiming the old one) and
    // `drop` unconditionally reclaims an owned array.

    declare!("ntsc_array_new", i64_ty, i64_ty, i64_ty);

    declare!("ntsc_array_new_typed", i64_ty, i64_ty, i64_ty, i8_ty);

    declare!("ntsc_array_set_string_elements", void_ty, i64_ty, i8_ty);

    declare!("ntsc_array_len", i64_ty, i64_ty);

    declare!("ntsc_array_get", i64_ty, i64_ty, i64_ty);

    declare!("ntsc_array_push", i64_ty, i64_ty, i64_ty);

    declare!("ntsc_array_set", i64_ty, i64_ty, i64_ty, i64_ty);

    declare!("ntsc_array_pop", i64_ty, i64_ty);

    declare!("ntsc_array_drop", void_ty, i64_ty);

    declare!("ntsc_array_remove_at", i64_ty, i64_ty, i64_ty);

    declare!("ntsc_array_clone", i64_ty, i64_ty);

    declare!("ntsc_array_deep_clone", i64_ty, i64_ty, i64_ty);

    declare!("ntsc_array_slice", i64_ty, i64_ty, i64_ty, i64_ty);

    declare!("ntsc_array_reverse", i64_ty, i64_ty);

    declare!("ntsc_array_fill", i64_ty, i64_ty, i64_ty, i64_ty, i8_ty);

    declare!("ntsc_array_range", i64_ty, i64_ty, i64_ty);

    declare!("ntsc_array_clear", i64_ty, i64_ty);

    declare!("ntsc_array_shuffle", i64_ty, i64_ty);

    declare!("ntsc_array_sort", i64_ty, i64_ty, i8_ty);

    // ── Shared values ────────────────────────────────────────────────────
    // `release` returns the inner handle when the count hits 0 (the caller
    // owns it), 0 otherwise; `inner` borrows the wrapped value.

    declare!("ntsc_shared_new", i64_ty, i64_ty);

    declare!("ntsc_shared_retain", i64_ty, i64_ty);

    declare!("ntsc_shared_release", i64_ty, i64_ty);

    declare!("ntsc_shared_inner", i64_ty, i64_ty);

    // ── Async ────────────────────────────────────────────────────────────

    declare!("ntsc_async_run", void_ty, async_poll_fn, i64_ty);

    declare!("ntsc_async_push", void_ty, async_poll_fn, i64_ty);

    declare!("ntsc_async_sleep_new", i64_ty, i64_ty);

    declare!("ntsc_async_sleep_poll", i8_ty, i64_ty);

    declare!("ntsc_async_sleep_drop", void_ty, i64_ty);

    declare!(
        "ntsc_async_op_new",
        i64_ty,
        ctx.ptr_type(AddressSpace::default()),
        i64_ty
    );
    declare!("ntsc_async_op_poll", i8_ty, i64_ty);
    declare!("ntsc_async_op_result", i64_ty, i64_ty);
    declare!("ntsc_async_op_drop", void_ty, i64_ty);

    declare!(
        "ntsc_async_wait_any",
        i64_ty,
        async_poll_fn,
        i64_ty,
        async_poll_fn,
        i64_ty
    );

    declare!(
        "ntsc_async_wait_all",
        i64_ty,
        async_poll_fn,
        i64_ty,
        async_poll_fn,
        i64_ty
    );

    // ── Virtual tasks / reactor ─────────────────────────────────────────
    declare!("ntask_go", i64_ty, async_poll_fn, i64_ty);
    declare!(
        "ntask_go_owned",
        i64_ty,
        async_poll_fn,
        i64_ty,
        ctx.ptr_type(AddressSpace::default())
    );
    declare!(
        "ntask_go_detached",
        void_ty,
        async_poll_fn,
        i64_ty,
        ctx.ptr_type(AddressSpace::default())
    );
    declare!("ntask_join", i64_ty, i64_ty);
    declare!("ntask_join_park", i8_ty, i64_ty);
    declare!("ntask_goroutine_drop", void_ty, i64_ty);
    declare!("ntask_chan_new", i64_ty, i64_ty, i8_ty);
    declare!("ntask_chan_retain", i64_ty, i64_ty);
    declare!("ntask_chan_send", i8_ty, i64_ty, i64_ty);
    declare!("ntask_chan_recv", i8_ty, i64_ty);
    declare!("ntask_chan_recv_result", i64_ty);
    declare!("ntask_chan_recv_ok", i8_ty);
    declare!("ntask_chan_close", void_ty, i64_ty);
    declare!("ntask_chan_drop", void_ty, i64_ty);
    declare!("ntask_timer_new", i64_ty);
    declare!("ntask_timer_park", i8_ty, i64_ty, i64_ty);
    declare!("ntask_reactor_drop", void_ty, i64_ty);
    // ── Offloaded http futures ──────────────────────────────────────────
    declare!("ntsc_async_http_get", i64_ty, i64_ty);
    declare!("ntsc_async_http_post", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_async_http_put", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_async_http_delete", i64_ty, i64_ty);
    declare!("ntsc_async_http_patch", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_async_http_head", i64_ty, i64_ty);
    declare!("ntsc_async_http_poll", i8_ty, i64_ty);
    declare!("ntsc_async_http_result", i64_ty, i64_ty);
    declare!("ntsc_async_http_drop", void_ty, i64_ty);
    // ── Offloaded net accept ────────────────────────────────────────────
    declare!("ntsc_async_net_accept", i64_ty, i64_ty);
    declare!("ntsc_async_net_accept_poll", i8_ty, i64_ty);
    declare!("ntsc_async_net_accept_result", i64_ty, i64_ty);
    declare!("ntsc_async_net_accept_drop", void_ty, i64_ty);
    declare!("ntsc_async_net_recv_line", i64_ty, i64_ty);
    declare!("ntsc_async_net_recv_line_poll", i8_ty, i64_ty);
    declare!("ntsc_async_net_recv_line_result", i64_ty, i64_ty);
    declare!("ntsc_async_net_recv_line_drop", void_ty, i64_ty);
    declare!("ntask_io_new", i64_ty);
    declare!("ntask_io_attach", void_ty, i64_ty, i64_ty, i8_ty);
    declare!("ntask_io_park", i8_ty, i64_ty, i8_ty);
    declare!("ntask_io_ready", i8_ty, i64_ty);
    declare!("ntask_io_drop", void_ty, i64_ty);

    // ── arrays module ────────────────────────────────────────────────────

    declare!("ntsc_arrays_new", i64_ty);
    declare!("ntsc_arrays_length", i64_ty, i64_ty);
    declare!("ntsc_arrays_at", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_push", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_pop", i64_ty, i64_ty);
    declare!("ntsc_arrays_remove", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_remove_at", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_contains", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_index_of", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_join", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_slice", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_range", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_reverse", i64_ty, i64_ty);
    declare!("ntsc_arrays_fill", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_flat", i64_ty, i64_ty);
    declare!("ntsc_arrays_clone", i64_ty, i64_ty);
    declare!("ntsc_arrays_clear", i64_ty, i64_ty);
    declare!("ntsc_arrays_sort", i64_ty, i64_ty);
    declare!("ntsc_arrays_shuffle", i64_ty, i64_ty);
    declare!("ntsc_arrays_every", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_arrays_some", i8_ty, i64_ty, i64_ty);

    // ── collections module ───────────────────────────────────────────────

    declare!("ntsc_collections_stack_new", i64_ty);
    declare!("ntsc_collections_stack_push", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_stack_pop", i64_ty, i64_ty);
    declare!("ntsc_collections_stack_peek", i64_ty, i64_ty);
    declare!("ntsc_collections_stack_size", i64_ty, i64_ty);
    declare!("ntsc_collections_stack_is_empty", i8_ty, i64_ty);
    declare!("ntsc_collections_queue_new", i64_ty);
    declare!("ntsc_collections_queue_enqueue", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_queue_dequeue", i64_ty, i64_ty);
    declare!("ntsc_collections_queue_peek", i64_ty, i64_ty);
    declare!("ntsc_collections_queue_size", i64_ty, i64_ty);
    declare!("ntsc_collections_queue_is_empty", i8_ty, i64_ty);
    declare!("ntsc_collections_set_new", i64_ty);
    declare!("ntsc_collections_set_add", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_set_remove", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_set_has", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_set_size", i64_ty, i64_ty);
    declare!("ntsc_collections_set_to_array", i64_ty, i64_ty);
    declare!("ntsc_collections_set_union", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_set_intersection", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_set_difference", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_channel", i64_ty, i64_ty);
    declare!("ntsc_collections_channel_sender", i64_ty, i64_ty);
    declare!("ntsc_collections_channel_send", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_collections_channel_recv", i64_ty, i64_ty);
    declare!("ntsc_collections_channel_try_recv", i64_ty, i64_ty);
    declare!("ntsc_collections_channel_close", void_ty, i64_ty);

    // ── crypto module ────────────────────────────────────────────────────

    declare!("ntsc_crypto_base64_encode", i64_ty, i64_ty);
    declare!("ntsc_crypto_base64_decode", i64_ty, i64_ty);
    declare!("ntsc_crypto_hex_encode", i64_ty, i64_ty);
    declare!("ntsc_crypto_hex_decode", i64_ty, i64_ty);
    declare!("ntsc_crypto_sha256", i64_ty, i64_ty);
    declare!("ntsc_crypto_sha512", i64_ty, i64_ty);
    declare!("ntsc_crypto_sha384", i64_ty, i64_ty);
    declare!("ntsc_crypto_sha224", i64_ty, i64_ty);
    declare!("ntsc_crypto_md5", i64_ty, i64_ty);
    declare!("ntsc_crypto_hmac_sha256", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_crypto_hmac_sha512", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_crypto_xor_cipher", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_crypto_random_bytes", i64_ty, i64_ty);
    declare!("ntsc_crypto_random_string", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_crypto_verify_sha256", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_crypto_verify_sha512", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_crypto_file_sha256", i64_ty, i64_ty);
    declare!("ntsc_crypto_file_sha512", i64_ty, i64_ty);

    // ── encoding module ──────────────────────────────────────────────────

    declare!("ntsc_encoding_base64_encode", i64_ty, i64_ty);
    declare!("ntsc_encoding_base64_decode", i64_ty, i64_ty);
    declare!("ntsc_encoding_hex_encode", i64_ty, i64_ty);
    declare!("ntsc_encoding_hex_decode", i64_ty, i64_ty);
    declare!("ntsc_encoding_utf8_valid", i8_ty, i64_ty);

    // ── fmt module ───────────────────────────────────────────────────────

    declare!("ntsc_fmt_to_int", i64_ty, i64_ty);
    declare!("ntsc_fmt_to_float", f64_ty, i64_ty);
    declare!("ntsc_fmt_i64_to_str", i64_ty, i64_ty);
    declare!("ntsc_fmt_f64_to_str", i64_ty, f64_ty);
    declare!("ntsc_fmt_type_name", i64_ty, i64_ty);
    declare!("ntsc_fmt_is_int", i8_ty, i64_ty);
    declare!("ntsc_fmt_is_float", i8_ty, i64_ty);
    declare!("ntsc_fmt_to_hex", i64_ty, i64_ty);
    declare!("ntsc_fmt_to_oct", i64_ty, i64_ty);
    declare!("ntsc_fmt_pad_left", i64_ty, i64_ty, i64_ty, i8_ty);
    declare!("ntsc_fmt_pad_right", i64_ty, i64_ty, i64_ty, i8_ty);

    // ── hash module ──────────────────────────────────────────────────────

    declare!("ntsc_hash_crc32", i64_ty, i64_ty);
    declare!("ntsc_hash_sha256", i64_ty, i64_ty);

    // ── http module ──────────────────────────────────────────────────────

    declare!("ntsc_http_get", i64_ty, i64_ty);
    declare!("ntsc_http_post", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_http_put", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_http_delete", i64_ty, i64_ty);
    declare!("ntsc_http_head", i64_ty, i64_ty);
    declare!("ntsc_http_patch", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_http_request", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!(
        "ntsc_http_request_full",
        i64_ty,
        i64_ty,
        i64_ty,
        i64_ty,
        i64_ty
    );
    declare!("ntsc_http_status_code", i64_ty, i64_ty);
    declare!("ntsc_http_get_range", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_http_get_file", i64_ty, i64_ty, i64_ty);
    declare!(
        "ntsc_http_download_with_progress",
        i64_ty,
        i64_ty,
        i64_ty,
        i64_ty
    );
    declare!(
        "ntsc_http_concurrent_download",
        i64_ty,
        i64_ty,
        i64_ty,
        i64_ty
    );

    // ── io module ────────────────────────────────────────────────────────

    declare!("ntsc_io_input", i64_ty);
    declare!("ntsc_io_stdin", i64_ty);
    declare!("ntsc_io_stdout", i64_ty);
    declare!("ntsc_io_stderr", i64_ty);
    declare!("ntsc_io_open", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_io_close", i8_ty, i64_ty);
    declare!("ntsc_io_read", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_io_read_all", i64_ty, i64_ty);
    declare!("ntsc_io_read_line", i64_ty, i64_ty);
    declare!("ntsc_io_write", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_io_write_line", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_io_flush", i8_ty, i64_ty);
    declare!("ntsc_io_seek", i8_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_io_tell", i64_ty, i64_ty);
    declare!("ntsc_io_eof", i8_ty, i64_ty);

    // ── csv module ───────────────────────────────────────────────────────

    declare!("ntsc_csv_parse", i64_ty, i64_ty);
    declare!("ntsc_csv_stringify", i64_ty, i64_ty);

    // ── json module ──────────────────────────────────────────────────────

    declare!("ntsc_json_parse", i64_ty, i64_ty);
    declare!("ntsc_json_stringify", i64_ty, i64_ty);
    declare!("ntsc_json_stringify_pretty", i64_ty, i64_ty);
    declare!("ntsc_json_get", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_json_length", i64_ty, i64_ty);
    declare!("ntsc_json_at", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_json_has", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_json_keys", i64_ty, i64_ty);
    declare!("ntsc_json_is_valid", i8_ty, i64_ty);
    declare!("ntsc_json_escape_string", i64_ty, i64_ty);

    // ── math module ──────────────────────────────────────────────────────

    declare!("ntsc_math_sqrt", f64_ty, f64_ty);
    declare!("ntsc_math_pow", f64_ty, f64_ty, f64_ty);
    declare!("ntsc_math_abs", f64_ty, f64_ty);
    declare!("ntsc_math_ceil", f64_ty, f64_ty);
    declare!("ntsc_math_floor", f64_ty, f64_ty);
    declare!("ntsc_math_round", f64_ty, f64_ty);
    declare!("ntsc_math_sin", f64_ty, f64_ty);
    declare!("ntsc_math_cos", f64_ty, f64_ty);
    declare!("ntsc_math_tan", f64_ty, f64_ty);

    // ── memory module ─────────────────────────────────────────────────────
    // ── slices module ─────────────────────────────────────────────────────
    declare!("ntsc_slices_of", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_slices_sub", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_slices_length", i64_ty, i64_ty);
    declare!("ntsc_slices_get", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_slices_set", i8_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_slices_to_array", i64_ty, i64_ty);
    declare!("ntsc_slices_fill", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_slices_copy_from", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_slices_equal", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_slices_drop", void_ty, i64_ty);

    declare!("ntsc_memory_alloc", i64_ty, i64_ty);
    declare!("ntsc_memory_offset", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_memory_clone", i64_ty, i64_ty);
    declare!("ntsc_memory_drop", void_ty, i64_ty);
    declare!("ntsc_memory_load8", i64_ty, i64_ty);
    declare!("ntsc_memory_load64", i64_ty, i64_ty);
    declare!("ntsc_memory_store8", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_memory_store64", i8_ty, i64_ty, i64_ty);

    // ── net module ───────────────────────────────────────────────────────

    declare!("ntsc_net_tcp_connect", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_net_tcp_listen", i64_ty, i64_ty);
    declare!("ntsc_net_tcp_accept", i64_ty, i64_ty);
    declare!("ntsc_net_accept_poll", i64_ty, i64_ty, f64_ty);
    declare!("ntsc_net_udp_bind", i64_ty, i64_ty);
    declare!("ntsc_net_udp_send", i64_ty, i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_net_udp_recv", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_net_send", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_net_send_line", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_net_recv", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_net_recv_line", i64_ty, i64_ty);
    declare!("ntsc_net_close", i8_ty, i64_ty);
    declare!("ntsc_net_local_port", i64_ty, i64_ty);

    // ── os module ────────────────────────────────────────────────────────

    declare!("ntsc_os_getenv", i64_ty, i64_ty);
    declare!("ntsc_os_setenv", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_os_has_env", i8_ty, i64_ty);
    declare!("ntsc_os_unsetenv", i8_ty, i64_ty);
    declare!("ntsc_os_path_abs", i64_ty, i64_ty);
    declare!("ntsc_os_path_basename", i64_ty, i64_ty);
    declare!("ntsc_os_path_dirname", i64_ty, i64_ty);
    declare!("ntsc_os_path_ext", i64_ty, i64_ty);
    declare!("ntsc_os_path_join", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_os_path_stem", i64_ty, i64_ty);
    declare!("ntsc_os_is_abs", i8_ty, i64_ty);
    declare!("ntsc_os_separator", i64_ty);
    declare!("ntsc_os_temp_dir", i64_ty);
    declare!("ntsc_os_temp_file", i64_ty, i64_ty);
    declare!("ntsc_os_temp_path", i64_ty, i64_ty);
    declare!("ntsc_os_file_lock", i64_ty, i64_ty);
    declare!("ntsc_os_file_unlock", i8_ty, i64_ty);
    declare!("ntsc_os_rename", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_os_executable_path", i64_ty);

    // ── native terminal substrate ────────────────────────────────────────
    declare!("ntsc_term_native_is_tty", i8_ty);
    declare!("ntsc_term_native_open", i64_ty);
    declare!("ntsc_term_native_restore", i8_ty, i64_ty);
    declare!("ntsc_term_native_poll_key", i8_ty, i64_ty, f64_ty);
    declare!("ntsc_term_native_key_code", i64_ty);
    declare!("ntsc_term_native_key_character", i64_ty);
    declare!("ntsc_term_native_key_ctrl", i8_ty);

    // ── process module ───────────────────────────────────────────────────

    declare!("ntsc_process_exec", i64_ty, i64_ty);
    declare!("ntsc_process_exec_output", i64_ty, i64_ty);
    declare!("ntsc_process_spawn", i64_ty, i64_ty, i64_ty);

    // Opaque LLVM pointer carrying a void(i64) worker callback.
    let thread_worker_ptr_ty = ctx.ptr_type(AddressSpace::default());
    declare!(
        "ntsc_process_spawn_thread",
        i64_ty,
        thread_worker_ptr_ty,
        i64_ty
    );
    declare!("ntsc_process_thread_join", i8_ty, i64_ty);
    declare!("ntsc_process_pid", i64_ty);

    // ── random module ────────────────────────────────────────────────────

    declare!("ntsc_random_seed", i8_ty, i64_ty);
    declare!("ntsc_random_int", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_random_float", f64_ty);
    declare!("ntsc_random_bool", i8_ty);
    declare!("ntsc_random_shuffle", i64_ty, i64_ty);
    declare!("ntsc_random_weighted", i64_ty, i64_ty, i8_ty);

    // ── regex module ─────────────────────────────────────────────────────

    declare!("ntsc_regex_test", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_regex_search", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_regex_find", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_regex_find_all", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_regex_replace", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_regex_split", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_regex_escape", i64_ty, i64_ty);
    declare!("ntsc_regex_is_valid", i8_ty, i64_ty);

    // ── sort module ──────────────────────────────────────────────────────

    declare!("ntsc_sort_stable_sort", i64_ty, i64_ty, i8_ty);

    declare!("ntsc_sort_binary_search", i64_ty, i64_ty, i64_ty, i8_ty);

    // Opaque LLVM pointer carrying an i8(i64, i64) comparator callback.
    let sort_comparator_ptr_ty = ctx.ptr_type(AddressSpace::default());
    declare!(
        "ntsc_sort_sort_by",
        i64_ty,
        i64_ty,
        sort_comparator_ptr_ty,
        i8_ty
    );

    // ── strings module ───────────────────────────────────────────────────

    declare!("ntsc_strings_length", i64_ty, i64_ty);
    declare!("ntsc_strings_char_at", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_char_code", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_from_char_code", i64_ty, i64_ty);
    declare!("ntsc_strings_contains", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_starts_with", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_ends_with", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_index_of", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_last_index_of", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_substring", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_upper", i64_ty, i64_ty);
    declare!("ntsc_strings_lower", i64_ty, i64_ty);
    declare!("ntsc_strings_trim", i64_ty, i64_ty);
    declare!("ntsc_strings_trim_left", i64_ty, i64_ty);
    declare!("ntsc_strings_trim_right", i64_ty, i64_ty);
    declare!("ntsc_strings_replace", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_replace_first", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_split", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_split_array", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_join", i64_ty, i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_join_all", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_repeat", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_reverse", i64_ty, i64_ty);
    declare!("ntsc_strings_count", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_strings_is_empty", i8_ty, i64_ty);
    declare!("ntsc_strings_is_alpha", i8_ty, i64_ty);
    declare!("ntsc_strings_is_digit", i8_ty, i64_ty);
    declare!("ntsc_strings_is_alnum", i8_ty, i64_ty);

    // ── sys module ───────────────────────────────────────────────────────

    declare!("ntsc_sys_args", i64_ty);
    declare!("ntsc_sys_cwd", i64_ty);
    declare!("ntsc_sys_env", i64_ty, i64_ty);
    declare!("ntsc_sys_exec", i64_ty, i64_ty);
    declare!("ntsc_sys_exit", void_ty, i64_ty);
    declare!("ntsc_sys_read", i64_ty, i64_ty);
    declare!("ntsc_sys_write", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_sys_append", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_sys_exists", i8_ty, i64_ty);
    declare!("ntsc_sys_listdir", i64_ty, i64_ty);
    declare!("ntsc_sys_mkdir", i8_ty, i64_ty);
    declare!("ntsc_sys_rm", i8_ty, i64_ty);
    declare!("ntsc_sys_cp", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_sys_cp_all", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_sys_rm_all", i8_ty, i64_ty);
    declare!("ntsc_sys_sleep", void_ty, f64_ty);
    declare!("ntsc_sys_walk", i64_ty, i64_ty);
    declare!("ntsc_sys_symlink", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_sys_readlink", i64_ty, i64_ty);
    declare!("ntsc_sys_is_symlink", i8_ty, i64_ty);

    // ── paths module ─────────────────────────────────────────────────────

    declare!("ntsc_paths_join", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_paths_parent", i64_ty, i64_ty);
    declare!("ntsc_paths_file_name", i64_ty, i64_ty);
    declare!("ntsc_paths_extension", i64_ty, i64_ty);
    declare!("ntsc_paths_with_extension", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_paths_stem", i64_ty, i64_ty);
    declare!("ntsc_paths_absolute", i64_ty, i64_ty);
    declare!("ntsc_paths_relative", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_paths_is_absolute", i8_ty, i64_ty);
    declare!("ntsc_paths_components", i64_ty, i64_ty);
    declare!("ntsc_paths_normalize", i64_ty, i64_ty);

    // ── glob module ──────────────────────────────────────────────────────

    declare!("ntsc_glob_matches", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_glob_find", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_glob_is_match", i8_ty, i64_ty, i64_ty);

    // ── archive module ───────────────────────────────────────────────────

    declare!("ntsc_archive_extract_tar_gz", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_archive_extract_tar", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_archive_extract_zip", i64_ty, i64_ty, i64_ty);
    declare!("ntsc_archive_list_tar", i64_ty, i64_ty);
    declare!("ntsc_archive_list_zip", i64_ty, i64_ty);

    // ── testing module ───────────────────────────────────────────────────

    declare!("ntsc_testing_assert_true", i8_ty, i8_ty);
    declare!("ntsc_testing_assert_false", i8_ty, i8_ty);
    declare!("ntsc_testing_assert_eq_int", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_testing_assert_ne_int", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_testing_assert_eq_float", i8_ty, f64_ty, f64_ty);
    declare!("ntsc_testing_assert_ne_float", i8_ty, f64_ty, f64_ty);
    declare!("ntsc_testing_assert_eq_string", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_testing_assert_ne_string", i8_ty, i64_ty, i64_ty);
    declare!("ntsc_testing_assert_eq_bool", i8_ty, i8_ty, i8_ty);
    declare!("ntsc_testing_assert_ne_bool", i8_ty, i8_ty, i8_ty);

    let bench_fn_ptr_ty = ctx.ptr_type(AddressSpace::default());
    declare!(
        "ntsc_testing_bench",
        f64_ty,
        bench_fn_ptr_ty,
        i64_ty,
        i64_ty
    );

    // ── time module ──────────────────────────────────────────────────────

    declare!("ntsc_time_now", f64_ty);
    declare!("ntsc_time_sleep", void_ty, f64_ty);
    declare!("ntsc_time_format", i64_ty, f64_ty, i64_ty);
}
