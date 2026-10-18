//! NTSC standard library: `process` module.
//! Process spawning plus the threaded workers used by `collections.channel`.

use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::registry;

fn make_response_json(status: i64, stdout: &str, stderr: &str) -> String {
    format!(
        "{{\"status\":{},\"stdout\":\"{}\",\"stderr\":\"{}\"}}",
        status,
        stdout.replace('"', "\\\"").replace('\n', "\\n"),
        stderr.replace('"', "\\\"").replace('\n', "\\n")
    )
}

/// Run a command through `/bin/sh -c` and return its exit code.
fn exec_blocking(command: &str) -> Result<i64, String> {
    let mut process = Command::new("sh");
    super::os::apply_environment(&mut process);
    match process.arg("-c").arg(command).status() {
        Ok(status) => Ok(status.code().unwrap_or(-1) as i64),
        Err(e) => Err(format!("process.exec: cannot start command: {e}")),
    }
}

/// Run a command through `/bin/sh -c` and capture its output as an owned
/// JSON string (`status`, `stdout`, `stderr`).
fn exec_output_json_blocking(command: &str) -> Result<String, String> {
    let mut process = Command::new("sh");
    super::os::apply_environment(&mut process);
    match process.arg("-c").arg(command).output() {
        Ok(output) => {
            let status = output.status.code().unwrap_or(-1) as i64;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok(make_response_json(status, &stdout, &stderr))
        }
        Err(e) => Err(format!("process.exec_output: cannot start command: {e}")),
    }
}

/// Run a command through `/bin/sh -c` and register its output for the
/// synchronous ABI.
fn exec_output_blocking(command: &str) -> Result<i64, String> {
    exec_output_json_blocking(command).map(registry::put_string)
}

/// `process.exec(command)` — runs through `/bin/sh -c`; returns the exit
/// code.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_process_exec(command: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let cmd = registry::get_string(command).unwrap_or_default();
    match exec_blocking(&cmd) {
        Ok(code) => code,
        Err(msg) => super::throw_str(msg),
    }
}

/// `process.exec_output(command)` — JSON with `status`, `stdout`, `stderr`.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_process_exec_output(command: i64) -> i64 {
    let _block = crate::BlockGuard::enter();
    let cmd = registry::get_string(command).unwrap_or_default();
    match exec_output_blocking(&cmd) {
        Ok(handle) => handle,
        Err(msg) => super::throw_str(msg),
    }
}

/// `process.spawn(program, args)` — `args` is split on spaces; there is no
/// quoting.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_process_spawn(program: i64, args: i64) -> i64 {
    let prog = registry::get_string(program).unwrap_or_default();
    let args_str = registry::get_string(args).unwrap_or_default();
    let arg_vec: Vec<&str> = if args_str.is_empty() {
        vec![]
    } else {
        args_str.split(' ').collect()
    };
    let mut process = Command::new(&prog);
    super::os::apply_environment(&mut process);
    let result = process.args(&arg_vec).output().map(|output| {
        let status = output.status.code().unwrap_or(-1) as i64;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        registry::put_string(make_response_json(status, &stdout, &stderr))
    });
    match result {
        Ok(handle) => handle,
        Err(e) => super::throw_str(format!("process.spawn: cannot start '{prog}': {e}")),
    }
}

// ── Offloaded (async) process calls ─────────────────────────────────────
//
// These register a reactive offload future that runs the blocking command on
// the worker pool instead of the scheduler thread. The job itself is pure and
// returns an owned process outcome so an error on a pool thread is delivered
// as a throw once the goroutine reaps it.

/// The result of a process offload stays in Rust-owned values until the
/// caller reaps it. In particular, an exit code is not a registry handle and
/// must never be passed to registry cleanup when a future is cancelled.
#[derive(Debug)]
pub(crate) enum OutcomeValue {
    ExitCode(i64),
    Output(String),
}

type BlockingOutcome = Result<OutcomeValue, String>;

fn put_outcome(work: Box<dyn FnOnce() -> BlockingOutcome + Send + 'static>) -> i64 {
    registry::async_op_new(Box::new(move || registry::put_opaque(work())))
}

/// Delivers the outcome of a completed offloaded future: throws if the job
/// errored, otherwise converts the owned result to its public ABI value.
fn deliver_outcome(id: i64) -> i64 {
    let outcome = registry::async_op_result(id);
    let value: Result<OutcomeValue, String> =
        registry::take_opaque::<Result<OutcomeValue, String>>(outcome)
            .unwrap_or(Err(String::new()));
    match value {
        Ok(OutcomeValue::ExitCode(code)) => code,
        Ok(OutcomeValue::Output(json)) => registry::put_string(json),
        Err(msg) => super::throw_str(msg),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_process_exec(command: i64) -> i64 {
    let cmd = registry::get_string(command).unwrap_or_default();
    put_outcome(Box::new(move || {
        exec_blocking(&cmd).map(OutcomeValue::ExitCode)
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_process_exec_output(command: i64) -> i64 {
    let cmd = registry::get_string(command).unwrap_or_default();
    put_outcome(Box::new(move || {
        exec_output_json_blocking(&cmd).map(OutcomeValue::Output)
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_process_exec_poll(id: i64) -> i8 {
    registry::async_op_poll(id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_process_exec_result(id: i64) -> i64 {
    deliver_outcome(id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_process_exec_output_poll(id: i64) -> i8 {
    registry::async_op_poll(id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_process_exec_output_result(id: i64) -> i64 {
    deliver_outcome(id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_process_exec_drop(id: i64) {
    registry::async_op_drop(id);
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_async_process_exec_output_drop(id: i64) {
    registry::async_op_drop(id);
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_process_pid() -> i64 {
    std::process::id() as i64
}

// ── Threads ─────────────────────────────────────────────────────────────

static NEXT_THREAD_ID: AtomicI64 = AtomicI64::new(1);

static THREAD_HANDLES: LazyLock<Mutex<HashMap<i64, std::thread::JoinHandle<()>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `process.spawn_thread(f, arg)` — `f` is generated `extern "C" fn(i64)`;
/// returns an opaque id for `thread_join`. An uncaught exception inside the
/// thread aborts the process.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_process_spawn_thread(f: extern "C" fn(i64), arg: i64) -> i64 {
    let id = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
    match std::thread::Builder::new().spawn(move || f(arg)) {
        Ok(handle) => {
            if let Ok(mut threads) = THREAD_HANDLES.lock() {
                threads.insert(id, handle);
            }
            id
        }
        Err(e) => super::throw_str(format!("process.spawn_thread: cannot start thread: {e}")),
    }
}

/// `process.thread_join(id)` — 1 when the thread was joined, 0 when `id`
/// does not name a live spawned thread.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_process_thread_join(id: i64) -> i8 {
    let _block = crate::BlockGuard::enter();
    let handle = match THREAD_HANDLES.lock() {
        Ok(mut threads) => threads.remove(&id),
        Err(poisoned) => poisoned.into_inner().remove(&id),
    };
    match handle {
        Some(handle) => match handle.join() {
            Ok(()) => 1,
            Err(_) => 1, // the thread finished even though it panicked
        },
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(s: &str) -> i64 {
        registry::put_string(s.to_string())
    }

    fn read(id: i64) -> String {
        let value = registry::get_string(id).unwrap_or_default();
        let _ = registry::take_string(id);
        value
    }

    #[test]
    fn test_exec() {
        let r = ntsc_process_exec(put("echo hello"));
        assert_eq!(r, 0);
    }

    #[test]
    fn test_exec_output_observes_environment_overlay() {
        let name = put("NTSC_PROCESS_ENV_OVERLAY_TEST");
        let value = put("child-value");
        assert_eq!(crate::modules::os::ntsc_os_setenv(name, value), 1);

        let result =
            ntsc_process_exec_output(put("printf '%s' \"$NTSC_PROCESS_ENV_OVERLAY_TEST\""));
        assert!(
            read(result).contains("\"stdout\":\"child-value\""),
            "child process did not receive the environment overlay"
        );

        assert_eq!(
            crate::modules::os::ntsc_os_unsetenv(put("NTSC_PROCESS_ENV_OVERLAY_TEST")),
            1
        );
        let result =
            ntsc_process_exec_output(put("printf '%s' \"$NTSC_PROCESS_ENV_OVERLAY_TEST\""));
        assert!(
            read(result).contains("\"stdout\":\"\""),
            "child process did not receive the environment removal"
        );
    }

    #[test]
    fn test_pid() {
        assert!(ntsc_process_pid() > 0);
    }

    #[test]
    fn test_spawn_missing_program_throws() {
        use crate::modules::test_util::catch_throw;
        let err = catch_throw(|| {
            let _ = ntsc_process_spawn(put("/nonexistent_program_xyz_123"), put(""));
        });
        let msg = err.unwrap();
        assert!(msg.contains("process.spawn"), "unexpected message: {msg}");
    }

    #[test]
    fn test_spawn_thread_and_join() {
        use std::sync::atomic::{AtomicI64, Ordering};
        extern "C" fn worker(arg: i64) {
            registry::with_opaque_mut::<_, AtomicI64>(arg, |counter| {
                counter.fetch_add(10, Ordering::SeqCst);
            });
        }
        let counter = registry::put_opaque(AtomicI64::new(0));
        let id = ntsc_process_spawn_thread(worker, counter);
        assert!(id > 0);
        assert_eq!(ntsc_process_thread_join(id), 1);
        assert_eq!(
            registry::with_opaque::<_, AtomicI64>(counter, |c| c.load(Ordering::SeqCst)),
            Some(10)
        );
        let _ = registry::take_opaque::<AtomicI64>(counter);
    }

    #[test]
    fn test_join_unknown_thread_returns_zero() {
        assert_eq!(ntsc_process_thread_join(999999), 0);
    }

    #[test]
    fn test_async_exec_offloads_to_pool() {
        let fut = ntsc_async_process_exec(put("echo offloaded-value >/dev/null"));
        wait_offloaded(|| ntsc_async_process_exec_poll(fut), "exec");
        let code = ntsc_async_process_exec_result(fut);
        ntsc_async_process_exec_drop(fut);
        assert_eq!(code, 0);
    }

    #[test]
    fn test_async_exec_output_offloads_to_pool() {
        let fut = ntsc_async_process_exec_output(put("printf async-offload-stdout"));
        wait_offloaded(|| ntsc_async_process_exec_output_poll(fut), "exec_output");
        let json = ntsc_async_process_exec_output_result(fut);
        ntsc_async_process_exec_output_drop(fut);
        assert!(
            read(json).contains("\"stdout\":\"async-offload-stdout\""),
            "unexpected exec_output result"
        );
    }

    #[test]
    fn test_async_exec_output_drop_reclaims_unreaped_payload() {
        let command = put("printf cancelled-process-output");
        let fut = ntsc_async_process_exec_output(command);
        wait_offloaded(|| ntsc_async_process_exec_output_poll(fut), "exec_output");
        let payload = registry::borrow(fut, |handle| match handle {
            registry::Handle::AsyncOp { result, .. } => *result,
            _ => registry::NULL,
        })
        .unwrap_or(registry::NULL);
        assert_ne!(payload, registry::NULL);

        ntsc_async_process_exec_output_drop(fut);
        assert!(registry::borrow(payload, |_| ()).is_none());
        let _ = registry::take_string(command);
    }

    /// Spin an offloaded future to completion with a generous wall-clock cap.
    /// Child-process work can take seconds on a slow or antivirus-loaded CI
    /// host, so a fixed iteration budget would fail before the pool finishes.
    fn wait_offloaded(poll: impl Fn() -> i8, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while poll() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "offloaded {what} never completed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
