//! End-to-end regression tests for per-iteration drops in loop bodies.
//!
//! A block that binds owned values inside a loop body re-executes every
//! iteration, so deferring those values to the function-exit drop frees
//! only the last iteration: each re-store overwrites the previous handle.
//! A 500-iteration loop over a tuple-returning call leaked 499 arrays.

use std::path::Path;

fn build_and_run(name: &str, source: &str) -> (bool, String, String) {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let runtime_lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );

    if !runtime_lib.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "ntsc-runtime"])
            .current_dir(rewrite_dir)
            .status()
            .expect("failed to run cargo");
        assert!(status.success(), "failed to build ntsc-runtime");
    }

    let dir = std::env::temp_dir().join(format!("ntsc_loop_drop_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!("{name}.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join(format!("{name}_ntsc_test"));

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &dir)
        .expect("compile_source failed");
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    (output.status.success(), stdout, stderr)
}

#[test]
fn tuple_destructured_values_drop_every_iteration() {
    let source = r#"use arrays
fun make_pair(int n) -> (array[int], string) {
    var array[int] xs = arrays.fill(0, 0)
    arrays.push(xs, n)
    arrays.push(xs, n + 1)
    return (xs, "v")
}
fun main() {
    var i = 0
    while (i < 500) {
        var (got, label) = make_pair(i)
        if (arrays.length(got) != 2 || label != "v") {
            say("WRONG")
        }
        i = i + 1
    }
    say("done")
}
"#;
    let (ok, stdout, stderr) = build_and_run("tuple_loop", source);
    assert!(ok, "binary exited with non-zero status");
    assert!(
        stdout.trim().ends_with("done"),
        "unexpected output:\n{stdout}"
    );
    assert!(
        !stdout.contains("WRONG"),
        "destructured values were wrong:\n{stdout}"
    );
    assert!(
        !stderr.contains("memory leak detected"),
        "loop-carried tuple values leaked:\n{stderr}"
    );
}

#[test]
fn plain_owned_values_drop_every_iteration() {
    let source = r#"use arrays
fun main() {
    var i = 0
    while (i < 500) {
        var array[int] xs = arrays.fill(0, 0)
        arrays.push(xs, i)
        i = i + 1
    }
    say("done")
}
"#;
    let (ok, stdout, stderr) = build_and_run("plain_loop", source);
    assert!(ok, "binary exited with non-zero status");
    assert!(
        stdout.trim().ends_with("done"),
        "unexpected output:\n{stdout}"
    );
    assert!(
        !stderr.contains("memory leak detected"),
        "loop-carried values leaked:\n{stderr}"
    );
}

#[test]
fn for_in_loop_values_drop_every_iteration() {
    let source = r#"use arrays
fun main() {
    var array[int] src = arrays.fill(0, 0)
    var i = 0
    while (i < 200) {
        arrays.push(src, i)
        i = i + 1
    }
    for (var v in src) {
        var array[int] scratch = arrays.fill(0, 0)
        arrays.push(scratch, v)
    }
    say("done")
}
"#;
    let (ok, stdout, stderr) = build_and_run("forin_loop", source);
    assert!(ok, "binary exited with non-zero status");
    assert!(
        stdout.trim().ends_with("done"),
        "unexpected output:\n{stdout}"
    );
    assert!(
        !stderr.contains("memory leak detected"),
        "for-in loop values leaked:\n{stderr}"
    );
}

#[test]
fn nested_call_results_drop_every_iteration() {
    // A call result passed straight into another call is a temporary with
    // no owning slot. stdlib functions missing from the return-type table
    // defaulted to `Any`, which the freshness check does not recognize, so
    // the temporary was never released: `json.get(json.at(..))` in a loop
    // leaked one string per element parsed.
    let source = r#"use arrays
use json
use strings
fun main() {
    var out = "["
    var k = 0
    while (k < 100) {
        if (k > 0) {
            out = out + ","
        }
        out = out + "{\"id\":\"x\",\"type\":\"spotify\"}"
        k = k + 1
    }
    var body = out + "]"
    var round = 0
    while (round < 5) {
        var n = json.length(body)
        var i = 0
        while (i < n) {
            var t = json.get(json.at(body, i), "type")
            if (t == "") {
            }
            i = i + 1
        }
        round = round + 1
    }
    say("done")
}
"#;
    let (ok, stdout, stderr) = build_and_run("nested_call", source);
    assert!(ok, "binary exited with non-zero status");
    assert!(
        stdout.trim().ends_with("done"),
        "unexpected output:\n{stdout}"
    );
    assert!(
        !stderr.contains("memory leak detected"),
        "nested call temporaries leaked:\n{stderr}"
    );
}
