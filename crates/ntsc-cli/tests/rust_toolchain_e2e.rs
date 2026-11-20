use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str, rust_modules: bool, source: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("ntsc_rust_toolchain_{name}_{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let mut manifest = String::from(
            "[package]\nentry = \"src/main.nt\"\noutput = \"fixture\"\n\
             compiler = \">=0.1.0\"\n\
             [experimental]\nrust_modules = true\n",
        );
        if rust_modules {
            manifest.push_str("[rust-deps.fixture]\npath = \"rust/fixture\"\n");
            std::fs::create_dir_all(root.join("rust/fixture/src")).unwrap();
            std::fs::write(
                root.join("rust/fixture/Cargo.toml"),
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .unwrap();
            std::fs::write(
                root.join("rust/fixture/src/lib.rs"),
                "pub fn value() -> i64 { 42 }\n",
            )
            .unwrap();
        }
        std::fs::write(root.join("neutron.toml"), manifest).unwrap();
        std::fs::write(root.join("src/main.nt"), source).unwrap();
        Self(root)
    }

    fn run_without_rust(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ntsc"))
            .args(arguments)
            .env("PATH", path_without_rust())
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("failed to remove fixture {}: {error}", self.0.display());
        }
    }
}

fn path_without_rust() -> OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::env::split_paths(&path).filter(|directory| {
        !["rustc", "cargo", "rustc.exe", "cargo.exe"]
            .iter()
            .any(|program| directory.join(program).is_file())
    }))
    .unwrap()
}

fn assert_no_build_artifacts(project: &Path) {
    for name in ["build", ".ntsc", "ntsc.lock"] {
        assert!(!project.join(name).exists(), "unexpected artifact: {name}");
    }
}

#[test]
fn rust_modules_fail_before_generation_without_rust() {
    let project = Fixture::new(
        "build",
        true,
        "use @fixture\nfun main() { say(@fixture.value()) }\n",
    );
    for arguments in [&["build"][..], &["build", "--json"][..]] {
        let output = project.run_without_rust(arguments);
        assert!(!output.status.success());
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(message.contains("Rust modules require"), "{message}");
        assert!(message.contains("https://rustup.rs"), "{message}");
        assert!(message.contains("rustup default stable"), "{message}");
        assert_no_build_artifacts(&project.0);
    }
}

#[test]
fn installing_rust_modules_reports_setup_before_resolution() {
    let project = Fixture::new("install", false, "fun main() {}\n");
    let output = project.run_without_rust(&["cargo", "install", "fixture"]);
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("Rust modules require"), "{message}");
    assert!(message.contains("https://rustup.rs"), "{message}");
    let manifest = std::fs::read_to_string(project.0.join("neutron.toml")).unwrap();
    assert!(!manifest.contains("[rust-deps"));
    assert_no_build_artifacts(&project.0);
}

#[test]
fn declared_rust_dependencies_require_tools_before_build_or_update() {
    let project = Fixture::new("declared", true, "fun main() {}\n");
    let original = std::fs::read_to_string(project.0.join("neutron.toml")).unwrap();
    for arguments in [&["build"][..], &["cargo", "update"][..]] {
        let output = project.run_without_rust(arguments);
        assert!(!output.status.success());
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(message.contains("Rust modules require"), "{message}");
        assert_eq!(
            std::fs::read_to_string(project.0.join("neutron.toml")).unwrap(),
            original
        );
        assert_no_build_artifacts(&project.0);
    }
}

#[test]
fn ordinary_neutron_diagnostics_do_not_require_rust() {
    let project = Fixture::new("neutron", false, "fun main() { missing_function() }\n");
    let output = project.run_without_rust(&["build"]);
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("NTSC-E0101"), "{message}");
    assert!(!message.contains("Rust modules require"), "{message}");
    let output = project.run_without_rust(&["cargo", "list"]);
    assert!(output.status.success(), "{output:?}");
}
