//! End-to-end tests for RFC 7.3 (view lowering) and 7.4 (move semantics):
//! `view var` / `view mut var` declarations borrow heap values as raw
//! pointers, are never treated as owned (no double-free, no leaks), and move
//! semantics (assignment / parameter / return) transfer ownership without
//! leaking or use-after-free. Ownership errors surface at compile time.
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
    let dir = std::env::temp_dir().join(format!("ntsc_view_move_e2e_{name}"));
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
    let dir = std::env::temp_dir().join(format!("ntsc_view_move_e2e_{name}"));
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

/// `view var r = matrix[i]` borrows the inner array and reads through it.
#[test]
fn view_var_reads_through_borrowed_element() {
    let source = r#"fun main() {
    var matrix = [[1, 2], [3, 4]];
    view var r = matrix[1];
    say("r0: " + r[0]);
    say("r1: " + r[1]);
    say("m10: " + matrix[1][0]);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "view_read", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "r0: 3\nr1: 4\nm10: 3\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// The annotation form (`var view array[int] r = ...`) lowers through the same
/// slot path; regression test for a segfault when indexing through it.
#[test]
fn annotation_form_view_var_indexes_safely() {
    let source = r#"fun main() {
    var matrix = [[1, 2], [3, 4]];
    var view array[int] r = matrix[1];
    say("r0: " + r[0]);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "view_anno", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "r0: 3\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// `view mut var m = xs` writes through the borrow; the source reflects the
/// change once the view's block ends.
#[test]
fn view_mut_var_writes_through_borrow() {
    let source = r#"fun main() {
    var xs = [1, 2, 3];
    {
        view mut var m = xs;
        m[0] = 99;
        say("m1: " + m[1]);
    }
    say("xs0: " + xs[0]);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "view_mut_write", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "m1: 2\nxs0: 99\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// A view of a class instance reads and writes fields through the raw
/// instance pointer.
#[test]
fn view_var_class_instance_fields() {
    let source = r#"class Box {
    var int value
}

fun main() {
    var b = Box()
    view mut var v = b
    v.value = 7
    say("b: " + b.value)
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "view_class", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "b: 7\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// Move semantics end-to-end: parameter moves, return moves, assignment
/// moves, and reassignment all transfer ownership without leaks.
#[test]
fn move_semantics_end_to_end() {
    let source = r#"fun total(array[int] xs) -> int {
    var s = 0
    for (var x in xs) { s = s + x }
    return s
}

fun make() -> array[int] {
    var a = [5, 6, 7]
    return a
}

fun main() {
    var a = [1, 2, 3]
    say("total: " + total(a))
    var b = make()
    say("b0: " + b[0])
    var c = b
    say("c2: " + c[2])
    b = [9]
    say("b0b: " + b[0])
    say("c0: " + c[0])
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "moves_e2e", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "total: 6\nb0: 5\nc2: 7\nb0b: 9\nc0: 5\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// Moving the source while a view variable still borrows it (it is used after
/// the move) must be a compile error.
#[test]
fn moving_viewed_source_is_an_error() {
    let err = compile_error(
        &workspace_root(),
        "err_move",
        "fun main() {\n    var a = [1, 2]\n    view var r = a\n    var b = a\n    say(\"r: \" + r[0])\n}\n",
    );
    assert!(
        err.contains("cannot move `a` while it is viewed"),
        "expected a move-while-viewed error, got: {err}"
    );
}

/// Under non-lexical lifetimes, moving the source after the view's last use
/// is legal.
#[test]
fn moving_source_after_views_last_use_is_allowed() {
    let source = r#"fun main() {
    var a = [1, 2];
    view var r = a;
    say("r: " + r[0]);
    var b = a;
    say("b0: " + b[0]);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "move_after_view", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "r: 1\nb0: 1\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// Borrowing a temporary value into a view variable must be rejected.
#[test]
fn viewing_a_temporary_is_an_error() {
    let err = compile_error(
        &workspace_root(),
        "err_temp",
        "fun main() {\n    view var r = [1, 2]\n    say(\"x\")\n}\n",
    );
    assert!(
        err.contains("temporary value"),
        "expected a temporary-value error, got: {err}"
    );
}

/// A `view mut` conflicts with a still-live view of the same source.
#[test]
fn view_mut_conflicts_with_existing_view() {
    let err = compile_error(
        &workspace_root(),
        "err_mut_conflict",
        "fun main() {\n    var xs = [1, 2]\n    view var r = xs\n    view mut var m = xs\n    say(\"r: \" + r[0])\n}\n",
    );
    assert!(
        err.contains("already viewed"),
        "expected an exclusive-view conflict, got: {err}"
    );
}

/// Under non-lexical lifetimes, a `view mut` may be taken once the shared
/// view has had its last use.
#[test]
fn view_mut_after_shared_views_last_use_is_allowed() {
    let source = r#"fun main() {
    var xs = [1, 2, 3];
    view var r = xs;
    say("r0: " + r[0]);
    view mut var m = xs;
    m[0] = 9;
    say("xs0: " + xs[0]);
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "view_mut_after_shared", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "r0: 1\nxs0: 9\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

/// Use-after-move of a plain owned variable is still reported.
#[test]
fn use_after_move_is_an_error() {
    let err = compile_error(
        &workspace_root(),
        "err_uam",
        "fun main() {\n    var a = [1, 2]\n    var b = a\n    say(\"\" + a[0])\n}\n",
    );
    assert!(
        err.contains("moved"),
        "expected a use-after-move error, got: {err}"
    );
}

/// `copy()` of a borrowed array element must materialize an independent
/// owned copy: the callee's drop of the copy must not reclaim the array's
/// own element (use-after-free on later reads of the same element).
#[test]
fn copy_of_borrowed_element_does_not_alias_it() {
    let source = r#"use arrays
use strings
fun check(view string ch) -> bool {
    var c = copy(ch)
    return c == "hello"
}
fun main() {
    var array[string] arr = arrays.fill("", 0)
    arrays.push(arr, "hello")
    if (check(arr[0])) {
        say("first-ok")
    } else {
        say("first-bad")
    }
    say("len:" + strings.length(arr[0]))
    if (arr[0] == "hello") {
        say("second-ok")
    } else {
        say("second-bad")
    }
}
"#;
    let (ok, stdout, stderr) = compile_run(&workspace_root(), "copy_borrowed_elem", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "first-ok\nlen:5\nsecond-ok");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}
