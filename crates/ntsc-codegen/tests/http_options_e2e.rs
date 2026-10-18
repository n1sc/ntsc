//! End-to-end tests for `http.request_full` per-request options: custom
//! headers, a custom user agent, redirects, and a read timeout. A local
//! server answers on `127.0.0.1`, so the assertions cover exactly what the
//! client put on the wire without reaching the network.
//!
//! TLS stays in the runtime's unit tests: the client trusts only the bundled
//! Mozilla roots, so a self-signed local server cannot be used here.

use std::io::{Read, Write};
use std::path::Path;

fn runtime_lib(rewrite_dir: &Path) -> std::path::PathBuf {
    let lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );
    assert!(
        lib.exists(),
        "runtime lib not found; run `cargo build -p ntsc-runtime`"
    );
    lib
}

fn response(status: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Serve `count` requests. `reply` turns the raw request text into the
/// response bytes, so each test can echo or redirect as it needs.
///
/// The server thread is detached: a client that never connects (a compile or
/// runtime failure) must surface as a failed assertion, not a hung join.
fn spawn_server<F>(count: usize, reply: F) -> u16
where
    F: Fn(&str) -> Vec<u8> + Send + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for _ in 0..count {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0u8; 8192];
            let Ok(read) = stream.read(&mut buffer) else {
                continue;
            };
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let _ = stream.write_all(&reply(&request));
            let _ = stream.flush();
        }
    });
    port
}

/// Strip CRLF so the echoed request is comparable as one line.
fn normalize(request: &str) -> String {
    request.replace("\r\n", "|")
}

fn run(source: &str, name: &str) -> (String, String) {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let dir = rewrite_dir
        .join("target")
        .join("http-options-e2e")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();

    let object = ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &dir)
        .expect("compile failed");
    let binary = dir.join(name);
    ntsc_codegen::link_binary(&object, &runtime_lib(rewrite_dir), &binary).expect("link failed");

    let output = std::process::Command::new(&binary)
        .output()
        .expect("failed to run binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A custom header and user agent reach the server, and the default
/// `Neutron/1.0` agent is replaced rather than duplicated.
#[test]
fn custom_headers_and_user_agent_are_sent() {
    let port = spawn_server(1, |request| response("200 OK", &[], &normalize(request)));
    let source = format!(
        r#"use http
use json
use strings
fun main() {{
    var q = strings.from_char_code(34)
    var options = "{{" + q + "headers" + q + ":{{" + q + "Referer" + q + ":" + q + "https://music.example" + q + "," + q + "Authorization" + q + ":" + q + "Bearer k" + q + "}}," + q + "user_agent" + q + ":" + q + "mewsic/1.3.2" + q + "," + q + "timeout_ms" + q + ":2000}}"
    var resp = http.request_full("GET", "http://127.0.0.1:{port}/x", "", options)
    say(json.get(resp, "body"))
}}
"#
    );
    let (stdout, stderr) = run(&source, "headers");

    let sent = stdout.trim();
    assert!(
        sent.contains("Referer: https://music.example"),
        "custom header missing from: {sent}"
    );
    assert!(
        sent.contains("Authorization: Bearer k"),
        "second custom header missing from: {sent}"
    );
    assert!(
        sent.contains("User-Agent: mewsic/1.3.2"),
        "custom user agent missing from: {sent}"
    );
    assert!(
        !sent.contains("Neutron/1.0"),
        "default user agent should be replaced, got: {sent}"
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

/// A 302 is followed and the second response body is what the caller sees;
/// the hop is an absolute URL, the usual shape of a real `Location`.
#[test]
fn redirects_are_followed() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0u8; 8192];
            let Ok(read) = stream.read(&mut buffer) else {
                continue;
            };
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let reply = if request.starts_with("GET /final") {
                response("200 OK", &[], "arrived")
            } else {
                let location = format!("http://127.0.0.1:{port}/final");
                response("302 Found", &[("Location", &location)], "moved")
            };
            let _ = stream.write_all(&reply);
            let _ = stream.flush();
        }
    });
    let source = format!(
        r#"use http
use json
fun main() {{
    var resp = http.get("http://127.0.0.1:{port}/start")
    say(json.get(resp, "body"))
}}
"#
    );
    let (stdout, stderr) = run(&source, "redirect");
    // `json.get` hands back a JSON value, so a string body keeps its quotes.
    assert_eq!(stdout.trim(), "\"arrived\"", "stderr: {stderr}");
}

/// With `follow_redirects` off the redirect response itself is returned.
#[test]
fn redirects_can_be_disabled() {
    let port = spawn_server(1, |_request| {
        response("302 Found", &[("Location", "/elsewhere")], "moved")
    });
    let source = format!(
        r#"use http
use json
use strings
fun main() {{
    var q = strings.from_char_code(34)
    var options = "{{" + q + "follow_redirects" + q + ":false}}"
    var resp = http.request_full("GET", "http://127.0.0.1:{port}/start", "", options)
    say("" + http.status_code(resp))
    say(json.get(resp, "body"))
}}
"#
    );
    let (stdout, stderr) = run(&source, "no_redirect");
    assert_eq!(stdout.trim(), "302\n\"moved\"", "stderr: {stderr}");
}

/// A server that never answers is abandoned once `timeout_ms` elapses, so a
/// hung peer cannot wedge the caller.
#[test]
fn timeout_ms_abandons_a_hung_server() {
    let port = spawn_server(1, |_request| {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        response("200 OK", &[], "too late")
    });
    let source = format!(
        r#"use http
use json
use strings
use time
fun main() {{
    var q = strings.from_char_code(34)
    var options = "{{" + q + "timeout_ms" + q + ":250}}"
    var start = time.now()
    var failed = false
    quiet [unused_variable] {{
        try {{
            http.request_full("GET", "http://127.0.0.1:{port}/hang", "", options)
        }} catch (e) {{
            failed = true
        }}
    }}
    say("failed=" + failed)
    say("elapsed-bounded=" + (time.now() - start < 1400))
}}
"#
    );
    let (stdout, stderr) = run(&source, "timeout");
    assert_eq!(
        stdout.trim(),
        "failed=true\nelapsed-bounded=true",
        "stderr: {stderr}"
    );
}
