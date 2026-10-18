//! End-to-end regressions for the codegen defects found by probing `ntsc run`:
//!
//! 1. Block scoping was flat, so a name declared inside a block stayed visible
//!    after it and a read in the outer block resolved to the inner slot.
//! 2. Taking a `view` of an instance (or of one of its fields) suppressed the
//!    scope-exit drop of that instance's owned fields, leaking them. A view only
//!    borrows, so it never takes ownership.
//! 3. A function with an empty body was left as a declaration with no
//!    definition, so a program that called it failed to link.
//! 4. A bare identifier naming a top-level function did not resolve to a
//!    function value, so `process.spawn_thread(worker, n)` reported the worker
//!    as an undefined variable.
//! 5. A class field's declared initializer (`var name = "bag"`) was parsed, used
//!    to infer the field's type, and then discarded, so the field held its zero
//!    value at run time.
//! 6. Indexing an owned array field (`a.xs[0]`) took the instance out of the
//!    field-drop set, leaking the array.
//!
//! Every program runs in a debug build, which has leak detection enabled, so
//! each case checks stdout *and* that nothing was leaked.

use std::path::Path;

/// Build the runtime static library if missing and return its path.
fn runtime_lib() -> std::path::PathBuf {
    let rewrite_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
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

/// Compile + link + run `source` as a debug build, returning (ok, stdout, stderr).
fn compile_run(name: &str, source: &str, test_mode: bool) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("ntsc_scoping_refs_{name}"));
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
        test_mode,
    )
    .expect("compile failed");

    let bin_path = dir.join(name);
    ntsc_codegen::link_binary(
        &dir.join(format!("{name}.{}", ntsc_codegen::object_extension())),
        &runtime_lib(),
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

/// Assert `source` runs, prints `expected`, and leaks nothing.
fn assert_runs(name: &str, source: &str, expected: &str) {
    let (ok, stdout, stderr) = compile_run(name, source, false);
    assert!(ok, "{name} exited non-zero: {stderr}\n{stdout}");
    assert_eq!(stdout.trim(), expected, "{name} stdout");
    assert!(
        !stderr.contains("memory leak detected"),
        "{name} leaked: {stderr}"
    );
}

#[test]
fn a_block_declaration_does_not_outlive_its_block() {
    assert_runs(
        "block_scope",
        r#"use arrays
fun main() {
    var n = 1
    {
        var n = 99
        say("in " + n)
    }
    say("out " + n)

    var s = "a"
    {
        var s = "b"
        say("in " + s)
    }
    say("out " + s)

    var xs = [1, 2]
    {
        var xs = [1, 2, 3]
        say("in " + arrays.length(xs))
    }
    say("out " + arrays.length(xs))
}
"#,
        "in 99\nout 1\nin b\nout a\nin 3\nout 2",
    );
}

#[test]
fn a_block_declaration_in_a_branch_or_loop_body_is_local_to_it() {
    assert_runs(
        "block_scope_flow",
        r#"fun main() {
    var s = "a"
    if (true) {
        var s = "b"
        say("in " + s)
    }
    say("out " + s)

    for (var i = 0; i < 2; i = i + 1) {
        var local = "x" + i
        say(local)
    }
    say("out " + s)
}
"#,
        "in b\nout a\nx0\nx1\nout a",
    );
}

#[test]
fn a_view_does_not_suppress_the_scope_exit_drop_of_owned_fields() {
    // `view` borrows, so the instance's owner is still this scope and its owned
    // fields must be reclaimed at exit. Each of these used to leak: a view
    // passed to a call, a view of an owned array field, and a view of an owned
    // string field.
    assert_runs(
        "view_field_drops",
        r#"use arrays
use strings
class Bag {
    var xs = ["a", "b", "c"]
    var name = "bag"
}

fun count(view Bag b) -> int {
    return arrays.length(b.xs)
}

fun main() {
    var b = Bag()
    say("n " + count(view b))

    var c = Bag()
    view var items = c.xs
    say("items " + arrays.length(items))

    var d = Bag()
    view var label = d.name
    say("label " + strings.length(label))
}
"#,
        "n 3\nitems 3\nlabel 3",
    );
}

#[test]
fn a_void_function_with_an_empty_body_links() {
    assert_runs(
        "empty_bodies",
        r#"fun nothing() { }

fun main() {
    nothing()
    say("done")
}
"#,
        "done",
    );
}

#[test]
fn a_non_void_empty_body_is_rejected() {
    let source = "fun zero() -> int { }";
    let tokens = ntsc_lexer::tokenize(source);
    let program = ntsc_parser::parse(&tokens).expect("parse failed");
    let dir = std::env::temp_dir().join("ntsc_scoping_refs_empty_non_void");
    std::fs::create_dir_all(&dir).unwrap();
    let result = ntsc_codegen::compile_program(
        &program,
        ntsc_codegen::host_triple(),
        inkwell::OptimizationLevel::None,
        "empty_non_void",
        &dir,
        false,
    );
    let _ = std::fs::remove_dir_all(&dir);
    let errors = match result {
        Err(ntsc_codegen::CodegenError::TypeCheck(errors)) => errors,
        other => panic!("expected a missing-return type error, got {other:?}"),
    };
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("may fall through")),
        "got: {errors:?}"
    );
}

#[test]
fn an_empty_test_block_passes() {
    let (ok, stdout, stderr) = compile_run(
        "empty_test_block",
        "test nothing_to_do { }\n\ntest still_runs { say(\"body\") }\n",
        true,
    );
    assert!(ok, "test harness exited non-zero: {stderr}\n{stdout}");
    assert!(stdout.contains("nothing_to_do"), "stdout: {stdout}");
    assert!(!stdout.contains("FAIL"), "stdout: {stdout}");
}

#[test]
fn a_named_function_can_be_a_spawn_thread_worker() {
    // A bare function name is a function value, so a named worker works exactly
    // like the lambda form.
    assert_runs(
        "named_worker",
        r#"use process
fun worker(int n) {
    say("worker " + n)
}

fun main() {
    var t = process.spawn_thread(worker, 7)
    process.thread_join(t)
    var u = process.spawn_thread(fun (int n) { say("lambda " + n) }, 9)
    process.thread_join(u)
}
"#,
        "worker 7\nlambda 9",
    );
}

#[test]
fn declared_field_initializers_run_at_construction() {
    // The initializer runs before `init`, so `init` can still overwrite it, and
    // the field owns what it holds either way.
    assert_runs(
        "field_inits",
        r#"use arrays
class Base {
    var int level = 1
    var tags = ["x"]
}

class Kid extends Base {
    var name = "kid"
    var option[int] maybe = 7
    fun init() {
        this.name = "overridden"
    }
}

class Chained {
    var xs = [1, 2, 3]
    fun init(int extra) { arrays.push(this.xs, extra) }
}

fun main() {
    var k = Kid()
    say("level " + k.level + " tags " + arrays.length(k.tags) + " name " + k.name)
    var c = Chained(9)
    say("xs " + arrays.length(c.xs) + " last " + c.xs[3])
    for (var i = 0; i < 2; i = i + 1) {
        var loop = Kid()
        say("loop " + loop.name + " " + arrays.length(loop.tags))
    }
}
"#,
        "level 1 tags 1 name overridden\n\
         xs 4 last 9\n\
         loop overridden 1\n\
         loop overridden 1",
    );
}

#[test]
fn an_array_field_initializer_keeps_its_element_type() {
    // The element type is inferred from the literal, so reading an element back
    // yields that type instead of an opaque `any` that prints as nothing.
    assert_runs(
        "field_init_elem_types",
        r#"use arrays
class Bag {
    var xs = [1, 2, 3]
    var names = ["a", "b"]
    var fs = [1.5, 2.5]
    var empty = []
}

fun main() {
    var b = Bag()
    say("i " + b.xs[0] + " s " + b.names[1] + " f " + b.fs[0] + " e " + arrays.length(b.empty))
    arrays.push(b.empty, 4)
    say("e2 " + arrays.length(b.empty))
}
"#,
        "i 1 s b f 1.5 e 0\ne2 1",
    );
}

#[test]
fn indexing_an_owned_array_field_still_reclaims_it() {
    // Reading or writing through `a.xs[i]` transfers no ownership, so the
    // instance stays a field-drop candidate and its array is reclaimed.
    assert_runs(
        "field_index_drops",
        r#"class Bag {
    var array[int] xs = [1, 2, 3]
    var s = "hi"
}

fun main() {
    var b = Bag()
    var v = b.xs[0]
    say("v " + v + " s " + b.s)
    b.xs[1] = 9
    say("w " + b.xs[1])
}
"#,
        "v 1 s hi\nw 9",
    );
}
