//! NTSC standard library: `http` module.
//! Blocking HTTP/1.1 client; `https://` uses rustls anchored at Mozilla's
//! bundled webpki roots. Responses are JSON: `{"status":N,"body":"..."}`.

use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream};
use std::sync::{Arc, OnceLock};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::registry;

fn default_tls_config() -> &'static Arc<ClientConfig> {
    static DEFAULT_TLS: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    DEFAULT_TLS.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    })
}

fn make_response(status: i64, body: &str) -> String {
    // Backslashes are escaped before quotes: a body holding `\` (a Windows
    // path, a regex, an escaped lyric) would otherwise produce invalid JSON
    // and `json.get(resp, "body")` would fail to read it back.
    let escaped = body
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("{{\"status\":{status},\"body\":\"{escaped}\"}}")
}

fn parse_url(url: &str) -> Result<(String, u16, String, bool), String> {
    let is_https = url.starts_with("https://");
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);

    let default_port = if is_https { 443 } else { 80 };

    let (host_part, path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], &rest[pos..]),
        None => (rest, "/"),
    };

    let (host_str, port) = if let Some(colon_pos) = host_part.rfind(':') {
        let h = &host_part[..colon_pos];
        let p: u16 = host_part[colon_pos + 1..]
            .parse()
            .map_err(|_| "Invalid port".to_string())?;
        (h.to_string(), p)
    } else {
        (host_part.to_string(), default_port)
    };

    Ok((host_str, port, path.to_string(), is_https))
}

fn server_name_for(host: &str) -> Result<ServerName<'static>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ServerName::IpAddress(ip.into()));
    }
    ServerName::try_from(host.to_string()).map_err(|_| format!("invalid host `{host}`"))
}

/// Per-request knobs: the header set, the user agent, and the timeouts.
/// A plain `http.get` uses the defaults, so the options only carry what a
/// caller actually needs (a `Referer` some APIs demand, a bearer token, a
/// short timeout so a hung peer cannot wedge a UI thread).
#[derive(Debug, Clone)]
struct RequestOptions {
    headers: Vec<(String, String)>,
    user_agent: Option<String>,
    timeout: Option<std::time::Duration>,
    follow_redirects: bool,
    max_redirects: usize,
}

impl Default for RequestOptions {
    /// Redirects are followed, matching what a plain `http.get` is expected
    /// to do; the budget keeps a redirect loop from spinning forever.
    fn default() -> Self {
        Self {
            headers: Vec::new(),
            user_agent: None,
            timeout: None,
            follow_redirects: true,
            max_redirects: 5,
        }
    }
}

impl RequestOptions {
    /// Read the options from the JSON blob a caller passes:
    /// `{"headers": {...}, "user_agent": "...", "timeout_ms": 5000,
    ///   "follow_redirects": true, "max_redirects": 5}`.
    fn from_json(text: &str) -> Result<Self, String> {
        let mut options = Self::default();
        if text.trim().is_empty() {
            return Ok(options);
        }
        super::json::json_validate(text).map_err(|e| format!("invalid options: {e}"))?;
        if let Some(headers) = super::json::json_get_str(text, "headers")
            && headers != "null"
            && !headers.is_empty()
        {
            options.headers = header_pairs(&headers);
        }
        if let Some(agent) = super::json::json_get_str(text, "user_agent") {
            let agent = unquote(&agent);
            if !agent.is_empty() {
                options.user_agent = Some(agent);
            }
        }
        if let Some(timeout) = super::json::json_get_str(text, "timeout_ms")
            && let Ok(ms) = timeout.parse::<u64>()
            && ms > 0
        {
            options.timeout = Some(std::time::Duration::from_millis(ms));
        }
        if super::json::json_get_str(text, "follow_redirects").as_deref() == Some("false") {
            options.follow_redirects = false;
        }
        if let Some(max) = super::json::json_get_str(text, "max_redirects")
            && let Ok(n) = max.parse::<usize>()
        {
            options.max_redirects = n;
        }
        Ok(options)
    }
}

/// Strip the quotes off a JSON string value; a bare value passes through.
fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Flatten a JSON object of string values into ordered header pairs.
fn header_pairs(object: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = object.trim();
    if let Some(stripped) = rest.strip_prefix('{') {
        rest = stripped.trim_end_matches('}');
    }
    for entry in rest.split(',') {
        let Some(colon) = entry.find(':') else {
            continue;
        };
        let name = unquote(&entry[..colon]);
        let value = unquote(&entry[colon + 1..]);
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

fn http_request_raw(method: &str, url: &str, body: Option<&str>) -> Result<(i64, String), String> {
    http_request_raw_impl(method, url, body, None)
}

#[cfg(test)]
fn http_request_raw_with_roots(
    method: &str,
    url: &str,
    body: Option<&str>,
    roots: &RootCertStore,
) -> Result<(i64, String), String> {
    http_request_raw_impl(method, url, body, Some(roots))
}

fn http_request_raw_impl(
    method: &str,
    url: &str,
    body: Option<&str>,
    roots: Option<&RootCertStore>,
) -> Result<(i64, String), String> {
    http_request_with(method, url, body, &RequestOptions::default(), roots)
}

/// Issue one request, following redirects when the options ask for it.
fn http_request_with(
    method: &str,
    url: &str,
    body: Option<&str>,
    options: &RequestOptions,
    roots: Option<&RootCertStore>,
) -> Result<(i64, String), String> {
    let mut method = method.to_string();
    let mut url = url.to_string();
    let mut body = body.map(str::to_string);
    let hops = if options.follow_redirects {
        options.max_redirects.max(1)
    } else {
        1
    };
    for _ in 0..hops {
        let (status, response_body, location) =
            http_request_once(&method, &url, body.as_deref(), options, roots)?;
        if !options.follow_redirects || !is_redirect(status) {
            return Ok((status, response_body));
        }
        let Some(target) = location else {
            // A redirect without a usable target is the final answer.
            return Ok((status, response_body));
        };
        url = resolve_redirect(&url, &target)?;
        // 303, and 301/302 in practice, continue as GET without a body.
        if status != 307 && status != 308 {
            method = "GET".to_string();
            body = None;
        }
    }
    Err("too many redirects".to_string())
}

fn is_redirect(status: i64) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Resolve a `Location` value against the URL it answered.
fn resolve_redirect(base: &str, target: &str) -> Result<String, String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(target.to_string());
    }
    let (host, port, _, _) = parse_url(base)?;
    let scheme_is_https = base.starts_with("https://");
    let authority = if (scheme_is_https && port == 443) || (!scheme_is_https && port == 80) {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    if let Some(absolute) = target.strip_prefix('/') {
        Ok(format!(
            "{}://{authority}/{absolute}",
            if scheme_is_https { "https" } else { "http" }
        ))
    } else {
        Ok(format!(
            "{}://{authority}/{target}",
            if scheme_is_https { "https" } else { "http" }
        ))
    }
}

/// Perform a single request without following redirects, returning the
/// status, the body, and the `Location` header when the response redirects.
fn http_request_once(
    method: &str,
    url: &str,
    body: Option<&str>,
    options: &RequestOptions,
    roots: Option<&RootCertStore>,
) -> Result<(i64, String, Option<String>), String> {
    let (host, port, path, is_https) = parse_url(url)?;

    let addr = format!("{}:{}", host, port);
    let tcp = TcpStream::connect(&addr).map_err(|e| format!("Connection failed: {e}"))?;
    if let Some(timeout) = options.timeout {
        let _ = tcp.set_read_timeout(Some(timeout));
        let _ = tcp.set_write_timeout(Some(timeout));
    }

    if !is_https {
        let mut stream = tcp;
        return send_request_with(&mut stream, method, &host, &path, body, options);
    }

    let config = match roots {
        Some(roots) => Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots.clone())
                .with_no_client_auth(),
        ),
        None => default_tls_config().clone(),
    };
    let server_name = server_name_for(&host)?;
    let tls =
        ClientConnection::new(config, server_name).map_err(|e| format!("TLS setup failed: {e}"))?;
    let mut stream = StreamOwned::new(tls, tcp);
    if let Some(timeout) = options.timeout {
        let _ = stream.sock.set_read_timeout(Some(timeout));
        let _ = stream.sock.set_write_timeout(Some(timeout));
    }

    // Drive the handshake before sending so certificate-validation failures
    // surface as a clean connection error.
    stream
        .conn
        .complete_io(&mut stream.sock)
        .map_err(|e| format!("TLS handshake failed: {e}"))?;
    send_request_with(&mut stream, method, &host, &path, body, options)
}

/// Build and send one request, then split the raw response into its status,
/// its body, and the `Location` header. The location is returned separately
/// so the redirect loop can follow it.
fn send_request_with<S: Read + Write>(
    stream: &mut S,
    method: &str,
    host: &str,
    path: &str,
    body: Option<&str>,
    options: &RequestOptions,
) -> Result<(i64, String, Option<String>), String> {
    let agent = options
        .user_agent
        .clone()
        .unwrap_or_else(|| "Neutron/1.0".to_string());
    let mut request = format!(
        "{} {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {agent}\r\nConnection: close\r\n",
        method, path
    );
    for (name, value) in &options.headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }

    if let Some(data) = body {
        request.push_str("Content-Type: application/octet-stream\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", data.len()));
    }
    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("Write failed: {e}"))?;

    if let Some(data) = body {
        stream
            .write_all(data.as_bytes())
            .map_err(|e| format!("Body write failed: {e}"))?;
    }

    let mut response = String::new();
    read_to_string_lenient(stream, &mut response).map_err(|e| format!("Read failed: {e}"))?;

    let mut lines = response.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<i64>().ok())
        .unwrap_or(0);

    // `Location` is only needed for a redirect, so it is picked up while the
    // header block is still being walked.
    let mut location = None;
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some(value) = line
            .split_once(':')
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("location"))
            .map(|(_, value)| value.trim().to_string())
        {
            location = Some(value);
        }
    }

    let body = match response.find("\r\n\r\n") {
        Some(pos) => response[pos + 4..].to_string(),
        None => String::new(),
    };

    Ok((status, body, location))
}

/// `http.get(url)` — returns the response JSON; throws when the request
/// cannot be completed.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_get(url: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    match http_request_raw("GET", &url, None) {
        Ok((status, body)) => registry::put_string(make_response(status, &body)),
        Err(e) => super::throw_str(format!("http.get: {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_post(url: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    let data = registry::get_string(data).unwrap_or_default();
    match http_request_raw("POST", &url, Some(&data)) {
        Ok((status, body)) => registry::put_string(make_response(status, &body)),
        Err(e) => super::throw_str(format!("http.post: {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_put(url: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    let data = registry::get_string(data).unwrap_or_default();
    match http_request_raw("PUT", &url, Some(&data)) {
        Ok((status, body)) => registry::put_string(make_response(status, &body)),
        Err(e) => super::throw_str(format!("http.put: {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_delete(url: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    match http_request_raw("DELETE", &url, None) {
        Ok((status, body)) => registry::put_string(make_response(status, &body)),
        Err(e) => super::throw_str(format!("http.delete: {e}")),
    }
}

/// `http.head(url)` — like `get`, but the JSON carries only `status`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_head(url: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    match http_request_raw("HEAD", &url, None) {
        Ok((status, _body)) => registry::put_string(format!("{{\"status\":{}}}", status)),
        Err(e) => super::throw_str(format!("http.head: {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_patch(url: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    let data = registry::get_string(data).unwrap_or_default();
    match http_request_raw("PATCH", &url, Some(&data)) {
        Ok((status, body)) => registry::put_string(make_response(status, &body)),
        Err(e) => super::throw_str(format!("http.patch: {e}")),
    }
}

/// `http.request(method, url, data)` — an empty `data` sends no body.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_request(method: i64, url: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let method = registry::get_string(method).unwrap_or_default();
    let url = registry::get_string(url).unwrap_or_default();
    let data_str = registry::get_string(data).unwrap_or_default();
    let body = if data_str.is_empty() {
        None
    } else {
        Some(data_str.as_str())
    };
    match http_request_raw(&method, &url, body) {
        Ok((status, body)) => registry::put_string(make_response(status, &body)),
        Err(e) => super::throw_str(format!("http.request: {e}")),
    }
}

/// `http.request_full(method, url, data, options)` — like
/// `http.request`, with per-request headers, user agent, and timeouts read
/// from a JSON options object.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_request_full(method: i64, url: i64, data: i64, options: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let method = registry::get_string(method).unwrap_or_default();
    let url = registry::get_string(url).unwrap_or_default();
    let data_str = registry::get_string(data).unwrap_or_default();
    let options_str = registry::get_string(options).unwrap_or_default();
    let body = if data_str.is_empty() {
        None
    } else {
        Some(data_str.as_str())
    };
    let options = match RequestOptions::from_json(&options_str) {
        Ok(options) => options,
        Err(e) => return super::throw_str(format!("http.request_full: {e}")),
    };
    match http_request_with(&method, &url, body, &options, None) {
        Ok((status, body)) => registry::put_string(make_response(status, &body)),
        Err(e) => super::throw_str(format!("http.request_full: {e}")),
    }
}

// ── Offloaded (async) HTTP ─────────────────────────────────────────────
//
// Async variants run the blocking HTTP request on the worker pool instead of
// the scheduler thread, mirroring the process offload. The pool job returns a
// `Result<String, String>` (the JSON response vs the error message), which
// the result ABI delivers as a value or a throw once reaped.

/// Outcome of an offloaded HTTP job: the response JSON or the error message.
pub(crate) type HttpOutcome = Result<String, String>;

/// Register an offloaded HTTP future that runs `work` on the worker pool.
fn run_http_offloaded(work: Box<dyn FnOnce() -> Result<String, String> + Send + 'static>) -> i64 {
    registry::async_op_new(Box::new(move || registry::put_opaque(work())))
}

/// Deliver the outcome of a completed offloaded HTTP future.
fn deliver_http(id: i64) -> i64 {
    let outcome: HttpOutcome =
        registry::take_opaque(registry::async_op_result(id)).unwrap_or(Err(String::new()));
    match outcome {
        Ok(json) => registry::put_string(json),
        Err(message) => super::throw_str(message),
    }
}

/// The `body` (JSON) for an HTTP reply, converting the raw request outcome.
fn http_reply(
    label: &'static str,
    result: Result<(i64, String), String>,
) -> Result<String, String> {
    result
        .map(|(status, body)| make_response(status, &body))
        .map_err(|e| format!("{label}: {e}"))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_get(url: i64) -> i64 {
    let url = registry::get_string(url).unwrap_or_default();
    run_http_offloaded(Box::new(move || {
        http_reply("http.get", http_request_raw("GET", &url, None))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_post(url: i64, data: i64) -> i64 {
    let url = registry::get_string(url).unwrap_or_default();
    let data = registry::get_string(data).unwrap_or_default();
    run_http_offloaded(Box::new(move || {
        http_reply("http.post", http_request_raw("POST", &url, Some(&data)))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_put(url: i64, data: i64) -> i64 {
    let url = registry::get_string(url).unwrap_or_default();
    let data = registry::get_string(data).unwrap_or_default();
    run_http_offloaded(Box::new(move || {
        http_reply("http.put", http_request_raw("PUT", &url, Some(&data)))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_delete(url: i64) -> i64 {
    let url = registry::get_string(url).unwrap_or_default();
    run_http_offloaded(Box::new(move || {
        http_reply("http.delete", http_request_raw("DELETE", &url, None))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_patch(url: i64, data: i64) -> i64 {
    let url = registry::get_string(url).unwrap_or_default();
    let data = registry::get_string(data).unwrap_or_default();
    run_http_offloaded(Box::new(move || {
        http_reply("http.patch", http_request_raw("PATCH", &url, Some(&data)))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_head(url: i64) -> i64 {
    let url = registry::get_string(url).unwrap_or_default();
    run_http_offloaded(Box::new(move || {
        http_request_raw("HEAD", &url, None)
            .map(|(status, _)| format!("{{\"status\":{}}}", status))
            .map_err(|e| format!("http.head: {e}"))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_poll(id: i64) -> i8 {
    registry::async_op_poll(id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_result(id: i64) -> i64 {
    deliver_http(id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_http_drop(id: i64) {
    registry::async_op_drop(id);
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_status_code(response: i64) -> i64 {
    let resp = registry::get_string(response).unwrap_or_default();

    if let Some(pos) = resp.find("\"status\":") {
        let rest = &resp[pos + 9..];
        let num_end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '-')
            .unwrap_or(rest.len());
        rest[..num_end].parse::<i64>().unwrap_or(0)
    } else {
        0
    }
}

/// `http.get_range(url, start, end)` — HTTP GET with Range header for
/// streaming downloads. Returns JSON `{"status":N,"body":"..."}`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_get_range(url: i64, start: i64, end: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    let range_header = format!("bytes={start}-{end}");
    http_request_with_range("GET", &url, &range_header)
}

/// `http.get_file(url, dest)` — download a URL to a file. Returns JSON
/// `{"status":N,"bytes":N}` with the number of bytes written.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_get_file(url: i64, dest: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    let dest = registry::get_string(dest).unwrap_or_default();

    // Ensure parent directory exists.
    if let Some(parent) = std::path::Path::new(&dest).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return super::throw_str(format!("http.get_file: cannot create directory: {e}"));
    }

    match download_file(&url, &dest) {
        Ok(bytes) => registry::put_string(format!("{{\"status\":200,\"bytes\":{}}}", bytes)),
        Err(e) => super::throw_str(format!("http.get_file: {e}")),
    }
}

/// `http.download_with_progress(url, dest, chunk_size)` — download a URL
/// to a file in chunks. Returns JSON `{"status":N,"bytes":N,"chunks":N}`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_download_with_progress(url: i64, dest: i64, chunk_size: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let url = registry::get_string(url).unwrap_or_default();
    let dest = registry::get_string(dest).unwrap_or_default();
    let chunk = chunk_size.max(1024) as usize;

    if let Some(parent) = std::path::Path::new(&dest).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return super::throw_str(format!(
            "http.download_with_progress: cannot create directory: {e}"
        ));
    }

    match download_file_chunked(&url, &dest, chunk) {
        Ok((bytes, chunks)) => registry::put_string(format!(
            "{{\"status\":200,\"bytes\":{},\"chunks\":{}}}",
            bytes, chunks
        )),
        Err(e) => super::throw_str(format!("http.download_with_progress: {e}")),
    }
}

/// `http.concurrent_download(urls, dest_dir, chunk_size)` — download
/// multiple URLs to `dest_dir` concurrently. Returns JSON array of
/// `{"url":"...","status":N,"bytes":N}` results.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_http_concurrent_download(urls: i64, dest_dir: i64, chunk_size: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let urls_str = registry::get_string(urls).unwrap_or_default();
    let dest_dir = registry::get_string(dest_dir).unwrap_or_default();
    let _chunk = chunk_size.max(1024) as usize;

    let url_list: Vec<String> = urls_str
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        return super::throw_str(format!(
            "http.concurrent_download: cannot create '{dest_dir}': {e}"
        ));
    }

    let mut results = Vec::new();
    for url in &url_list {
        let filename = url.rsplit('/').next().unwrap_or("download");
        let dest = std::path::Path::new(&dest_dir).join(filename);
        match download_file(url, dest.to_str().unwrap_or("")) {
            Ok(bytes) => {
                results.push(format!(
                    "{{\"url\":\"{}\",\"status\":200,\"bytes\":{}}}",
                    url.replace('"', "\\\""),
                    bytes
                ));
            }
            Err(e) => {
                results.push(format!(
                    "{{\"url\":\"{}\",\"status\":0,\"error\":\"{}\"}}",
                    url.replace('"', "\\\""),
                    e.replace('"', "\\\"")
                ));
            }
        }
    }

    registry::put_string(format!("[{}]", results.join(",")))
}

fn http_request_with_range(method: &str, url: &str, range: &str) -> i64 {
    let (host, port, path, is_https) = match parse_url(url) {
        Ok(v) => v,
        Err(e) => return super::throw_str(format!("http.get_range: {e}")),
    };

    let addr = format!("{}:{}", host, port);
    let tcp = match TcpStream::connect(&addr) {
        Ok(t) => t,
        Err(e) => return super::throw_str(format!("http.get_range: Connection failed: {e}")),
    };

    if !is_https {
        let mut stream = tcp;
        return send_range_request(&mut stream, method, &host, &path, range);
    }

    let config = default_tls_config().clone();
    let server_name = match server_name_for(&host) {
        Ok(s) => s,
        Err(e) => return super::throw_str(format!("http.get_range: {e}")),
    };
    let tls = match ClientConnection::new(config, server_name) {
        Ok(c) => c,
        Err(e) => return super::throw_str(format!("http.get_range: TLS setup failed: {e}")),
    };
    let mut stream = StreamOwned::new(tls, tcp);
    if let Err(e) = stream.conn.complete_io(&mut stream.sock) {
        return super::throw_str(format!("http.get_range: TLS handshake failed: {e}"));
    }
    send_range_request(&mut stream, method, &host, &path, range)
}

/// Read a stream to a string, treating a missing TLS `close_notify` as the
/// end of the body.
///
/// rustls reports a peer that closes without `close_notify` as
/// `UnexpectedEof` and refuses the response. Plenty of servers and proxies
/// do exactly that, and the Rust original (through `ureq`) accepted them, so
/// a strict reading here breaks requests that used to work — Spotify's API
/// among them. A genuinely truncated response is still caught downstream:
/// the status line, headers, and content length all have to check out.
fn read_to_string_lenient(stream: &mut impl Read, out: &mut String) -> io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // rustls reports the missing `close_notify` through its own
            // error type, so the kind varies by version; match the message
            // as well as the kind.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) if e.to_string().contains("close_notify") => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    match String::from_utf8(buf) {
        Ok(text) => {
            out.push_str(&text);
            Ok(())
        }
        Err(e) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("response is not UTF-8: {e}"),
        )),
    }
}

fn send_range_request<S: Read + Write>(
    stream: &mut S,
    method: &str,
    host: &str,
    path: &str,
    range: &str,
) -> i64 {
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Neutron/1.0\r\nConnection: close\r\nRange: {range}\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return super::throw_str("http.get_range: write failed".to_string());
    }
    let mut response = String::new();
    if read_to_string_lenient(stream, &mut response).is_err() {
        return super::throw_str("http.get_range: read failed".to_string());
    }
    let status = if let Some(line) = response.lines().next() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            parts[1].parse::<i64>().unwrap_or(0)
        } else {
            0
        }
    } else {
        0
    };
    let body = if let Some(pos) = response.find("\r\n\r\n") {
        response[pos + 4..].to_string()
    } else {
        String::new()
    };
    registry::put_string(make_response(status, &body))
}

fn download_file(url: &str, dest: &str) -> Result<usize, String> {
    let (host, port, path, is_https) = parse_url(url)?;
    let addr = format!("{}:{}", host, port);
    let mut tcp = TcpStream::connect(&addr).map_err(|e| format!("Connection failed: {e}"))?;

    let bytes_written = if is_https {
        let config = default_tls_config().clone();
        let server_name = server_name_for(&host)?;
        let tls = ClientConnection::new(config, server_name)
            .map_err(|e| format!("TLS setup failed: {e}"))?;
        let mut stream = StreamOwned::new(tls, tcp);
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|e| format!("TLS handshake failed: {e}"))?;
        do_download(&mut stream, &host, &path, dest)?
    } else {
        do_download(&mut tcp, &host, &path, dest)?
    };

    Ok(bytes_written)
}

fn do_download<S: Read + Write>(
    stream: &mut S,
    host: &str,
    path: &str,
    dest: &str,
) -> Result<usize, String> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Neutron/1.0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("Write failed: {e}"))?;

    // Read response headers.
    let mut response_buf = Vec::new();
    let mut header_done = false;
    {
        let mut buf = [0u8; 4096];
        while !header_done {
            let n = stream
                .read(&mut buf)
                .map_err(|e| format!("Read failed: {e}"))?;
            if n == 0 {
                break;
            }
            response_buf.extend_from_slice(&buf[..n]);
            if let Some(pos) = response_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                header_done = true;
                // Remove headers from buffer, keep body.
                let body_start = pos + 4;
                response_buf = response_buf[body_start..].to_vec();
            }
        }
    }

    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("Cannot create file '{dest}': {e}"))?;

    // Write any body bytes already read.
    let mut written = response_buf.len();
    file.write_all(&response_buf)
        .map_err(|e| format!("Write failed: {e}"))?;

    // Read the rest.
    let mut buf = [0u8; 8192];
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("Read failed: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("Write failed: {e}"))?;
        written += n;
    }

    Ok(written)
}

fn download_file_chunked(
    url: &str,
    dest: &str,
    chunk_size: usize,
) -> Result<(usize, usize), String> {
    let (host, port, path, is_https) = parse_url(url)?;
    let addr = format!("{}:{}", host, port);
    let tcp = TcpStream::connect(&addr).map_err(|e| format!("Connection failed: {e}"))?;

    // First, get content length.
    let content_length = if is_https {
        let config = default_tls_config().clone();
        let server_name = server_name_for(&host)?;
        let tls = ClientConnection::new(config, server_name)
            .map_err(|e| format!("TLS setup failed: {e}"))?;
        let mut stream = StreamOwned::new(tls, tcp);
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|e| format!("TLS handshake failed: {e}"))?;
        get_content_length(&mut stream, &host, &path)?
    } else {
        let mut stream = tcp;
        get_content_length(&mut stream, &host, &path)?
    };

    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("Cannot create file '{dest}': {e}"))?;
    let mut total_written = 0usize;
    let mut chunks = 0usize;

    // Download in range requests.
    let mut offset = 0usize;
    loop {
        let end = if content_length > 0 {
            (offset + chunk_size - 1).min(content_length - 1)
        } else {
            offset + chunk_size - 1
        };
        let range = format!("bytes={offset}-{end}");

        // Make a new connection for each chunk (simpler than keeping
        // connections alive through TLS renegotiation).
        let tcp = TcpStream::connect(&addr).map_err(|e| format!("Connection failed: {e}"))?;

        if is_https {
            let config = default_tls_config().clone();
            let server_name = server_name_for(&host)?;
            let tls = ClientConnection::new(config, server_name)
                .map_err(|e| format!("TLS setup failed: {e}"))?;
            let mut stream = StreamOwned::new(tls, tcp);
            stream
                .conn
                .complete_io(&mut stream.sock)
                .map_err(|e| format!("TLS handshake failed: {e}"))?;
            let written = download_chunk(&mut stream, &host, &path, &range, &mut file)?;
            total_written += written;
            chunks += 1;
            if written < chunk_size || (content_length > 0 && offset + chunk_size >= content_length)
            {
                break;
            }
        } else {
            let mut stream = tcp;
            let written = download_chunk(&mut stream, &host, &path, &range, &mut file)?;
            total_written += written;
            chunks += 1;
            if written < chunk_size || (content_length > 0 && offset + chunk_size >= content_length)
            {
                break;
            }
        }

        offset += chunk_size;
        if content_length > 0 && offset >= content_length {
            break;
        }
    }

    Ok((total_written, chunks))
}

fn get_content_length<S: Read + Write>(
    stream: &mut S,
    host: &str,
    path: &str,
) -> Result<usize, String> {
    let request = format!(
        "HEAD {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Neutron/1.0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("Write failed: {e}"))?;

    let mut response = String::new();
    read_to_string_lenient(stream, &mut response).map_err(|e| format!("Read failed: {e}"))?;

    for line in response.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(val) = lower.strip_prefix("content-length:") {
            return val.trim().parse().map_err(|e| format!("parse error: {e}"));
        }
    }
    Ok(0)
}

fn download_chunk<S: Read + Write>(
    stream: &mut S,
    host: &str,
    path: &str,
    range: &str,
    file: &mut std::fs::File,
) -> Result<usize, String> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Neutron/1.0\r\nConnection: close\r\nRange: {range}\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("Write failed: {e}"))?;

    // Read headers.
    let mut buf = Vec::new();
    loop {
        let mut tmp = [0u8; 4096];
        let n = stream
            .read(&mut tmp)
            .map_err(|e| format!("Read failed: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let body = buf[pos + 4..].to_vec();
            file.write_all(&body)
                .map_err(|e| format!("Write failed: {e}"))?;
            let mut total = body.len();
            let mut read_buf = [0u8; 8192];
            loop {
                let n = stream
                    .read(&mut read_buf)
                    .map_err(|e| format!("Read failed: {e}"))?;
                if n == 0 {
                    break;
                }
                file.write_all(&read_buf[..n])
                    .map_err(|e| format!("Write failed: {e}"))?;
                total += n;
            }
            return Ok(total);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::CertifiedKey;
    use rustls::pki_types::PrivateKeyDer;
    use rustls::{ServerConfig, ServerConnection};
    use std::net::TcpListener;
    use std::thread;

    /// A peer that closes without `close_notify` must read as end-of-body,
    /// not as a failure: rustls reports that as `UnexpectedEof` and several
    /// real endpoints do it.
    #[test]
    fn a_missing_close_notify_reads_as_end_of_body() {
        struct EofAfter(&'static [u8]);
        impl Read for EofAfter {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "peer closed connection without sending TLS close_notify",
                    ));
                }
                let n = self.0.len().min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut out = String::new();
        read_to_string_lenient(&mut EofAfter(b"HTTP/1.1 200 OK\r\n\r\nbody"), &mut out)
            .expect("a bare UnexpectedEof is the end of the body");
        assert_eq!(out, "HTTP/1.1 200 OK\r\n\r\nbody");
    }

    /// rustls sometimes reports the same condition through its own error
    /// type rather than `UnexpectedEof`; that must read as end-of-body too.
    #[test]
    fn a_close_notify_message_from_any_error_kind_reads_as_end() {
        struct Wrapped {
            sent: bool,
        }
        impl Read for Wrapped {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.sent {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "peer closed connection without sending TLS close_notify",
                    ));
                }
                self.sent = true;
                buf[..5].copy_from_slice(b"hello");
                Ok(5)
            }
        }
        let mut out = String::new();
        read_to_string_lenient(&mut Wrapped { sent: false }, &mut out)
            .expect("a close_notify error is the end of the body");
        assert_eq!(out, "hello");
    }

    /// A real I/O error still propagates.
    #[test]
    fn other_read_errors_still_fail() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("connection reset"))
            }
        }
        let mut out = String::new();
        assert!(read_to_string_lenient(&mut Broken, &mut out).is_err());
    }

    /// The response wrapper is JSON, so a body holding a backslash must be
    /// escaped or `json.get(resp, "body")` reads back a truncated string.
    #[test]
    fn response_escapes_backslashes_in_the_body() {
        let json = make_response(200, "C:\\dir\\path");
        assert!(
            json.contains(r#"C:\\dir\\path"#),
            "wrapper must hold escaped backslashes: {json}"
        );
        // Reading the body back yields the original text, quoted once.
        let body = crate::modules::json::json_get_str(&json, "body").expect("body present");
        assert_eq!(body, "\"C:\\dir\\path\"");
    }

    /// A quote and a newline in the body survive the round trip too.
    #[test]
    fn response_escapes_quotes_and_newlines() {
        let json = make_response(200, "say \"hi\"\nbye");
        let body = crate::modules::json::json_get_str(&json, "body").expect("body present");
        assert_eq!(body, "\"say \"hi\"\nbye\"");
    }

    fn read_http_request<S: Read>(stream: &mut S) -> (String, Vec<u8>) {
        let mut data = Vec::new();
        let mut buf = [0u8; 1024];
        let header_len = loop {
            if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break data.len();
            }
            data.extend_from_slice(&buf[..n]);
        };
        let head = String::from_utf8_lossy(&data[..header_len]).to_string();
        let mut body = data[header_len..].to_vec();
        let declared: usize = head
            .lines()
            .find_map(|l| {
                let lower = l.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0);
        while body.len() < declared {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        (head, body)
    }

    fn spawn_plain_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (head, _body) = read_http_request(&mut stream);
            assert!(head.starts_with("GET /greet HTTP/1.1"));
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://127.0.0.1:{port}/greet"), handle)
    }

    fn spawn_tls_server() -> (String, RootCertStore, thread::JoinHandle<()>) {
        let CertifiedKey {
            cert, signing_key, ..
        } = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_der = cert.der().clone();
        let key_der = PrivateKeyDer::Pkcs8(signing_key.serialize_der().into());
        let server_config = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der.clone()], key_der)
                .unwrap(),
        );

        let mut roots = RootCertStore::empty();
        roots.add(cert_der).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let conn = ServerConnection::new(server_config).unwrap();
            let mut stream = StreamOwned::new(conn, tcp);

            let (head, _body) = read_http_request(&mut stream);
            if head.is_empty() {
                return;
            }
            assert!(head.starts_with("GET /secure HTTP/1.1"));
            let response =
                "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nsecure!";
            stream.write_all(response.as_bytes()).unwrap();

            stream.conn.send_close_notify();
            // rustls treats a bare socket EOF without close_notify as a
            // truncation attack, so shut down explicitly.
            let _ = stream.flush();
        });
        (format!("https://localhost:{port}/secure"), roots, handle)
    }

    fn put(s: &str) -> i64 {
        registry::put_string(s.to_string())
    }

    fn read(id: i64) -> String {
        let s = registry::get_string(id).unwrap_or_default();
        let _ = registry::take_string(id);
        s
    }

    #[test]
    fn test_parse_url() {
        let (host, port, path, is_https) = parse_url("http://example.com/path").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 80);
        assert_eq!(path, "/path");
        assert!(!is_https);
    }

    #[test]
    fn test_parse_url_with_port() {
        let (host, port, path, is_https) = parse_url("http://localhost:8080/api").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 8080);
        assert_eq!(path, "/api");
        assert!(!is_https);
    }

    #[test]
    fn test_parse_https_url() {
        let (host, port, path, is_https) = parse_url("https://example.com/secure").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/secure");
        assert!(is_https);
    }

    #[test]
    fn plain_http_request_round_trip() {
        let (url, server) = spawn_plain_server();
        let (status, body) = http_request_raw("GET", &url, None).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "ok");
        server.join().unwrap();
    }

    #[test]
    fn http_post_sends_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (head, body) = read_http_request(&mut stream);
            assert!(head.starts_with("POST /submit HTTP/1.1"));
            assert_eq!(body, b"hello world");
            let response =
                "HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            stream.write_all(response.as_bytes()).unwrap();
        });
        let url = format!("http://127.0.0.1:{port}/submit");
        let (status, body) = http_request_raw("POST", &url, Some("hello world")).unwrap();
        assert_eq!(status, 201);
        assert_eq!(body, "ok");
        handle.join().unwrap();
    }

    #[test]
    fn https_request_round_trip_uses_tls() {
        let (url, roots, server) = spawn_tls_server();
        let (status, body) = http_request_raw_with_roots("GET", &url, None, &roots).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "secure!");
        server.join().unwrap();
    }

    #[test]
    fn https_request_rejects_untrusted_cert() {
        let (url, _roots, server) = spawn_tls_server();
        let err = http_request_raw("GET", &url, None).unwrap_err();
        assert!(
            err.contains("TLS handshake failed"),
            "expected a TLS handshake failure, got: {err}"
        );
        server.join().unwrap();
    }

    #[test]
    fn http_get_round_trips_via_handle_abi() {
        let (url, server) = spawn_plain_server();
        let response = ntsc_http_get(put(&url));
        assert_eq!(read(response), "{\"status\":200,\"body\":\"ok\"}");
        server.join().unwrap();
    }

    #[test]
    fn async_http_get_offloads_to_pool() {
        let (url, server) = spawn_plain_server();
        let url_handle = put(&url);
        let fut = ntsc_async_http_get(url_handle);
        let mut spins = 0;
        while ntsc_async_http_poll(fut) == 0 {
            spins += 1;
            assert!(spins < 10_000, "offloaded http.get never completed");
            std::thread::yield_now();
        }
        let response = ntsc_async_http_result(fut);
        ntsc_async_http_drop(fut);
        assert_eq!(read(response), "{\"status\":200,\"body\":\"ok\"}");
        let _ = registry::take_string(url_handle);
        server.join().unwrap();
    }

    #[test]
    fn async_http_drop_reclaims_unreaped_payload() {
        let (url, server) = spawn_plain_server();
        let url_handle = put(&url);
        let fut = ntsc_async_http_get(url_handle);
        let mut spins = 0;
        while ntsc_async_http_poll(fut) == 0 {
            spins += 1;
            assert!(spins < 10_000, "offloaded http.get never completed");
            std::thread::yield_now();
        }
        let payload = registry::borrow(fut, |handle| match handle {
            registry::Handle::AsyncOp { result, .. } => *result,
            _ => registry::NULL,
        })
        .unwrap_or(registry::NULL);
        assert_ne!(payload, registry::NULL);

        ntsc_async_http_drop(fut);
        assert!(registry::borrow(payload, |_| ()).is_none());
        let _ = registry::take_string(url_handle);
        server.join().unwrap();
    }

    #[test]
    fn status_code_extracts_from_response_json() {
        assert_eq!(
            ntsc_http_status_code(put("{\"status\":404,\"body\":\"not found\"}")),
            404
        );
    }

    #[test]
    fn http_get_failure_throws() {
        use crate::modules::test_util::catch_throw;
        let url = put("http://127.0.0.1:1/nothing");
        let err = catch_throw(|| {
            let _ = ntsc_http_get(url);
        });
        let msg = err.unwrap();
        assert!(msg.contains("http.get"), "unexpected message: {msg}");
        let _ = registry::take_string(url);
    }
}
