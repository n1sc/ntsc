//! End-to-end coverage for C ABI declarations and unsafe call sites.

use std::path::Path;

#[test]
fn c_abi_function_call_runs() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = workspace.parent().unwrap().parent().unwrap();
    let runtime_lib = root.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );
    if !runtime_lib.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "ntsc-runtime"])
            .current_dir(root)
            .status()
            .expect("failed to build ntsc-runtime");
        assert!(status.success(), "failed to build ntsc-runtime");
    }

    let source = r#"
extern "C" { int abs(int value); }

fun main() -> int {
    unsafe {
        if (abs(-7) == 7) {
            say("7")
        }
    }
    return 0
}
"#;
    let dir = std::env::temp_dir().join(format!("ntsc_ffi_e2e_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "ffi", &dir)
        .expect("FFI source should compile");
    let binary = dir.join("ffi");
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &binary).expect("FFI source should link");
    let output = std::process::Command::new(&binary)
        .output()
        .expect("failed to run FFI binary");
    assert!(output.status.success(), "FFI binary failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "7");
    let _ = std::fs::remove_dir_all(dir);
}
