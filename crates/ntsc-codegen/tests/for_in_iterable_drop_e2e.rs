//! Fresh for-in iterables stay owned across iterations and every loop exit.

use std::path::Path;

fn assert_output_without_leaks(name: &str, source: &str, expected: &str) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let runtime_lib = workspace
        .join("target/debug")
        .join(ntsc_codegen::runtime_lib_name());
    if !runtime_lib.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "ntsc-runtime"])
            .current_dir(workspace)
            .status()
            .expect("failed to build runtime");
        assert!(status.success(), "runtime build failed");
    }

    let dir = std::env::temp_dir().join(format!(
        "ntsc_for_in_iterable_drop_{}_{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = format!(
        "fun words() -> array[string] {{ return [\"skip\", \"keep\", \"last\"] }}\n{source}"
    );
    ntsc_codegen::compile_source(&source, ntsc_codegen::host_triple(), name, &dir)
        .expect("compile failed");
    let binary = dir.join(name);
    ntsc_codegen::link_binary(
        &dir.join(format!("{name}.{}", ntsc_codegen::object_extension())),
        &runtime_lib,
        &binary,
    )
    .expect("link failed");
    let output = std::process::Command::new(binary)
        .output()
        .expect("failed to run binary");
    std::fs::remove_dir_all(dir).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "binary failed: {stderr}");
    assert_eq!(stdout.trim(), expected, "stderr: {stderr}");
    assert!(
        !stderr.contains("memory leak detected"),
        "for-in iterable leaked: {stderr}"
    );
}

#[test]
fn fresh_string_array_drops_on_early_return() {
    assert_output_without_leaks(
        "early_return",
        r#"fun find_expected() -> bool {
    for (var line in words()) {
        if (line == "keep") {
            return true
        }
    }
    return false
}
fun main() {
    var i = 0
    while (i < 50) {
        if (!find_expected()) {
            say("WRONG")
        }
        i = i + 1
    }
    say("done")
}
"#,
        "done",
    );
}

#[test]
fn nested_fresh_iterables_drop_on_early_return() {
    assert_output_without_leaks(
        "nested_return",
        r#"fun find_nested() -> int {
    for (var outer in words()) {
        if (outer == "skip") {
            continue
        }
        for (var inner in words()) {
            if (inner == "keep") {
                return 42
            }
        }
    }
    return 0
}
fun main() {
    say("" + find_nested())
}
"#,
        "42",
    );
}

#[test]
fn fresh_iterables_drop_on_fallthrough_and_empty_iteration() {
    assert_output_without_leaks(
        "fallthrough",
        r#"fun main() {
    var count = 0
    var round = 0
    while (round < 20) {
        for (var word in words()) {
            for (var suffix in ["a", "b"]) {
                count = count + 1
            }
        }
        round = round + 1
    }
    for (var unused in []) {
        say("WRONG")
    }
    say("" + count)
}
"#,
        "120",
    );
}

#[test]
fn fresh_iterables_survive_continue_and_drop_on_break() {
    assert_output_without_leaks(
        "continue_break",
        r#"fun main() {
    var count = 0
    for (var outer in words()) {
        if (outer == "skip") {
            continue
        }
        for (var inner in words()) {
            if (inner == "skip") {
                continue
            }
            if (inner == "keep") {
                count = count + 1
                break
            }
        }
    }
    say("" + count)
}
"#,
        "2",
    );
}

#[test]
fn borrowed_iterables_remain_live_after_loop_exits() {
    assert_output_without_leaks(
        "borrowed",
        r#"fun contains(view array[string] source) -> bool {
    for (var word in source) {
        if (word == "keep") {
            return true
        }
    }
    return false
}
fun main() {
    var source = words()
    say("" + contains(source))
    for (var word in source) {
        if (word == "skip") {
            continue
        }
        say(word)
        break
    }
    for (var word in source) {
        say(word)
    }
    say(source[2])
}
"#,
        "true\nkeep\nskip\nkeep\nlast\nlast",
    );
}

#[test]
fn fresh_iterable_drops_when_body_throws() {
    assert_output_without_leaks(
        "throw",
        r#"fun fail() {
    for (var word in words()) {
        throw "failed"
    }
}
fun main() {
    try {
        fail()
    } catch (error) {
        say(error)
    }
}
"#,
        "failed",
    );
}

#[test]
fn nested_fresh_iterables_drop_when_call_throws() {
    assert_output_without_leaks(
        "nested_throw",
        r#"fun fail() {
    throw "failed"
}
fun iterate() {
    for (var outer in words()) {
        for (var inner in words()) {
            fail()
        }
    }
}
fun main() {
    try {
        iterate()
    } catch (error) {
        say(error)
    }
}
"#,
        "failed",
    );
}

#[test]
fn caught_throw_does_not_leak_when_iterable_slot_is_reused() {
    assert_output_without_leaks(
        "caught_throw",
        r#"fun main() {
    var count = 0
    while (count < 20) {
        try {
            for (var word in words()) {
                throw "failed"
            }
        } catch (error) {
            count = count + 1
        }
    }
    say("" + count)
}
"#,
        "20",
    );
}
