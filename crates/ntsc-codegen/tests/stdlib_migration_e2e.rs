//! End-to-end tests for the stdlib & runtime migration (PROMT 7.4 /
//! `ownership-rfc.md` §Stdlib adjustments):
//!
//! - `arrays.push` / `arrays.pop` are in-place `view mut` ops: they mutate
//!   the array behind the handle and never move it; `push` returns `void`.
//! - Functional `arrays.*` ops (`sort`, `slice`, ...) take a `view` and
//!   return a new owned array; the input is not consumed.
//! - Only thread-safe values cross a thread boundary: `process.spawn_thread`
//!   takes scalars and handles, and rejects views, `shared` values, and owned
//!   heap payloads (see `docs/guide/concurrency.md`).
//!
//! All programs run leak-free under the owned model (no RC traffic).
use std::path::Path;

fn runtime_lib(rewrite_dir: &Path) -> std::path::PathBuf {
    let lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );
    if !lib.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "ntsc-runtime"])
            .current_dir(rewrite_dir)
            .status()
            .expect("failed to run cargo");
        assert!(status.success(), "failed to build ntsc-runtime");
    }
    assert!(lib.exists(), "runtime lib not found at {lib:?}");
    lib
}

fn compile_run(rewrite_dir: &Path, name: &str, source: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("ntsc_stdlib_migration_{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let program = {
        let tokens = ntsc_lexer::tokenize(source);
        ntsc_parser::parse(&tokens).expect("parse failed")
    };
    ntsc_codegen::compile_program(
        &program,
        ntsc_codegen::host_triple(),
        inkwell::OptimizationLevel::None,
        name,
        &dir,
        false,
    )
    .expect("compile failed");

    let bin_path = dir.join(name);
    ntsc_codegen::link_binary(
        &dir.join(format!("{name}.{}", ntsc_codegen::object_extension())),
        &runtime_lib(rewrite_dir),
        &bin_path,
    )
    .expect("link failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    let _ = std::fs::remove_dir_all(&dir);
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn compile_error(_rewrite_dir: &Path, name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("ntsc_stdlib_migration_{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let result = ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(_) => String::new(),
        Err(e) => format!("{e}"),
    }
}

fn workspace_root() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().parent().unwrap().to_path_buf()
}

/// `arrays.push` mutates in place and returns void; no reassignment is needed
/// and the array is never moved.
#[test]
fn arrays_push_is_in_place_and_returns_void() {
    let source = r#"use arrays
fun main() {
    var a = [1];
    arrays.push(a, 2);
    arrays.push(a, 3);
    say("len: " + arrays.length(a));
    say("a1: " + a[1]);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "push_in_place", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "len: 3\na1: 2\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// `arrays.pop` mutates in place and returns the removed element.
#[test]
fn arrays_pop_mutates_in_place_and_returns_element() {
    let source = r#"use arrays
fun main() {
    var p = [5, 6, 7];
    var last = arrays.pop(p);
    say("last: " + last);
    say("len: " + arrays.length(p));
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "pop_in_place", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "last: 7\nlen: 2\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// Functional ops (`sort`, `slice`, `reverse`) take a view: the input is not
/// consumed and a new owned array is returned.
#[test]
fn functional_arrays_ops_do_not_consume_input() {
    let source = r#"use arrays
fun main() {
    var nums = [3, 1, 2];
    var sorted = arrays.sort(nums);
    say("sorted: " + sorted[0] + sorted[1] + sorted[2]);
    say("orig: " + nums[0] + nums[1] + nums[2]);
    var s = arrays.slice(nums, 1, 3);
    say("slice: " + s[0] + s[1]);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "func_view", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "sorted: 123\norig: 312\nslice: 12\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// Pushing into an array while a view variable still borrows it (it is used
/// after the push) is a compile error.
#[test]
fn push_while_viewed_is_an_error() {
    let err = compile_error(
        &workspace_root(),
        "err_push_viewed",
        "use arrays\nfun main() {\n    var a = [1, 2]\n    view var r = a\n    arrays.push(a, 3)\n    say(\"r: \" + r[0])\n}\n",
    );
    assert!(
        err.contains("already viewed"),
        "expected an exclusivity error, got: {err}"
    );
}

/// Under non-lexical lifetimes, the borrow ends once the view has had its
/// final use, so pushing afterwards is legal.
#[test]
fn push_after_views_last_use_is_allowed() {
    let source = r#"use arrays
fun main() {
    var a = [1, 2];
    view var r = a;
    say("r: " + r[0]);
    arrays.push(a, 3);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "func_view_nll", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "r: 1\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// Passing a view to `process.spawn_thread` is rejected: views cannot cross
/// threads.
#[test]
fn views_cannot_cross_threads() {
    let err = compile_error(
        &workspace_root(),
        "err_thread_view",
        "use process\nfun worker(int ch) { }\nfun main() {\n    var xs = [1, 2]\n    view var r = xs\n    process.spawn_thread(worker, r)\n}\n",
    );
    assert!(
        err.contains("views cannot cross threads"),
        "expected a cross-thread view error, got: {err}"
    );
}

/// An owned heap payload cannot cross to a thread: it would arrive as a raw
/// handle aliased by both sides, and the caller's scope exit would free it
/// under the running thread.
#[test]
fn owned_heap_values_cannot_cross_threads() {
    for (name, decl, payload) in [
        ("err_thread_array", "var xs = [1, 2]", "xs"),
        ("err_thread_string", "var xs = \"payload\"", "xs"),
    ] {
        let err = compile_error(
            &workspace_root(),
            name,
            &format!(
                "use process\nfun worker(int ch) {{ }}\nfun main() {{\n    {decl}\n    process.spawn_thread(worker, {payload})\n}}\n"
            ),
        );
        assert!(
            err.contains("cannot pass `xs` to process.spawn_thread"),
            "expected an owned-heap thread error for {name}, got: {err}"
        );
        assert!(
            err.contains("collections.channel_send"),
            "the error must point at the supported pattern, got: {err}"
        );
    }
}

/// A `shared` value is reference-counted without synchronization, so it cannot
/// cross either thread boundary.
#[test]
fn shared_values_cannot_cross_threads() {
    for (name, call) in [
        ("err_thread_shared_spawn", "process.spawn_thread(worker, s)"),
        (
            "err_thread_shared_send",
            "collections.channel_send(collections.channel_sender(collections.channel(1)), s)",
        ),
    ] {
        let err = compile_error(
            &workspace_root(),
            name,
            &format!(
                "use process\nuse collections\nfun worker(int ch) {{ }}\nfun main() {{\n    shared array[int] s = [1, 2]\n    {call}\n}}\n"
            ),
        );
        assert!(
            err.contains("cannot pass `s` to") && err.contains("reference-counted"),
            "expected a cross-thread shared error for {name}, got: {err}"
        );
    }
}

/// The supported pattern: only channel handles (plain `int`s) and scalars cross
/// to the worker, and the data itself travels through the channel.
#[test]
fn channel_handles_and_scalars_cross_threads() {
    let source = r#"use collections
use process
fun main() {
    var rx = collections.channel(2);
    var tx = collections.channel_sender(rx);
    var n = 7;
    var worker = process.spawn_thread(fun(int tx) {
        collections.channel_send(tx, "from worker");
        collections.channel_close(tx);
    }, tx);
    say("got: " + collections.channel_recv(rx));
    process.thread_join(worker);
    collections.channel_close(rx);
    say("n: " + n);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "thread_handles_ok", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "got: from worker\nn: 7\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// A stdlib call with too few arguments must be rejected at compile time.
/// Regression: the emitter zipped the provided values against the runtime
/// function's fixed C signature, so the truncated call reached LLVM and
/// failed module verification with an unfollowable error instead.
#[test]
fn stdlib_call_with_too_few_arguments_is_an_error() {
    let err = compile_error(
        &workspace_root(),
        "err_too_few_args",
        "use strings\nfun main() {\n    say(strings.substring(\"abc\", 1))\n}\n",
    );
    assert!(
        err.contains("`strings.substring` expects 3 arguments, got 2"),
        "expected an arity error, got: {err}"
    );
}

/// Too many arguments must be rejected as well: they used to be silently
/// dropped by the same zip, so the program compiled and ran with the extra
/// values ignored.
#[test]
fn stdlib_call_with_too_many_arguments_is_an_error() {
    let err = compile_error(
        &workspace_root(),
        "err_too_many_args",
        "use strings\nfun main() {\n    say(strings.substring(\"abc\", 1, 2, 3))\n}\n",
    );
    assert!(
        err.contains("`strings.substring` expects 3 arguments, got 4"),
        "expected an arity error, got: {err}"
    );
}
