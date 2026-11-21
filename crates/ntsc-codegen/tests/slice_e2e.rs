//! End-to-end tests for `slice[T]`: bounds-checked windows over an owned
//! array.
//!
//! A slice stores the source handle plus a window, never a pointer, so every
//! access re-validates bounds. Debug builds report leaks, so a window that is
//! not reclaimed shows up on stderr.

use std::path::Path;

use ntsc_codegen::{compile_source, link_binary};

fn runtime_lib(rewrite_dir: &Path) -> std::path::PathBuf {
    let lib = rewrite_dir
        .join("target")
        .join("debug")
        .join(ntsc_codegen::runtime_lib_name());
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

fn compile_run(name: &str, source: &str) -> (bool, String, String) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let out_dir = rewrite_dir.join("target").join("slice-e2e").join(name);
    std::fs::create_dir_all(&out_dir).unwrap();
    let object = compile_source(source, ntsc_codegen::host_triple(), name, &out_dir).unwrap();
    let binary = out_dir.join(name);
    link_binary(&object, &runtime_lib(rewrite_dir), &binary).unwrap();
    let output = std::process::Command::new(binary).output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn compile_error(name: &str, source: &str) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let out_dir = rewrite_dir.join("target").join("slice-e2e").join(name);
    std::fs::create_dir_all(&out_dir).unwrap();
    match compile_source(source, ntsc_codegen::host_triple(), name, &out_dir) {
        Ok(_) => panic!("expected `{name}` to be rejected"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn a_slice_reads_writes_and_subslices_within_bounds() {
    let source = r#"use slices
fun main() {
    var array[int] xs = [10, 20, 30, 40, 50]
    var slice[int] window = slices.of(xs, 1, 4)

    say("len: " + slices.length(window))
    say("first: " + slices.get(window, 0))
    say("index: " + window[2])

    slices.set(window, 0, 99)
    say("after set: " + xs[1])

    var slice[int] inner = slices.sub(window, 1, 3)
    say("inner len: " + slices.length(inner))
    say("inner first: " + slices.get(inner, 0))
}
"#;
    let (ok, stdout, stderr) = compile_run("slice_basics", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(
        stdout,
        "len: 3\nfirst: 20\nindex: 40\nafter set: 99\ninner len: 2\ninner first: 30\n"
    );
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn slice_bulk_operations_are_length_checked() {
    let source = r#"use slices
fun main() {
    var array[int] xs = [1, 2, 3, 4]
    var array[int] ys = [9, 9, 9, 9]
    var slice[int] left = slices.of(xs, 0, 2)
    var slice[int] right = slices.of(ys, 2, 4)

    say("equal: " + slices.equal(left, right))
    slices.copy_from(right, left)
    say("copied: " + ys[2] + "," + ys[3])
    say("equal after copy: " + slices.equal(left, right))

    slices.fill(left, 7)
    say("filled: " + xs[0] + "," + xs[1])

    var array[int] owned = slices.to_array(right)
    say("owned: " + owned[0] + "," + owned[1])
}
"#;
    let (ok, stdout, stderr) = compile_run("slice_bulk", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(
        stdout,
        "equal: false\ncopied: 1,2\nequal after copy: true\nfilled: 7,7\nowned: 1,2\n"
    );
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn an_out_of_range_window_throws() {
    let source = r#"use slices
fun main() {
    var array[int] xs = [1, 2, 3]
    try {
        var slice[int] bad = slices.of(xs, 1, 9)
        say("unreachable " + slices.length(bad))
    } catch (err) {
        say(err)
    }
}
"#;
    let (ok, stdout, stderr) = compile_run("slice_range", source);
    assert!(ok, "program failed: {stderr}");
    assert!(
        stdout.contains("slices.of: range is out of bounds"),
        "unexpected stdout: {stdout}"
    );
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn indexing_past_the_window_throws_even_when_the_array_is_longer() {
    let source = r#"use slices
fun main() {
    var array[int] xs = [1, 2, 3, 4, 5]
    var slice[int] window = slices.of(xs, 0, 2)
    try {
        say("unreachable " + slices.get(window, 3))
    } catch (err) {
        say(err)
    }
}
"#;
    let (ok, stdout, stderr) = compile_run("slice_bounds", source);
    assert!(ok, "program failed: {stderr}");
    assert!(
        stdout.contains("slices.get: index out of bounds"),
        "unexpected stdout: {stdout}"
    );
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn a_subslice_cannot_widen_its_parent() {
    let source = r#"use slices
fun main() {
    var array[int] xs = [1, 2, 3, 4, 5]
    var slice[int] window = slices.of(xs, 1, 3)
    try {
        var slice[int] wider = slices.sub(window, 0, 4)
        say("unreachable " + slices.length(wider))
    } catch (err) {
        say(err)
    }
}
"#;
    let (ok, stdout, stderr) = compile_run("slice_widen", source);
    assert!(ok, "program failed: {stderr}");
    assert!(
        stdout.contains("slices.sub: range is out of bounds"),
        "unexpected stdout: {stdout}"
    );
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn copying_a_slice_produces_an_owned_array() {
    let source = r#"use slices
fun main() {
    var array[int] xs = [1, 2, 3]
    var slice[int] window = slices.of(xs, 0, 2)
    var array[int] owned = copy(window)
    slices.set(window, 0, 42)
    say("array: " + xs[0])
    say("copy is independent: " + owned[0])
}
"#;
    let (ok, stdout, stderr) = compile_run("slice_copy", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "array: 42\ncopy is independent: 1\n");
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn a_slice_cannot_cross_a_thread_boundary() {
    let err = compile_error(
        "slice_thread",
        "use slices\nuse process\nfun main() {\n    var array[int] xs = [1, 2]\n    var slice[int] w = slices.of(xs, 0, 2)\n    var t = process.spawn_thread(fun(int x) { say(\"\" + x) }, w)\n}\n",
    );
    assert!(
        err.contains("spawn_thread") || err.contains("cannot cross"),
        "expected a thread-transfer rejection, got: {err}"
    );
}

#[test]
fn an_unknown_slices_function_is_rejected() {
    let err = compile_error(
        "slice_unknown",
        "use slices\nfun main() {\n    var array[int] xs = [1]\n    var n = slices.nope(xs)\n}\n",
    );
    assert!(
        err.contains("unknown function `slices.nope`"),
        "expected an unknown-function error, got: {err}"
    );
}
