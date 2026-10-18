//! NTSC standard library: `sys` module.
//! File/system utilities. A null handle reads as an empty string; "unset"
//! values are reported as the null handle.

use std::fs;
use std::path::Path;

use crate::registry;

fn fail(fn_name: &str, msg: impl std::fmt::Display) -> i64 {
    super::throw_str(format!("sys.{fn_name}: {msg}"))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_read(path: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let path = registry::get_string(path).unwrap_or_default();
    match fs::read_to_string(&path) {
        Ok(content) => registry::put_string(content),
        Err(e) => fail("read", format!("cannot read file '{path}': {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_write(path: i64, content: i64) -> i8 {
    let _block = crate::BlockGuard::enter();
    let path = registry::get_string(path).unwrap_or_default();
    let content = registry::get_string(content).unwrap_or_default();
    match fs::write(&path, content) {
        Ok(_) => 1,
        Err(e) => {
            let _ = fail("write", format!("cannot write file '{path}': {e}"));
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_append(path: i64, content: i64) -> i8 {
    let _block = crate::BlockGuard::enter();
    let path = registry::get_string(path).unwrap_or_default();
    let content = registry::get_string(content).unwrap_or_default();
    use std::io::Write;
    match fs::OpenOptions::new().append(true).create(true).open(&path) {
        Ok(mut file) => match write!(file, "{content}") {
            Ok(_) => 1,
            Err(e) => {
                let _ = fail("append", format!("cannot append to file '{path}': {e}"));
                0
            }
        },
        Err(e) => {
            let _ = fail(
                "append",
                format!("cannot open file '{path}' for append: {e}"),
            );
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_exists(path: i64) -> i8 {
    let path = registry::get_string(path).unwrap_or_default();
    if Path::new(&path).exists() { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_mkdir(path: i64) -> i8 {
    let path = registry::get_string(path).unwrap_or_default();
    match fs::create_dir_all(&path) {
        Ok(_) => 1,
        Err(e) => {
            let _ = fail("mkdir", format!("cannot create directory '{path}': {e}"));
            0
        }
    }
}

/// `sys.listdir(path)` — entry names joined by newlines.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_listdir(path: i64) -> i64 {
    let path = registry::get_string(path).unwrap_or_default();
    match fs::read_dir(&path) {
        Ok(entries) => {
            let names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect();
            registry::put_string(names.join("\n"))
        }
        Err(e) => fail("listdir", format!("cannot list directory '{path}': {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_cwd() -> i64 {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    registry::put_string(cwd)
}

/// `sys.env(var_name)` — the variable's value, or the null handle when
/// unset.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_env(var_name: i64) -> i64 {
    let name = registry::get_string(var_name).unwrap_or_default();
    match super::os::environment_var(&name) {
        Some(value) => registry::put_string(value),
        None => registry::NULL,
    }
}

/// `sys.args()` — command-line arguments as an array of strings.
/// Element 0 is the program name, mirroring Go's `os.Args`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_args() -> i64 {
    let args = registry::array_new(registry::PTR_SIZE as i64, 0, 1);
    for arg in std::env::args() {
        let temp = registry::put_string(arg);
        registry::array_push(args, temp);
        crate::ntsc_string_drop(temp);
    }
    args
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_exit(code: i64) {
    std::process::exit(code as i32);
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_sleep(ms: f64) {
    let _block = crate::BlockGuard::enter();
    let duration = std::time::Duration::from_millis(ms.max(0.0) as u64);
    std::thread::sleep(duration);
}

/// `sys.exec(command)` — runs through `/bin/sh -c`; returns the exit code.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_exec(command: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let cmd = registry::get_string(command).unwrap_or_default();
    let mut process = std::process::Command::new("sh");
    super::os::apply_environment(&mut process);
    match process.arg("-c").arg(&cmd).status() {
        Ok(status) => status.code().unwrap_or(-1) as i64,
        Err(e) => fail("exec", format!("cannot run command '{cmd}': {e}")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_cp(src: i64, dst: i64) -> i8 {
    let _block = crate::BlockGuard::enter();
    let src = registry::get_string(src).unwrap_or_default();
    let dst = registry::get_string(dst).unwrap_or_default();
    match fs::copy(&src, &dst) {
        Ok(_) => 1,
        Err(e) => {
            let _ = fail("cp", format!("cannot copy '{src}' to '{dst}': {e}"));
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_rm(path: i64) -> i8 {
    let path = registry::get_string(path).unwrap_or_default();
    match fs::remove_file(&path) {
        Ok(_) => 1,
        Err(e) => {
            let _ = fail("rm", format!("cannot remove file '{path}': {e}"));
            0
        }
    }
}

/// `sys.walk(path)` — recursively yields every file and directory under
/// `path`. Returns newline-separated relative paths, with directories
/// ending with `/`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_walk(path: i64) -> i64 {
    let path = registry::get_string(path).unwrap_or_default();
    let root = Path::new(&path);
    if !root.is_dir() {
        return fail("walk", format!("'{path}' is not a directory"));
    }
    let mut results = Vec::new();
    walk_recursive(root, root, &mut results);
    registry::put_string(results.join("\n"))
}

fn walk_recursive(base: &Path, dir: &Path, results: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    sorted.sort_by_key(|e| e.file_name());
    for entry in sorted {
        let path = entry.path();
        let relative = path.strip_prefix(base).unwrap_or(&path);
        let rel_str = relative.to_string_lossy().to_string();
        if path.is_dir() {
            results.push(format!("{}/", rel_str));
            walk_recursive(base, &path, results);
        } else {
            results.push(rel_str);
        }
    }
}

/// `sys.symlink(target, link)` — create a symbolic link at `link` pointing
/// to `target`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_symlink(_target: i64, _link: i64) -> i8 {
    #[cfg(unix)]
    {
        let target = registry::get_string(_target).unwrap_or_default();
        let link = registry::get_string(_link).unwrap_or_default();
        match std::os::unix::fs::symlink(&target, &link) {
            Ok(_) => 1,
            Err(e) => {
                let _ = fail(
                    "symlink",
                    format!("cannot create symlink '{link}' -> '{target}': {e}"),
                );
                0
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = fail(
            "symlink",
            "symlink creation is not supported on this platform",
        );
        0
    }
}

/// `sys.readlink(path)` — read the target of a symbolic link.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_readlink(path: i64) -> i64 {
    let path = registry::get_string(path).unwrap_or_default();
    match fs::read_link(&path) {
        Ok(target) => registry::put_string(target.to_string_lossy().to_string()),
        Err(e) => fail("readlink", format!("cannot read symlink '{path}': {e}")),
    }
}

/// `sys.is_symlink(path)` — returns 1 if the path is a symbolic link.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sys_is_symlink(path: i64) -> i8 {
    let path = registry::get_string(path).unwrap_or_default();
    if std::path::Path::new(&path).is_symlink() {
        1
    } else {
        0
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
    fn test_exists() {
        assert_eq!(ntsc_sys_exists(put("/")), 1);

        assert_eq!(ntsc_sys_exists(put("/nonexistent_path_xyz123")), 0);
    }

    #[test]
    fn test_cwd() {
        let cwd = ntsc_sys_cwd();
        assert_ne!(cwd, registry::NULL);
        let cwd = read(cwd);
        assert!(!cwd.is_empty());
    }

    #[test]
    fn test_env() {
        let val = ntsc_sys_env(put("PATH"));
        assert_ne!(val, registry::NULL);
        let val = read(val);
        assert!(!val.is_empty());
    }

    #[test]
    fn test_env_observes_os_overlay() {
        let name = put("NTSC_SYS_ENV_OVERLAY_TEST");
        let value = put("overlay-value");
        assert_eq!(crate::modules::os::ntsc_os_setenv(name, value), 1);

        let result = ntsc_sys_env(put("NTSC_SYS_ENV_OVERLAY_TEST"));
        assert_eq!(read(result), "overlay-value");

        assert_eq!(
            crate::modules::os::ntsc_os_unsetenv(put("NTSC_SYS_ENV_OVERLAY_TEST")),
            1
        );
        assert_eq!(
            ntsc_sys_env(put("NTSC_SYS_ENV_OVERLAY_TEST")),
            registry::NULL
        );
    }

    #[test]
    fn test_args_returns_argv_array() {
        let args = ntsc_sys_args();
        let len = registry::array_len(args);
        assert!(len >= 1, "argv always contains at least the program name");
        let prog = registry::array_get(args, 0).expect("argv[0] exists");
        let name = registry::get_string(prog).unwrap_or_default();
        assert!(!name.is_empty());
        registry::array_drop(args);
    }

    #[test]
    fn test_read_missing_file_throws() {
        use crate::modules::test_util::catch_throw;
        let err = catch_throw(|| {
            let _ = ntsc_sys_read(put("/nonexistent_file_xyz_123"));
        });
        let msg = err.unwrap();
        assert!(msg.contains("sys.read"), "unexpected message: {msg}");
    }

    #[test]
    fn test_write_to_missing_dir_throws() {
        use crate::modules::test_util::catch_throw;
        let err = catch_throw(|| {
            let _ = ntsc_sys_write(put("/nonexistent_dir_xyz_123/file"), put("x"));
        });
        let msg = err.unwrap();
        assert!(msg.contains("sys.write"), "unexpected message: {msg}");
    }
}
