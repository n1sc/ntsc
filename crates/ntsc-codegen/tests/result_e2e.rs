//! End-to-end tests: the built-in `result[.., ..]` type, the `?` propagation
//! operator, throw-to-Err integration, the result/option combinators, and
//! destructuring `match` arms over results.

use std::path::Path;

/// Build `source` against the debug runtime and return the program's stdout.
fn compile_and_run(source: &str, name: &str) -> String {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let runtime_lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );

    // Ensure runtime is built.
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

    let dir = std::env::temp_dir().join(format!("ntsc_{name}_e2e_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!("{name}.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join(name);

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &dir)
        .expect("compile_source failed");

    assert!(obj_path.exists(), "object file not produced");

    // Link.
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    assert!(bin_path.exists(), "binary not produced");

    // Run and capture output.
    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "binary exited with non-zero status: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Cleanup.
    let _ = std::fs::remove_dir_all(&dir);

    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn result_e2e() {
    let source = r#"use fmt
fun double_it(int x) -> int {
    return x * 2
}

fun half(int x) -> result[int, string] {
    if (x % 2 == 0) {
        return Ok(x / 2)
    }
    return Err("odd input")
}

fun recover(string msg) -> result[int, string] {
    return Ok(99)
}

fun parse_num(bool good) -> result[int, string] {
    if (good) {
        return Ok(42)
    }
    return Err("not a number")
}

fun caller_ok() -> result[int, string] {
    var n = parse_num(true)?
    return Ok(n)
}

fun caller_err() -> result[int, string] {
    var n = parse_num(false)?
    return Ok(n)
}

fun thrower() -> result[string, string] {
    throw "kaboom"
    return Ok("never")
}

fun converter() -> result[int, string] {
    var code = Err(7)?
    return Ok(code)
}

// Re-wraps an error payload as an Ok value so tests can observe it.
fun reveal(string msg) -> result[string, string] {
    return Ok(msg)
}

fun reveal_int(int code) -> result[int, int] {
    return Ok(code)
}

fun main() -> int {
    say(fmt.i64_to_str(Ok(5).unwrap_or(0)))
    var e = Err("boom")
    say(e.unwrap_or("safe"))

    var ok = caller_ok()
    say(fmt.i64_to_str(ok.unwrap_or(0)))
    var bad = caller_err()
    say(fmt.i64_to_str(bad.unwrap_or(-1)))

    var m = Ok(21).map(fun(int x) -> int { return x * 2 })
    say(fmt.i64_to_str(m.unwrap_or(0)))

    var c1 = Ok(8).and_then(half)
    say(fmt.i64_to_str(c1.unwrap_or(-2)))
    var c2 = Ok(7).and_then(half)
    say(fmt.i64_to_str(c2.unwrap_or(-3)))

    var c3 = Err("lost").or_else(recover)
    say(fmt.i64_to_str(c3.unwrap_or(-4)))

    var t = thrower()
    say(t.or_else(reveal).unwrap_or("never"))

    say(converter().or_else(reveal).unwrap_or("none"))

    var option[int] none = nil
    say(fmt.i64_to_str(none.ok_or(-5).or_else(reveal_int).unwrap_or(-6)))
    none = 3
    say(fmt.i64_to_str(none.ok_or(-5).unwrap_or(-6)))

    var option[string] empty = nil
    say(empty.ok_or_else(fun() -> string { return "made" }).or_else(reveal).unwrap_or("?"))

    return 0
}
"#;

    let stdout = compile_and_run(source, "result");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    let expected = [
        "5",      // Ok(5).unwrap_or(0)
        "safe",   // Err("boom").unwrap_or("safe")
        "42",     // ? through caller_ok
        "-1",     // ? propagates parse failure; unwrap_or default
        "42",     // map over Ok(21)
        "4",      // and_then on even input
        "-3",     // and_then short-circuits the odd-input error
        "99",     // or_else recovers from Err
        "kaboom", // throw inside a result function becomes Err (revealed via or_else)
        "7",      // int error converted to the string error type by ?
        "-5",     // option.ok_or turns nil into Err(-5)
        "3",      // option.ok_or passes a value through as Ok
        "made",   // option.ok_or_else builds the error lazily
    ];
    assert_eq!(lines, expected, "unexpected program output:\n{stdout}");
}

#[test]
fn result_payload_ownership_e2e() {
    // Regression: `Err("..." + x)` and `Ok(call())` used to store the payload
    // and then drop the same fresh temporary, leaving the cell holding a freed
    // handle, so the message read back empty. A borrowed payload must be
    // copied instead of being aliased.
    let source = r#"use strings
fun fail_concat(view string name) -> result[string, string] {
    return Err("unknown package '" + copy(name) + "'")
}

fun fail_view(view string name) -> result[string, string] {
    return Err(name)
}

fun fail_in_try(view string name) -> result[string, string] {
    try {
        if (strings.is_empty(name)) {
            return Err("empty name")
        }
        return Ok("name:" + copy(name))
    } catch (err) {
        return Err("caught: " + err)
    }
}

fun reveal(string msg) -> result[string, string] {
    return Ok(msg)
}

fun main() -> int {
    say(fail_concat("json").or_else(reveal).unwrap_or("?"))
    say(fail_view("plain view").or_else(reveal).unwrap_or("?"))
    say(fail_in_try("json").or_else(reveal).unwrap_or("?"))
    say(fail_in_try("").or_else(reveal).unwrap_or("?"))
    return 0
}
"#;

    let stdout = compile_and_run(source, "result_payload_ownership");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    let expected = [
        "unknown package 'json'", // concatenated error payload survives
        "plain view",             // borrowed payload is copied into the cell
        "name:json",              // Ok(...) inside try keeps its payload
        "empty name",             // Err(...) inside try keeps its payload
    ];
    assert_eq!(lines, expected, "unexpected program output:\n{stdout}");
}

#[test]
fn result_match_e2e() {
    // Destructuring match over results: variant binders, `_` payload skip,
    // guards reading the bound payload, default fallback, and a fresh call
    // scrutinee (which must not leak its cell).
    let source = r#"use fmt
fun parse(int n) -> result[int, string] {
    if (n < 0) { return Err("negative") }
    return Ok(n * 2)
}

fun describe(result[int, string] r) -> string {
    match (r) {
        case Ok(v) => { return "ok:" + fmt.i64_to_str(v) }
        case Err(e) => { return "err:" + e }
    }
}

fun main() -> int {
    var good = parse(21)
    match (good) {
        case Ok(v) => { say("ok: " + fmt.i64_to_str(v)) }
        case Err(e) => { say("err: " + e) }
        default => { say("unreachable") }
    }

    match (parse(-5)) {
        case Ok(v) => { say("no: " + fmt.i64_to_str(v)) }
        case Err(e) if e == "negative" => { say("caught: " + e) }
        case Err(_) => { say("other error") }
    }

    match (parse(7)) {
        case Ok(v) if v > 100 => { say("huge") }
        case Ok(v) => { say("small: " + fmt.i64_to_str(v)) }
        case Err(_) => { say("none") }
    }

    say(describe(Err("plain")))
    say(describe(Ok(3)))
    return 0
}
"#;

    let stdout = compile_and_run(source, "result_match");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    let expected = [
        "ok: 42",           // owned scrutinee, Ok binder sees the int payload
        "caught: negative", // guard reads the bound error; fresh cell released
        "small: 14",        // first arm's guard fails, second Ok arm matches
        "err:plain",        // binder scoped per arm through a call boundary
        "ok:3",
    ];
    assert_eq!(lines, expected, "unexpected program output:\n{stdout}");
}
