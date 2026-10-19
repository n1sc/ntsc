//! End-to-end test: `arrays.get`/`arrays.at` on a `view array[T]` parameter.
//!
//! Regression: `array_elem_ty` only stripped `Ty::Array`, so an element read
//! routed through a `view array[int]` argument collapsed to `Ty::Any` and any
//! arithmetic on the result emitted the string-ABI pointer as an int operand
//! (`unsupported binary operation Minus on type string`).

use std::path::Path;

fn runtime_lib() -> std::path::PathBuf {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target")
        .join("debug")
        .join(ntsc_codegen::runtime_lib_name());
    if !lib.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "ntsc-runtime"])
            .current_dir(lib.parent().unwrap().parent().unwrap().parent().unwrap())
            .status()
            .expect("failed to run cargo");
        assert!(status.success(), "failed to build ntsc-runtime");
    }
    lib
}

fn compile_run(name: &str, source: &str) -> (bool, String) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let out_dir = rewrite_dir
        .join("target")
        .join("view-array-elem-e2e")
        .join(name);
    std::fs::create_dir_all(&out_dir).unwrap();
    let object = ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &out_dir)
        .expect("compile failed");
    let binary = out_dir.join(name);
    ntsc_codegen::link_binary(&object, &runtime_lib(), &binary).unwrap();
    let output = std::process::Command::new(binary).output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn view_array_element_arithmetic() {
    let source = r#"use arrays
use fmt

fun bump(view array[int] xs, int i) -> string {
    return fmt.i64_to_str(arrays.get(xs, i) - 1)
}

fun main() {
    var array[int] nums = [4, 5, 6]
    say(bump(nums, 0))
    say(bump(nums, 1))
}
"#;
    let (ok, stdout) = compile_run("view_array_elem_arith", source);
    assert!(ok);
    assert_eq!(stdout, "3\n4\n");
}
