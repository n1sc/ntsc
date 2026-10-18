use std::path::Path;

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
    let out_dir = rewrite_dir.join("target").join("tuples-e2e").join(name);
    std::fs::create_dir_all(&out_dir).unwrap();
    let object = ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &out_dir)
        .expect("compile failed");
    let binary = out_dir.join(name);
    ntsc_codegen::link_binary(&object, &runtime_lib(rewrite_dir), &binary).unwrap();
    let output = std::process::Command::new(binary).output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn tuple_literal_and_index() {
    let source = r#"fun main() {
    var t = (10, 20)
    say("" + t.0)
    say("" + t.1)
}
"#;
    let (ok, stdout, stderr) = compile_run("tuple_literal_index", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "10\n20\n");
}

#[test]
fn tuple_destructure() {
    let source = r#"fun main() {
    var t = (42, "hello")
    var (a, b) = t
    say("" + a)
    say(b)
}
"#;
    let (ok, stdout, stderr) = compile_run("tuple_destructure", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "42\nhello\n");
}

#[test]
fn tuple_multi_return() {
    let source = r#"fun bounds() -> (int, int) {
    return (100, 200)
}

fun main() {
    var (w, h) = bounds()
    say("" + w)
    say("" + h)
}
"#;
    let (ok, stdout, stderr) = compile_run("tuple_multi_return", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "100\n200\n");
}

#[test]
fn tuple_index_into_result_of_function() {
    let source = r#"fun pair() -> (int, int) {
    return (7, 14)
}

fun main() {
    var t = pair()
    say("" + t.0)
    say("" + t.1)
}
"#;
    let (ok, stdout, stderr) = compile_run("tuple_index_func", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "7\n14\n");
}

#[test]
fn tuple_with_strings() {
    let source = r#"fun main() {
    var t = ("alice", "bob")
    var (a, b) = t
    say(a + " and " + b)
}
"#;
    let (ok, stdout, stderr) = compile_run("tuple_strings", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "alice and bob\n");
}

/// Returning a tuple that *moves* locals into it must hand the caller live
/// values: the callee's exit-time drop would otherwise release them and the
/// caller would read an empty array or string. A `copy(...)` element keeps
/// its own reference, so the callee still drops the original.
#[test]
fn returning_a_tuple_of_moved_locals_keeps_them_alive() {
    let source = r#"use arrays
fun make_arr() -> array[int] {
    var array[int] out = arrays.fill(0, 0)
    arrays.push(out, 7)
    return out
}

fun pair_locals() -> (array[int], string) {
    var arr = make_arr()
    var text = "kept"
    return (arr, text)
}

fun pair_copied() -> (array[int], string) {
    var arr = make_arr()
    return (copy(arr), arr[0] + "")
}

fun main() {
    var (a, s) = pair_locals()
    say("len:" + arrays.length(a))
    say("text:" + s)
    var (b, t) = pair_copied()
    say("clen:" + arrays.length(b))
    say("ctext:" + t)
    say("done")
}
"#;
    let (ok, stdout, stderr) = compile_run("tuple_move_locals", source);
    assert!(ok, "binary must exit 0, stderr: {stderr}");
    assert_eq!(stdout.trim(), "len:1\ntext:kept\nclen:1\nctext:7\ndone");
    assert!(
        !stderr.contains("memory leak detected"),
        "no leaks expected: {stderr}"
    );
}

#[test]
fn tuple_class_return_preserves_owned_fields_and_reclaims_them() {
    let source = r#"class FetchError {
    var string message

    fun init() {
        this.message = ""
    }
}

fun make_error() -> (FetchError, string) {
    var err = FetchError()
    err.message = copy("request failed")
    return (err, "body")
}

fun main() {
    var tuple = make_error()
    var (err, body) = tuple
    say(err.message)
    say(body)
}
"#;
    let (ok, stdout, stderr) = compile_run("tuple_class_owned_fields", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "request failed\nbody\n");
    assert!(
        !stderr.contains("memory leak detected"),
        "class fields moved through a tuple must be reclaimed: {stderr}"
    );
}

#[test]
fn tuple_class_borrow_does_not_double_drop_a_field() {
    let source = r#"class Record {
    var string label

    fun init() {
        this.label = "field"
    }
}

class Holder {
    var Record record

    fun init() {
        this.record = Record()
    }
}

fun main() {
    var holder = Holder()
    var tuple = (holder.record, "tag")
    var (record, tag) = tuple
    say(record.label + ":" + tag)
}
"#;
    let (ok, stdout, stderr) = compile_run("tuple_class_borrowed_field", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "field:tag\n");
    assert!(
        !stderr.contains("memory leak detected"),
        "borrowing a class field through a tuple must not double-drop it: {stderr}"
    );
}

#[test]
fn fresh_class_temporary_is_reclaimed_after_view_call() {
    let source = r#"class Action {
    var string kind
    var string text
    var string emoji

    fun init() {
        this.kind = "none"
        this.text = ""
        this.emoji = ""
    }
}

fun make_action(string kind) -> Action {
    var action = Action()
    action.kind = copy(kind)
    return action
}

class Context {
    var int ticks
}

fun tick(view mut Context ctx) -> Action {
    ctx.ticks = ctx.ticks + 1
    var sent = make_action("none")
    if (sent.kind != "none") {
        return sent
    }
    return make_action("none")
}

fun consume(view Action action) {
    say(action.kind)
}

fun main() {
    var ctx = Context()
    var int i = 0
    while (i < 4) {
        consume(tick(ctx))
        i = i + 1
    }
}
"#;
    let (ok, stdout, stderr) = compile_run("fresh_class_view_call", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "none\nnone\nnone\nnone\n");
    assert!(
        !stderr.contains("memory leak detected"),
        "a fresh class temporary borrowed by a call must be reclaimed: {stderr}"
    );
}
