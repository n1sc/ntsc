//! End-to-end regression test for 3.6: the iterator protocol. A class is
//! iterable with `for (var x in obj)` if it defines `length() -> int` and
//! `get(i) -> T`; the loop variable is typed from `get`'s declared return
//! type. Classes without the protocol are a compile-time error.

use std::path::Path;

fn runtime_lib() -> std::path::PathBuf {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
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

fn compile_and_run(source: &str, test_id: &str) -> String {
    let dir = std::env::temp_dir().join(format!("ntsc_iterator_protocol_{test_id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!("iter.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("iter_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "iter", &dir)
        .expect("compile_source failed");
    assert!(obj_path.exists(), "object file not produced");

    let runtime_lib = runtime_lib();
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");
    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "binary exited with non-zero status"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _ = std::fs::remove_dir_all(&dir);
    stdout.trim().to_string()
}

#[test]
fn for_in_over_class_uses_length_and_get() {
    let source = r#"class Counter {
    var int base

    fun init(int start) {
        this.base = start;
    }

    fun length() -> int {
        return this.base;
    }

    fun get(int i) -> int {
        return i * 2;
    }
}

fun main() {
    var c = Counter(3);
    var total = 0;
    for (var v in c) {
        total = total + v;
    }
    say("total: " + total);
    say("done")
}
"#;
    // `length()` returns 3, `get(i)` returns 2*i, so the loop variable is an
    // int typed from `get(i) -> int` and the sum is 0 + 2 + 4 = 6.
    assert_eq!(compile_and_run(source, "counter"), "total: 6\ndone");
}

#[test]
fn for_in_over_class_typing_loops_from_get_return_type() {
    let source = r#"class Words {
    var int dummy

    fun init() {
        this.dummy = 0;
    }

    fun length() -> int {
        return 3;
    }

    fun get(int i) -> string {
        if (i == 0) {
            return "a";
        }
        if (i == 1) {
            return "b";
        }
        return "c";
    }
}

fun main() {
    var ws = Words();
    var joined = "";
    for (var w in ws) {
        joined = joined + w;
    }
    say("joined: " + joined);

    // Iterating a freshly constructed instance works too.
    var joined2 = "";
    for (var e in Words()) {
        joined2 = joined2 + e;
    }
    say("joined2: " + joined2);
    say("done")
}
"#;
    // String-typed loop variables are inferred from `get(i) -> string`.
    assert_eq!(
        compile_and_run(source, "words"),
        "joined: abc\njoined2: abc\ndone"
    );
}

#[test]
fn for_in_over_class_without_protocol_is_compile_error() {
    let source = r#"class Plain {
    var int x

    fun init(int v) {
        this.x = v;
    }
}

fun main() {
    var p = Plain(1);
    for (var v in p) {
        say(v);
    }
}
"#;
    let dir = std::env::temp_dir().join("ntsc_iterator_protocol_err");
    std::fs::create_dir_all(&dir).unwrap();
    let result =
        ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "iter_err", &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        result.is_err(),
        "for-in over a class without length()/get(i) must fail to compile"
    );
}

/// Non-iterable element kinds must be rejected at compile time. Regression:
/// for-in over a string, slice, or tuple used to emit the loop against the
/// runtime's array accessors anyway, which panicked on tuples and silently
/// ran zero iterations on strings and slices.
#[test]
fn for_in_over_non_iterable_kinds_is_compile_error() {
    for (name, source) in [
        (
            "err_forin_string",
            "fun main() {\n    for (var c in \"abc\") {\n        say(c);\n    }\n}\n",
        ),
        (
            "err_forin_slice",
            "use slices\nfun main() {\n    for (var v in slices.of([1, 2], 0, 1)) {\n        say(v);\n    }\n}\n",
        ),
        (
            "err_forin_tuple",
            "fun main() {\n    var t = (1, \"two\")\n    for (var v in t) {\n        say(v);\n    }\n}\n",
        ),
    ] {
        let dir = std::env::temp_dir().join(format!("ntsc_iterator_e2e_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let result = ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            result.is_err(),
            "{name}: non-iterable for-in must not compile"
        );
        let message = format!("{}", result.expect_err("non-iterable for-in must fail"));
        assert!(
            message.contains("for-in is not supported"),
            "{name}: expected a not-supported diagnostic, got: {message}"
        );
    }
}
