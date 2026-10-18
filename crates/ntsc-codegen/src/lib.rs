//! LLVM IR code generation for NTSC.
//!
//! Generates LLVM IR from the parsed AST and emits object files.

pub mod context;
pub mod emit;

use std::path::{Path, PathBuf};

use inkwell::OptimizationLevel;
use ntsc_diag::Diagnostic;
use ntsc_diag::codes;

/// Errors that can occur during code generation.
#[derive(Debug)]
pub enum CodegenError {
    /// The source failed to parse; errors keep their spans for rendering.
    Parse(Vec<ntsc_parser::ParseError>),

    /// The program failed type checking; errors keep their spans.
    TypeCheck(Vec<ntsc_typeck::TypeError>),
    LLVMError(String),
    LinkError(String),
    IoError(std::io::Error),
}

impl CodegenError {
    /// Convert into one or more ready-to-render diagnostics.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        match self {
            Self::Parse(errors) => errors
                .into_iter()
                .map(|error| Diagnostic::from(&error))
                .collect(),
            Self::TypeCheck(errors) => errors
                .into_iter()
                .map(|error| Diagnostic::from(&error))
                .collect(),
            Self::LLVMError(msg) => {
                vec![Diagnostic::error(msg).with_code(codes::CODEGEN)]
            }
            Self::LinkError(msg) => {
                vec![Diagnostic::error(msg).with_code(codes::CODEGEN)]
            }
            Self::IoError(err) => {
                vec![Diagnostic::error(format!("I/O error: {err}")).with_code(codes::CODEGEN)]
            }
        }
    }
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(errors) => write!(
                f,
                "parse error: {}",
                errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::TypeCheck(errors) => write!(
                f,
                "type check error: {}",
                errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::LLVMError(msg) => write!(f, "codegen error: {msg}"),
            Self::LinkError(msg) => write!(f, "link error: {msg}"),
            Self::IoError(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for CodegenError {}

impl From<std::io::Error> for CodegenError {
    fn from(err: std::io::Error) -> Self {
        Self::IoError(err)
    }
}

impl From<inkwell::builder::BuilderError> for CodegenError {
    fn from(err: inkwell::builder::BuilderError) -> Self {
        Self::LLVMError(format!("builder error: {err}"))
    }
}

/// Line prefix the generated test harness prints with its totals.
///
/// Internal protocol between the harness (`emit_test_harness`) and `ntsc
/// test`, which consumes the line and renders the totals. Deliberately
/// unlikely to collide with user output.
pub const SUMMARY_MARKER: &str = "__NTSC_SUMMARY__";

/// Generate an object file from the given source text, returning its path.
pub fn compile_source(
    source: &str,
    target_triple: &str,
    output_name: &str,
    out_dir: &Path,
) -> Result<PathBuf, CodegenError> {
    compile_source_at(
        source,
        target_triple,
        OptimizationLevel::None,
        output_name,
        out_dir,
    )
}

/// Generate an object file with the aggressive optimization pipeline
/// (the `--release` path). Resolution of `getelementptr` offsets against
/// the module data layout only happens under this pipeline.
pub fn compile_source_release(
    source: &str,
    target_triple: &str,
    output_name: &str,
    out_dir: &Path,
) -> Result<PathBuf, CodegenError> {
    compile_source_at(
        source,
        target_triple,
        OptimizationLevel::Aggressive,
        output_name,
        out_dir,
    )
}

fn compile_source_at(
    source: &str,
    target_triple: &str,
    opt_level: OptimizationLevel,
    output_name: &str,
    out_dir: &Path,
) -> Result<PathBuf, CodegenError> {
    let program = parse_and_check(source)?;
    compile_program(
        &program,
        target_triple,
        opt_level,
        output_name,
        out_dir,
        false,
    )
}

/// Compile the program's `test` blocks into an object file.
///
/// Test blocks become `test_<name>` functions; a generated harness `main`
/// runs each and prints `PASS`/`FAIL` lines, exiting non-zero on failure.
/// The user's own `main` is not invoked in test mode.
pub fn compile_tests(
    source: &str,
    target_triple: &str,
    output_name: &str,
    out_dir: &Path,
) -> Result<PathBuf, CodegenError> {
    let program = parse_and_check(source)?;
    compile_program(
        &program,
        target_triple,
        OptimizationLevel::None,
        output_name,
        out_dir,
        true,
    )
}

pub fn object_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "obj"
    } else {
        "o"
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsLinker {
    /// The `ld.lld` + `mingw/` import-library bundle shipped with the MSI,
    /// so linking works without MSVC or gcc installed.
    BundledLld,

    /// MSVC `link.exe` on PATH (developer machines).
    Msvc,

    /// MinGW `gcc` on PATH.
    Gcc,
}

/// Static library name `ntsc-runtime` produces for the host toolchain.
///
/// The name must match the consuming linker's archive format: MSVC
/// `link.exe` expects `ntsc_runtime.lib`; the bundled `ld.lld` and MinGW
/// `gcc` expect the GNU-flavoured `libntsc_runtime.a`. Every other host
/// uses `libntsc_runtime.a`.
pub fn runtime_lib_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        match windows_linker() {
            Some(WindowsLinker::Msvc) => "ntsc_runtime.lib",
            _ => "libntsc_runtime.a",
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        "libntsc_runtime.a"
    }
}

/// LLVM target triple for the host platform.
///
/// Codegen tests and `ntsc init` need a triple that matches the linker that
/// will consume the emitted object; hardcoding a Linux triple on a macOS or
/// Windows host would emit ELF/COFF objects no local linker can consume.
pub fn host_triple() -> &'static str {
    ntsc_build::host_triple()
}

pub fn llvm_version() -> String {
    let (major, minor, patch) = inkwell::support::get_llvm_version();
    format!("{major}.{minor}.{patch}")
}

pub fn with_executable_extension(name: &str) -> String {
    if cfg!(target_os = "windows") && !name.to_ascii_lowercase().ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Link an emitted object file with the runtime library into an executable.
///
/// Unix hosts use the system C compiler. Windows prefers MSVC `link.exe`
/// (the default toolchain) with MinGW `gcc` as a POSIX-style fallback.
pub fn link_binary(
    obj_path: &Path,
    runtime_lib: &Path,
    output_path: &Path,
) -> Result<(), CodegenError> {
    link_binary_with_extras(obj_path, runtime_lib, &[], &[], output_path)
}

/// Link an emitted object file with the runtime library plus additional
/// static archives (e.g. compiled Rust-crate adaptors from `ntsc-cargo`)
/// into an executable.
pub fn link_binary_with_extras(
    obj_path: &Path,
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    native_link_args: &[String],
    output_path: &Path,
) -> Result<(), CodegenError> {
    #[cfg(target_os = "windows")]
    {
        link_windows(
            obj_path,
            runtime_lib,
            extra_archives,
            native_link_args,
            output_path,
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        link_unix(
            obj_path,
            runtime_lib,
            extra_archives,
            native_link_args,
            output_path,
        )
    }
}

#[cfg(not(target_os = "windows"))]
fn link_unix(
    obj_path: &Path,
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    native_link_args: &[String],
    output_path: &Path,
) -> Result<(), CodegenError> {
    // Pick the fastest available linker. The runtime archive statically
    // bundles the whole stdlib (net/TLS, regex, archive, ...), so linking
    // against it is the dominant cost of building a program; a fast linker
    // (mold/lld/gold) cuts that down substantially where one is installed.
    let mut command = match select_unix_linker() {
        UnixLinker::FuseLd { driver, flavor } => {
            let mut command = std::process::Command::new(driver);
            command.arg(format!("-fuse-ld={flavor}"));
            command
        }
        UnixLinker::System => std::process::Command::new("cc"),
    };
    command
        .arg("-o")
        .arg(output_path)
        .arg(obj_path)
        .arg(runtime_lib);
    for archive in extra_archives {
        command.arg(archive);
    }
    for arg in native_link_args {
        command.arg(arg);
    }
    command
        .arg("-lm")
        // Strip the produced binary: the runtime archive carries the full
        // stdlib, so an unstripped executable balloons into tens of megabytes
        // of symbols and debug info. Local symbols and .eh_frame are kept as
        // needed for unwinding; only the debug/global symbol tables go away.
        .arg("-s");

    // macOS links pthread and dl into libSystem, so the flags are
    // unnecessary there; on other Unix hosts they name real libraries.
    if !cfg!(target_os = "macos") {
        command.arg("-lpthread").arg("-ldl");
    }
    run_linker(&mut command)
}

/// A linker configuration for non-Windows hosts.
#[cfg(not(target_os = "windows"))]
enum UnixLinker {
    /// A fast ELF linker selected via the C driver's `-fuse-ld=<flavor>`.
    FuseLd {
        driver: &'static str,
        flavor: &'static str,
    },
    /// The system default linker (GNU ld on Linux, ld64 on macOS).
    System,
}

#[cfg(not(target_os = "windows"))]
fn select_unix_linker() -> UnixLinker {
    // On macOS the system linker is ld64, which `-fuse-ld=gold` etc. do not
    // target; keep the prior Clang-vs-ld.lld behaviour there.
    if !cfg!(target_os = "macos") {
        // Prefer the fastest ELF linkers first: mold, then lld, then gold.
        if command_available("mold") {
            return UnixLinker::FuseLd {
                driver: "cc",
                flavor: "mold",
            };
        }
        if command_available("ld.lld") {
            return UnixLinker::FuseLd {
                driver: "cc",
                flavor: "lld",
            };
        }
        if command_available("ld.gold") {
            return UnixLinker::FuseLd {
                driver: "cc",
                flavor: "gold",
            };
        }
    }
    if command_available("clang") && command_available("ld.lld") {
        return UnixLinker::FuseLd {
            driver: "clang",
            flavor: "lld",
        };
    }
    UnixLinker::System
}

#[cfg(not(target_os = "windows"))]
fn command_available(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

#[cfg(target_os = "windows")]
fn link_windows(
    obj_path: &Path,
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    native_link_args: &[String],
    output_path: &Path,
) -> Result<(), CodegenError> {
    match windows_linker() {
        Some(WindowsLinker::BundledLld) => link_with_lld(
            obj_path,
            runtime_lib,
            extra_archives,
            native_link_args,
            output_path,
        ),
        Some(WindowsLinker::Msvc) => {
            let mut command = std::process::Command::new("link.exe");
            command
                .arg("/NOLOGO")
                .arg("/SUBSYSTEM:CONSOLE")
                .arg("/ENTRY:mainCRTStartup")
                .arg(format!("/OUT:{}", output_path.display()))
                .arg(obj_path)
                .arg(runtime_lib);
            for archive in extra_archives {
                command.arg(archive);
            }
            for arg in native_link_args {
                command.arg(arg);
            }
            const LIBS: &[&str] = &[
                "advapi32",
                "shell32",
                "user32",
                "kernel32",
                "imagehlp",
                "comctl32",
                "comdlg32",
                "winspool",
                "winmm",
                "ole32",
                "oleaut32",
                "uuid",
                "rpcrt4",
                "ws2_32",
                "bcrypt",
                "ntdll",
                "userenv",
                "crypt32",
                "shlwapi",
                "version",
                // A Rust staticlib does not carry its CRT default-lib directives
                // into the archive — rustc emits `/defaultlib:msvcrt` only when
                // it drives the final link itself — so linking the archive with
                // link.exe must name the /MD CRT set explicitly. Without it a
                // release-built runtime leaves CRT symbols (memcpy, ceil,
                // __CxxFrameHandler3, _tls_index, ...) unresolved.
                "ucrt",
                "vcruntime",
                "msvcrt",
            ];
            for lib in LIBS {
                command.arg(format!("/DEFAULTLIB:{lib}"));
            }
            run_linker(&mut command)
        }
        Some(WindowsLinker::Gcc) => {
            let mut command = std::process::Command::new("gcc");
            command
                .arg("-o")
                .arg(output_path)
                .arg(obj_path)
                .arg(runtime_lib);
            for archive in extra_archives {
                command.arg(archive);
            }
            for arg in native_link_args {
                command.arg(arg);
            }
            command.arg("-lpthread");
            run_linker(&mut command)
        }
        None => Err(CodegenError::LinkError(
            "no Windows linker found: install the MSVC toolchain (link.exe), \
             MinGW (gcc), or use the bundled installer which ships ld.lld"
                .to_string(),
        )),
    }
}

#[cfg(target_os = "windows")]
fn link_with_lld(
    obj_path: &Path,
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    native_link_args: &[String],
    output_path: &Path,
) -> Result<(), CodegenError> {
    // `ld.lld`'s MinGW emulation does not add the CRT startup object or
    // default libraries on its own (unlike `gcc`), so they are passed
    // explicitly, mirroring what clang's MinGW driver emits. Import
    // libraries are looked up in the bundle first, then in the ntsc
    // executable's Rust toolchain sysroot (so a MinGW compiler or rustup
    // toolchain still works), then filtered to files that actually exist.
    let bin_dir = bundled_lld().ok_or_else(|| {
        CodegenError::LinkError("bundled ld.lld was not found next to the executable".to_string())
    })?;

    let mut command = std::process::Command::new(bin_dir.join("ld.lld.exe"));
    command
        .arg("-m")
        .arg("i386pep")
        .arg("-Bstatic")
        .arg("-o")
        .arg(output_path)
        .arg(bin_dir.join("mingw").join("crt2.o"))
        .arg(obj_path)
        .arg(runtime_lib);
    for archive in extra_archives {
        command.arg(archive);
    }
    for arg in native_link_args {
        command.arg(arg);
    }
    command
        .arg("-L")
        .arg(bin_dir.join("mingw"))
        .arg("--start-group");

    let sysroot_mingw = sysroot_mingw_lib_dir();
    if sysroot_mingw.is_dir() {
        command.arg("-L").arg(&sysroot_mingw);
    }

    const LIBS: &[&str] = &[
        "mingw32", "gcc", "moldname", "mingwex", "msvcrt", "advapi32", "shell32", "user32",
        "kernel32", "imagehlp", "comctl32", "comdlg32", "winspool", "winmm", "ole32", "oleaut32",
        "uuid", "rpcrt4", "ws2_32", "bcrypt", "ntdll", "userenv", "crypt32", "shlwapi", "version",
    ];
    for lib in LIBS {
        if bin_dir.join("mingw").join(format!("lib{lib}.a")).is_file() {
            command.arg(format!("-l{lib}"));
        }
    }
    command.arg("--end-group");

    run_linker(&mut command)
}

#[cfg(target_os = "windows")]
fn windows_linker() -> Option<WindowsLinker> {
    if bundled_lld().is_some() {
        return Some(WindowsLinker::BundledLld);
    }
    if which("link.exe") {
        return Some(WindowsLinker::Msvc);
    }
    if which("gcc") {
        return Some(WindowsLinker::Gcc);
    }
    None
}

#[cfg(target_os = "windows")]
fn bundled_lld() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let bundled = dir.join("ld.lld.exe").is_file() && dir.join("mingw").is_dir();
    bundled.then(|| dir.to_path_buf())
}

#[cfg(target_os = "windows")]
fn sysroot_mingw_lib_dir() -> PathBuf {
    let sysroot = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default();
    PathBuf::from(sysroot.trim())
        .join("lib")
        .join("rustlib")
        .join("x86_64-pc-windows-gnu")
        .join("lib")
}

#[cfg(target_os = "windows")]
fn which(program: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut found = false;
    for dir in std::env::split_paths(&path) {
        // Git Bash and other MSYS environments put a GNU coreutils
        // `link.exe` (hardlink creator) in `<root>\usr\bin`, ahead of the
        // MSVC tools; it rejects `/ENTRY:`-style flags, so skip those dirs.
        if is_msys_bin_dir(&dir) {
            continue;
        }
        let candidate = dir.join(program);
        if candidate.is_file() {
            found = true;
            break;
        }
    }
    found
}

#[cfg(target_os = "windows")]
fn is_msys_bin_dir(dir: &std::path::Path) -> bool {
    use std::path::Component;

    match dir.components().rev().take(2).collect::<Vec<_>>()[..] {
        [Component::Normal(a), Component::Normal(b)] => {
            a.eq_ignore_ascii_case("usr") && b.eq_ignore_ascii_case("bin")
        }
        _ => false,
    }
}

fn run_linker(command: &mut std::process::Command) -> Result<(), CodegenError> {
    let output = command.output().map_err(CodegenError::IoError)?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CodegenError::LinkError(format!(
            "linker failed with exit code: {0:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        )));
    }
    Ok(())
}

/// Runs type checking, LLVM init, and IR emission for a program.
///
/// Leak reporting and optimization are derived from `opt_level`: leak
/// reports only run at `OptimizationLevel::None`, which the optimizer would
/// eliminate as dead metadata.
pub fn compile_program(
    program: &ntsc_ast::stmt::Program,
    target_triple: &str,
    opt_level: OptimizationLevel,
    output_name: &str,
    out_dir: &Path,
    test_mode: bool,
) -> Result<PathBuf, CodegenError> {
    let prepared = ntsc_typeck::prepare_program(program).map_err(CodegenError::TypeCheck)?;
    ntsc_typeck::check_program(&prepared).map_err(CodegenError::TypeCheck)?;

    context::init_llvm();
    let target_machine = context::create_target_machine(target_triple, opt_level)?;

    let report_leaks = opt_level == OptimizationLevel::None;
    let optimize = opt_level != OptimizationLevel::None;

    let llvm_context = inkwell::context::Context::create();
    let obj_path = out_dir.join(format!("{output_name}.{}", object_extension()));
    emit::emit_module(
        &llvm_context,
        &target_machine,
        &prepared,
        &obj_path,
        test_mode,
        report_leaks,
        optimize,
    )?;

    Ok(obj_path)
}

fn parse_and_check(source: &str) -> Result<ntsc_ast::stmt::Program, CodegenError> {
    let tokens = ntsc_lexer::tokenize(source);
    ntsc_parser::parse(&tokens).map_err(CodegenError::Parse)
}
