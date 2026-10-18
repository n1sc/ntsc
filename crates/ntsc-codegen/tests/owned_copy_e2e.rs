//! End-to-end regression tests for owned deep-copy semantics of `option[T]`
//! and class instances.
//!
//! `option[T]` is a boxed cell: `nil` is a null pointer and a set option owns a
//! heap cell holding the inner value. Options are owned, never aliased, so
//! every assignment or copy must produce an independent cell. Class instances
//! are heap structs whose owned fields must be deep-copied recursively.
//!
//! Regressions covered here:
//!   * copying a `nil` option dereferenced the null cell (SIGSEGV),
//!   * dropping a `nil` option loaded through the null cell (SIGSEGV),
//!   * an `option[T]` field of a class was skipped by the class drop thunk, so
//!     its cell and inner payload leaked,
//!   * a moved-from option slot was not nulled, so the cell was freed twice,
//!   * `option[bool]` asked for a 0-byte cell (`i1` / 8 == 0), and
//!     `ntsc_alloc(0)` returns null.
//!
//! Leak assertions lean on the debug-build `ntsc_runtime_shutdown` report,
//! which counts RC allocations (strings and arrays). Double frees surface as a
//! non-zero exit status from the allocator.

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

/// Compile + link + run `source`, returning (status, stdout, stderr).
fn compile_run(name: &str, source: &str) -> (std::process::ExitStatus, String, String) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let dir = std::env::temp_dir().join(format!("ntsc_owned_copy_e2e_{name}"));
    std::fs::create_dir_all(&dir).unwrap();

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &dir)
        .expect("compile failed");

    let bin_path = dir.join(format!("{name}_bin"));
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
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Assert the program ran cleanly: exit 0 (no SIGSEGV / allocator abort on a
/// double free), the expected stdout, and no leak warning.
fn assert_clean(name: &str, source: &str, expected_stdout: &str) {
    let (status, stdout, stderr) = compile_run(name, source);
    assert!(
        status.success(),
        "`{name}` must exit 0 (a crash here means a null deref or double free); \
         status was {status:?}, stderr: {stderr:?}"
    );
    assert_eq!(
        stdout.trim(),
        expected_stdout,
        "stdout mismatch for `{name}`"
    );
    assert!(
        !stderr.contains("memory leak detected"),
        "`{name}` must not leak, stderr was: {stderr:?}"
    );
}

/// Regression: copying a `nil` option loaded through the null cell and
/// segfaulted. `clone_option_value` had no nullness branch.
#[test]
fn copying_a_nil_option_does_not_crash() {
    let source = r#"fun main() -> int {
    var option[string] a = nil
    var option[string] b = copy(a)
    say("" + (a == nil))
    say("" + (b == nil))
    return 0
}
"#;
    assert_clean("copy_nil_option", source, "true\ntrue");
}

/// A set `option[string]` copies its inner string into a fresh cell, so both
/// options own independent payloads and each is freed exactly once.
#[test]
fn copying_a_set_string_option_deep_copies_the_cell() {
    let source = r#"fun main() -> int {
    var option[string] a = "hello"
    var option[string] b = copy(a)
    say("" + (a == nil))
    say("" + (b == nil))
    say("" + (a == b))
    return 0
}
"#;
    // Each option owns a distinct cell, so the identity comparison is false.
    assert_clean("copy_set_string_option", source, "false\nfalse\nfalse");
}

/// Regression: assigning `nil` over a set option dropped the old cell and then
/// re-marked the slot as owned. The exit-time drop then loaded through the now
/// null cell.
#[test]
fn assigning_nil_over_a_set_option_does_not_crash() {
    let source = r#"fun main() -> int {
    var option[string] a = "hello"
    a = nil
    var option[int] n = 5
    n = nil
    say("" + (a == nil))
    say("" + (n == nil))
    return 0
}
"#;
    assert_clean("assign_nil_over_set", source, "true\ntrue");
}

/// Regression: `option_cell_size` computed `i1 / 8 == 0` for `option[bool]`,
/// and `ntsc_alloc(0)` returns null, so the box store wrote through null.
#[test]
fn bool_option_allocates_a_nonzero_cell() {
    let source = r#"fun main() -> int {
    var option[bool] flag = true
    var option[bool] other = copy(flag)
    say("" + (flag == nil))
    say("" + (other == nil))
    return 0
}
"#;
    assert_clean("bool_option_cell", source, "false\nfalse");
}

/// An option holding an owned array deep-copies the array, so dropping either
/// option frees its own container exactly once.
#[test]
fn copying_an_array_option_deep_copies_the_payload() {
    let source = r#"fun main() -> int {
    var option[array[int]] a = [1, 2, 3]
    var option[array[int]] b = copy(a)
    say("" + (a == b))
    return 0
}
"#;
    assert_clean("copy_array_option", source, "false");
}

/// Regression: the class drop thunk filtered fields to
/// `Array | String | Shared | Class`, skipping `Option`, so an option field's
/// cell and its inner payload leaked.
#[test]
fn class_with_string_option_and_array_fields_is_reclaimed() {
    let source = r#"use arrays
class Record {
    var string label
    var option[string] note
    var array[int] values

    fun init() {
        this.label = "rec"
        this.note = "a note"
        this.values = []
    }

    fun add(int v) {
        arrays.push(this.values, v)
    }
}

fun main() -> int {
    var r = Record()
    r.add(1)
    r.add(2)
    say("label: " + r.label)
    say("values: " + arrays.length(r.values))
    say("" + (r.note == nil))
    return 0
}
"#;
    assert_clean(
        "class_option_field_reclaimed",
        source,
        "label: rec\nvalues: 2\nfalse",
    );
}

/// A class field left as `nil` must still be safe to drop: the thunk now visits
/// option fields, so the null cell has to be branched around.
#[test]
fn class_with_nil_option_field_is_reclaimed() {
    let source = r#"class Record {
    var string label
    var option[string] note

    fun init() {
        this.label = "rec"
    }
}

fun main() -> int {
    var r = Record()
    say("label: " + r.label)
    say("" + (r.note == nil))
    return 0
}
"#;
    assert_clean("class_nil_option_field", source, "label: rec\ntrue");
}

/// A class instance with owned fields deep-copies each one, so mutating the
/// copy leaves the source untouched and both are reclaimed independently.
#[test]
fn copying_a_class_instance_deep_copies_owned_fields() {
    let source = r#"use arrays
class Record {
    var string label
    var array[int] values

    fun init() {
        this.label = "original"
        this.values = []
    }

    fun add(int v) {
        arrays.push(this.values, v)
    }
}

fun main() -> int {
    var a = Record()
    a.add(1)
    a.add(2)
    var b = copy(a)
    b.label = "duplicate"
    arrays.push(b.values, 3)
    say("a.label: " + a.label)
    say("b.label: " + b.label)
    say("a.values: " + arrays.length(a.values))
    say("b.values: " + arrays.length(b.values))
    return 0
}
"#;
    assert_clean(
        "copy_class_deep",
        source,
        "a.label: original\nb.label: duplicate\na.values: 2\nb.values: 3",
    );
}

/// Regression: an inherited field must be copied at its *flattened* layout
/// index. A derived struct lays base fields out first, so a copy that used the
/// class's own field order would write the wrong slots.
#[test]
fn copying_a_derived_class_uses_flattened_field_indices() {
    let source = r#"use arrays
class Base {
    var string name
    var array[int] tags

    fun init() {
        this.name = "base"
        this.tags = []
    }
}

class Derived extends Base {
    var int extra

    fun init() {
        this.name = "derived"
        this.tags = []
        this.extra = 7
    }
}

fun main() -> int {
    var d = Derived()
    arrays.push(d.tags, 5)
    var e = copy(d)
    e.extra = 9
    say("d.name: " + d.name)
    say("e.name: " + e.name)
    say("d.extra: " + d.extra)
    say("e.extra: " + e.extra)
    say("e.tags: " + arrays.length(e.tags))
    return 0
}
"#;
    assert_clean(
        "copy_derived_class",
        source,
        "d.name: derived\ne.name: derived\nd.extra: 7\ne.extra: 9\ne.tags: 1",
    );
}

/// Regression: passing an owned option into a function moved it out of the
/// caller's slot, but `null_var_slot` ignored option slots, so both the callee
/// and the caller's exit freed the same cell.
#[test]
fn passing_an_option_to_a_function_does_not_double_free() {
    let source = r#"fun consume(option[string] o) -> bool {
    return o == nil
}

fun main() -> int {
    var option[string] a = "owned"
    say("" + consume(a))
    var option[string] b = nil
    say("" + consume(b))
    return 0
}
"#;
    assert_clean("option_arg_move", source, "false\ntrue");
}

/// Options reassigned in a loop must reclaim the previous cell on every
/// iteration, not just at function exit.
#[test]
fn reassigning_an_option_in_a_loop_is_balanced() {
    let source = r#"fun main() -> int {
    var option[string] o = nil
    for (var i = 0; i < 5; i = i + 1) {
        o = "value"
        o = nil
    }
    var option[array[int]] arr = nil
    for (var i = 0; i < 5; i = i + 1) {
        arr = [i, i + 1]
    }
    say("" + (o == nil))
    say("" + (arr == nil))
    return 0
}
"#;
    assert_clean("option_loop_reassign", source, "true\nfalse");
}

/// A class that transitively contains a field of its own type cannot be deep
/// copied — the emission would recurse without bound. That must be a clean
/// codegen error, not a compiler stack overflow.
#[test]
fn copying_a_self_referential_class_is_rejected() {
    let source = r#"class Node {
    var int value
    var Node next
}

fun main() -> int {
    var n = Node()
    var m = copy(n)
    return 0
}
"#;
    let dir = std::env::temp_dir().join("ntsc_owned_copy_e2e_self_ref");
    std::fs::create_dir_all(&dir).unwrap();
    let result =
        ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "self_ref", &dir);
    let _ = std::fs::remove_dir_all(&dir);

    let err = match result {
        Ok(_) => panic!("copying a self-referential class must be rejected"),
        Err(err) => err.to_string(),
    };
    assert!(
        err.contains("field of its own type"),
        "error must explain the unbounded copy, got: {err}"
    );
}

/// Regression: `copy(f())` where `f` returns a fresh owned string cloned the
/// result but never dropped the original fresh temporary, so every evaluation
/// leaked one string. `emit_copy` now drops the fresh source after cloning it.
#[test]
fn copying_a_fresh_call_result_drops_the_source() {
    let source = r#"use strings
fun strip_v(view string version) -> string {
    if (strings.starts_with(copy(version), "v")) {
        return strings.substring(copy(version), 1, -1)
    }
    return copy(version)
}

fun main() -> int {
    var n = 0
    while (n < 3) {
        var clean = copy(strip_v("v1.2.3"))
        n = n + 1
    }
    say("done")
    return 0
}
"#;
    assert_clean("copy_fresh_call_result", source, "done");
}

/// Regression: reassigning a class-typed field (`outer.item = Inner()`)
/// overwrote the field without dropping the instance it already held, leaking
/// that instance and its owned array. `store_into_field` now reclaims a
/// replaced class field the same way it does strings and arrays.
#[test]
fn reassigning_a_class_field_reclaims_the_old_instance() {
    let source = r#"use arrays
class Inner {
    var array[int] data
    fun init() {
        this.data = [1, 2, 3]
    }
}

class Outer {
    var Inner item
    fun init() {
        this.item = Inner()
    }
}

fun main() -> int {
    var o = Outer()
    o.item = Inner()
    say("len: " + arrays.length(o.item.data))
    return 0
}
"#;
    assert_clean("reassign_class_field", source, "len: 3");
}

/// Regression: `expr_is_fresh` decided whether a call was fresh by checking
/// whether the callee's LLVM body had been emitted yet, so a forward reference
/// (callee defined later in the module) was treated as non-fresh and its owned
/// return leaked. It now consults the stable user-function registry, so a fresh
/// call consumed by string concatenation is dropped regardless of emission
/// order.
#[test]
fn forward_referenced_call_result_is_dropped() {
    let source = r#"fun caller() -> int {
    var n = 0
    while (n < 3) {
        var s = "[" + make_tag() + "]"
        n = n + 1
    }
    return 0
}

fun make_tag() -> string {
    return "tag" + "!"
}

fun main() -> int {
    var r = caller()
    say("done")
    return 0
}
"#;
    assert_clean("forward_ref_fresh_call", source, "done");
}
