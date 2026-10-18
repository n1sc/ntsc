//! NTSC standard library: `io` module.
//! Files are opaque registry handles released with `io.close`; the standard
//! streams are the sentinel handles -1, -2, -3.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::registry;

/// Cap for `io.read`/`io.read_all` buffers, in bytes.
const MAX_READ_SIZE: u64 = 64 * 1024 * 1024;

/// Sentinel handles for the standard streams, distinct from registry handles.
const STDIN_HANDLE: i64 = -1;
const STDOUT_HANDLE: i64 = -2;
const STDERR_HANDLE: i64 = -3;

fn read_bytes(reader: &mut impl Read, count: i64) -> std::io::Result<String> {
    if count <= 0 {
        return Ok(String::new());
    }
    let count = (count as u64).min(MAX_READ_SIZE) as usize;
    let mut bytes = vec![0_u8; count];
    let bytes_read = reader.read(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes[..bytes_read]).into_owned())
}

fn read_line(reader: &mut impl Read) -> std::io::Result<String> {
    let mut bytes = Vec::with_capacity(64);
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte)? {
            0 => break,
            _ => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_all(reader: &mut impl Read) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    reader.take(MAX_READ_SIZE).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn write_bytes(writer: &mut impl Write, bytes: &[u8]) -> std::io::Result<usize> {
    writer.write_all(bytes)?;
    Ok(bytes.len())
}

fn fail(fn_name: &str, msg: impl std::fmt::Display) -> i64 {
    super::throw_str(format!("io.{fn_name}: {msg}"))
}

fn open_options(mode: &str) -> Result<std::fs::OpenOptions, String> {
    let mut opts = std::fs::OpenOptions::new();
    match mode {
        "r" => {
            opts.read(true);
        }
        "r+" => {
            opts.read(true).write(true);
        }
        "w" => {
            opts.write(true).create(true).truncate(true);
        }
        "w+" => {
            opts.read(true).write(true).create(true).truncate(true);
        }
        "a" => {
            opts.append(true).create(true);
        }
        "a+" => {
            opts.read(true).append(true).create(true);
        }
        _ => {
            return Err(format!(
                "invalid mode '{mode}' (expected r, r+, w, w+, a, a+)"
            ));
        }
    }
    Ok(opts)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_stdin() -> i64 {
    STDIN_HANDLE
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_stdout() -> i64 {
    STDOUT_HANDLE
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_stderr() -> i64 {
    STDERR_HANDLE
}

/// `io.input()` — one line from stdin with the trailing newline removed;
/// "" at end of input.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_input() -> i64 {
    let _block = crate::BlockGuard::enter();
    let result = read_line(&mut std::io::stdin().lock());
    match result {
        Ok(mut text) => {
            if text.ends_with('\n') {
                text.pop();
                if text.ends_with('\r') {
                    text.pop();
                }
            }
            registry::put_string(text)
        }
        Err(error) => fail("input", format!("cannot read from standard input: {error}")),
    }
}

/// `io.open(path, mode)` — mode is one of `r`, `r+`, `w` (truncate), `w+`,
/// `a`, `a+` (append), defaulting to `r`. Throws when the file cannot be
/// opened.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_open(path: i64, mode: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let path = match registry::get_string(path) {
        Some(path) => path,
        None => return fail("open", "null path"),
    };
    let mode = registry::get_string(mode).unwrap_or_else(|| "r".to_string());
    let opts = match open_options(&mode) {
        Ok(opts) => opts,
        Err(msg) => return fail("open", msg),
    };
    match opts.open(&path) {
        Ok(file) => registry::put_opaque(file),
        Err(e) => fail("open", format!("cannot open file '{path}': {e}")),
    }
}

/// `io.close(handle)` — a no-op for the standard-stream sentinels, which
/// the process owns.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_close(handle: i64) -> i8 {
    if matches!(handle, STDIN_HANDLE | STDOUT_HANDLE | STDERR_HANDLE) {
        return 1;
    }
    if registry::take_opaque::<File>(handle).is_none() {
        let _ = super::throw_str("io.close: invalid (null) file handle".to_string());
    }
    1
}

/// `io.read(handle, count)` — up to `count` bytes; "" at end of file.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_read(handle: i64, count: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let outcome = match handle {
        STDIN_HANDLE => Some(read_bytes(&mut std::io::stdin().lock(), count)),
        STDOUT_HANDLE | STDERR_HANDLE => {
            return fail("read", "standard output streams are not readable");
        }
        _ => registry::with_opaque_mut(handle, |file: &mut File| read_bytes(file, count)),
    };
    match outcome {
        Some(Ok(text)) => registry::put_string(text),
        Some(Err(error)) => fail("read", format!("cannot read from stream: {error}")),
        None => fail("read", "invalid (null) file handle"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_read_line(handle: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let outcome = match handle {
        STDIN_HANDLE => Some(read_line(&mut std::io::stdin().lock())),
        STDOUT_HANDLE | STDERR_HANDLE => {
            return fail("read_line", "standard output streams are not readable");
        }
        _ => registry::with_opaque_mut(handle, |file: &mut File| read_line(file)),
    };
    match outcome {
        Some(Ok(text)) => registry::put_string(text),
        Some(Err(error)) => fail(
            "read_line",
            format!("cannot read line from stream: {error}"),
        ),
        None => fail("read_line", "invalid (null) file handle"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_read_all(handle: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let outcome = match handle {
        STDIN_HANDLE => Some(read_all(&mut std::io::stdin().lock())),
        STDOUT_HANDLE | STDERR_HANDLE => {
            return fail("read_all", "standard output streams are not readable");
        }
        _ => registry::with_opaque_mut(handle, |file: &mut File| read_all(file)),
    };
    match outcome {
        Some(Ok(text)) => registry::put_string(text),
        Some(Err(error)) => fail("read_all", format!("cannot read stream contents: {error}")),
        None => fail("read_all", "invalid (null) file handle"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_write(handle: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let data = registry::get_string(data).unwrap_or_default();
    let outcome = match handle {
        STDIN_HANDLE => return fail("write", "standard input is not writable"),
        STDOUT_HANDLE => Some(write_bytes(&mut std::io::stdout().lock(), data.as_bytes())),
        STDERR_HANDLE => Some(write_bytes(&mut std::io::stderr().lock(), data.as_bytes())),
        _ => {
            registry::with_opaque_mut(handle, |file: &mut File| write_bytes(file, data.as_bytes()))
        }
    };
    match outcome {
        Some(Ok(bytes_written)) => bytes_written as i64,
        Some(Err(error)) => fail("write", format!("cannot write to stream: {error}")),
        None => fail("write", "invalid (null) file handle"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_write_line(handle: i64, data: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let data = registry::get_string(data).unwrap_or_default();
    let mut bytes = data.into_bytes();
    bytes.push(b'\n');
    let outcome = match handle {
        STDIN_HANDLE => return fail("write_line", "standard input is not writable"),
        STDOUT_HANDLE => Some(write_bytes(&mut std::io::stdout().lock(), &bytes)),
        STDERR_HANDLE => Some(write_bytes(&mut std::io::stderr().lock(), &bytes)),
        _ => registry::with_opaque_mut(handle, |file: &mut File| write_bytes(file, &bytes)),
    };
    match outcome {
        Some(Ok(bytes_written)) => bytes_written as i64,
        Some(Err(error)) => fail("write_line", format!("cannot write to stream: {error}")),
        None => fail("write_line", "invalid (null) file handle"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_flush(handle: i64) -> i8 {
    let outcome = match handle {
        STDIN_HANDLE => {
            let _ = fail("flush", "standard input is not writable");
            return 0;
        }
        STDOUT_HANDLE => Some(std::io::stdout().lock().flush()),
        STDERR_HANDLE => Some(std::io::stderr().lock().flush()),
        _ => registry::with_opaque_mut(handle, |file: &mut File| file.flush()),
    };
    match outcome {
        Some(Ok(())) => 1,
        Some(Err(error)) => {
            let _ = fail("flush", format!("cannot flush stream: {error}"));
            0
        }
        None => {
            let _ = fail("flush", "invalid (null) file handle");
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_eof(handle: i64) -> i8 {
    if matches!(handle, STDIN_HANDLE | STDOUT_HANDLE | STDERR_HANDLE) {
        let _ = fail("eof", "eof is not supported for standard streams");
        return 0;
    }
    let outcome = registry::with_opaque_mut(handle, |file: &mut File| -> i8 {
        let position = match file.stream_position() {
            Ok(p) => p,
            Err(_) => return 0,
        };
        match file.metadata() {
            Ok(meta) if position >= meta.len() => 1,
            _ => 0,
        }
    });
    match outcome {
        Some(result) => result,
        None => {
            let _ = fail("eof", "invalid (null) file handle");
            0
        }
    }
}

/// `io.seek(handle, offset, whence)` — `whence`: 0 = start, 1 = current,
/// 2 = end. Returns 1 on success, throws on failure.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_seek(handle: i64, offset: i64, whence: i64) -> i8 {
    if matches!(handle, STDIN_HANDLE | STDOUT_HANDLE | STDERR_HANDLE) {
        let _ = fail("seek", "standard streams are not seekable");
        return 0;
    }
    let seek_from = match whence {
        0 => SeekFrom::Start(offset.max(0) as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => {
            let _ = super::throw_str("io.seek: invalid whence (expected 0, 1, or 2)".to_string());
            return 0;
        }
    };
    let outcome = registry::with_opaque_mut(handle, |file: &mut File| -> Result<u64, String> {
        file.seek(seek_from)
            .map_err(|e| format!("cannot seek file handle: {e}"))
    });
    match outcome {
        Some(Ok(_)) => 1,
        Some(Err(msg)) => {
            let _ = fail("seek", msg);
            0
        }
        None => {
            let _ = fail("seek", "invalid (null) file handle");
            0
        }
    }
}

/// `io.tell(handle)` — the current position, or -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_io_tell(handle: i64) -> i64 {
    if matches!(handle, STDIN_HANDLE | STDOUT_HANDLE | STDERR_HANDLE) {
        return fail("tell", "standard streams do not have a seek position");
    }
    let outcome = registry::with_opaque_mut(handle, |file: &mut File| -> i64 {
        match file.stream_position() {
            Ok(p) => p as i64,
            Err(_) => -1,
        }
    });
    match outcome {
        Some(position) => position,
        None => fail("tell", "invalid (null) file handle"),
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

    #[test]
    fn test_write_read_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ntsc_io_test_{}.txt", std::process::id()));
        let path_str = path.to_string_lossy().to_string();

        let h = ntsc_io_open(put(&path_str), put("w"));
        assert_ne!(h, 0);
        assert_eq!(ntsc_io_write_line(h, put("hello")), 6);
        assert_eq!(ntsc_io_write(h, put("world")), 5);
        assert_eq!(ntsc_io_close(h), 1);

        let h = ntsc_io_open(put(&path_str), put("r"));
        assert_ne!(h, 0);
        assert_eq!(read(ntsc_io_read_line(h)), "hello\n");
        assert_eq!(ntsc_io_eof(h), 0);
        assert_eq!(read(ntsc_io_read_line(h)), "world");
        assert_eq!(ntsc_io_eof(h), 1);
        assert_eq!(read(ntsc_io_read_line(h)), "");
        assert_eq!(ntsc_io_eof(h), 1);
        assert_eq!(ntsc_io_close(h), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_seek_tell() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ntsc_io_seek_{}.txt", std::process::id()));
        let path_str = path.to_string_lossy().to_string();

        let h = ntsc_io_open(put(&path_str), put("w"));
        let _ = ntsc_io_write(h, put("0123456789"));
        let _ = ntsc_io_close(h);

        let h = ntsc_io_open(put(&path_str), put("r"));
        assert_eq!(ntsc_io_seek(h, 3, 0), 1);
        assert_eq!(ntsc_io_tell(h), 3);
        assert_eq!(read(ntsc_io_read(h, 3)), "345");
        assert_eq!(ntsc_io_seek(h, -2, 2), 1);
        assert_eq!(read(ntsc_io_read_all(h)), "89");
        let _ = ntsc_io_close(h);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_open_missing_throws() {
        use crate::modules::test_util::catch_throw;
        let err = catch_throw(|| {
            let path = put("/nonexistent_dir_xyz_123/f");
            let mode = put("r");
            let _ = ntsc_io_open(path, mode);
            let _ = registry::take_string(path);
            let _ = registry::take_string(mode);
        });
        let msg = err.unwrap();
        assert!(msg.contains("io.open"), "unexpected message: {msg}");
    }

    #[test]
    fn test_invalid_mode_throws() {
        use crate::modules::test_util::catch_throw;
        let err = catch_throw(|| {
            let path = put("/tmp/x");
            let mode = put("x");
            let _ = ntsc_io_open(path, mode);
            let _ = registry::take_string(path);
            let _ = registry::take_string(mode);
        });
        let msg = err.unwrap();
        assert!(msg.contains("io.open"), "unexpected message: {msg}");
    }
}
