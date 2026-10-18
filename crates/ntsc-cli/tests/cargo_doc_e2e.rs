use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ntsc_cargo_doc_{tag}_{}_{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn fake_opener(root: &Path) -> (PathBuf, PathBuf) {
    let bin = root.join("fake-bin");
    let marker = root.join("browser-invoked");
    std::fs::create_dir_all(&bin).unwrap();
    #[cfg(unix)]
    {
        let opener = bin.join("xdg-open");
        std::fs::write(
            &opener,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$NTSC_TEST_OPENER_MARKER\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&opener).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(opener, permissions).unwrap();
    }
    (bin, marker)
}

fn run_doc(
    root: &Path,
    cache_home: &Path,
    cargo_home: &Path,
    fake_bin: &Path,
    marker: &Path,
    args: &[&str],
) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ntsc"))
        .args(["cargo", "doc"])
        .args(args)
        .current_dir(root)
        .env("NTSC_CARGO_HOME", cache_home)
        .env("CARGO_HOME", cargo_home)
        .env("HOME", root.join("home"))
        .env("PATH", fake_bin)
        .env("NTSC_TEST_OPENER_MARKER", marker)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn assert_success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn normalized_html(html: &str) -> String {
    html.split_whitespace()
        .collect::<String>()
        .to_ascii_lowercase()
}

fn write_project(root: &Path) {
    write_file(root, "neutron.toml", "[rust-deps]\nfoo-bar = \"1\"\n");
    write_file(
        root,
        "ntsc.lock",
        "[[package]]\nname = \"foo-bar\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"fixture\"\n",
    );
}

fn write_export(root: &Path) -> Vec<u8> {
    let export = r#"{
  "schema_version": 3,
  "module": "foo-bar",
  "functions": [
    {
      "name": "legacy_async",
      "path": "legacy_async",
      "params": [],
      "return": {"tier": 1, "type": "string"},
      "symbol": "ntsc_adaptor_foo_bar_legacy_async",
      "async": true
    },
    {
      "name": "typed_sync",
      "path": "typed_sync",
      "status": "typed",
      "params": [],
      "return": {"tier": 1, "type": "int"},
      "symbol": "ntsc_adaptor_foo_bar_typed_sync",
      "async": false
    },
    {
      "name": "opaque_async",
      "path": "opaque_async",
      "status": "opaque",
      "params": [],
      "return": {"tier": 1, "type": "string"},
      "symbol": "ntsc_adaptor_foo_bar_opaque_async",
      "async": true
    },
    {
      "name": "unsupported",
      "path": "unsupported",
      "status": "unsupported",
      "reason": "unsupported return type",
      "signature": "pub fn unsupported()"
    }
  ],
  "callbacks": [
    {"trait": "Handler", "type": "Handler", "methods": []}
  ]
}
"#;
    let path = root.join(".ntsc/foo-bar/foo-bar.export.json");
    write_file(root, ".ntsc/foo-bar/foo-bar.export.json", export);
    std::fs::read(path).unwrap()
}

fn write_cached_source(root: &Path, cargo_home: &Path, source_name: &str) {
    let package = "[package]\nname = \"foo-bar\"\nversion = \"1.0.0\"\nedition = \"2021\"\n";
    let source = format!("pub fn {source_name}() -> &'static str {{ \"from cached source\" }}\n");
    write_file(root, "registry/src/foo-bar-1.0.0/Cargo.toml", package);
    write_file(root, "registry/src/foo-bar-1.0.0/src/lib.rs", &source);
    write_file(
        cargo_home,
        "registry/src/index.crates.io-0000/foo-bar-1.0.0/Cargo.toml",
        package,
    );
    write_file(
        cargo_home,
        "registry/src/index.crates.io-0000/foo-bar-1.0.0/src/lib.rs",
        &source,
    );
}

#[test]
fn cargo_doc_no_open_uses_manifest_for_aliases_counts_and_html() {
    let fixture = Fixture::new("manifest");
    let cache_home = fixture.0.join("ntsc-cargo");
    let cargo_home = fixture.0.join("cargo-home");
    let (fake_bin, marker) = fake_opener(&fixture.0);
    write_project(&fixture.0);
    let original_export = write_export(&fixture.0);
    write_cached_source(&cache_home, &cargo_home, "cached_only");

    let output = run_doc(
        &fixture.0,
        &cache_home,
        &cargo_home,
        &fake_bin,
        &marker,
        &["--no-open", "foo-bar"],
    );
    let stdout = assert_success(&output);
    assert!(stdout.contains("use @foo_bar"), "{stdout}");
    assert!(
        stdout.contains(
            "API: 2 callable functions (1 synchronous, 1 async), 2 not exposed, 1 callbacks"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("source: "), "{stdout}");
    assert!(stdout.contains("refresh: ntsc build"), "{stdout}");
    assert!(!marker.exists(), "--no-open invoked the browser opener");

    let html_path = fixture.0.join(".ntsc/foo-bar/foo-bar.export.html");
    assert!(html_path.is_file());
    let html = std::fs::read_to_string(&html_path).unwrap();
    let normalized = normalized_html(&html);
    assert!(normalized.contains("id=\"api-filter\""), "{html}");
    assert!(normalized.contains("type=\"search\""), "{html}");
    assert!(normalized.contains("<nav>"), "{html}");
    assert!(normalized.contains("href=\"#functions-0\""), "{html}");
    assert!(normalized.contains("href=\"#functions-1\""), "{html}");
    assert!(
        normalized.contains("await@foo_bar.legacy_async()"),
        "{html}"
    );
    assert!(normalized.contains("@foo_bar.typed_sync()"), "{html}");
    assert!(normalized.contains("addeventlistener('input'"), "{html}");

    // The source cache has a different API, but documentation is generated
    // from the existing export manifest and must not rewrite that manifest.
    write_cached_source(&cache_home, &cargo_home, "different_cached_api");
    let output = run_doc(
        &fixture.0,
        &cache_home,
        &cargo_home,
        &fake_bin,
        &marker,
        &["@foo_bar", "--no-open"],
    );
    let stdout = assert_success(&output);
    assert!(stdout.contains("crate foo-bar"), "{stdout}");
    assert!(!marker.exists(), "--no-open invoked the browser opener");
    assert_eq!(
        std::fs::read(fixture.0.join(".ntsc/foo-bar/foo-bar.export.json")).unwrap(),
        original_export
    );
    let html = std::fs::read_to_string(html_path).unwrap();
    assert!(html.contains("legacy_async"), "{html}");
    assert!(!html.contains("different_cached_api"), "{html}");
}

#[cfg(target_os = "linux")]
#[test]
fn cargo_doc_opens_generated_html_with_system_opener() {
    let fixture = Fixture::new("open");
    let cache_home = fixture.0.join("ntsc-cargo");
    let cargo_home = fixture.0.join("cargo-home");
    let (fake_bin, marker) = fake_opener(&fixture.0);
    write_project(&fixture.0);
    write_export(&fixture.0);

    let output = run_doc(
        &fixture.0,
        &cache_home,
        &cargo_home,
        &fake_bin,
        &marker,
        &["foo-bar"],
    );
    let stdout = assert_success(&output);
    assert!(stdout.contains("wrote "), "{stdout}");

    for _ in 0..100 {
        if marker.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.is_file(), "fake browser opener was not invoked");
    let opened = std::fs::read_to_string(marker).unwrap();
    assert!(opened.starts_with("file://"), "{opened}");
    assert!(opened.ends_with("foo-bar.export.html\n"), "{opened}");
}

#[test]
fn cargo_doc_reports_malformed_manifests_and_invalid_arguments() {
    let fixture = Fixture::new("errors");
    let cache_home = fixture.0.join("ntsc-cargo");
    let cargo_home = fixture.0.join("cargo-home");
    let (fake_bin, marker) = fake_opener(&fixture.0);
    write_project(&fixture.0);
    write_file(
        &fixture.0,
        ".ntsc/foo-bar/foo-bar.export.json",
        "{ malformed json\n",
    );

    let output = run_doc(
        &fixture.0,
        &cache_home,
        &cargo_home,
        &fake_bin,
        &marker,
        &["foo-bar", "--no-open"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot parse"), "{stderr}");
    assert!(stderr.contains("foo-bar.export.json"), "{stderr}");

    let output = run_doc(
        &fixture.0,
        &cache_home,
        &cargo_home,
        &fake_bin,
        &marker,
        &["--no-open"],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("usage: ntsc cargo doc"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = run_doc(
        &fixture.0,
        &cache_home,
        &cargo_home,
        &fake_bin,
        &marker,
        &["foo-bar", "--unknown"],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("usage: ntsc cargo doc"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
