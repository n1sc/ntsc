//! End-to-end tests for newly added language features:
//! - Struct literals and field shorthand
//! - `static const`
//! - Enums with associated data

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
    let out_dir = rewrite_dir
        .join("target")
        .join("lang-features-e2e")
        .join(name);
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

/// Compile `source` and return the first error message; panics if the source
/// compiles.
fn compile_error(name: &str, source: &str) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let out_dir = rewrite_dir
        .join("target")
        .join("lang-features-e2e")
        .join(name);
    std::fs::create_dir_all(&out_dir).unwrap();
    match ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &out_dir) {
        Ok(_) => panic!("expected compilation to fail for `{name}`"),
        Err(e) => e.to_string(),
    }
}

// ── Struct literals ──────────────────────────────────────────────────────

#[test]
fn struct_literal_basic_fields() {
    let source = r#"class Point {
    var int x
    var int y
    fun init(int px, int py) {
        this.x = px
        this.y = py
    }
}

fun main() {
    var Point p = Point { x: 10, y: 20 }
    say("x=" + p.x + " y=" + p.y)
}
"#;
    let (ok, stdout, stderr) = compile_run("struct_basic", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "x=10 y=20\n");
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn struct_literal_shorthand() {
    let source = r#"class Pair {
    var int first
    var int second
    fun init(int a, int b) {
        this.first = a
        this.second = b
    }
}

fun main() {
    var int first = 42
    var int second = 99
    var Pair p = Pair { first, second }
    say("a=" + p.first + " b=" + p.second)
}
"#;
    let (ok, stdout, stderr) = compile_run("struct_shorthand", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "a=42 b=99\n");
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn struct_literal_single_field() {
    let source = r#"class Wrapper {
    var int value
    fun init(int v) {
        this.value = v
    }
}

fun main() {
    var Wrapper w = Wrapper { value: 7 }
    say("val=" + w.value)
}
"#;
    let (ok, stdout, stderr) = compile_run("struct_single", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "val=7\n");
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn struct_literal_without_init() {
    let source = r#"class Bare {
    var int a
    var string b
}

fun main() {
    var Bare x = Bare { a: 5, b: "hey" }
    say("a=" + x.a + " b=" + x.b)
}
"#;
    let (ok, stdout, stderr) = compile_run("struct_noinit", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "a=5 b=hey\n");
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

// ── Struct update (`..base`) ─────────────────────────────────────────────

#[test]
fn struct_update_with_init() {
    let source = r#"class Point {
    var int x
    var int y
    fun init(int px, int py) {
        this.x = px
        this.y = py
    }
}

fun main() {
    var Point base = Point { x: 10, y: 20 }
    var Point copied = Point { ..base }
    var Point moved = Point { ..base, y: 30 }
    say("copy=" + copied.x + "," + copied.y + " moved=" + moved.x + "," + moved.y)
}
"#;
    let (ok, stdout, stderr) = compile_run("struct_update_init", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "copy=10,20 moved=10,30\n");
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn struct_update_no_init() {
    let source = r#"class Bare {
    var int a
    var string b
}

fun main() {
    var Bare base = Bare { a: 5, b: "hey" }
    var Bare copied = Bare { ..base }
    var Bare overridden = Bare { ..base, a: 9 }
    say("copy=" + copied.a + "," + copied.b + " overridden=" + overridden.a + "," + overridden.b)
}
"#;
    let (ok, stdout, stderr) = compile_run("struct_update_noinit", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "copy=5,hey overridden=9,hey\n");
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn struct_update_source_stays_usable() {
    let source = r#"class Viewer {
    var string label
    var int count
    fun init(string l, int c) {
        this.label = l
        this.count = c
    }
}

fun main() {
    var Viewer base = Viewer { label: "orig", count: 1 }
    var Viewer v = Viewer { ..base }
    base.label = "changed"
    say("base=" + base.label + " copy=" + v.label)
}
"#;
    let (ok, stdout, stderr) = compile_run("struct_update_source", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "base=changed copy=orig\n");
    assert!(!stderr.contains("memory leak detected"), "{stderr}");
}

#[test]
fn struct_update_wrong_class_fails() {
    let source = r#"class Point { var int x }
class Line { var int a }

fun main() {
    var Point p = Point { x: 1 }
    var Point bad = Point { ..p }
    var Line l = Line { a: 2 }
    var Point worse = Point { ..l }
}
"#;
    let error = compile_error("struct_update_wrong_class", source);
    assert!(
        error.contains("requires an instance of `Point`"),
        "unexpected error: {error}"
    );
}

#[test]
fn struct_update_non_class_fails() {
    let source = r#"class Point { var int x }

fun main() {
    var int n = 5
    var Point p = Point { ..n }
}
"#;
    let error = compile_error("struct_update_non_class", source);
    assert!(
        error.contains("requires a class instance"),
        "unexpected error: {error}"
    );
}

// ── static const ─────────────────────────────────────────────────────────

#[test]
fn static_const_integer() {
    let source = r#"static const var int MAX = 100

fun main() {
    say("max=" + MAX)
}
"#;
    let (ok, stdout, stderr) = compile_run("static_const_int", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "max=100\n");
}

#[test]
fn static_const_string() {
    let source = r#"static const var string GREETING = "hello world"

fun main() {
    say(GREETING)
}
"#;
    let (ok, stdout, stderr) = compile_run("static_const_str", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "hello world\n");
}

// ── Enums with associated data ───────────────────────────────────────────

#[test]
fn enum_with_data_declaration() {
    let source = r#"enum Shape {
    Circle(float)
    Rect(float, float)
}

fun main() {
    say("enums declared ok")
}
"#;
    let (ok, _stdout, stderr) = compile_run("enum_data_decl", source);
    assert!(ok, "program failed: {stderr}");
}

#[test]
fn enum_with_data_and_plain_mix() {
    let source = r#"enum Status {
    Ok
    Error(int)
    Custom(int, string)
}

fun main() {
    say("mixed ok")
}
"#;
    let (ok, _stdout, stderr) = compile_run("enum_mixed", source);
    assert!(ok, "program failed: {stderr}");
}

#[test]
fn plain_enum_still_works() {
    let source = r#"enum Color {
    Red
    Green
    Blue
}

fun main() {
    say("color=" + Color.Red)
}
"#;
    let (ok, stdout, stderr) = compile_run("enum_plain", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "color=0\n");
}

/// `&&` and `||` short-circuit: the right operand is not evaluated when the
/// left one already decides the result. An out-of-bounds read guarded by a
/// false left operand must not trap, and a call on that side must not run.
#[test]
fn logical_operators_short_circuit() {
    let source = r#"class Counter {
    var int calls
    fun init() {
        this.calls = 0
    }
    fun bump() -> bool {
        this.calls = this.calls + 1
        return true
    }
}

fun main() {
    var xs = [10, 20]
    if (false && xs[99] == 5) {
        say("and-wrong")
    } else {
        say("and-skipped")
    }
    if (true || xs[99] == 5) {
        say("or-took")
    } else {
        say("or-wrong")
    }
    if (1 < 2 && 3 < 4) {
        say("and-true")
    }
    var c = Counter()
    var i = 0
    while (i < 5 && c.bump()) {
        i = i + 1
        if (i == 3) {
            break
        }
    }
    say("calls=" + c.calls)
}
"#;
    let (ok, stdout, stderr) = compile_run("short_circuit", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "and-skipped\nor-took\nand-true\ncalls=3\n");
}

/// A ternary whose branches are string literals: the literal's own handle
/// blocks must not be mistaken for the branch's predecessor, or the phi
/// names blocks that are not its predecessors and the module fails to verify.
#[test]
fn ternary_with_string_literal_branches() {
    let source = r#"fun main() {
    var flag = true
    say(flag ? "yes" : "no")
    var other = false
    say(other ? "yes" : "no")
    var text = flag ? "a" : "b"
    say(text)
    say("" + (flag ? 1 : 2))
}
"#;
    let (ok, stdout, stderr) = compile_run("ternary_string_literals", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "yes\nno\na\n1\n");
}
