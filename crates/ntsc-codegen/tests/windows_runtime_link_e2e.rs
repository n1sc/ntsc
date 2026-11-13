#![cfg(target_env = "msvc")]

use std::path::Path;
use std::process::Command;

#[test]
fn release_runtime_links_windows_futex_imports() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            "ntsc-runtime",
            "--release",
            "--no-default-features",
        ])
        .current_dir(workspace)
        .status()
        .expect("build release runtime");
    assert!(status.success(), "release runtime build failed");

    let directory = std::env::temp_dir().join(format!(
        "ntsc_windows_release_runtime_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let program = ntsc_parser::parse(&ntsc_lexer::tokenize(
        r#"fun main() { say("Hello, World!") }"#,
    ))
    .unwrap();
    let object = ntsc_codegen::compile_program(
        &program,
        ntsc_codegen::host_triple(),
        inkwell::OptimizationLevel::Aggressive,
        "hello",
        &directory,
        false,
    )
    .expect("compile release program");
    let runtime = workspace
        .join("target/release")
        .join(ntsc_codegen::runtime_lib_name());
    let binary = directory.join("hello.exe");
    ntsc_codegen::link_binary(&object, &runtime, &binary)
        .expect("link release runtime with Windows synchronization imports");
    let output = Command::new(&binary).output().expect("run release program");
    assert!(
        output.status.success(),
        "release program failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Hello, World!"
    );
    std::fs::remove_dir_all(directory).unwrap();
}
