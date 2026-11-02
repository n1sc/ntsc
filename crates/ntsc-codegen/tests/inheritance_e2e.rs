//! End-to-end regression test for single inheritance (`class Dog extends
//! Animal`).
//!
//! Regression: derived classes used to emit a struct with only their own
//! fields, and method dispatch looked up `{Derived}.{method}` directly, so
//! inherited fields were out of range and inherited (non-overridden) methods
//! failed with "undefined method". Derived structs now lay out base fields
//! first, member access resolves the flattened field index, and method
//! dispatch walks the parent chain (casting the receiver to the declaring
//! class so base methods see a layout-compatible instance).

use std::path::Path;

#[test]
fn inheritance_e2e() {
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
    assert!(
        runtime_lib.exists(),
        "runtime lib not found at {runtime_lib:?}"
    );

    let source = r#"use arrays
class Creature {
    var level = 1;
    fun base() -> int { return this.level; }
}
class Animal extends Creature {
    var tags = ["x"];
    fun label() -> string { return "animal"; }
    fun describe() -> string { return "a " + this.label(); }
}
class Dog extends Animal {
    fun wag() -> string { return "wagging"; }
    fun total() -> int { return this.base() + this.level; }
}

fun main() {
    var d = Dog();
    d.level = 40;
    say("level: " + d.level);
    say("base: " + d.base());
    say("total: " + d.total());
    say("label: " + d.label());
    say("describe: " + d.describe());
    say("wag: " + d.wag());
    say("tags len: " + arrays.length(d.tags));
    say("done");
}
"#;

    let dir = std::env::temp_dir().join("ntsc_inheritance_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!("inheritance.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("inheritance_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "inheritance", &dir)
        .expect("compile_source failed");

    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "binary exited with non-zero status"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    let expected_lines = [
        "level: 40",
        "base: 40",
        "total: 80",
        "label: animal",
        "describe: a animal",
        "wag: wagging",
        // `var tags = ["x"]` is applied at construction, so the inherited
        // field holds the one-element array the class declared.
        "tags len: 1",
        "done",
    ];
    for line in expected_lines {
        assert!(
            stdout.lines().any(|l| l == line),
            "missing output line {line:?}:\n{stdout}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: a class that reached itself through `extends` sent codegen into
/// unbounded parent-chain recursion, so the compiler died with a stack
/// overflow instead of reporting the cycle. Typeck now rejects the cycle before
/// codegen runs, so these must come back as ordinary errors.
#[test]
fn inheritance_cycles_are_rejected_before_codegen() {
    let dir = std::env::temp_dir().join("ntsc_inheritance_cycle_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let cases = [
        ("direct", "class A extends A { }\nfun main() { say(\"x\") }"),
        (
            "mutual",
            "class A extends B { }\nclass B extends A { }\nfun main() { say(\"x\") }",
        ),
        (
            "three_way",
            "class A extends B { }\nclass B extends C { }\nclass C extends A { }\nfun main() { say(\"x\") }",
        ),
    ];

    for (name, source) in cases {
        let err = ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &dir)
            .expect_err(&format!("{name}: cyclic inheritance should not compile"));
        let message = err.to_string();
        assert!(
            message.contains("cannot inherit"),
            "{name}: expected an inheritance-cycle diagnostic, got {message:?}"
        );
    }

    // The acyclic chain must still compile, so the guard is not over-eager.
    ntsc_codegen::compile_source(
        "class Base { }\nclass Mid extends Base { }\nclass Leaf extends Mid { }\nfun main() { say(\"ok\") }",
        ntsc_codegen::host_triple(),
        "acyclic_chain",
        &dir,
    )
    .expect("acyclic inheritance chain should compile");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: `super` was lexed but rejected in expressions, so a derived
/// class could not call the base implementation it overrides.
#[test]
fn super_calls_the_base_implementation() {
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

    let source = r#"class Base {
    var string name

    fun init(string n) {
        this.name = n
    }

    fun greet() -> string {
        return "base:" + this.name
    }

    fun kind() -> string {
        return "base"
    }
}

class Child extends Base {
    fun init(string n) {
        super.init(n)
    }

    fun greet() -> string {
        return super.greet() + "/child"
    }

    fun kind() -> string {
        return super.kind() + "-child"
    }
}

fun main() {
    var Child c = Child("ada")
    say(c.greet())
    say(c.kind())
}
"#;

    let dir = std::env::temp_dir().join("ntsc_super_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!("super_call.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("super_call_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "super_call", &dir)
        .expect("compile_source failed");
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "binary exited with non-zero status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.lines().any(|line| line == "base:ada/child"),
        "{stdout}"
    );
    assert!(stdout.lines().any(|line| line == "base-child"), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}
