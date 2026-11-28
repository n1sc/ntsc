use std::fs;
use std::process::Command;

#[test]
fn ordinary_run_reports_build_work_on_stderr() {
    let project = std::env::temp_dir().join(format!("ntsc_cargo_style_{}", std::process::id()));
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("neutron.toml"),
        "[package]\nentry = \"main.nt\"\noutput = \"demo\"\ncompiler = \">=0.1.0\"\n",
    )
    .unwrap();
    fs::write(project.join("main.nt"), "fun main() { say(\"hello\"); }").unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ntsc"))
            .args(args)
            .current_dir(&project)
            .env("NO_COLOR", "1")
            .env_remove("CLICOLOR_FORCE")
            .output()
            .unwrap()
    };
    let run = invoke(&["run"]);
    let log = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{log}");
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim_end(), "hello");
    let mut lines = log.lines().map(str::trim_start);
    for verb in [
        "Loading",
        "Compiling",
        "Linking",
        "Timing",
        "Finished",
        "Running",
    ] {
        assert!(lines.any(|line| line.starts_with(verb)), "{log}");
    }
    assert!(
        log.contains("codegen") && log.contains("[unoptimized]"),
        "{log}"
    );
    assert!(!log.contains('\x1b'), "{log}");

    let json = invoke(&["build", "--json"]);
    let result = String::from_utf8_lossy(&json.stdout);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    assert_eq!(result.lines().count(), 1, "{result}");
    assert!(
        result.starts_with(r#"{"success":true,"output":"#),
        "{result}"
    );
    assert!(
        json.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );

    fs::write(project.join("main.nt"), "fun main() { missing(); }").unwrap();
    let failed = invoke(&["build"]);
    let log = String::from_utf8_lossy(&failed.stderr);
    assert!(!failed.status.success() && log.contains("missing"), "{log}");
    assert!(
        !log.contains("Finished") && failed.stdout.is_empty(),
        "{log}"
    );
    fs::remove_dir_all(project).unwrap();
}
