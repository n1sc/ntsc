//! End-to-end test for the `csv` standard library module and the
//! `testing.bench` benchmark harness.

use std::path::Path;
use std::process::Stdio;

fn runtime_lib() -> std::path::PathBuf {
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
    assert!(
        runtime_lib.exists(),
        "runtime lib not found at {runtime_lib:?}"
    );
    runtime_lib
}

fn build_and_run(source: &str, test_name: &str, dir: &std::path::Path) -> std::process::Output {
    let runtime_lib = runtime_lib();
    let obj_path = dir.join(format!("{test_name}.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join(format!("{test_name}_bin"));

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), test_name, dir)
        .expect("compile_source failed");
    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");
    assert!(bin_path.exists(), "binary not produced");

    std::process::Command::new(&bin_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run binary")
}

fn build_and_run_tests(
    source: &str,
    test_name: &str,
    dir: &std::path::Path,
) -> std::process::Output {
    let runtime_lib = runtime_lib();
    let obj_path = dir.join(format!("{test_name}.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join(format!("{test_name}_bin"));

    ntsc_codegen::compile_tests(source, ntsc_codegen::host_triple(), test_name, dir)
        .expect("compile_tests failed");
    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");
    assert!(bin_path.exists(), "binary not produced");

    std::process::Command::new(&bin_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run binary")
}

#[test]
fn csv_parse_and_stringify() {
    let source = "use csv\nfun main() {\n    var csv_input = \"name,age,city\\nAlice,30,NYC\\nBob,25,LA\";\n    var parsed = csv.parse(csv_input);\n    say(\"csv0: \" + parsed);\n\n    var output = csv.stringify(parsed);\n    say(\"csv1: \" + output);\n\n    var empty = csv.parse(\"\");\n    say(\"csv2: \" + empty);\n\n    try {\n        var bad = csv.stringify(\"{\\\"a\\\":1}\");\n        say(\"unreached-csv-stringify\");\n    } catch (err) {\n        say(\"csv3: \" + err);\n    }\n\n    say(\"done\")\n}\n";

    let dir = std::env::temp_dir().join("ntsc_csv_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let output = build_and_run(source, "csv_e2e", &dir);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        output.status.success(),
        "binary exited with non-zero status:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("csv0:"), "missing csv0 output:\n{stdout}");
    assert!(stdout.contains("csv1:"), "missing csv1 output:\n{stdout}");
    assert!(
        stdout.contains("csv2: []"),
        "csv2 should be empty array:\n{stdout}"
    );
    assert!(
        stdout.contains("csv3:") && stdout.contains("csv.stringify"),
        "csv3 should contain error:\n{stdout}"
    );
    assert!(stdout.trim().ends_with("done"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csv_roundtrip() {
    let source = "use csv\nfun main() {\n    var input = \"name,age\\nAlice,30\\nBob,25\";\n    var data = csv.parse(input);\n    var output = csv.stringify(data);\n    say(\"rt0: \" + output);\n\n    var reparsed = csv.parse(output);\n    say(\"rt1: \" + reparsed);\n\n    say(\"done\")\n}\n";

    let dir = std::env::temp_dir().join("ntsc_csv_roundtrip_e2e");
    std::fs::create_dir_all(&dir).unwrap();

    let output = build_and_run(source, "csv_roundtrip", &dir);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    assert!(
        output.status.success(),
        "binary exited with non-zero status:\n{stdout}"
    );
    assert!(stdout.contains("rt0:"), "missing rt0 output:\n{stdout}");
    assert!(stdout.contains("rt1:"), "missing rt1 output:\n{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn benchmark_harness() {
    let source = "use testing\n\nfun add_one(int x) -> int {\n    return x + 1;\n}\n\ntest bench_runs {\n    var us = testing.bench(add_one, 1000, 100);\n    testing.assert_true(us >= 0.0);\n    say(\"bench0: \" + us);\n}\n\nfun main() {\n    say(\"main should not run in test mode\")\n}\n";

    let dir = std::env::temp_dir().join("ntsc_bench_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let output = build_and_run_tests(source, "bench_e2e", &dir);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        output.status.success(),
        "binary exited with non-zero status:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("PASS bench_runs"),
        "expected PASS bench_runs:\n{stdout}"
    );
    assert!(
        stdout.contains("bench0:"),
        "missing bench0 output:\n{stdout}"
    );
    assert!(
        stdout.contains(ntsc_codegen::SUMMARY_MARKER),
        "missing summary:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
