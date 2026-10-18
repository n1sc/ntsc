//! NTSC standard library: `os` module.
//! Environment (via the NTSC overlay), path utilities, and temp files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use crate::registry;

// NTSC-owned environment overlay: Rust 2024 cannot safely mutate the
// process-global C environment, so setenv/unsetenv record here, and reads
// plus spawned child processes consult this map instead.
static ENVIRONMENT: LazyLock<Mutex<HashMap<String, Option<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn environment_lock() -> std::sync::MutexGuard<'static, HashMap<String, Option<String>>> {
    ENVIRONMENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn environment_var(name: &str) -> Option<String> {
    match environment_lock().get(name) {
        Some(value) => value.clone(),
        None => std::env::var(name).ok(),
    }
}

pub(crate) fn apply_environment(command: &mut std::process::Command) {
    for (name, value) in environment_lock().iter() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
}

fn fail(fn_name: &str, msg: impl std::fmt::Display) -> i64 {
    super::throw_str(format!("os.{fn_name}: {msg}"))
}

/// Unique temp-file token: process-wide counter + pid + clock seed, so
/// concurrent processes cannot collide.
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(counter.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    state ^= state >> 30;
    state = state.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    state ^= state >> 27;
    state = state.wrapping_mul(0x94d0_49bb_1331_11eb);
    state ^= state >> 31;
    format!("{}-{}-{:016x}", counter, std::process::id(), state)
}

/// `os.getenv(name)` — the variable's value, or "" when unset (use
/// `os.has_env` to distinguish).
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_getenv(name: i64) -> i64 {
    let name = registry::get_string(name).unwrap_or_default();
    registry::put_string(environment_var(&name).unwrap_or_default())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_setenv(name: i64, value: i64) -> i8 {
    let name = registry::get_string(name).unwrap_or_default();
    let value = registry::get_string(value).unwrap_or_default();
    environment_lock().insert(name, Some(value));
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_unsetenv(name: i64) -> i8 {
    let name = registry::get_string(name).unwrap_or_default();
    environment_lock().insert(name, None);
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_has_env(name: i64) -> i8 {
    let name = registry::get_string(name).unwrap_or_default();
    i8::from(environment_var(&name).is_some())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_path_join(a: i64, b: i64) -> i64 {
    let a = registry::get_string(a).unwrap_or_default();
    let b = registry::get_string(b).unwrap_or_default();
    let joined = Path::new(&a).join(b);
    registry::put_string(joined.to_string_lossy().to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_path_dirname(p: i64) -> i64 {
    let p = registry::get_string(p).unwrap_or_default();
    let dirname = Path::new(&p)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(dirname)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_path_basename(p: i64) -> i64 {
    let p = registry::get_string(p).unwrap_or_default();
    let basename = Path::new(&p)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(basename)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_path_ext(p: i64) -> i64 {
    let p = registry::get_string(p).unwrap_or_default();
    let ext = Path::new(&p)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(ext)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_path_stem(p: i64) -> i64 {
    let p = registry::get_string(p).unwrap_or_default();
    let stem = Path::new(&p)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(stem)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_path_abs(p: i64) -> i64 {
    let p = registry::get_string(p).unwrap_or_default();
    let abs = match std::path::absolute(&p) {
        Ok(a) => a.to_string_lossy().to_string(),
        Err(e) => return fail("path_abs", format!("cannot absolutize '{p}': {e}")),
    };
    registry::put_string(abs)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_is_abs(p: i64) -> i8 {
    let p = registry::get_string(p).unwrap_or_default();
    if Path::new(&p).is_absolute() { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_separator() -> i64 {
    registry::put_string(std::path::MAIN_SEPARATOR.to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_temp_dir() -> i64 {
    registry::put_string(std::env::temp_dir().to_string_lossy().to_string())
}

/// `os.temp_path(prefix)` — a unique path; the file itself is not created.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_temp_path(prefix: i64) -> i64 {
    let prefix = registry::get_string(prefix).unwrap_or_default();
    let path = unique_temp_path(&prefix);
    registry::put_string(path.to_string_lossy().to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_temp_file(prefix: i64) -> i64 {
    let prefix = registry::get_string(prefix).unwrap_or_default();
    let path = unique_temp_path(&prefix);
    match std::fs::File::create(&path) {
        Ok(_) => registry::put_string(path.to_string_lossy().to_string()),
        Err(e) => fail(
            "temp_file",
            format!("cannot create '{}': {e}", path.display()),
        ),
    }
}

fn unique_temp_path(prefix: &str) -> PathBuf {
    let base: PathBuf = std::env::temp_dir().join(prefix);
    let dir = base.parent().unwrap_or(Path::new("."));
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let suffix = unique_suffix();
    let name = match base.extension() {
        Some(ext) => format!("{stem}-{suffix}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{suffix}"),
    };
    dir.join(name)
}

/// Files this process currently holds advisory locks on, keyed by path.
/// The lock lives as long as the `File` stays here; `os.file_unlock`
/// removes and releases it.
static LOCKED_FILES: LazyLock<Mutex<HashMap<String, std::fs::File>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `os.file_lock(path)` — acquire an exclusive advisory file lock on the
/// given path. Creates the file if it does not exist. Returns a file
/// handle (int) on success. Throws on failure.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_file_lock(path: i64) -> i64 {
    let path = registry::get_string(path).unwrap_or_default();
    use std::io::Result;
    fn lock_file(path: &str) -> Result<i64> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        if let Err(e) = file.try_lock() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("file is already locked: {e}"),
            ));
        }
        let handle = registry::put_string(path.to_string());
        LOCKED_FILES
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(path.to_string(), file);
        Ok(handle)
    }
    match lock_file(&path) {
        Ok(h) => h,
        Err(e) => fail("file_lock", format!("cannot lock '{path}': {e}")),
    }
}

/// `os.file_unlock(path)` — release the advisory file lock acquired by
/// `os.file_lock`. Returns 1 when the path is unlocked in this process
/// (including when it was never locked here), 0 on failure.
/// `os.rename(from, to)` — moves a file or directory. Returns false when the
/// move is not possible; the target is left alone in that case.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_rename(from: i64, to: i64) -> i8 {
    let _block = crate::BlockGuard::enter();
    let from = registry::get_string(from).unwrap_or_default();
    let to = registry::get_string(to).unwrap_or_default();
    i8::from(std::fs::rename(&from, &to).is_ok())
}

/// `os.executable_path()` — the running program's own path, or `""` when the
/// platform cannot report it.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_executable_path() -> i64 {
    let _block = crate::BlockGuard::enter();
    match std::env::current_exe() {
        Ok(path) => registry::put_string(path.to_string_lossy().to_string()),
        Err(_) => registry::put_string(String::new()),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_os_file_unlock(path: i64) -> i8 {
    let path = registry::get_string(path).unwrap_or_default();
    let file = LOCKED_FILES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&path);
    match file {
        // Dropping the File releases the lock too; unlock() makes it explicit.
        Some(file) => i8::from(file.unlock().is_ok()),
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ntsc_exception_clear, ntsc_exception_pending};

    fn put(s: &str) -> i64 {
        registry::put_string(s.to_string())
    }

    fn read(id: i64) -> String {
        let s = registry::get_string(id).unwrap_or_default();
        let _ = registry::take_string(id);
        s
    }

    #[test]
    fn test_getenv_setenv_unsetenv() {
        assert_eq!(ntsc_os_setenv(put("NTSC_TEST_ENV"), put("hello")), 1);
        assert_eq!(ntsc_os_has_env(put("NTSC_TEST_ENV")), 1);
        assert_eq!(read(ntsc_os_getenv(put("NTSC_TEST_ENV"))), "hello");
        assert_eq!(ntsc_os_unsetenv(put("NTSC_TEST_ENV")), 1);
        assert_eq!(ntsc_os_has_env(put("NTSC_TEST_ENV")), 0);
        assert_eq!(read(ntsc_os_getenv(put("NTSC_TEST_ENV"))), "");
    }

    #[test]
    fn test_path_ops() {
        assert_eq!(
            read(ntsc_os_path_join(put("a"), put("b"))),
            format!("a{}b", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(read(ntsc_os_path_dirname(put("/a/b/c.txt"))), "/a/b");
        assert_eq!(read(ntsc_os_path_basename(put("/a/b/c.txt"))), "c.txt");
        assert_eq!(read(ntsc_os_path_ext(put("/a/b/c.txt"))), "txt");
        assert_eq!(read(ntsc_os_path_stem(put("/a/b/c.txt"))), "c");
        assert_eq!(
            ntsc_os_is_abs(put("/a/b")),
            std::path::Path::new("/a/b").is_absolute() as i8
        );
        assert_eq!(ntsc_os_is_abs(put("a/b")), 0);
    }

    #[test]
    fn test_temp_ops() {
        assert!(!read(ntsc_os_temp_dir()).is_empty());
        let p1 = read(ntsc_os_temp_path(put("ntsc-test-")));
        let p2 = read(ntsc_os_temp_path(put("ntsc-test-")));
        assert!(!p1.is_empty());
        assert_ne!(p1, p2);
        let f = read(ntsc_os_temp_file(put("ntsc-test-")));
        assert!(!f.is_empty());
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn file_lock_is_exclusive_until_unlock() {
        let path = std::env::temp_dir().join(format!("ntsc-os-lock-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let key = put(path.to_str().unwrap());

        let first = ntsc_os_file_lock(key);
        assert_ne!(first, 0);

        // A second exclusive lock on the same path must throw.
        assert_eq!(ntsc_os_file_lock(key), 0);
        assert_eq!(ntsc_exception_pending(), 1);
        ntsc_exception_clear();
        assert_eq!(ntsc_exception_pending(), 0);

        assert_eq!(ntsc_os_file_unlock(key), 1);
        // The lock is really released: re-acquiring must succeed.
        assert_ne!(ntsc_os_file_lock(key), 0);
        assert_eq!(ntsc_os_file_unlock(key), 1);
        let _ = std::fs::remove_file(&path);
    }
}
