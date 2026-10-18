//! End-to-end regression test for arrays of class instances.
//!
//! Instances travel as raw pointers, but the runtime stores array elements
//! as i64 bit patterns, so push/array-literal emission must ptrtoint on the
//! way in and inttoptr on the way out — otherwise LLVM verification fails
//! and element reads come back as integers. The array drop loop must also
//! run the per-class drop thunk for each element so owned string fields are
//! reclaimed.

use std::path::Path;
fn build_runtime() -> std::path::PathBuf {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
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

fn compile_and_run(source: &str, test_id: &str) -> String {
    let dir = std::env::temp_dir().join(format!("ntsc_class_array_{test_id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!("cls.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("cls_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "cls", &dir)
        .expect("compile_source failed");
    assert!(obj_path.exists(), "object file not produced");

    let runtime_lib = build_runtime();
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");
    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(output.status.success(), "binary exited non-zero");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Memory leak"),
        "registry objects leaked:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    stdout.trim().to_string()
}

#[test]
fn class_array_literal_and_field_reads() {
    let source = r#"use arrays
class Astro {
    var string name

    fun init(string n) {
        this.name = n;
    }
}

fun main() {
    var crew = [Astro("a"), Astro("bb"), Astro("ccc")];
    for (var member in crew) {
        say("- " + member.name);
    }
    say("crew: " + arrays.length(crew));
    say("done")
}
"#;
    assert_eq!(
        compile_and_run(source, "literal"),
        "- a\n- bb\n- ccc\ncrew: 3\ndone"
    );
}

#[test]
fn class_array_field_push_and_iterate() {
    let source = r#"use arrays
class Astro {
    var string name
    var float mass

    fun init(string n, float m) {
        this.name = n;
        this.mass = m;
    }
}

class Mission {
    var array[Astro] crew

    fun add(string n, float m) {
        arrays.push(this.crew, Astro(n, m));
    }

    fun count() -> int {
        return arrays.length(this.crew);
    }
}

fun main() {
    var m = Mission();
    m.add("anna", 62.0);
    m.add("bob", 74.5);
    var total = 0.0;
    for (var a in m.crew) {
        total = total + a.mass;
    }
    say("crew: " + m.count());
    say("total: " + total);
    say("done")
}
"#;
    assert_eq!(
        compile_and_run(source, "field_push"),
        "crew: 2\ntotal: 136.5\ndone"
    );
}

#[test]
fn method_on_this_returning_owned_string_is_dropped() {
    let source = r#"class Astro {
    var string name

    fun init(string n) {
        this.name = n;
    }

    fun badge() -> string {
        return "cdr." + this.name;
    }
}

fun main() {
    var crew = [Astro("anna"), Astro("bob")];
    for (var a in crew) {
        say(a.badge());
    }
    say("done")
}
"#;
    assert_eq!(
        compile_and_run(source, "method_on_this_string"),
        "cdr.anna\ncdr.bob\ndone"
    );
}

#[test]
fn fresh_pushed_class_element_keeps_owned_string_field() {
    let source = r#"use arrays
class Line {
    var string text

    fun init(string t) {
        this.text = "[" + t + "]";
    }
}

fun main() {
    var seed = Line("seed");
    var lines = [Line("first")];
    arrays.push(lines, copy(seed));
    for (var line in lines) {
        say(line.text);
    }
    say("done")
}
"#;
    assert_eq!(
        compile_and_run(source, "fresh_push_string_field"),
        "[first]\n[seed]\ndone"
    );
}
