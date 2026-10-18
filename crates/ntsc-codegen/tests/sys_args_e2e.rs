//! End-to-end regression test for `sys.args()` returning an argument
//! array (Go-style `os.Args`): element 0 is the program name, user
//! arguments follow, and `arrays.length`/`arrays.get` work on the result.

use std::path::Path;

#[test]
fn sys_args_e2e() {
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

    let source = r#"use sys
use arrays
use strings
fun main() -> int {
    var args = sys.args()
    say("count=" + arrays.length(args));
    say("prog=" + !strings.is_empty(arrays.get(args, 0)));
    if (arrays.length(args) < 2) {
        say("usage: myprog <name>");
        return 1;
    }
    say("hello, " + arrays.get(args, 1));
    return 0;
}
"#;

    let dir = std::env::temp_dir().join("ntsc_sys_args_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!("sys_args.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("sys_args_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "sys_args", &dir)
        .expect("compile_source failed");
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    // With user arguments: argv is [prog, test, hello].
    let output = std::process::Command::new(&bin_path)
        .args(["test", "hello"])
        .output()
        .expect("failed to run binary");
    assert!(output.status.success(), "binary exited non-zero with args");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    for line in ["count=3", "prog=true", "hello, test"] {
        assert!(
            stdout.lines().any(|l| l == line),
            "missing {line:?}:\n{stdout}"
        );
    }

    // Without user arguments: usage path, argv is [prog].
    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success(), "binary should fail without args");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    for line in ["count=1", "prog=true", "usage: myprog <name>"] {
        assert!(
            stdout.lines().any(|l| l == line),
            "missing {line:?}:\n{stdout}"
        );
    }

    for args in [vec!["test"], Vec::new()] {
        let output = std::process::Command::new(&bin_path)
            .args(&args)
            .output()
            .expect("failed to re-run binary");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("NTSC-W0002"),
            "sys.args() program leaked registry handles (args {args:?}): {stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
