//! End-to-end regression tests for checked numeric semantics:
//!
//! - Invalid integer division and remainder (zero divisor or `i64::MIN / -1`)
//!   throw a catchable exception instead of relying on LLVM poison semantics.
//! - Out-of-bounds array writes (`a[i] = v`) throw like out-of-bounds reads
//!   instead of silently no-oping.
//!
//! Regression coverage: these used to be silent (UB or dropped writes).

use std::path::Path;

#[test]
fn checked_arithmetic_semantics_e2e() {
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

    let source = r#"use arrays
fun main() {
    // Division by zero throws a catchable exception.
    try {
        var q = 10 / 0;
        say("d1: WRONG no throw")
    } catch (err) {
        say("d1: caught " + err)
    }
    try {
        var r = 10 % 0;
        say("d2: WRONG no throw")
    } catch (err) {
        say("d2: caught " + err)
    }
    var zero = 0;
    try {
        var q2 = 7 / zero;
        say("d3: WRONG no throw")
    } catch (err) {
        say("d3: caught " + err)
    }
    // Division still works when the divisor is nonzero.
    say("d4: " + (10 / 2) + " " + (10 % 3) + " " + (-7 / 2))
    var min = -9223372036854775807 - 1;
    try {
        var overflow = min / -1;
        say("d5: WRONG no throw")
    } catch (err) {
        say("d5: caught " + err)
    }
    try {
        var overflow_rem = min % -1;
        say("d6: WRONG no throw")
    } catch (err) {
        say("d6: caught " + err)
    }

    // Out-of-bounds array writes throw, like reads.
    var a = [1, 2, 3];
    try {
        a[5] = 9;
        say("s1: WRONG no throw")
    } catch (err) {
        say("s1: caught " + err)
    }
    try {
        a[-1] = 9;
        say("s2: WRONG no throw")
    } catch (err) {
        say("s2: caught " + err)
    }
    try {
        var g = a[-1];
        say("s3: WRONG no throw")
    } catch (err) {
        say("s3: caught " + err)
    }
    a[1] = 42;
    say("s4: " + a[1])
    say("s5: " + arrays.length(a))

    say("done")
}
"#;

    let dir = std::env::temp_dir().join("ntsc_checked_arithmetic_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!("checked.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("checked_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "checked", &dir)
        .expect("compile_source failed");
    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");
    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "binary exited with non-zero status"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    let expected_lines = [
        "d1: caught division by zero",
        "d2: caught division by zero",
        "d3: caught division by zero",
        "d4: 5 1 -3",
        "d5: caught integer division overflow",
        "d6: caught integer division overflow",
        "s1: caught array index out of bounds",
        "s2: caught array index out of bounds",
        "s3: caught array index out of bounds",
        "s4: 42",
        "s5: 3",
    ];
    for line in expected_lines {
        assert!(
            stdout.lines().any(|l| l == line),
            "missing output line {line:?}:\n{stdout}"
        );
    }
    assert!(
        stdout.trim().ends_with("done"),
        "unexpected output:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `float` reaching an `int` slot converts (saturating) instead of being
/// emitted as a raw double. The type checker types a stdlib call like
/// `time.now()` loosely enough that this reaches codegen, and passing the
/// double straight through produced a module that failed to verify.
#[test]
fn a_float_argument_reaching_an_int_parameter_converts() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let runtime_lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );
    let source = r#"use time
use fmt
fun takes_int(int value) -> int {
    return value + 1
}
fun zero_over() -> float {
    return 0.0 / 0.0
}
fun main() {
    say("" + takes_int(time.now()))
    say(fmt.i64_to_str(takes_int(2.9)))
    say("nan:" + fmt.i64_to_str(takes_int(zero_over())))
}
"#;
    let dir = std::env::temp_dir().join("ntsc_float_arg_int_param");
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!("floatarg.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("floatarg_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "floatarg", &dir)
        .expect("compile_source failed");
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");
    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "binary exited with non-zero status"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut lines = stdout.lines();
    let first: i64 = lines.next().expect("one line").parse().expect("a number");
    // `time.now()` is milliseconds since the epoch, so the sum is large.
    assert!(first > 1_700_000_000_000, "unexpected value: {first}");
    // The literal truncates 2.9 to 2 before the call.
    assert_eq!(lines.next(), Some("3"), "unexpected output:\n{stdout}");
    // NaN converts to 0, as the reference documents.
    assert_eq!(lines.next(), Some("nan:1"), "unexpected output:\n{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}
