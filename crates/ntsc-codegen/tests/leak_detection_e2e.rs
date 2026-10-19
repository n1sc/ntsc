//! End-to-end test for 4.2 memory-leak detection: `ntsc_runtime_shutdown` is
//! emitted by the generated `main` with a report flag that is nonzero only in
//! debug (unoptimized) builds. Debug builds warn on stderr when RC allocations
//! remain at exit; release builds stay silent.
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
fn compile_run(
    rewrite_dir: &Path,
    name: &str,
    source: &str,
    opt_level: inkwell::OptimizationLevel,
) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("ntsc_leak_e2e_{name}"));
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
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn debug_build_warns_on_leaked_allocations() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    // `var c = b` gives the instance a second name. The class-drop analysis
    // rejects a candidate that is aliased, because either name could still read
    // the fields and freeing them once per name would double-free them, so the
    // fields are deliberately leaked instead. That makes this the positive
    // control for the detector itself: a debug build must report the abandoned
    // array.
    let source = r#"use arrays
class Bag {
    var array[int] items

    fun init() {
        this.items = [1, 2]
    }
}

fun main() -> int {
    var b = Bag()
    var c = b
    say("aliased " + arrays.length(c.items))
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "debug_warn",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "aliased 2");
    assert!(
        stderr.contains("warning[NTSC-W0002]: memory leak detected:"),
        "debug build must warn about leaked allocations, stderr was: {stderr:?}"
    );
    assert!(
        stderr.contains("--> <source>:6:22")
            && stderr.contains("array handle")
            && stderr.contains("was allocated here"),
        "leak warning must identify the surviving registry object, stderr was: {stderr:?}"
    );
}

/// Regression: overwriting a field drops the value it held, including the empty
/// array an `init`-less class default-initializes it with. This program used to
/// leak that abandoned default.
#[test]
fn overwritten_field_default_is_reclaimed() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"class Box {
    var array[int] data
}

fun main() -> int {
    var b = Box()
    b.data = [1, 2, 3]
    say("boxed")
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "field_default_reclaimed",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "boxed");
    assert!(
        !stderr.contains("memory leak detected"),
        "the overwritten default must be reclaimed, stderr was: {stderr:?}"
    );
}

/// Regression: a class with a declared `init` stores an owned array in a field
/// and is heap-allocated. The instance's fields are now reclaimed by the class
/// drop thunk, so a debug build must not report the array as leaked.
#[test]
fn heap_class_with_owned_field_is_reclaimed() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
class Ledger {
    var array[int] amounts

    fun init() {
        this.amounts = [];
    }

    fun add(int v) {
        arrays.push(this.amounts, v);
    }
}

fun main() -> int {
    var ledger = Ledger()
    ledger.add(5)
    ledger.add(5)
    say("total: " + arrays.length(ledger.amounts))
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "class_field_reclaimed",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "total: 2");
    assert!(
        !stderr.contains("memory leak detected"),
        "class field arrays must be reclaimed, stderr was: {stderr:?}"
    );
}

/// Regression: a class local that escapes via `return` on one branch is
/// excluded from the normal owned-slot/class-drop set, but on the branches
/// where a *different* value is returned it must still be dropped. Here `sent`
/// escapes only when `flag > 0`; the `flag == 0` path returns a fresh instance
/// instead, so `sent` (with its owned string fields) has to be reclaimed on the
/// normal return path. Called in a loop, a missed drop leaks every iteration.
#[test]
fn conditionally_returned_class_local_is_reclaimed_on_other_branches() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"class Action {
    var string kind
    var string text
    var string emoji

    fun init() {
        this.kind = "none"
        this.text = ""
        this.emoji = ""
    }
}

fun make_action(string kind) -> Action {
    var a = Action()
    a.kind = copy(kind)
    a.text = copy("")
    a.emoji = copy("")
    return a
}

fun tick(int flag) -> Action {
    var sent = make_action("none")
    if (flag > 0) {
        return sent
    }
    return make_action("keepalive")
}

fun consume(view Action action) {
    if (action.kind == "x") {
        say("never")
    }
}

fun main() -> int {
    var int i = 0
    while (i < 4) {
        consume(tick(0))
        i = i + 1
    }
    say("done")
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "return_escape_class_local",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "done");
    assert!(
        !stderr.contains("memory leak detected"),
        "conditionally-returned class local must be reclaimed on non-returning branches, stderr was: {stderr:?}"
    );
}

/// Regression: an `init`-less class (stack-allocated by escape analysis) must
/// default-initialize its owned container fields. Otherwise pushing into
/// `this.amounts` writes to a null handle and the value is silently lost.
#[test]
fn initless_class_default_initializes_owned_fields() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
class Ledger {
    var array[int] amounts

    fun add(int v) {
        arrays.push(this.amounts, v);
    }
}

fun main() -> int {
    var ledger = Ledger()
    ledger.add(5)
    ledger.add(5)
    say("total: " + arrays.length(ledger.amounts))
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "initless_defaults",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "total: 2");
    assert!(
        !stderr.contains("memory leak detected"),
        "init-less class fields must be reclaimed, stderr was: {stderr:?}"
    );
}

#[test]
fn debug_build_clean_program_is_silent() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    // No heap arrays / RC objects are allocated, so nothing is leaked.
    let source = r#"fun main() -> int {
    say("no allocations")
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "debug_clean",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "no allocations");
    assert!(
        !stderr.contains("memory leak detected"),
        "clean program must not warn, stderr was: {stderr:?}"
    );
}

#[test]
fn release_build_is_silent_even_with_allocations() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"fun main() -> int {
    var a = [1, 2, 3]
    say("sum")
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "release_silent",
        source,
        inkwell::OptimizationLevel::Aggressive,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "sum");
    assert!(
        !stderr.contains("memory leak detected"),
        "release build must stay silent, stderr was: {stderr:?}"
    );
}

/// Regression: the codegen now emits release code for arrays the program owns
/// (local variables, fresh temporaries, function returns), so a debug build
/// must not report RC leaks for programs that exercise those paths.
#[test]
fn reclaimed_arrays_do_not_warn() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
fun total(array[int] xs) -> int {
    var sum = 0;
    for (var x in xs) {
        sum = sum + x;
    }
    return sum;
}

fun doubles(array[int] xs) -> array[int] {
    var array[int] out = [];
    for (var x in xs) {
        arrays.push(out, x * 2);
    }
    return out;
}

fun main() -> int {
    var a = [1, 2, 3];
    for (var i = 0; i < 10; i = i + 1) {
        a = [i, i + 1];
    }
    var nested = [[1], [2, 2], [3, 3, 3]];
    var t = total([1, 2, 3, 4]);
    var d = doubles([5, 6, 7]);
    var sum = t;
    for (var x in d) {
        sum = sum + x;
    }
    for (var row in [[1, 1], [2, 2]]) {
        sum = sum + total(row);
    }
    say("sum: " + sum);
    say("a: " + arrays.length(a));
    say("nested: " + arrays.length(nested));
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "reclaimed",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "sum: 52\na: 2\nnested: 3");
    assert!(
        !stderr.contains("memory leak detected"),
        "owned arrays must be reclaimed, stderr was: {stderr:?}"
    );
}

/// Regression: nested arrays, element overwrites, fresh element reads, and
/// aliased arrays must all reclaim their RC references in a debug build.
#[test]
fn nested_and_aliased_arrays_do_not_warn() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"fun main() -> int {
    var a = [[1, 2], [3, 4]];
    var x = a[0][1];
    a[0] = [9, 9];
    var b = copy(a);
    b[1] = [7, 7];
    var c = [[[1]], [[2]]];
    c[0] = [[7]];
    var lit = [[1, 2], [3, 4]][1][0];
    var same = [1, 2];
    var alias = copy(same);
    same = copy(alias);
    say("x: " + x);
    say("a00: " + a[0][0]);
    say("b10: " + b[1][0]);
    say("c: " + c[0][0][0]);
    say("lit: " + lit);
    say("sum: " + (same[0] + alias[1]));
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "nested_alias",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "x: 2\na00: 9\nb10: 7\nc: 7\nlit: 3\nsum: 3");
    assert!(
        !stderr.contains("memory leak detected"),
        "nested/aliased arrays must be reclaimed, stderr was: {stderr:?}"
    );
}

/// Regression: `sort.*` and `random.shuffle` clone their input into a new
/// array. The fresh result must be balanced at the consuming site and the
/// borrowed input must not be released.
#[test]
fn sort_and_shuffle_do_not_warn() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
use random
use sort
fun main() -> int {
    var nums = [3, 1, 2];
    var sorted = sort.stable_sort(nums);
    var inline = sort.stable_sort([9, 7, 8]);
    var shuffled = random.shuffle(nums);
    say("sorted: " + sorted[0]);
    say("inline: " + inline[2]);
    say("len: " + arrays.length(shuffled));
    say("nums: " + nums[0]);
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "sort_shuffle",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "sorted: 1\ninline: 9\nlen: 3\nnums: 3");
    assert!(
        !stderr.contains("memory leak detected"),
        "sort/shuffle results must be reclaimed, stderr was: {stderr:?}"
    );
}

/// Regression: an async future reuses its class local across loop iterations.
/// Fresh copied instances must be dropped before the next iteration replaces
/// the future field, including owned string fields.
#[test]
fn async_channel_workers_reclaim_reused_class_locals() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
use collections
use process
class Ticket {
    var int id
    var string requester

    fun init(int id, string requester) {
        this.id = id
        this.requester = requester
    }
}

fun worker(int worker_id, int rx) {
    while (true) {
        var job = collections.channel_recv(rx)
        if (job == "") {
            break
        }
        say("worker " + worker_id + " took: " + job)
    }
}

async fun main() -> int {
    var array[Ticket] tickets = [Ticket(0, "caller-0"), Ticket(1, "caller-1"), Ticket(2, "caller-2")]
    var int rx = collections.channel(8)
    var int w1 = process.spawn_thread(fun(int rx) { worker(1, rx) }, rx)
    var int w2 = process.spawn_thread(fun(int rx) { worker(2, rx) }, rx)
    var int tx = collections.channel_sender(rx)
    for (var i = 0; i < arrays.length(tickets); i = i + 1) {
        var Ticket ticket = copy(tickets[i])
        collections.channel_send(tx, "job for #" + ticket.id)
    }
    collections.channel_close(tx)
    process.thread_join(w1)
    process.thread_join(w2)
    collections.channel_close(rx)
    say("done")
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "async_channel_class_locals",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "async worker program must run, stderr was: {stderr:?}");
    assert!(stdout.lines().any(|line| line.starts_with("worker ")));
    assert!(stdout.lines().any(|line| line == "done"));
    assert!(
        !stderr.contains("memory leak detected"),
        "reused async class locals must be reclaimed, stderr was: {stderr:?}"
    );
}

/// Write a one-function export manifest returning `object[Thing]` under the
/// project's `.ntsc` directory, so codegen tracks `@stub.pick()` as an owned
/// adaptor object.
fn write_object_manifest(project_dir: &Path) {
    let crate_dir = project_dir.join(".ntsc").join("stub");
    std::fs::create_dir_all(&crate_dir).unwrap();
    let manifest = r#"{
  "schema_version": 3,
  "module": "stub",
  "functions": [
    {
      "name": "pick",
      "path": "pick",
      "status": "typed",
      "params": [],
      "return": { "tier": 1, "type": "object[Thing]" },
      "symbol": "ntsc_adaptor_stub_pick",
      "async": false
    }
  ],
  "callbacks": []
}
"#;
    std::fs::write(crate_dir.join("stub.export.json"), manifest).unwrap();
}

/// Build a C stub archive that satisfies the adaptor object symbols codegen
/// emits: `pick` registers a shared object in the runtime and `object_release`
/// releases it. A plain C archive avoids the duplicate-`std` link conflict two
/// Rust staticlibs would create.
fn build_adaptor_stub_archive(dir: &Path) -> std::path::PathBuf {
    let stub_c = r#"#include <stdint.h>
extern uint64_t ntsc_shared_object_tag(const unsigned char* ptr, int64_t len);
extern int64_t ntsc_shared_object_put(void* ptr, void (*drop_fn)(void*), uint64_t tag);
extern void ntsc_shared_object_release(int64_t id);

static unsigned char stub_storage;
static void stub_drop(void* p) { (void)p; }

int64_t ntsc_adaptor_stub_pick(void) {
    const unsigned char* tag_name = (const unsigned char*)"stub@0.1.0+/Thing";
    uint64_t tag = ntsc_shared_object_tag(tag_name, 17);
    return ntsc_shared_object_put(&stub_storage, stub_drop, tag);
}

int64_t ntsc_adaptor_stub_object_retain(int64_t id) { return id; }

void ntsc_adaptor_stub_object_release(int64_t id) {
    ntsc_shared_object_release(id);
}
"#;
    let c_path = dir.join("stub.c");
    let obj_path = dir.join("stub.o");
    let archive = dir.join("libstub_adaptor.a");
    std::fs::write(&c_path, stub_c).unwrap();

    let status = std::process::Command::new("cc")
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&c_path)
        .status()
        .expect("failed to run cc");
    assert!(status.success(), "compiling the adaptor stub failed");

    let status = std::process::Command::new("ar")
        .arg("rcs")
        .arg(&archive)
        .arg(&obj_path)
        .status()
        .expect("failed to run ar");
    assert!(status.success(), "archiving the adaptor stub failed");

    archive
}

/// Regression: a loop-local adaptor `object[T]` (tracked apart from
/// `owned_slots`) leaked once per iteration because `drop_loop_locals` released
/// only owned slots, never tracked objects. A `continue` from inside a nested
/// block skips that block's own release, so every handle escaped. The
/// back-edge must now release loop-local tracked objects too.
#[test]
fn loop_local_adaptor_object_is_released_each_iteration() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let dir = std::env::temp_dir().join("ntsc_leak_e2e_adaptor_loop");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    write_object_manifest(&dir);
    ntsc_codegen::emit::set_rust_export_types(&dir);

    let source = r#"use @stub
fun run() -> int {
    var int total = 0
    var int i = 0
    while (i < 5) {
        if (i >= 0) {
            var int h = @stub.pick()
            if (h >= 0) {
                total = total + 1
            }
            i = i + 1
            continue
        }
        i = i + 1
    }
    return total
}

fun main() -> int {
    say("count " + run())
    return 0
}
"#;

    let program = {
        let tokens = ntsc_lexer::tokenize(source);
        ntsc_parser::parse(&tokens).expect("parse failed")
    };
    ntsc_codegen::compile_program(
        &program,
        ntsc_codegen::host_triple(),
        inkwell::OptimizationLevel::None,
        "adaptor_loop",
        &dir,
        false,
    )
    .expect("compile failed");

    let stub_archive = build_adaptor_stub_archive(&dir);
    let bin_path = dir.join("adaptor_loop");
    ntsc_codegen::link_binary_with_extras(
        &dir.join(format!("adaptor_loop.{}", ntsc_codegen::object_extension())),
        &runtime_lib(rewrite_dir),
        std::slice::from_ref(&stub_archive),
        &[],
        &bin_path,
    )
    .expect("link failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "binary must exit 0: {stderr:?}");
    assert_eq!(stdout.trim(), "count 5");
    assert!(
        !stderr.contains("memory leak detected"),
        "loop-local adaptor objects must be released each iteration, stderr was: {stderr:?}"
    );
}

/// Regression: a function that constructs `var s = SomeClass()` and then throws
/// before `return s` used to leak the whole instance tree. `s` escapes via the
/// return, so it is excluded from the scope-exit drop set; the function-level
/// exception-return path returned a default without dropping it, leaking its
/// owned array and nested-class string fields. The unwind path must now drop
/// these return-escaping class locals.
#[test]
fn thrown_return_value_class_tree_is_reclaimed() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
class Inner {
    var string label
    fun init() {
        this.label = "inner-" + "value"
    }
}

class Settings {
    var array[int] values
    var Inner inner
    fun init() {
        this.values = [1, 2, 3]
        this.inner = Inner()
    }
}

fun build(bool boom) -> Settings {
    var s = Settings()
    s.values = [4, 5, 6]
    if (boom) {
        throw "boom"
    }
    return s
}

fun main() -> int {
    try {
        var s = build(true)
        say("built " + arrays.length(s.values))
    } catch (e) {
        say("caught")
    }
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "thrown_return_value_tree",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0");
    assert_eq!(stdout.trim(), "caught");
    assert!(
        !stderr.contains("memory leak detected"),
        "a class tree built then abandoned by a throw must be reclaimed, stderr was: {stderr:?}"
    );
}

/// Regression: reassigning a class local (`var s = default()` then
/// `s = from_file()`) must drop the instance the slot held before the store,
/// including its nested-class tree of owned string/array fields. The escape
/// analysis used to reject *any* reassigned class local from field-drop
/// analysis, so the abandoned first tree was never reclaimed — the exact
/// shape of mewsic's `config_load`, which leaked its default `ConfigSettings`
/// tree on every run. Two variants: the reassigned local is returned by field
/// (`return s.token`, a normal scope-exit drop) and returned whole
/// (`return s`, where only the *replaced* value is ours to drop and the final
/// one moves to the caller). Neither may leak or double-free.
#[test]
fn reassigned_class_local_drops_replaced_tree() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let prelude = r#"use arrays
class Deep {
    var array[string] providers
    var string template
    fun init() {
        this.providers = ["lrclib", "netease", "qqmusic"]
        this.template = "[{timestamp}] [{lyrics}]"
    }
}

class Settings {
    var Deep deep
    var string token
    fun init() {
        this.deep = Deep()
        this.token = ""
    }
}

fun load_default() -> Settings {
    return Settings()
}

fun load_from_file() -> Settings {
    var s = Settings()
    s.token = "loaded"
    return s
}
"#;

    // Variant A: the reassigned local is consumed by a field read, so `s`
    // itself does not escape and gets a normal scope-exit drop on top of the
    // reassign-time drop of the replaced default tree.
    let return_field = format!(
        "{prelude}
fun config_load() -> string {{
    var s = load_default()
    s = load_from_file()
    return s.token
}}

fun main() -> int {{
    var t = config_load()
    say(\"token \" + t)
    return 0
}}
"
    );

    // Variant B: the reassigned local is returned whole, so its final value
    // moves to the caller; only the replaced default tree is dropped here.
    let return_whole = format!(
        "{prelude}
fun config_load() -> Settings {{
    var s = load_default()
    s = load_from_file()
    return s
}}

fun main() -> int {{
    var s = config_load()
    say(\"token \" + s.token)
    return 0
}}
"
    );

    for (name, source, want) in [
        ("reassign_return_field", &return_field, "token loaded"),
        ("reassign_return_whole", &return_whole, "token loaded"),
    ] {
        let (ok, stdout, stderr) =
            compile_run(rewrite_dir, name, source, inkwell::OptimizationLevel::None);
        assert!(ok, "{name}: binary must exit 0, stderr was: {stderr:?}");
        assert_eq!(stdout.trim(), want, "{name}: unexpected stdout");
        assert!(
            !stderr.contains("memory leak detected"),
            "{name}: the replaced class tree must be reclaimed, stderr was: {stderr:?}"
        );
    }
}

/// Manifest for a crate whose `make` hands back a fresh adaptor object and
/// whose `borrow` takes one by reference (returning an int, like every
/// mewsic keyring accessor), so the caller stays the sole owner.
fn write_forward_object_manifest(project_dir: &Path) {
    let crate_dir = project_dir.join(".ntsc").join("fwd");
    std::fs::create_dir_all(&crate_dir).unwrap();
    let manifest = r#"{
  "schema_version": 3,
  "module": "fwd",
  "functions": [
    {
      "name": "make",
      "path": "make",
      "status": "typed",
      "params": [],
      "return": { "tier": 1, "type": "object[Thing]" },
      "symbol": "ntsc_adaptor_fwd_make",
      "async": false
    },
    {
      "name": "borrow",
      "path": "borrow",
      "status": "typed",
      "params": [{ "name": "a0", "tier": 1, "type": "object[Thing]" }],
      "return": { "tier": 1, "type": "int" },
      "symbol": "ntsc_adaptor_fwd_borrow",
      "async": false
    }
  ],
  "callbacks": []
}
"#;
    std::fs::write(crate_dir.join("fwd.export.json"), manifest).unwrap();
}

/// Stub archive: `make` registers a shared object, `borrow` only reads its
/// handle, and `object_release` frees the registry entry. The caller is
/// responsible for the release, matching the real adaptor contract.
fn build_forward_stub_archive(dir: &Path) -> std::path::PathBuf {
    let stub_c = r#"#include <stdint.h>
extern uint64_t ntsc_shared_object_tag(const unsigned char* ptr, int64_t len);
extern int64_t ntsc_shared_object_put(void* ptr, void (*drop_fn)(void*), uint64_t tag);
extern void ntsc_shared_object_release(int64_t id);

static unsigned char fwd_storage;
static void fwd_drop(void* p) { (void)p; }

int64_t ntsc_adaptor_fwd_make(void) {
    const unsigned char* tag_name = (const unsigned char*)"fwd@0.1.0+/Thing";
    uint64_t tag = ntsc_shared_object_tag(tag_name, 16);
    return ntsc_shared_object_put(&fwd_storage, fwd_drop, tag);
}

int64_t ntsc_adaptor_fwd_borrow(int64_t id) { (void)id; return 0; }

int64_t ntsc_adaptor_fwd_object_retain(int64_t id) { return id; }

void ntsc_adaptor_fwd_object_release(int64_t id) {
    ntsc_shared_object_release(id);
}
"#;
    let c_path = dir.join("fwd.c");
    let obj_path = dir.join("fwd.o");
    let archive = dir.join("libfwd_adaptor.a");
    std::fs::write(&c_path, stub_c).unwrap();

    let status = std::process::Command::new("cc")
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&c_path)
        .status()
        .expect("failed to run cc");
    assert!(status.success(), "compiling the forward stub failed");

    let status = std::process::Command::new("ar")
        .arg("rcs")
        .arg(&archive)
        .arg(&obj_path)
        .status()
        .expect("failed to run ar");
    assert!(status.success(), "archiving the forward stub failed");

    archive
}

/// Regression: a user function returning a fresh adaptor `object[T]` (directly
/// or by forwarding another such function) transfers ownership across the call
/// boundary. Before cross-boundary tracking, the caller had no handle to
/// release, so each returned object leaked. The result now flows through
/// `fresh_object_call` exactly like a bridge result and is released after the
/// borrowing call.
#[test]
fn user_function_object_return_is_released_across_the_boundary() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let dir = std::env::temp_dir().join("ntsc_leak_e2e_fwd_object");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    write_forward_object_manifest(&dir);
    ntsc_codegen::emit::set_rust_export_types(&dir);

    let source = r#"use @fwd
fun make_entry() -> int {
    return @fwd.make()
}

fun forward_entry() -> int {
    return make_entry()
}

fun main() -> int {
    @fwd.borrow(make_entry())
    @fwd.borrow(forward_entry())
    return 0
}
"#;

    let program = {
        let tokens = ntsc_lexer::tokenize(source);
        ntsc_parser::parse(&tokens).expect("parse failed")
    };
    ntsc_codegen::compile_program(
        &program,
        ntsc_codegen::host_triple(),
        inkwell::OptimizationLevel::None,
        "fwd_object",
        &dir,
        false,
    )
    .expect("compile failed");

    let stub_archive = build_forward_stub_archive(&dir);
    let bin_path = dir.join("fwd_object");
    ntsc_codegen::link_binary_with_extras(
        &dir.join(format!("fwd_object.{}", ntsc_codegen::object_extension())),
        &runtime_lib(rewrite_dir),
        std::slice::from_ref(&stub_archive),
        &[],
        &bin_path,
    )
    .expect("link failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "binary must exit 0: {stderr:?}");
    assert!(
        !stderr.contains("memory leak detected"),
        "an object returned across a user-function boundary must be released, stderr was: {stderr:?}"
    );
}

#[test]
fn fresh_string_index_assignment_is_reclaimed() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let source = r#"use arrays
use strings
fun main() -> int {
    var words = ["first", "second"]
    var source = "changed"
    words[0] = copy(source)
    words[1] = strings.upper(copy(source))
    say(words[0])
    say(words[1])
    say(source)
    var assigned = words[0] = copy(source)
    say(assigned)
    words[0] = source
    say(words[0])
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "fresh_index_assignment",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "{stderr}");
    assert_eq!(stdout, "changed\nCHANGED\nchanged\nchanged\nchanged\n");
    assert!(!stderr.contains("NTSC-W0002"), "{stderr}");
}

#[test]
fn class_borrowed_by_method_keeps_caller_cleanup() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let source = r#"class Settings {
    var string name
    var array[string] providers
    fun init() {
        this.name = "song"
        this.providers = ["one", "two"]
    }
}
class Reader {
    fun inspect(view Settings settings) -> string {
        return copy(settings.name)
    }
    fun update(view mut Settings settings) {
        settings.name = "updated"
    }
}
fun make_reader() -> Reader {
    return Reader()
}
fun main() -> int {
    var reader = make_reader()
    var settings = Settings()
    say(reader.inspect(settings))
    reader.update(settings)
    say(reader.inspect(settings))
    say(settings.name)
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "method_view_argument",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "{stderr}");
    assert_eq!(stdout, "song\nupdated\nupdated\n");
    assert!(!stderr.contains("NTSC-W0002"), "{stderr}");
}

/// Regression: `arrays.join` of a string array is desugared into an
/// accumulation loop. The loop used to alias the first element into its result
/// slot and leak one delimiter-prefixed scratch handle plus the overwritten
/// previous result per extra element, and the joined string was not recognized
/// as a fresh call result, so consuming it as an argument leaked it too.
#[test]
fn string_array_join_is_reclaimed() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
fun main() -> int {
    var parts = ["alpha", "beta", "gamma\""]
    var joined = arrays.join(parts, ",")
    say(joined)
    say(arrays.join(["tab\t", "nl\n"], "-"))
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "string_array_join",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0, stderr was: {stderr:?}");
    // Escape sequences are processed by the lexer: `\"` is a real quote and
    // `\t`/`\n` in the elements are real control characters; only the `say`
    // separators add extra newlines.
    assert_eq!(stdout, "alpha,beta,gamma\"\ntab\t-nl\n\n");
    assert!(
        !stderr.contains("NTSC-W0002"),
        "joining a string array must leave no scratch handles, stderr was: {stderr:?}"
    );
}

/// Regression: joining a scalar array renders each element with a fresh
/// to-string handle. The loop never dropped those per-element conversions.
#[test]
fn scalar_array_join_is_reclaimed() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
fun main() -> int {
    var arr = [1, 2, 3]
    say(arrays.join(arr, "-"))
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "scalar_array_join",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0, stderr was: {stderr:?}");
    assert_eq!(stdout.trim(), "1-2-3");
    assert!(
        !stderr.contains("NTSC-W0002"),
        "scalar element conversions must be reclaimed, stderr was: {stderr:?}"
    );
}

/// Regression: the join loop leaked one scratch handle *per iteration*, so a
/// repeated join in a loop multiplied the leak. Each iteration must come out
/// clean.
#[test]
fn repeated_join_in_loop_is_reclaimed() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use arrays
fun main() -> int {
    var arr = ["p", "q"]
    for (var int i = 0; i < 3; i = i + 1) {
        say(arrays.join(arr, ","))
    }
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "repeated_join_loop",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0, stderr was: {stderr:?}");
    assert_eq!(stdout, "p,q\np,q\np,q\n");
    assert!(
        !stderr.contains("NTSC-W0002"),
        "repeated joins must not accumulate scratch handles, stderr was: {stderr:?}"
    );
}

/// Regression: `strings.split_array` results were never dropped — the
/// freshness table did not know the call materializes a fresh array, so the
/// array and every element copy leaked, and the runtime leaked the original
/// handle of each part on top. A debug build must come out clean.
#[test]
fn split_array_result_is_reclaimed() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();

    let source = r#"use strings
use arrays
fun main() -> int {
    var parts = strings.split_array("ntsc,compiler,runtime", ",")
    for (var int i = 0; i < arrays.length(parts); i = i + 1) {
        say(arrays.get(parts, i))
    }
    return 0
}
"#;
    let (ok, stdout, stderr) = compile_run(
        rewrite_dir,
        "split_array_reclaimed",
        source,
        inkwell::OptimizationLevel::None,
    );
    assert!(ok, "binary must exit 0, stderr was: {stderr:?}");
    assert_eq!(stdout, "ntsc\ncompiler\nruntime\n");
    assert!(
        !stderr.contains("NTSC-W0002"),
        "the split result and its parts must be reclaimed, stderr was: {stderr:?}"
    );
}
