//! End-to-end regression tests for Rust-bridge values used as conditions.
//!
//! A bridge call is typed `Any` by the checker but takes its real type from
//! the adaptor export manifest at codegen, so the two can disagree. The case
//! that broke: an `option[bool]` return is a cell handle (an i64), and using
//! it as a condition compared that i64 against an i1 zero. The mismatched
//! icmp failed module verification and the whole program was rejected.
//!
//! These tests drive codegen through a hand-written export manifest, since
//! the divergence is only reachable when a manifest is present. They assert
//! the module compiles; linking would need a real adaptor archive.

use std::path::Path;

/// Write a one-function export manifest for `crate_name` and return the
/// `.ntsc` directory holding it.
fn write_manifest(project_dir: &Path, crate_name: &str, return_type: &str) -> std::path::PathBuf {
    let crate_dir = project_dir.join(".ntsc").join(crate_name);
    std::fs::create_dir_all(&crate_dir).unwrap();
    let manifest = format!(
        r#"{{
  "schema_version": 3,
  "module": "{crate_name}",
  "functions": [
    {{
      "name": "pick",
      "path": "pick",
      "status": "typed",
      "params": [],
      "return": {{ "tier": 1, "type": "{return_type}" }},
      "symbol": "ntsc_adaptor_{crate_name}_pick",
      "async": false
    }}
  ],
  "callbacks": []
}}
"#
    );
    std::fs::write(
        crate_dir.join(format!("{crate_name}.export.json")),
        manifest,
    )
    .unwrap();
    project_dir.join(".ntsc")
}

#[test]
fn an_option_return_used_as_a_condition_compiles() {
    let dir = std::env::temp_dir().join("ntsc_rust_option_cond");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write_manifest(&dir, "optbool", "option[bool]");

    ntsc_codegen::emit::set_rust_export_types(&dir);
    let result = ntsc_codegen::compile_source(
        "use @optbool\nfun main() {\n    if (@optbool.pick()) {\n        say(\"yes\")\n    } else {\n        say(\"no\")\n    }\n}\n",
        ntsc_codegen::host_triple(),
        "optbool_cond",
        &dir,
    );
    assert!(
        result.is_ok(),
        "an option-typed bridge result in a condition must not produce invalid IR: {:?}",
        result.err()
    );
}

#[test]
fn an_option_result_compared_to_nil_compiles() {
    let dir = std::env::temp_dir().join("ntsc_rust_option_nil");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write_manifest(&dir, "optstr", "option[string]");

    ntsc_codegen::emit::set_rust_export_types(&dir);
    let result = ntsc_codegen::compile_source(
        "use @optstr\nfun main() {\n    var got = @optstr.pick()\n    if (got != nil) {\n        say(got)\n    }\n}\n",
        ntsc_codegen::host_triple(),
        "optstr_nil",
        &dir,
    );
    assert!(
        result.is_ok(),
        "comparing an option-typed bridge result with nil must compile: {:?}",
        result.err()
    );
}

#[test]
fn an_object_return_in_a_condition_compiles() {
    // A plain handle used as a condition takes the same path, and is the
    // shape most adaptor calls have.
    let dir = std::env::temp_dir().join("ntsc_rust_option_obj");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write_manifest(&dir, "obtref", "object[Thing]");

    ntsc_codegen::emit::set_rust_export_types(&dir);
    let result = ntsc_codegen::compile_source(
        "use @obtref\nfun main() {\n    if (@obtref.pick()) {\n        say(\"live\")\n    }\n}\n",
        ntsc_codegen::host_triple(),
        "obtref_cond",
        &dir,
    );
    assert!(
        result.is_ok(),
        "an object-typed bridge result in a condition must compile: {:?}",
        result.err()
    );
}

#[test]
fn a_hyphenated_crate_name_resolves_its_return_types() {
    // A package may be `mewsic-tui` while the module that imports it is
    // `use @mewsic_tui`. The manifest is stored under the hyphenated
    // directory, so a lookup that missed the normalization fell back to the
    // handle assumption: a `bool`-returning function was typed as a handle
    // and `return` emitted `ret i1` of an i64, failing verification.
    let dir = std::env::temp_dir().join("ntsc_rust_option_hyphen");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write_manifest(&dir, "mewsic-tui", "bool");

    ntsc_codegen::emit::set_rust_export_types(&dir);
    // The import is spelled with an underscore, the manifest with a hyphen.
    let result = ntsc_codegen::compile_source(
        "use @mewsic_tui\nfun main() {\n    if (@mewsic_tui.pick()) {\n        say(\"yes\")\n    }\n}\n",
        ntsc_codegen::host_triple(),
        "hyphen_bool",
        &dir,
    );
    assert!(
        result.is_ok(),
        "a hyphenated crate must resolve its manifest return types: {:?}",
        result.err()
    );

    // And the reverse spelling resolves to the same entry.
    ntsc_codegen::emit::set_rust_export_types(&dir);
    let other = ntsc_codegen::compile_source(
        "use @mewsic_tui\nfun main() {\n    var flag = @mewsic_tui.pick()\n    say(\"\" + flag)\n}\n",
        ntsc_codegen::host_triple(),
        "hyphen_str",
        &dir,
    );
    assert!(
        other.is_ok(),
        "the normalized name must resolve from either spelling: {:?}",
        other.err()
    );
}
