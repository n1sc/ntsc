use std::path::Path;

#[test]
fn inline_async_block_e2e() {
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

    let source = r#"async fun compute() -> int {
    await async.sleep(10);
    return 42
}

async fun main() -> int {
    await async {
        await async.sleep(5)
    }
    say("block done")
    var int x = await compute();
    if (x == 42) {
        say("correct value")
    } else {
        say("wrong value")
    }
    return 0
}
"#;

    let dir = std::env::temp_dir().join("ntsc_inline_async_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!("inline_async.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("inline_async_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "inline_async", &dir)
        .expect("compile_source failed");

    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "binary exited with non-zero status: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let expected_lines = ["block done", "correct value"];
    for line in expected_lines {
        assert!(
            stdout.lines().any(|l| l == line),
            "missing expected output {line:?}:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn for_await_e2e() {
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

    let source = r#"fun main() {
    var items = ["a", "b", "c"]
    for await x in items {
        say(x)
    }
    say("for await done")
}
"#;

    let dir = std::env::temp_dir().join("ntsc_for_await_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!("for_await.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("for_await_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "for_await", &dir)
        .expect("compile_source failed");

    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "binary exited with non-zero status: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    for expected in ["a", "b", "c", "for await done"] {
        assert!(
            stdout.lines().any(|l| l == expected),
            "missing expected output {expected:?}:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wait_any_e2e() {
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

    let source = r#"async fun main() -> int {
    var int v = wait_any(async -> int { return 1 }, async -> int { return 2 })
    say("" + v)
    return 0
}
"#;

    let dir = std::env::temp_dir().join("ntsc_wait_any_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!("wait_any.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("wait_any_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "wait_any", &dir)
        .expect("compile_source failed");

    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "binary exited with non-zero status: {:?}\nstdout: {:?}\nstderr: {:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let expected = "1";
    assert!(
        stdout.lines().any(|l| l == expected),
        "missing expected output {expected:?}:\n{stdout}\nstderr:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wait_all_e2e() {
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

    let source = r#"async fun main() -> int {
    var int v = wait_all(async -> int { return 10 }, async -> int { return 20 })
    say("" + v)
    return 0
}
"#;

    let dir = std::env::temp_dir().join("ntsc_wait_all_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!("wait_all.{}", ntsc_codegen::object_extension()));
    let bin_path = dir.join("wait_all_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "wait_all", &dir)
        .expect("compile_source failed");

    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "binary exited with non-zero status: {:?}\nstdout: {:?}\nstderr: {:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let expected = "20";
    assert!(
        stdout.lines().any(|l| l == expected),
        "missing expected output {expected:?}:\n{stdout}\nstderr:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timeout_catchable_error_e2e() {
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

    let source = "async fun main() -> int {\n    var int status = 0\n    try {\n        var int v = wait_any(async -> int { await async.sleep(10000); return 1 }, async -> int { await async.sleep(10); throw \"timeout\" })\n        say(\"\" + v)\n        status = 1\n    } catch (e) {\n        say(\"caught: \" + e)\n        status = 0\n    }\n    return status\n}\n";

    let dir = std::env::temp_dir().join("ntsc_timeout_catchable_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();

    let obj_path = dir.join(format!(
        "timeout_catch.{}",
        ntsc_codegen::object_extension()
    ));
    let bin_path = dir.join("timeout_catch_ntsc_test");

    ntsc_codegen::compile_source(source, ntsc_codegen::host_triple(), "timeout_catch", &dir)
        .expect("compile_source failed");

    assert!(obj_path.exists(), "object file not produced");

    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    assert!(bin_path.exists(), "binary not produced");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "binary exited with non-zero status: {:?}\nstdout: {:?}\nstderr: {:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let expected = "caught: timeout";
    assert!(
        stdout.lines().any(|l| l == expected),
        "missing expected output {expected:?}:\n{stdout}\nstderr:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn await_in_control_flow_e2e() {
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

    let source = r#"async fun value() -> int {
    return 7
}

async fun fail() {
    await async.sleep(1)
    throw "boom"
}

async fun main() -> int {
    var int x = 0
    var int status = 0
    if (true) {
        x = await value()
    }
    if (x != 7) {
        status = 1
    }
    var int i = 0
    while (i < 2) {
        var int local = await value()
        i = i + 1
    }
    if (i != 2) {
        status = 1
    }
    try {
        await fail()
    } catch (e) {
        var int ignored = await value()
        say("caught: " + e)
    }
    if (status == 0) {
        return status
    }
    return 1
}
"#;
    let dir = std::env::temp_dir().join("ntsc_await_control_flow_e2e_test");
    std::fs::create_dir_all(&dir).unwrap();
    let obj_path = dir.join(format!(
        "await_control_flow.{}",
        ntsc_codegen::object_extension()
    ));
    let bin_path = dir.join("await_control_flow_ntsc_test");

    ntsc_codegen::compile_source(
        source,
        ntsc_codegen::host_triple(),
        "await_control_flow",
        &dir,
    )
    .expect("compile_source failed");
    ntsc_codegen::link_binary(&obj_path, &runtime_lib, &bin_path).expect("link_binary failed");

    let output = std::process::Command::new(&bin_path)
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "binary exited with non-zero status: {:?}\nstderr: {:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "caught: boom\n");

    let _ = std::fs::remove_dir_all(&dir);
}
