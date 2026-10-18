//! An exception that no `catch` handles must exit with a failure code and the
//! thrown message on stderr — not crash the process with SIGABRT. The `abort`
//! behavior turned every unhandled user error into a core dump.
use std::path::Path;

/// Build the runtime static library if missing and return its path.
fn runtime_lib(rewrite_dir: &Path) -> std::path::PathBuf {
    let lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );
    if !lib.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "ntsc-runtime"])
            .current_dir(rewrite_dir)
            .status()
            .expect("failed to run cargo");
        assert!(status.success(), "failed to build ntsc-runtime");
    }
    assert!(lib.exists(), "runtime lib not found at {lib:?}");
    lib
}

/// Compile + link + run `source`, returning (exit code, stdout, stderr).
fn compile_run(
    rewrite_dir: &Path,
    name: &str,
    source: &str,
    opt_level: inkwell::OptimizationLevel,
) -> (i32, String, String) {
    let dir = std::env::temp_dir().join(format!("ntsc_uncaught_e2e_{name}"));
    std::fs::create_dir_all(&dir).unwrap();

    let program = {
        let tokens = ntsc_lexer::tokenize(source);
        ntsc_parser::parse(&tokens).expect("parse failed")
    };
    ntsc_codegen::compile_program(
        &program,
        ntsc_codegen::host_triple(),
        opt_level,
        name,
        &dir,
        false,
    )
    .expect("compile failed");

    let bin_path = dir.join(name);
    ntsc_codegen::link_binary(
        &dir.join(format!("{name}.{}", ntsc_codegen::object_extension())),
        &runtime_lib(rewrite_dir),
        &bin_path,
    )
    .expect("link failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    let _ = std::fs::remove_dir_all(&dir);
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn uncaught_exception_exits_with_failure_code() {
    let rewrite_dir = workspace_root();
    let source = "fun main() {\n    throw \"boom\"\n}\n";

    let (code, stdout, stderr) = compile_run(
        &rewrite_dir,
        "uncaught_debug",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert_eq!(
        code, 1,
        "uncaught exception must exit 1 (stdout was: {stdout:?})"
    );
    assert!(
        stderr.contains("uncaught exception: boom"),
        "stderr must name the thrown value, was: {stderr:?}"
    );
}

/// An exception the user *does* catch must keep the process healthy: exit 0
/// with `catch` doing the recovery work.
#[test]
fn caught_exception_exits_zero() {
    let rewrite_dir = workspace_root();
    let source = "fun main() {\n    try {\n        throw \"handled\"\n    } catch (err) {\n        say(err)\n    }\n}\n";

    let (code, stdout, stderr) = compile_run(
        &rewrite_dir,
        "caught_ok",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert_eq!(code, 0, "caught exception must exit 0 (stderr: {stderr:?})");
    assert_eq!(stdout, "handled\n");
}
