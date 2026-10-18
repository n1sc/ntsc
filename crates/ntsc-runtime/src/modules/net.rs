//! NTSC standard library: `net` module.
//! TCP and UDP sockets, stored in the registry as opaque handles that must
//! be released with `net.close`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};

use crate::registry;

/// Cap for `net.recv`/`net.udp_recv` buffers, in bytes.
const MAX_RECV_SIZE: usize = 64 * 1024 * 1024;

/// Tagged socket handle so every operation can validate the variant and
/// throw instead of misusing memory.
enum NetHandle {
    TcpStream(TcpStream),
    TcpListener(TcpListener),
    UdpSocket(UdpSocket),
}

fn fail(fn_name: &str, msg: impl std::fmt::Display) -> i64 {
    super::throw_str(format!("net.{fn_name}: {msg}"))
}

fn stream_mut<R>(
    fn_name: &str,
    handle: i64,
    f: impl FnOnce(&mut TcpStream) -> Result<R, String>,
) -> Result<R, String> {
    match registry::with_opaque_io::<NetHandle, Option<Result<R, String>>>(
        handle,
        |net| match net {
            NetHandle::TcpStream(mut stream) => {
                let result = f(&mut stream);
                (NetHandle::TcpStream(stream), Some(result))
            }
            other => (other, None),
        },
    ) {
        Some(Some(result)) => result,
        Some(None) => Err(format!("net.{fn_name}: handle is not a TCP stream")),
        None => Err(format!("net.{fn_name}: invalid (null) socket handle")),
    }
}

fn udp_mut<R>(
    fn_name: &str,
    handle: i64,
    f: impl FnOnce(&mut UdpSocket) -> Result<R, String>,
) -> Result<R, String> {
    match registry::with_opaque_io::<NetHandle, Option<Result<R, String>>>(
        handle,
        |net| match net {
            NetHandle::UdpSocket(mut socket) => {
                let result = f(&mut socket);
                (NetHandle::UdpSocket(socket), Some(result))
            }
            other => (other, None),
        },
    ) {
        Some(Some(result)) => result,
        Some(None) => Err(format!("net.{fn_name}: handle is not a UDP socket")),
        None => Err(format!("net.{fn_name}: invalid (null) socket handle")),
    }
}

/// Register a connected TCP socket as a handle.
///
/// `TCP_NODELAY` is set on every stream, matching Go (`net` sets it on all TCP
/// connections by default). Without it Nagle's algorithm holds a small response
/// waiting for an ACK of the previous segment, which for a request/response
/// exchange of two short lines adds a delayed-ACK stall to every round trip.
fn adopt_stream(stream: TcpStream) -> i64 {
    let _ = stream.set_nodelay(true);
    registry::put_opaque(NetHandle::TcpStream(stream))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_tcp_connect(host: i64, port: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let host = match registry::get_string(host) {
        Some(host) => host,
        None => return fail("tcp_connect", "null string argument"),
    };
    let addr = format!("{host}:{port}");
    match TcpStream::connect(&addr) {
        Ok(stream) => adopt_stream(stream),
        Err(e) => fail("tcp_connect", format!("cannot connect to '{addr}': {e}")),
    }
}

/// `net.tcp_listen(port)` — binds on 0.0.0.0; port 0 picks an ephemeral port
/// (see `net.local_port`).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_tcp_listen(port: i64) -> i64 {
    let addr = SocketAddr::from(([0, 0, 0, 0], port.clamp(0, 65_535) as u16));
    match TcpListener::bind(addr) {
        Ok(listener) => registry::put_opaque(NetHandle::TcpListener(listener)),
        Err(e) => fail("tcp_listen", format!("cannot bind port {port}: {e}")),
    }
}

/// `net.local_port(handle)` — the local port, or -1 when unavailable;
/// useful with port 0.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_local_port(handle: i64) -> i64 {
    let outcome = registry::with_opaque(handle, |net: &NetHandle| -> Option<i64> {
        match net {
            NetHandle::TcpListener(listener) => match listener.local_addr() {
                Ok(addr) => Some(addr.port() as i64),
                Err(_) => Some(-1),
            },
            NetHandle::UdpSocket(socket) => match socket.local_addr() {
                Ok(addr) => Some(addr.port() as i64),
                Err(_) => Some(-1),
            },
            _ => None,
        }
    });
    match outcome {
        Some(Some(port)) => port,
        Some(None) => fail("local_port", "handle is not a TCP listener or UDP socket"),
        None => fail("local_port", "invalid (null) socket handle"),
    }
}

/// `net.tcp_accept(listener)` — blocks until a client connects; returns a
/// stream handle.
/// `net.accept_poll(listener, timeout_ms)` — accept a connection, giving up
/// after `timeout_ms` and returning the null handle.
///
/// The listener is switched to non-blocking the first time and left that
/// way. Toggling it back per call looked harmless and was not: a connection
/// that arrived while the socket was in blocking mode was accepted by the
/// kernel but then dropped, so the client connected and never got a reply.
/// A listener polled this way must not also be used with `net.tcp_accept`,
/// which expects a blocking listener. The wait is a short sleep between
/// tries: the runtime has no readiness poller to wait on.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_accept_poll(handle: i64, timeout_ms: f64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(0.0) as u64);
    loop {
        let outcome = registry::with_opaque_io::<
            NetHandle,
            Option<Result<Option<TcpStream>, String>>,
        >(handle, |net| match net {
            NetHandle::TcpListener(listener) => {
                let _ = listener.set_nonblocking(true);
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        // The client socket must block: a non-blocking read
                        // returns "would block" with no bytes, which the
                        // line reader cannot tell from end-of-stream, so the
                        // request looked empty and the connection closed with
                        // no response at all. It also gets a read timeout: a
                        // half-open connection must not stall the caller in
                        // a blocking read forever.
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                        (NetHandle::TcpListener(listener), Some(Ok(Some(stream))))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        (NetHandle::TcpListener(listener), Some(Ok(None)))
                    }
                    Err(e) => (
                        NetHandle::TcpListener(listener),
                        Some(Err(format!("cannot accept connection: {e}"))),
                    ),
                }
            }
            other => (other, None),
        });
        match outcome {
            Some(Some(Ok(Some(stream)))) => return adopt_stream(stream),
            Some(Some(Ok(None))) => {}
            Some(Some(Err(msg))) => return fail("accept_poll", msg),
            Some(None) => return fail("accept_poll", "handle is not a TCP listener"),
            None => return fail("accept_poll", "invalid (null) socket handle"),
        }
        if std::time::Instant::now() >= deadline {
            return registry::NULL;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_tcp_accept(handle: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let outcome =
        registry::with_opaque_io::<NetHandle, Option<Result<TcpStream, String>>>(handle, |net| {
            match net {
                NetHandle::TcpListener(listener) => match listener.accept() {
                    Ok((stream, _peer)) => (NetHandle::TcpListener(listener), Some(Ok(stream))),
                    Err(e) => (
                        NetHandle::TcpListener(listener),
                        Some(Err(format!("cannot accept connection: {e}"))),
                    ),
                },
                other => (other, None),
            }
        });
    match outcome {
        Some(Some(Ok(stream))) => adopt_stream(stream),
        Some(Some(Err(msg))) => fail("tcp_accept", msg),
        Some(None) => fail("tcp_accept", "handle is not a TCP listener"),
        None => fail("tcp_accept", "invalid (null) socket handle"),
    }
}

/// `net.send(handle, data)` — returns the number of bytes written.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_send(handle: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let data = registry::get_string(data).unwrap_or_default();
    match stream_mut("send", handle, |stream| -> Result<usize, String> {
        match stream.write_all(data.as_bytes()) {
            Ok(_) => Ok(data.len()),
            Err(e) => Err(format!("net.send: cannot write to stream: {e}")),
        }
    }) {
        Ok(n) => n as i64,
        Err(msg) => {
            let _ = super::throw_str(msg);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_send_line(handle: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let data = registry::get_string(data).unwrap_or_default();
    let mut bytes = data.as_bytes().to_vec();
    bytes.push(b'\n');
    match stream_mut("send_line", handle, |stream| -> Result<usize, String> {
        match stream.write_all(&bytes) {
            Ok(_) => Ok(bytes.len()),
            Err(e) => Err(format!("net.send_line: cannot write to stream: {e}")),
        }
    }) {
        Ok(n) => n as i64,
        Err(msg) => {
            let _ = super::throw_str(msg);
            0
        }
    }
}

/// `net.recv(handle, count)` — up to `count` bytes; "" at end of stream.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_recv(handle: i64, count: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    match stream_mut("recv", handle, |stream| -> Result<String, String> {
        if count <= 0 {
            return Ok(String::new());
        }
        let count = (count as usize).min(MAX_RECV_SIZE);
        let mut buf = vec![0u8; count];
        match stream.read(&mut buf) {
            Ok(n) => Ok(String::from_utf8_lossy(&buf[..n]).to_string()),
            Err(e) => Err(format!("net.recv: cannot read from stream: {e}")),
        }
    }) {
        Ok(text) => registry::put_string(text),
        Err(msg) => {
            let _ = super::throw_str(msg);
            0
        }
    }
}

/// `net.recv_line(handle)` — one line, newline included; "" at end of
/// stream.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_recv_line(handle: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    match stream_mut("recv_line", handle, |stream| -> Result<String, String> {
        // Read in blocks and keep what follows the newline for the next call:
        // one byte per `read` syscall made a short line cost as many syscalls
        // as it had characters.
        let mut bytes = Vec::with_capacity(64);
        let mut chunk = [0u8; 512];
        loop {
            match stream.peek(&mut chunk) {
                Ok(0) => break,
                Ok(available) => {
                    let upto = chunk[..available]
                        .iter()
                        .position(|&b| b == b'\n')
                        .map(|index| index + 1)
                        .unwrap_or(available);
                    // Consume exactly the bytes taken, so anything after the
                    // newline stays queued for the next read.
                    let mut taken = vec![0u8; upto];
                    match stream.read_exact(&mut taken) {
                        Ok(()) => {}
                        Err(e) => {
                            return Err(format!("net.recv_line: cannot read from stream: {e}"));
                        }
                    }
                    let hit_newline = taken.last() == Some(&b'\n');
                    bytes.extend_from_slice(&taken);
                    if hit_newline {
                        break;
                    }
                }
                Err(e) => return Err(format!("net.recv_line: cannot read from stream: {e}")),
            }
        }
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }) {
        Ok(text) => registry::put_string(text),
        Err(msg) => {
            let _ = super::throw_str(msg);
            0
        }
    }
}

/// `net.close(handle)` — releases the handle; it must not be used afterwards.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_close(handle: i64) -> i8 {
    if registry::take_opaque::<NetHandle>(handle).is_none() {
        let _ = super::throw_str("net.close: invalid (null) socket handle".to_string());
    }
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_udp_bind(port: i64) -> i64 {
    let addr = SocketAddr::from(([0, 0, 0, 0], port.clamp(0, 65_535) as u16));
    match UdpSocket::bind(addr) {
        Ok(socket) => registry::put_opaque(NetHandle::UdpSocket(socket)),
        Err(e) => fail("udp_bind", format!("cannot bind port {port}: {e}")),
    }
}

/// `net.udp_send(socket, host, port, data)` — returns the number of bytes
/// sent.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_udp_send(handle: i64, host: i64, port: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let host = match registry::get_string(host) {
        Some(host) => host,
        None => return fail("udp_send", "null string argument"),
    };
    let data = match registry::get_string(data) {
        Some(data) => data,
        None => return fail("udp_send", "null string argument"),
    };
    let addr = format!("{host}:{port}");
    match udp_mut("udp_send", handle, |socket| -> Result<usize, String> {
        match socket.send_to(data.as_bytes(), &addr) {
            Ok(n) => Ok(n),
            Err(e) => Err(format!("net.udp_send: cannot send to '{addr}': {e}")),
        }
    }) {
        Ok(n) => n as i64,
        Err(msg) => {
            let _ = super::throw_str(msg);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_net_udp_recv(handle: i64, count: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    match udp_mut("udp_recv", handle, |socket| -> Result<String, String> {
        if count <= 0 {
            return Ok(String::new());
        }
        let count = (count as usize).min(MAX_RECV_SIZE);
        let mut buf = vec![0u8; count];
        match socket.recv(&mut buf) {
            Ok(n) => Ok(String::from_utf8_lossy(&buf[..n]).to_string()),
            Err(e) => Err(format!("net.udp_recv: cannot receive datagram: {e}")),
        }
    }) {
        Ok(text) => registry::put_string(text),
        Err(msg) => {
            let _ = super::throw_str(msg);
            0
        }
    }
}

// ── Awaitable accept ───────────────────────────────────────────────────────
//
// `net.tcp_accept` blocks the OS thread it runs on, so an accept loop parked
// in it never yields and the per-client goroutines it spawns are not polled.
//
// `accept_async` is reactor-backed rather than offloaded: the listener is set
// non-blocking and registered with the reactor, each poll tries a
// non-blocking `accept` on the worker itself, and the goroutine parks on
// readiness when nothing is pending. Handing accepts to the offload pool
// instead would cap a single accept loop at the pool's thread count.

/// A pending reactor-backed accept: the reactor registration watching the
/// listener, the listener handle to accept from, and the accepted socket once
/// a poll has taken it.
struct PendingAccept {
    io: i64,
    listener: i64,
    accepted: Option<TcpStream>,
    error: Option<String>,
}

/// Set the listener non-blocking and return its raw descriptor. A listener used
/// with `accept_async` stays non-blocking; the reactor reports its readiness, so
/// a blocking accept would only risk stalling a worker.
fn listener_watch_fd(handle: i64) -> Option<i64> {
    registry::with_opaque(handle, |net: &NetHandle| match net {
        NetHandle::TcpListener(listener) => {
            if listener.set_nonblocking(true).is_err() {
                return None;
            }
            #[cfg(unix)]
            {
                Some(std::os::fd::AsRawFd::as_raw_fd(listener) as i64)
            }
            #[cfg(windows)]
            {
                Some(std::os::windows::io::AsRawSocket::as_raw_socket(listener) as i64)
            }
        }
        _ => None,
    })
    .flatten()
}

/// Try one non-blocking accept on `listener`. `Ok(None)` means "nothing
/// pending", which is not an error: the caller parks and retries.
fn try_accept(listener: i64) -> Result<Option<TcpStream>, String> {
    registry::with_opaque(listener, |net: &NetHandle| match net {
        NetHandle::TcpListener(listener) => match listener.accept() {
            Ok((stream, _peer)) => Ok(Some(stream)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(format!("net.accept_async: cannot accept connection: {e}")),
        },
        _ => Err("net.accept_async: handle is not a TCP listener".to_string()),
    })
    .unwrap_or_else(|| Err("net.accept_async: invalid (null) socket handle".to_string()))
}

/// `net.accept_async(listener)` — a future that completes with the next client
/// socket. Await it; the goroutine parks on listener readiness meanwhile.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_net_accept(listener: i64) -> i64 {
    let Some(fd) = listener_watch_fd(listener) else {
        return fail("accept_async", "handle is not a TCP listener");
    };
    let io = crate::ntask_io_new();
    crate::ntask_io_attach(io, fd, 1);
    registry::put_opaque(PendingAccept {
        io,
        listener,
        accepted: None,
        error: None,
    })
}

/// Poll a pending accept: take a connection if one is pending, else park on the
/// listener's readiness. The accept runs on the worker, so a ready listener is
/// served without a thread handoff.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_net_accept_poll(id: i64) -> i8 {
    let Some((io, listener, done)) = registry::with_opaque(id, |state: &PendingAccept| {
        (
            state.io,
            state.listener,
            state.accepted.is_some() || state.error.is_some(),
        )
    }) else {
        return 1;
    };
    if done {
        return 1;
    }
    // Consume any recorded readiness so a later park re-arms the interest.
    let _ = crate::ntask_io_ready(io);
    match try_accept(listener) {
        Ok(Some(stream)) => {
            registry::with_opaque_mut(id, |state: &mut PendingAccept| {
                state.accepted = Some(stream)
            });
            1
        }
        Ok(None) => {
            crate::ntask_io_park(io, 1);
            0
        }
        Err(message) => {
            registry::with_opaque_mut(id, |state: &mut PendingAccept| state.error = Some(message));
            1
        }
    }
}

/// Deliver the accepted socket handle, or throw the recorded error. Releases the
/// reactor registration and the pending-accept state either way.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_net_accept_result(id: i64) -> i64 {
    let Some(state) = registry::take_opaque::<PendingAccept>(id) else {
        return fail("accept_async", "invalid (null) future handle");
    };
    crate::ntask_io_drop(state.io);
    match (state.accepted, state.error) {
        (Some(stream), _) => adopt_stream(stream),
        (None, Some(message)) => super::throw_str(message),
        (None, None) => fail("accept_async", "future completed without a connection"),
    }
}

/// Drop a pending accept, releasing its reactor registration and any socket it
/// already took (the awaiting goroutine went away before reaping it).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_net_accept_drop(id: i64) {
    if let Some(state) = registry::take_opaque::<PendingAccept>(id) {
        crate::ntask_io_drop(state.io);
    }
}

// ── Awaitable line read ────────────────────────────────────────────────────
//
// The blocking `recv_line` owns its worker thread until the client's bytes
// arrive, so with one worker per core a slow client caps the server's
// concurrency at the worker count. `recv_line_async` follows the pattern Go's
// netpoller and Tokio both use: try the syscall first and only park when it
// reports `WouldBlock`. A ready socket costs one `recv` with no scheduler
// involvement at all; a socket with nothing pending releases the worker to run
// another goroutine.

/// A pending awaitable line read: the reactor registration watching the socket,
/// the socket itself, and the line once a poll has completed it.
struct PendingRecvLine {
    io: i64,
    sock: i64,
    line: Option<String>,
    error: Option<String>,
}

/// Set the stream non-blocking and return its raw descriptor. Only the async
/// read path does this; the synchronous `recv`/`recv_line` keep blocking
/// semantics, so a program mixing the two is unaffected.
fn stream_watch_fd(handle: i64) -> Option<i64> {
    registry::with_opaque(handle, |net: &NetHandle| match net {
        NetHandle::TcpStream(stream) => {
            if stream.set_nonblocking(true).is_err() {
                return None;
            }
            #[cfg(unix)]
            {
                Some(std::os::fd::AsRawFd::as_raw_fd(stream) as i64)
            }
            // The reactor's Windows backend watches sockets with `WSAPoll`, which
            // keys on the raw `SOCKET` handle rather than a file descriptor.
            #[cfg(windows)]
            {
                Some(std::os::windows::io::AsRawSocket::as_raw_socket(stream) as i64)
            }
        }
        _ => None,
    })
    .flatten()
}

/// Try to take one line without blocking. `Ok(None)` means "nothing pending
/// yet", which is not an error: the caller parks and retries.
///
/// `peek` leaves the bytes in the socket, so the newline scan can decide how
/// much to consume and anything past the newline stays queued for the next
/// read. That keeps line framing correct without a userspace buffer.
fn try_recv_line(sock: i64) -> Result<Option<String>, String> {
    registry::with_opaque(sock, |net: &NetHandle| match net {
        NetHandle::TcpStream(stream) => {
            let mut chunk = [0u8; 512];
            match stream.peek(&mut chunk) {
                // Orderly shutdown with nothing buffered: an empty line.
                Ok(0) => Ok(Some(String::new())),
                Ok(available) => {
                    let upto = chunk[..available]
                        .iter()
                        .position(|&b| b == b'\n')
                        .map(|index| index + 1);
                    let Some(upto) = upto else {
                        // A partial line: wait for the rest rather than
                        // consuming what would split it.
                        return Ok(None);
                    };
                    let mut taken = vec![0u8; upto];
                    match std::io::Read::read_exact(&mut &*stream, &mut taken) {
                        Ok(()) => Ok(Some(String::from_utf8_lossy(&taken).to_string())),
                        Err(e) => Err(format!("net.recv_line_async: cannot read: {e}")),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
                Err(e) => Err(format!("net.recv_line_async: cannot read: {e}")),
            }
        }
        _ => Err("net.recv_line_async: handle is not a TCP stream".to_string()),
    })
    .unwrap_or_else(|| Err("net.recv_line_async: invalid (null) socket handle".to_string()))
}

/// `net.recv_line_async(sock)` — a future completing with one line. Await it;
/// the goroutine parks on socket readiness instead of holding a worker.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_net_recv_line(sock: i64) -> i64 {
    if stream_watch_fd(sock).is_none() {
        return fail("recv_line_async", "handle is not a TCP stream");
    }
    // Optimistic read: a socket whose bytes already arrived completes here with
    // no reactor registration, no park, and no wake — the common case for a
    // request/response server, where the client writes before the handler runs.
    // The registration is created lazily, only once a read reports WouldBlock.
    let (line, error) = match try_recv_line(sock) {
        Ok(line) => (line, None),
        Err(message) => (None, Some(message)),
    };
    registry::put_opaque(PendingRecvLine {
        io: registry::NULL,
        sock,
        line,
        error,
    })
}

/// The reactor registration for this pending read, created on first need.
fn pending_recv_io(id: i64, sock: i64) -> i64 {
    if let Some(io) = registry::with_opaque(id, |state: &PendingRecvLine| state.io)
        && io != registry::NULL
    {
        return io;
    }
    let Some(fd) = stream_watch_fd(sock) else {
        return registry::NULL;
    };
    let io = crate::ntask_io_new();
    crate::ntask_io_attach(io, fd, 1);
    registry::with_opaque_mut(id, |state: &mut PendingRecvLine| state.io = io);
    io
}

/// Poll a pending line read: take the line if one is available, else park on
/// the socket's readiness.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_net_recv_line_poll(id: i64) -> i8 {
    let Some((sock, done)) = registry::with_opaque(id, |state: &PendingRecvLine| {
        (state.sock, state.line.is_some() || state.error.is_some())
    }) else {
        return 1;
    };
    if done {
        return 1;
    }
    let io = pending_recv_io(id, sock);
    let _ = crate::ntask_io_ready(io);
    match try_recv_line(sock) {
        Ok(Some(line)) => {
            registry::with_opaque_mut(id, |state: &mut PendingRecvLine| state.line = Some(line));
            1
        }
        Ok(None) => {
            crate::ntask_io_park(io, 1);
            0
        }
        Err(message) => {
            registry::with_opaque_mut(id, |state: &mut PendingRecvLine| {
                state.error = Some(message)
            });
            1
        }
    }
}

/// Deliver the line, or throw the recorded error. Releases the registration and
/// the pending state either way.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_net_recv_line_result(id: i64) -> i64 {
    let Some(state) = registry::take_opaque::<PendingRecvLine>(id) else {
        return fail("recv_line_async", "invalid (null) future handle");
    };
    if state.io != registry::NULL {
        crate::ntask_io_drop(state.io);
    }
    match (state.line, state.error) {
        (Some(line), _) => registry::put_string(line),
        (None, Some(message)) => super::throw_str(message),
        (None, None) => registry::put_string(String::new()),
    }
}

/// Drop a pending line read, releasing its reactor registration.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_net_recv_line_drop(id: i64) {
    if let Some(state) = registry::take_opaque::<PendingRecvLine>(id)
        && state.io != registry::NULL
    {
        crate::ntask_io_drop(state.io);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(s: &str) -> i64 {
        registry::put_string(s.to_string())
    }

    fn read(id: i64) -> String {
        let s = registry::get_string(id).unwrap_or_default();
        let _ = registry::take_string(id);
        s
    }

    /// A short line must cost one block read, and anything after the newline
    /// must stay queued for the next call rather than being swallowed.
    #[test]
    fn recv_line_reads_in_blocks_and_keeps_the_remainder() {
        let listener = ntsc_net_tcp_listen(0);
        let port = ntsc_net_local_port(listener);
        let host = put("127.0.0.1");
        let client = ntsc_net_tcp_connect(host, port);
        let _ = registry::take_string(host);
        let conn = ntsc_net_tcp_accept(listener);

        // Two lines in one write: the first read must return only the first.
        let payload = put("alpha\nbeta\n");
        assert!(ntsc_net_send(conn, payload) > 0);
        let _ = registry::take_string(payload);

        assert_eq!(read(ntsc_net_recv_line(client)), "alpha\n");
        assert_eq!(read(ntsc_net_recv_line(client)), "beta\n");

        assert_eq!(ntsc_net_close(client), 1);
        assert_eq!(ntsc_net_close(conn), 1);
        assert_eq!(ntsc_net_close(listener), 1);
    }

    /// Every accepted and connected stream has Nagle disabled, matching Go's
    /// default, so a small response is not held waiting for an ACK.
    #[test]
    fn streams_are_created_with_tcp_nodelay() {
        let listener = ntsc_net_tcp_listen(0);
        let port = ntsc_net_local_port(listener);
        let host = put("127.0.0.1");
        let client = ntsc_net_tcp_connect(host, port);
        let _ = registry::take_string(host);
        let conn = ntsc_net_tcp_accept(listener);

        for handle in [client, conn] {
            let nodelay = registry::with_opaque(handle, |net: &NetHandle| match net {
                NetHandle::TcpStream(stream) => stream.nodelay().ok(),
                _ => None,
            })
            .flatten();
            assert_eq!(nodelay, Some(true));
        }

        assert_eq!(ntsc_net_close(client), 1);
        assert_eq!(ntsc_net_close(conn), 1);
        assert_eq!(ntsc_net_close(listener), 1);
    }

    #[test]
    fn test_tcp_echo() {
        let listener = ntsc_net_tcp_listen(0);
        assert_ne!(listener, 0);
        let port = ntsc_net_local_port(listener);
        assert!(port > 0);

        let client = ntsc_net_tcp_connect(put("127.0.0.1"), port);
        assert_ne!(client, 0);
        let server = ntsc_net_tcp_accept(listener);
        assert_ne!(server, 0);

        assert_eq!(ntsc_net_send_line(client, put("ping")), 5);
        assert_eq!(read(ntsc_net_recv_line(server)), "ping\n");

        assert_eq!(ntsc_net_send(server, put("pong")), 4);
        assert_eq!(read(ntsc_net_recv(client, 4)), "pong");

        assert_eq!(ntsc_net_close(client), 1);
        assert_eq!(ntsc_net_close(server), 1);
        assert_eq!(ntsc_net_close(listener), 1);
    }

    #[test]
    fn test_udp_roundtrip() {
        let receiver = ntsc_net_udp_bind(0);
        assert_ne!(receiver, 0);
        let port = ntsc_net_local_port(receiver);
        assert!(port > 0);

        let sender = ntsc_net_udp_bind(0);
        assert_ne!(sender, 0);
        let sent = ntsc_net_udp_send(sender, put("127.0.0.1"), port, put("hello"));
        assert_eq!(sent, 5);
        assert_eq!(read(ntsc_net_udp_recv(receiver, 64)), "hello");

        assert_eq!(ntsc_net_close(sender), 1);
        assert_eq!(ntsc_net_close(receiver), 1);
    }

    #[test]
    fn test_connect_failure_throws() {
        use crate::modules::test_util::catch_throw;

        let err = catch_throw(|| {
            let host = put("127.0.0.1");
            let _ = ntsc_net_tcp_connect(host, 1);
            let _ = registry::take_string(host);
        });
        let msg = err.unwrap();
        assert!(msg.contains("net.tcp_connect"), "unexpected message: {msg}");
    }

    #[test]
    fn test_wrong_variant_throws() {
        use crate::modules::test_util::catch_throw;
        let listener = ntsc_net_tcp_listen(0);
        let err = catch_throw(|| {
            let data = put("x");
            let _ = ntsc_net_send(listener, data);
            let _ = registry::take_string(data);
        });
        let msg = err.unwrap();
        assert!(msg.contains("net.send"), "unexpected message: {msg}");
        assert_eq!(ntsc_net_close(listener), 1);
    }
}
