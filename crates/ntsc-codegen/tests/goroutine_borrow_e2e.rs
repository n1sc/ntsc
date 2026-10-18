//! Regression tests for `go f(<element read>)`: an argument that borrows a
//! container's owned element (`names[0]`, `grid[0]`) must not enter the
//! goroutine's future as an aliased handle. The future's drop previously
//! reclaimed the source container's registry entry from under it, so later
//! reads returned a dead value. `main` sleeps after the handshake so the
//! completed future's cleanup has certainly run before the read.

use std::path::Path;

fn build_and_run(source: &str, name: &str) -> (String, String, std::process::Output) {
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

    let dir = std::env::temp_dir().join(format!("ntsc_{name}_e2e_test"));
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
    (stdout, stderr, output)
}

fn assert_clean(stdout: &str, stderr: &str, output: &std::process::Output, expected: &[&str]) {
    assert!(
        output.status.success(),
        "binary exited with non-zero status:\n{stdout}\n{stderr}"
    );
    for line in expected {
        assert!(
            stdout.lines().any(|l| l == *line),
            "missing output line {line:?}:\n{stdout}\n{stderr}"
        );
    }
    assert!(
        !stderr.contains("NTSC-W0002"),
        "the container's values must not be leaked or freed early:\n{stderr}"
    );
}

#[test]
fn borrowed_string_element_survives_goroutine_drop() {
    let source = r#"
async fun worker(string msg, chan[int] done) {
    await async.sleep(20)
    say("worker " + msg)
    7 |> done
}

async fun main() -> int {
    var array[string] names = ["alpha", "beta"]
    var chan[int] done = chan.new(1)
    go worker(names[0], done)
    x <| done
    await async.sleep(50)
    say("after " + names[0])
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "goroutine_borrow_string");
    assert_clean(&stdout, &stderr, &output, &["worker alpha", "after alpha"]);
}

#[test]
fn borrowed_array_element_survives_goroutine_drop() {
    let source = r#"
async fun worker(array[string] row, chan[int] done) {
    await async.sleep(20)
    say("worker " + row[0])
    7 |> done
}

async fun main() -> int {
    var array[array[string]] grid = [["a1", "a2"], ["b1", "b2"]]
    var chan[int] done = chan.new(1)
    go worker(grid[0], done)
    x <| done
    await async.sleep(50)
    say("after " + grid[0][0])
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "goroutine_borrow_array");
    assert_clean(&stdout, &stderr, &output, &["worker a1", "after a1"]);
}
