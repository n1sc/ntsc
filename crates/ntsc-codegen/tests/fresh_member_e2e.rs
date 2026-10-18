//! Regression coverage for owned fields read from temporary class receivers.

use std::path::{Path, PathBuf};

fn runtime_lib(root: &Path) -> PathBuf {
    let lib = root
        .join("target/debug")
        .join(ntsc_codegen::runtime_lib_name());
    if !lib.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "ntsc-runtime"])
            .current_dir(root)
            .status()
            .expect("failed to run cargo");
        assert!(status.success(), "failed to build ntsc-runtime");
    }
    assert!(lib.exists(), "runtime lib not found at {lib:?}");
    lib
}

fn assert_clean(name: &str, source: &str, expected_stdout: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let dir = std::env::temp_dir().join(format!(
        "ntsc_fresh_member_e2e_{}_{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &dir)
        .expect("compile failed");
    let binary = dir.join(format!("{name}_bin"));
    ntsc_codegen::link_binary(
        &dir.join(format!("{name}.{}", ntsc_codegen::object_extension())),
        &runtime_lib(root),
        &binary,
    )
    .expect("link failed");
    let output = std::process::Command::new(&binary)
        .output()
        .expect("failed to run binary");
    std::fs::remove_dir_all(&dir).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{name} failed: {:?}, stderr: {stderr}",
        output.status
    );
    assert_eq!(stdout.trim(), expected_stdout, "{name}");
    assert!(
        !stderr.contains("NTSC-W0002"),
        "{name} leaked allocations: {stderr}"
    );
}

#[test]
fn temporary_string_and_scalar_members_release_the_receiver() {
    assert_clean(
        "string_and_scalar",
        r#"class Action {
    var string kind = "ready"
    var string unused = "payload"
    var int count = 7
}

fun main() -> int {
    var int i = 0
    while (i < 3) {
        say(make_action().kind)
        say("" + make_action().count)
        say("" + (make_action().kind == "ready"))
        make_action().kind
        i = i + 1
    }
    return 0
}

fun make_action() -> Action {
    return Action()
}
"#,
        "ready\n7\ntrue\nready\n7\ntrue\nready\n7\ntrue",
    );
}

#[test]
fn temporary_members_transfer_to_slots_returns_and_arguments() {
    assert_clean(
        "owned_consumers",
        r#"class Action {
    var string kind = "ready"
    var string unused = "payload"
}

fun make_action() -> Action {
    return Action()
}

fun read_kind() -> string {
    return make_action().kind
}

fun consume(string value) {
    say(value)
}

fun inspect(view string value) {
    say(value)
}

fun main() -> int {
    var kind = (make_action()).kind
    say(kind)
    kind = make_action().kind
    say(kind)
    say(read_kind())
    consume(make_action().kind)
    inspect(make_action().kind)
    say(copy(make_action().kind))
    say(Action().kind)
    say(copy(make_action()).kind)
    return 0
}
"#,
        "ready\nready\nready\nready\nready\nready\nready\nready",
    );
}

#[test]
fn temporary_array_member_outlives_its_receiver() {
    assert_clean(
        "array_member",
        r#"use arrays
class Bag {
    var array[string] items = ["first", "second"]
    var string unused = "payload"
}

fun make_bag() -> Bag {
    return Bag()
}

fun consume(array[string] items) {
    say(items[1])
}

fun main() -> int {
    var items = make_bag().items
    say(items[0])
    say(arrays.length(make_bag().items))
    consume(make_bag().items)
    say(make_bag().items[0])
    return 0
}
"#,
        "first\n2\nsecond\nfirst",
    );
}

#[test]
fn temporary_nested_class_member_survives_receiver_drop() {
    assert_clean(
        "nested_class_member",
        r#"class Action {
    var string kind = "ready"
}

class Envelope {
    var Action action = Action()
    var string unused = "payload"
}

fun make_envelope() -> Envelope {
    return Envelope()
}

fun inspect(view Action action) {
    say(action.kind)
}

fun main() -> int {
    say(make_envelope().action.kind)
    inspect(make_envelope().action)
    var action = make_envelope().action
    inspect(action)
    action = make_envelope().action
    say(action.kind)
    return 0
}
"#,
        "ready\nready\nready\nready",
    );
}

#[test]
fn temporary_option_member_is_copied_before_drop() {
    assert_clean(
        "option_member",
        r#"class Record {
    var option[string] note = "present"
    var string unused = "payload"
}

fun make_record() -> Record {
    return Record()
}

fun main() -> int {
    var note = make_record().note
    say("" + (note == nil))
    var another = make_record().note
    say("" + (another == nil))
    make_record().note
    return 0
}
"#,
        "false\nfalse",
    );
}

#[test]
fn live_receiver_members_remain_borrowed() {
    assert_clean(
        "borrowed_members",
        r#"use arrays
class Bag {
    var string label = "live"
    var array[int] items = [3, 5]
}

fun inspect(view Bag bag) {
    say(bag.label)
    say("" + bag.items[1])
}

fun main() -> int {
    var bag = Bag()
    view var label = bag.label
    view var items = bag.items
    say(copy(label))
    say(arrays.length(items))
    inspect(bag)
    say(bag.label)
    say("" + bag.items[0])
    return 0
}
"#,
        "live\n2\nlive\n5\nlive\n3",
    );
}

#[test]
fn temporary_class_array_member_keeps_its_elements() {
    assert_clean(
        "class_array_member",
        r#"use arrays
class Line {
    var string text = "line"
}
class Page {
    var array[Line] lines
    var string unused = "payload"
    fun init() {
        this.lines = []
        arrays.push(this.lines, Line())
    }
}
fun make_page() -> Page { return Page() }
fun main() -> int {
    var lines = make_page().lines
    say(lines[0].text)
    return 0
}
"#,
        "line",
    );
}
