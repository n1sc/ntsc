//! End-to-end regression test for the standard library modules added in the
//! 4.4 "Stdlib Coverage Gaps" work: `os`, `io`, `net`, `encoding`, `hash`,
//! `random`, `sort`, and `testing`.
//!
//! Coverage:
//! - `os`: environment variables, path manipulation.
//! - `io`: open/close/read/write/seek/tell/read_all over a temp file.
//! - `net`: localhost TCP echo and UDP loopback.
//! - `encoding`/`hash`: base64/hex roundtrips and known checksums.
//! - `random`: seeded PRNG, shuffle, and weighted selection.
//! - `sort`: stable_sort, comparator-driven sort_by, binary_search.
//! - `testing`: passing assertions and thrown failures caught by `try`/`catch`.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;
#[test]
fn stdlib_modules_e2e() {
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
use encoding
use hash
use io
use net
use os
use random
use sort
use strings
use testing
fun main() {
    // ── os ────────────────────────────────────────────────────────────────
    os.setenv("NTSC_E2E_VAR", "hello");
    say("os0: " + os.getenv("NTSC_E2E_VAR"));
    say("os1: " + os.has_env("NTSC_E2E_VAR"));
    os.unsetenv("NTSC_E2E_VAR");
    say("os2: " + os.has_env("NTSC_E2E_VAR"));
    say("os3: [" + os.getenv("NTSC_E2E_VAR") + "]");
    say("os4: " + os.path_join("a", "b"));
    say("os5: " + os.path_dirname("/a/b/c.txt"));
    say("os6: " + os.path_basename("/a/b/c.txt"));
    say("os7: " + os.path_ext("/a/b/c.txt"));
    say("os8: " + os.path_stem("/a/b/c.txt"));
    say("os9: " + os.is_abs("/a/b"));
    say("os10: " + os.is_abs("a/b"));
    say("os11: " + os.separator());
    say("os12: " + strings.is_empty(os.temp_dir()));

    // ── encoding / hash ───────────────────────────────────────────────────
    say("e0: " + encoding.base64_encode("hello"));
    say("e1: " + encoding.base64_decode("aGVsbG8="));
    say("e2: " + encoding.hex_encode("AB"));
    say("e3: " + encoding.hex_decode("4142"));
    say("e4: " + encoding.utf8_valid("ok"));
    say("h0: " + hash.sha256("hello"));
    say("h1: " + hash.crc32("123456789"));

    // ── io ────────────────────────────────────────────────────────────────
    say("input: [" + io.input() + "]");
    var fpath = os.temp_file("ntsc-e2e-");
    var f = io.open(fpath, "w+");
    // String literals are raw: "line one\n" stores a literal backslash-n.
    say("io0: " + io.write(f, "line one\n"));
    say("io1: " + io.write_line(f, "line two"));
    io.flush(f);
    io.seek(f, 0, 0);
    say("io2: [" + strings.trim(io.read_line(f)) + "]");
    say("io3: [" + strings.trim(io.read_line(f)) + "]");
    say("io4: " + io.eof(f));
    io.seek(f, 0, 0);
    say("io5: " + strings.replace(io.read_all(f), "\n", "|"));
    say("io6: " + io.tell(f));
    io.seek(f, 0, 0);
    say("io7: " + io.read(f, 4));
    io.close(f);
    try {
        var bad = io.open("/nonexistent_dir_xyz_123/ntsc-e2e", "r");
        say("unreached-io-open");
    } catch (err) {
        say("io8: " + err);
    }

    // ── net ───────────────────────────────────────────────────────────────
    var server = net.tcp_listen(0);
    var port = net.local_port(server);
    var client = net.tcp_connect("127.0.0.1", port);
    var conn = net.tcp_accept(server);
    net.send(conn, "hello");
    say("n0: " + net.recv(client, 5));
    net.send_line(conn, "line");
    say("n1: " + strings.trim(net.recv_line(client)));
    net.send(client, "world");
    say("n2: " + net.recv(conn, 5));
    net.close(client);
    net.close(conn);
    net.close(server);

    var u = net.udp_bind(0);
    var uport = net.local_port(u);
    net.udp_send(u, "127.0.0.1", uport, "ping");
    say("n3: " + net.udp_recv(u, 64));
    net.close(u);

    try {
        var c2 = net.tcp_connect("127.0.0.1", 1);
        say("unreached-net-connect");
    } catch (err) {
        say("n4: " + err);
    }

    // ── sort ──────────────────────────────────────────────────────────────
    var nums = [3, 1, 2];
    var sorted = sort.stable_sort(nums);
    say("s0: " + sorted[0] + sorted[1] + sorted[2]);
    var desc = sort.sort_by(nums, fun(int a, int b) -> bool { return a > b; });
    say("s1: " + desc[0] + desc[1] + desc[2]);
    say("s2: " + sort.binary_search(sorted, 2));
    say("s3: " + sort.binary_search(sorted, 9));
    var strs = ["b", "a", "c"];
    var ss = sort.stable_sort(strs);
    say("s4: " + ss[0] + ss[1] + ss[2]);
    var floats = [2.5, 1.5, 3.5];
    var fs = sort.stable_sort(floats);
    say("s5: " + fs[0]);

    // ── random ────────────────────────────────────────────────────────────
    random.seed(42);
    var ri = random.int(1, 6);
    say("r0: " + (ri >= 1 && ri <= 6));
    var rf = random.float();
    say("r1: " + (rf >= 0.0 && rf < 1.0));
    say("r2: " + (random.bool() == true || random.bool() == false));
    var arr5 = [1, 2, 3, 4, 5];
    var sh = random.shuffle(arr5);
    say("r3: " + arrays.length(sh));
    say("r4: " + random.weighted([0, 0, 5]));
    try {
        var z = random.weighted([0, 0, 0]);
        say("unreached-random-weighted");
    } catch (err) {
        say("r5: " + err);
    }
    try {
        var i2 = random.int(10, 5);
        say("unreached-random-int");
    } catch (err) {
        say("r6: " + err);
    }

    // ── testing ───────────────────────────────────────────────────────────
    testing.assert_true(true);
    testing.assert_false(false);
    testing.assert_eq(1, 1);
    testing.assert_ne(1, 2);
    testing.assert_eq(1.5, 1.5);
    testing.assert_ne(1.5, 2.5);
    testing.assert_eq("a", "a");
    testing.assert_ne("a", "b");
    testing.assert_eq(true, true);
    testing.assert_ne(true, false);
    say("t0: ok");
    try {
        testing.assert_eq(1, 2);
        say("unreached-assert-eq");
    } catch (err) {
        say("t1: " + err);
    }
    try {
        testing.assert_true(false);
        say("unreached-assert-true");
    } catch (err) {
        say("t2: " + err);
    }
    say("done")
}
"#;

    let dir = std::env::temp_dir().join("ntsc_stdlib_modules_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!(
        "stdlib_modules.{}",
        ntsc_codegen::object_extension()
    ));
    let bin_path = dir.join("stdlib_modules_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "stdlib_modules", &dir)
        .expect("compile_source failed");

    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    assert!(bin_path.exists(), "binary not produced");

    let mut child = std::process::Command::new(&bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to run binary");
    child
        .stdin
        .take()
        .expect("child stdin was not piped")
        .write_all(b"Ada\r\n")
        .expect("failed to write child stdin");
    let output = child
        .wait_with_output()
        .expect("failed to collect binary output");

    assert!(
        output.status.success(),
        "binary exited with non-zero status"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    assert!(
        !stdout.contains("unreached"),
        "unexpected output:\n{stdout}"
    );

    let expected_lines = [
        "os0: hello",
        "os1: true",
        "os2: false",
        "os3: []",
        // os.path_join / is_abs / separator follow the host platform.
        #[cfg(target_os = "windows")]
        "os4: a\\b",
        #[cfg(not(target_os = "windows"))]
        "os4: a/b",
        "os5: /a/b",
        "os6: c.txt",
        "os7: txt",
        "os8: c",
        #[cfg(target_os = "windows")]
        "os9: false",
        #[cfg(not(target_os = "windows"))]
        "os9: true",
        "os10: false",
        #[cfg(target_os = "windows")]
        "os11: \\",
        #[cfg(not(target_os = "windows"))]
        "os11: /",
        "os12: false",
        "e0: aGVsbG8=",
        "e1: hello",
        "e2: 4142",
        "e3: AB",
        "e4: true",
        "h0: 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        "h1: 3421780262",
        "input: [Ada]",
        // `io.write` returns 9 bytes: `line one` plus a real newline (the
        // `\n` is processed by the lexer), so the file holds two lines.
        "io0: 9",
        "io1: 9",
        "io2: [line one]",
        "io3: [line two]",
        "io4: true",
        "io5: line one|line two|",
        "io6: 18",
        "io7: line",
        "n0: hello",
        "n1: line",
        "n2: world",
        "n3: ping",
        "s0: 123",
        "s1: 321",
        "s2: 1",
        "s3: -1",
        "s4: abc",
        "s5: 1.5",
        "r0: true",
        "r1: true",
        "r2: true",
        "r3: 5",
        "r4: 2",
        "t0: ok",
    ];
    for line in expected_lines {
        assert!(
            stdout.lines().any(|l| l == line),
            "missing output line {line:?}:\n{stdout}"
        );
    }

    let expected_error_kinds = [
        "io.open:",
        "net.tcp_connect:",
        "random.weighted:",
        "random.int:",
        "testing.assert_eq:",
        "testing.assert_true:",
    ];
    for kind in expected_error_kinds {
        assert!(
            stdout.contains(kind),
            "missing error kind {kind:?}:\n{stdout}"
        );
    }

    assert!(
        stdout.trim().ends_with("done"),
        "unexpected output:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn io_input_reads_stdin_without_leaking() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let runtime_lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );

    let status = std::process::Command::new("cargo")
        .args(["build", "-p", "ntsc-runtime"])
        .current_dir(rewrite_dir)
        .status()
        .expect("failed to build ntsc-runtime");
    assert!(status.success(), "failed to build ntsc-runtime");

    let source = r#"use io
use strings
fun main() {
    var input = io.stdin();
    var output = io.stdout();
    var errors = io.stderr();
    io.write(output, "prompt: ");
    io.flush(output);
    say("first: [" + strings.trim(io.read_line(input)) + "]");
    io.write_line(errors, "diagnostic");
    io.flush(errors);
    io.close(input);
    io.close(output);
    io.close(errors);
    say("eof: [" + io.input() + "]")
}
"#;
    let dir = std::env::temp_dir().join("ntsc_io_input_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!("io_input.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("io_input_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "io_input", &dir)
        .expect("compile_source failed");
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    let mut child = std::process::Command::new(&bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run binary");
    child
        .stdin
        .take()
        .expect("child stdin was not piped")
        .write_all(b"Ada\r\n")
        .expect("failed to write child stdin");
    let output = child
        .wait_with_output()
        .expect("failed to collect binary output");

    assert!(
        output.status.success(),
        "binary exited with non-zero status"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "prompt: first: [Ada]\neof: []\n"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr, "diagnostic\n");
    assert!(
        !stderr.contains("Memory leak detected"),
        "standard-stream program leaked registry handles: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paths_join_accepts_two_components() {
    // Regression: the runtime ABI used to take one newline-packed
    // argument while every caller (and the docs) used `paths.join(a, b)`;
    // the stdlib arity checker then rejected those documented calls.
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let runtime_lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );

    let status = std::process::Command::new("cargo")
        .args(["build", "-p", "ntsc-runtime"])
        .current_dir(rewrite_dir)
        .status()
        .expect("failed to build ntsc-runtime");
    assert!(status.success(), "failed to build ntsc-runtime");

    let source = r#"
use paths
fun main() {
    say(paths.join("/a", "b.txt"))
}
"#;
    let dir = std::env::temp_dir().join("ntsc_paths_join_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!("paths_join.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("paths_join_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "paths_join", &dir)
        .expect("compile_source failed");
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "binary exited with non-zero status"
    );
    let separator = std::path::MAIN_SEPARATOR;
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("/a{separator}b.txt\n")
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Memory leak detected"),
        "paths.join program leaked registry handles: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
