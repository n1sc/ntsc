use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_manifest(root: &Path, name: &str) {
    let directory = root.join(".ntsc").join(name);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("{name}.export.json")),
        r#"{"functions":[{"status":"typed"},{"status":"opaque"}]}"#,
    )
    .unwrap();
}

#[test]
fn cargo_coverage_defaults_to_all_rust_dependencies() {
    let root =
        Fixture(std::env::temp_dir().join(format!("ntsc_cargo_coverage_{}", std::process::id())));
    std::fs::create_dir_all(&root.0).unwrap();
    std::fs::write(
        root.0.join("neutron.toml"),
        "[rust-deps]\nalpha = \"1\"\nbeta = \"1\"\n",
    )
    .unwrap();
    write_manifest(&root.0, "alpha");
    write_manifest(&root.0, "beta");
    write_manifest(&root.0, "stale");

    let output = Command::new(env!("CARGO_BIN_EXE_ntsc"))
        .args(["cargo", "coverage"])
        .current_dir(&root.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate alpha"), "{stdout}");
    assert!(stdout.contains("crate beta"), "{stdout}");
    assert!(!stdout.contains("crate stale"), "{stdout}");

    let output = Command::new(env!("CARGO_BIN_EXE_ntsc"))
        .args(["cargo", "coverage", "alpha"])
        .current_dir(&root.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate alpha"), "{stdout}");
    assert!(!stdout.contains("crate beta"), "{stdout}");
}
