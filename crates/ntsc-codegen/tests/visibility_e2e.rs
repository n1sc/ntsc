//! End-to-end test: `private` declarations are visible only inside the file
//! that declares them. Public symbols keep working across bare and aliased
//! imports, while private helpers stay callable from their own module.

use inkwell::OptimizationLevel;

use std::path::Path;

fn build_runtime() -> std::path::PathBuf {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let runtime_lib = rewrite_dir.join(
        Path::new("target")
            .join("debug")
            .join(ntsc_codegen::runtime_lib_name()),
    );

    if !runtime_lib.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "ntsc-runtime"])
            .current_dir(rewrite_dir)
            .status()
            .expect("failed to run cargo");
        assert!(status.success(), "failed to build ntsc-runtime");
    }
    runtime_lib
}

struct Project {
    dir: std::path::PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ntsc_visibility_e2e_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        Self { dir }
    }

    fn write(&self, rel: &str, contents: &str) {
        std::fs::write(self.dir.join(rel), contents).unwrap();
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn private_helpers_stay_inside_their_module() {
    let project = Project::new("helpers");
    project.write(
        "src/main.nt",
        r#"use "math.nt"
use "strings.nt" as strings

fun main() {
    say("quad: " + quadruple(3))
    say(strings.shout("hi"))
    var box = strings.Box("x")
    say(box.describe())
    say(strings.describe_buffer("y"))
}
"#,
    );
    project.write(
        "src/math.nt",
        r#"private fun twice(int n) -> int {
    return n * 2
}

fun quadruple(int n) -> int {
    return twice(twice(n))
}
"#,
    );
    project.write(
        "src/strings.nt",
        r#"private fun wrap(string s) -> string {
    return "[" + s + "]"
}

fun shout(string s) -> string {
    return wrap(s) + "!"
}

private class Buffer {
    var string text

    fun init(string text) {
        this.text = text
    }

    fun value() -> string {
        return wrap(this.text)
    }
}

fun describe_buffer(string s) -> string {
    var buffer = Buffer(s)
    return buffer.value()
}

class Box {
    var string text

    fun init(string text) {
        this.text = text
    }

    fun describe() -> string {
        return wrap(this.text)
    }
}
"#,
    );

    let runtime_lib = build_runtime();
    let obj_path = project
        .dir
        .join(format!("app.{}", ntsc_codegen::object_extension()));
    let bin_path = project.dir.join("app");

    let loaded = ntsc_build::modules::load_program(&project.dir.join("src/main.nt"))
        .expect("load_program failed");
    ntsc_codegen::compile_program(
        &loaded.program,
        ntsc_codegen::host_triple(),
        OptimizationLevel::None,
        "app",
        &project.dir,
        false,
    )
    .expect("compile_program failed");
    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");
    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(output.status.success(), "binary exited non-zero");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "quad: 12\n[hi]!\n[x]\n[y]");
}
