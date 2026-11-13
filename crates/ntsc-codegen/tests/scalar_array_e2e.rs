//! End-to-end regression tests for direct access to nonescaping scalar arrays
//! and for the loop-versioning guard that removes per-access checks.

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

fn run(name: &str, source: &str) -> String {
    run_with_optimization(name, source, inkwell::OptimizationLevel::None)
}

fn run_with_optimization(
    name: &str,
    source: &str,
    optimization: inkwell::OptimizationLevel,
) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let runtime_lib = runtime_lib(rewrite_dir);

    let dir = std::env::temp_dir().join(format!("ntsc_scalar_array_e2e_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!("{name}.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join(format!("{name}_ntsc_test"));

    let program = ntsc_parser::parse(&ntsc_lexer::tokenize(source)).expect("parse failed");
    ntsc_codegen::compile_program(
        &program,
        ntsc_codegen::host_triple(),
        optimization,
        name,
        &dir,
        false,
    )
    .expect("compile_program failed");
    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "binary exited with non-zero status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let _ = std::fs::remove_dir_all(&dir);
    stdout
}

#[test]
fn nonescaping_scalar_arrays_keep_bounds_and_value_semantics() {
    let source = r#"use arrays
fun main() {
    var array[int] a = arrays.fill(0, 4);
    var int i = 0;
    while (i < 4) {
        a[i] = i * 10;
        i = i + 1;
    }
    var int sum = 0;
    i = 0;
    while (i < 4) {
        sum = sum + a[i];
        i = i + 1;
    }
    say("sum: " + sum)

    var array[float] f = arrays.fill(0.0, 3);
    f[0] = 1.5;
    f[1] = f[0] + 2.0;
    f[2] = f[1] * 2.0;
    say("f2: " + f[2])

    var array[bool] b = arrays.fill(false, 2);
    b[1] = true;
    say("b1: " + b[1])

    try {
        say("oob: " + a[4])
    } catch (err) {
        say("read: " + err)
    }
    try {
        a[-1] = 1
    } catch (err) {
        say("write: " + err)
    }

    a[0] = 99;
    say("after: " + a[0])
    say("done")
}
"#;
    assert_eq!(
        run("scalar", source),
        "sum: 60\nf2: 7\nb1: true\nread: array index out of bounds\nwrite: array index out of bounds\nafter: 99\ndone"
    );
}

/// A loop whose proven index range exceeds the array must run the checked
/// fallback, so the exception fires at the same iteration and the writes
/// before it stay visible.
#[test]
fn out_of_range_versioned_loop_keeps_checked_fallbacks_partial_writes() {
    let source = r#"use arrays
fun main() {
    var array[int] a = arrays.fill(0, 3);
    var int i = 0;
    try {
        while (i < 5) {
            a[i] = i + 1;
            i = i + 1;
        }
    } catch (err) {
        say("caught: " + err)
    }
    say("i: " + i)
    say("a0: " + a[0])
    say("a1: " + a[1])
    say("a2: " + a[2])
    say("done")
}
"#;
    assert_eq!(
        run("versioned_fallback", source),
        "caught: array index out of bounds\ni: 3\na0: 1\na1: 2\na2: 3\ndone"
    );
}

/// A base-offset index (`row * n + col`) is proven as a range, not per access.
#[test]
fn affine_base_offsets_are_versioned_without_changing_results() {
    let source = r#"use arrays
fun main() {
    var int n = 4;
    var array[int] grid = arrays.fill(0, n * n);
    var int row = 0;
    while (row < n) {
        var int col = 0;
        while (col < n) {
            grid[row * n + col] = row * n + col;
            col = col + 1;
        }
        row = row + 1;
    }
    var int total = 0;
    var int i = 0;
    while (i < n * n) {
        total = total + grid[i];
        i = i + 1;
    }
    say("total: " + total)
    say("done")
}
"#;
    assert_eq!(run("versioned_affine", source), "total: 120\ndone");
}

/// A heap sift-down loop: `position * 2 + 1 < size` bounds every index, so
/// the guard-bounded proof removes all of its checks.
fn sift_program(capacity: i64, size: i64) -> String {
    format!(
        r#"use arrays
fun main() {{
    var array[int] heap = arrays.fill(0, {capacity});
    var int i = 0;
    while (i < {capacity}) {{
        heap[i] = i + 1;
        i = i + 1;
    }}
    var int size = {size};
    var int position = 0;
    var int last = 1000;
    while (position * 2 + 1 < size) {{
        var int child = position * 2 + 1;
        if (child + 1 < size && heap[child + 1] < heap[child]) {{
            child = child + 1;
        }}
        if (last <= heap[child]) {{ break; }}
        heap[position] = heap[child];
        position = child;
    }}
    heap[position] = last;
    var int total = 0;
    i = 0;
    while (i < size) {{
        total = total + heap[i] * (i + 1);
        i = i + 1;
    }}
    say("position: " + position)
    say("total: " + total)
    say("done")
}}
"#
    )
}

/// The guard-bounded fast body and the checked fallback must agree. The
/// second run pads the bound past the array, so the preheader rejects it and
/// the checked body runs instead.
#[test]
fn guard_bounded_sift_loop_matches_its_checked_fallback() {
    let expected = "position: 7\ntotal: 8161\ndone";
    assert_eq!(
        run("bounded_sift_fast", &sift_program(16, 8)),
        expected,
        "unchecked body"
    );
    assert_eq!(
        run("bounded_sift_slow", &sift_program(8, 8)),
        expected,
        "checked fallback"
    );
}

/// The same loop over a bound larger than the array must still throw: the
/// proof cannot cover it, so the checked body runs and catches the index
/// that leaves the array.
#[test]
fn guard_bounded_sift_loop_keeps_its_checks_when_the_bound_overflows_the_array() {
    let source = r#"use arrays
fun main() {
    var array[int] heap = arrays.fill(0, 8);
    var int i = 0;
    while (i < 8) {
        heap[i] = i + 1;
        i = i + 1;
    }
    var int size = 10;
    var int position = 0;
    var int last = 1000;
    try {
        while (position * 2 + 1 < size) {
            var int child = position * 2 + 1;
            if (child + 1 < size && heap[child + 1] < heap[child]) {
                child = child + 1;
            }
            if (last <= heap[child]) { break; }
            heap[position] = heap[child];
            position = child;
        }
    } catch (err) {
        say("caught: " + err)
    }
    say("done")
}
"#;
    assert_eq!(
        run("bounded_sift_trap", source),
        "caught: array index out of bounds\ndone"
    );
}

#[test]
fn unproven_offsets_and_early_updates_keep_bounds_checks() {
    for (name, body) in [
        (
            "data_dependent_offset",
            "var int offset = a[i]; b[i + offset] = 7; i = i + 1;",
        ),
        ("early_update", "i = i + 1; b[i] = 7;"),
    ] {
        let source = format!(
            r#"use arrays
fun main() {{
    var array[int] a = arrays.fill(4, 4);
    var array[int] b = arrays.fill(0, 4);
    var int i = 0;
    try {{
        while (i < 4) {{ {body} }}
    }} catch (err) {{
        say("caught: " + err)
    }}
    say("done")
}}
"#
        );
        for (suffix, optimization) in [
            ("debug", inkwell::OptimizationLevel::None),
            ("release", inkwell::OptimizationLevel::Aggressive),
        ] {
            assert_eq!(
                run_with_optimization(&format!("{name}_{suffix}"), &source, optimization),
                "caught: array index out of bounds\ndone"
            );
        }
    }
}

#[test]
fn bounded_loops_keep_overflow_checks_and_wrapped_index_checks() {
    for (name, start, index, update, debug_error) in [
        (
            "guard_overflow",
            "9223372036854775807",
            "i * 2 + 1",
            "i + 1",
            "integer multiplication overflow",
        ),
        (
            "update_overflow",
            "1",
            "i",
            "i + 9223372036854775807",
            "integer addition overflow",
        ),
    ] {
        let source = format!(
            r#"use arrays
fun main() {{
    var array[int] a = arrays.fill(0, 4);
    var int i = {start};
    try {{
        while (i * 2 + 1 < 4) {{
            a[{index}] = 7;
            i = {update};
        }}
    }} catch (err) {{
        say("caught: " + err)
    }}
    say("done")
}}
"#
        );
        assert_eq!(
            run(&format!("{name}_debug"), &source),
            format!("caught: {debug_error}\ndone")
        );
        assert_eq!(
            run_with_optimization(
                &format!("{name}_release"),
                &source,
                inkwell::OptimizationLevel::Aggressive,
            ),
            "caught: array index out of bounds\ndone"
        );
    }
}

#[test]
fn zero_scaled_indices_do_not_hide_intermediate_overflow() {
    let source = r#"use arrays
fun main() {
    var array[int] a = arrays.fill(0, 4);
    var int i = 1;
    try {
        while (i * 2 + 1 < 4) {
            a[(i * 9223372036854775807 + 1) * 0] = 7;
            i = i + 1;
        }
    } catch (err) {
        say("caught: " + err)
    }
    say("done")
}
"#;
    assert_eq!(
        run("zero_scaled_index_debug", source),
        "caught: integer addition overflow\ndone"
    );
    assert_eq!(
        run_with_optimization(
            "zero_scaled_index_release",
            source,
            inkwell::OptimizationLevel::Aggressive,
        ),
        "done"
    );
}
