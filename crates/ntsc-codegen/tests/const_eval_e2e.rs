//! End-to-end tests for compile-time constant evaluation:
//! - Folded arithmetic in `static const` initializers
//! - Constants referencing earlier constants
//! - Pure function calls evaluated at build time

use std::path::Path;

fn runtime_lib(rewrite_dir: &Path) -> std::path::PathBuf {
    let lib = rewrite_dir
        .join("target")
        .join("debug")
        .join(ntsc_codegen::runtime_lib_name());
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

fn compile_run(name: &str, source: &str) -> (bool, String, String) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rewrite_dir = workspace.parent().unwrap().parent().unwrap();
    let out_dir = rewrite_dir.join("target").join("const-eval-e2e").join(name);
    std::fs::create_dir_all(&out_dir).unwrap();
    let object = ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), name, &out_dir)
        .expect("compile failed");
    let binary = out_dir.join(name);
    ntsc_codegen::link_binary(&object, &runtime_lib(rewrite_dir), &binary).unwrap();
    let output = std::process::Command::new(binary).output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ── Folded arithmetic ───────────────────────────────────────────────────

#[test]
fn folded_integer_arithmetic() {
    let source = r#"static const var int A = 2 + 3
static const var int B = A * 10
static const var int C = B - 5

fun main() {
    say("A=" + A)
    say("B=" + B)
    say("C=" + C)
}
"#;
    let (ok, stdout, stderr) = compile_run("folded_int_arith", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "A=5\nB=50\nC=45\n");
}

#[test]
fn folded_float_arithmetic() {
    let source = r#"static const var float X = 1.5 + 2.5
static const var float Y = X * 2.0

fun main() {
    say("X=" + X)
    say("Y=" + Y)
}
"#;
    let (ok, stdout, stderr) = compile_run("folded_float_arith", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "X=4\nY=8\n");
}

#[test]
fn folded_mixed_int_float() {
    let source = r#"static const var float V = 3 + 0.5

fun main() {
    say("V=" + V)
}
"#;
    let (ok, stdout, stderr) = compile_run("folded_mixed", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "V=3.5\n");
}

#[test]
fn folded_negation() {
    let source = r#"static const var int NEG = -42
static const var int POS = -NEG

fun main() {
    say("NEG=" + NEG)
    say("POS=" + POS)
}
"#;
    let (ok, stdout, stderr) = compile_run("folded_neg", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "NEG=-42\nPOS=42\n");
}

#[test]
fn folded_grouping() {
    let source = r#"static const var int G = (2 + 3) * (10 - 4)

fun main() {
    say("G=" + G)
}
"#;
    let (ok, stdout, stderr) = compile_run("folded_group", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "G=30\n");
}

#[test]
fn folded_chained_constants() {
    let source = r#"static const var int P1 = 2
static const var int P2 = P1 * 2
static const var int P3 = P2 * 2
static const var int P4 = P3 * 2
static const var int P5 = P4 * 2

fun main() {
    say("P5=" + P5)
}
"#;
    let (ok, stdout, stderr) = compile_run("folded_chain", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "P5=32\n");
}

// ── Constants referencing earlier constants ──────────────────────────────

#[test]
fn constants_reference_earlier() {
    let source = r#"static const var int BASE = 100
static const var int OFFSET = 23
static const var int LIMIT = BASE + OFFSET

fun main() {
    say("LIMIT=" + LIMIT)
}
"#;
    let (ok, stdout, stderr) = compile_run("const_ref_earlier", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "LIMIT=123\n");
}

// ── Build-time function calls ───────────────────────────────────────────

#[test]
fn pure_function_at_build_time() {
    let source = r#"fun double(int x) -> int {
    return x * 2
}

static const var int VAL = double(21)

fun main() {
    say("VAL=" + VAL)
}
"#;
    let (ok, stdout, stderr) = compile_run("pure_fn_build", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "VAL=42\n");
}

#[test]
fn pure_function_with_multiple_args() {
    let source = r#"fun add(int a, int b) -> int {
    return a + b
}

static const var int SUM = add(17, 25)

fun main() {
    say("SUM=" + SUM)
}
"#;
    let (ok, stdout, stderr) = compile_run("pure_fn_multi", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "SUM=42\n");
}

#[test]
fn pure_function_composing_constants() {
    let source = r#"fun square(int x) -> int {
    return x * x
}

static const var int N = 5
static const var int SQ = square(N)

fun main() {
    say("SQ=" + SQ)
}
"#;
    let (ok, stdout, stderr) = compile_run("pure_fn_compose", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "SQ=25\n");
}

#[test]
fn folded_comparison() {
    let source = r#"static const var bool LT = 3 < 5
static const var bool GT = 10 > 20
static const var bool EQ = 7 == 7

fun main() {
    say("LT=" + LT)
    say("GT=" + GT)
    say("EQ=" + EQ)
}
"#;
    let (ok, stdout, stderr) = compile_run("folded_cmp", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "LT=true\nGT=false\nEQ=true\n");
}

#[test]
fn const_used_as_loop_bound() {
    let source = r#"static const var int COUNT = 3 * 2

fun main() {
    var i = 0
    while (i < COUNT) {
        say("i=" + i)
        i = i + 1
    }
}
"#;
    let (ok, stdout, stderr) = compile_run("const_loop_bound", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "i=0\ni=1\ni=2\ni=3\ni=4\ni=5\n");
}

// ── Radix number literals ───────────────────────────────────────────────

#[test]
fn radix_literals() {
    let source = r#"static const var int HEX = 0xff

fun main() {
    say("sum=" + (0x10 + 0b101))
    say("hex=" + HEX)
    say("oct=" + 0o17)
}
"#;
    let (ok, stdout, stderr) = compile_run("radix_literals", source);
    assert!(ok, "program failed: {stderr}");
    assert_eq!(stdout, "sum=21\nhex=255\noct=15\n");
}
