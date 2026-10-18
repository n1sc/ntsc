//! End-to-end regression tests for the ntroutine phases (see ntroutine.md):
//! `go` spawns, `chan[T]` channels with `<|`/`|>`/`close`/`for v in chan`,
//! and awaited offloaded `http.*_async` futures, compiled and linked against
//! the runtime, then executed.

use std::io::{Read, Write};
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
        !stderr.contains("memory leak detected"),
        "goroutine and channel values must be reclaimed:\n{stderr}"
    );
}

#[test]
fn go_call_and_channel_handshake() {
    let source = r#"
async fun producer(chan[int] ch) {
    7 |> ch
    close(ch)
}

async fun main() -> int {
    var chan[int] ch = chan.new(2)
    go producer(ch)
    x <| ch
    if (x == 7) {
        say("handshake ok")
    }
    y <| ch
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "ntroutine_go_call");
    assert_clean(&stdout, &stderr, &output, &["handshake ok"]);
}

#[test]
fn go_block_captures_and_for_in_chan() {
    let source = r#"
async fun main() -> int {
    var chan[string] words = chan.new(4)
    go {
        for w in words {
            say(w)
        }
    }
    "alpha" |> words
    "beta" |> words
    close(words)
    await async.sleep(100)
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "ntroutine_go_block");
    assert_clean(&stdout, &stderr, &output, &["alpha", "beta"]);
}

#[test]
fn chan_handle_copies_keep_the_channel_alive() {
    let source = r#"
async fun worker(chan[int] done) {
    1 |> done
}

fun make_chan() -> chan[int] {
    var chan[int] c = chan.new(8)
    return c
}

async fun main() -> int {
    var chan[int] done = make_chan()
    var chan[int] alias = done
    var int i = 0
    while (i < 8) {
        go worker(alias)
        i = i + 1
    }
    var int received = 0
    for count in done {
        received = received + count
        if (received == 8) {
            close(done)
        }
    }
    say("fanout " + received)
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "ntroutine_chan_copies");
    assert_clean(&stdout, &stderr, &output, &["fanout 8"]);
}

/// A goroutine still parked when `main` returns must not leak its future:
/// the scheduler reclaims an abandoned future at shutdown, releasing the
/// armed `async.sleep` handle and every owned local the future's slots hold.
#[test]
fn abandoned_goroutine_futures_are_reclaimed_at_shutdown() {
    let source = r#"
use fmt
use arrays

async fun napper(int id) {
    var string tag = "worker-" + fmt.i64_to_str(id)
    var array[int] work = [1, 2, 3]
    await async.sleep(50)
    say(tag)
    say(fmt.i64_to_str(arrays.length(work)))
}

async fun main() -> int {
    var int i = 0
    while (i < 8) {
        go napper(i)
        i = i + 1
    }
    say("spawned, returning while they sleep")
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "ntroutine_abandoned_future");
    assert_clean(
        &stdout,
        &stderr,
        &output,
        &["spawned, returning while they sleep"],
    );
}

#[test]
fn async_goroutine_drops_do_not_destroy_string_literals() {
    let source = r#"
async fun worker(string message, chan[int] done) {
    say(message)
    1 |> done
}

async fun main() -> int {
    var chan[int] done = chan.new(2)
    go worker("same literal", done)
    x <| done
    go worker("same literal", done)
    y <| done
    if (x == 1 && y == 1) {
        say("literal survived")
    }
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "ntroutine_literal_reuse");
    assert_clean(
        &stdout,
        &stderr,
        &output,
        &["same literal", "literal survived"],
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| *line == "same literal")
            .count(),
        2
    );
}

#[test]
fn recursive_detached_goroutines_reclaim_completed_futures() {
    let source = r#"
async fun poll_loop(int remaining) {
    await async.sleep(1)
    if (remaining > 1) {
        go poll_loop(remaining - 1)
    }
}

async fun main() -> int {
    go poll_loop(20)
    await async.sleep(100)
    say("recursive loop completed")
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "ntroutine_recursive_detached");
    assert_clean(&stdout, &stderr, &output, &["recursive loop completed"]);
}

/// A server whose clients connect and stay silent for a while before sending.
/// `recv_line_async` must let each handler park instead of pinning a worker, so
/// 32 concurrent slow clients are all served; with a blocking read only four
/// (one per worker) could be mid-read at once and the rest would time out.
#[test]
fn awaitable_recv_line_serves_many_slow_clients() {
    // Pick a port by binding and dropping: the NTSC server must bind the same
    // address, so the probe listener cannot stay alive.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    };
    let clients = std::thread::spawn(move || {
        let mut handles = Vec::new();
        for i in 0..32 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            let stream = loop {
                match std::net::TcpStream::connect(("127.0.0.1", port)) {
                    Ok(stream) => break stream,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(error) => panic!("connect: {error}"),
                }
            };
            handles.push(std::thread::spawn(move || {
                use std::io::{BufRead, BufReader, Write};
                let mut stream = stream;
                // Silent for a while: the server's read has nothing yet.
                std::thread::sleep(std::time::Duration::from_millis(80));
                let _ = writeln!(stream, "{}", i);
                let _ = stream.flush();
                let mut line = String::new();
                let _ = BufReader::new(&mut stream).read_line(&mut line);
                line.trim() == format!("ok:{}", i + 1)
            }));
        }
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .filter(|ok| *ok)
            .count()
    });

    let source = format!(
        r#"
use net
use fmt

async fun handle_client(int sock) {{
    var string line = await net.recv_line_async(sock)
    net.send_line(sock, "ok:" + fmt.i64_to_str(fmt.to_int(line) + 1))
    net.close(sock)
}}

async fun accept_loop(int listener) {{
    var int sock = await net.accept_async(listener)
    go handle_client(sock)
    go accept_loop(listener)
}}

async fun main() -> int {{
    var int listener = net.tcp_listen({port})
    go accept_loop(listener)
    await async.sleep(6000)
    net.close(listener)
    return 0
}}
"#
    );
    let (stdout, stderr, output) = build_and_run(&source, "ntroutine_recv_line_async");
    assert!(
        output.status.success(),
        "binary exited with non-zero status:\n{stdout}\n{stderr}"
    );
    let served = clients.join().expect("clients");
    assert_eq!(served, 32, "every slow client must be served");
    assert!(
        !stderr.contains("memory leak detected"),
        "awaitable reads must be reclaimed:\n{stderr}"
    );
}

#[test]
fn go_block_scalar_capture() {
    let source = r#"
async fun main() -> int {
    var int base = 40
    go {
        say("capture " + base)
    }
    await async.sleep(100)
    return 0
}
"#;
    let (stdout, stderr, output) = build_and_run(source, "ntroutine_go_scalar");
    assert_clean(&stdout, &stderr, &output, &["capture 40"]);
}

#[test]
fn await_offloaded_http_get() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\nhello from test\r\n",
            );
            let _ = stream.flush();
        }
    });

    let source = format!(
        r#"
use http

async fun main() -> int {{
    var resp = await http.get_async("http://127.0.0.1:{port}/")
    say(resp)
    return 0
}}
"#
    );

    let (stdout, stderr, output) = build_and_run(&source, "ntroutine_http");
    server.join().expect("server thread");
    assert!(
        output.status.success(),
        "binary exited with non-zero status:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("hello from test"),
        "missing response body:\n{stdout}\n{stderr}"
    );
    assert!(
        !stderr.contains("memory leak detected"),
        "offloaded http futures must be reclaimed:\n{stderr}"
    );
}
