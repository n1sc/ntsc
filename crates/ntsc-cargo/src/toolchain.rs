use std::io;
use std::process::{Command, ExitStatus, Output};

#[derive(Debug)]
pub struct RustToolchainError {
    program: &'static str,
    failure: ToolchainFailure,
}

#[derive(Debug)]
enum ToolchainFailure {
    Launch(io::Error),
    Exit { status: ExitStatus, stderr: String },
}

impl std::fmt::Display for RustToolchainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            formatter,
            "Rust modules require a working Rust toolchain (`rustc` and `cargo`)."
        )?;
        match &self.failure {
            ToolchainFailure::Launch(error) => {
                writeln!(
                    formatter,
                    "Could not run `{} --version`: {error}",
                    self.program
                )?;
            }
            ToolchainFailure::Exit { status, stderr } => {
                writeln!(formatter, "`{} --version` failed ({status}).", self.program)?;
                if !stderr.is_empty() {
                    writeln!(formatter, "{stderr}")?;
                }
            }
        }
        write!(
            formatter,
            "\nInstall stable Rust from https://rustup.rs, then reopen your terminal and retry.\n\
             If Rust is already installed, run `rustup default stable` and make sure \
             `rustc` and `cargo` are on PATH.\n\
             Ordinary Neutron programs using the bundled standard library do not need Rust."
        )
    }
}

impl std::error::Error for RustToolchainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.failure {
            ToolchainFailure::Launch(error) => Some(error),
            ToolchainFailure::Exit { .. } => None,
        }
    }
}

pub fn check_rust_toolchain() -> Result<(), RustToolchainError> {
    for program in ["rustc", "cargo"] {
        check_program(program)?;
    }
    Ok(())
}

fn check_program(program: &'static str) -> Result<(), RustToolchainError> {
    let output = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|error| RustToolchainError {
            program,
            failure: ToolchainFailure::Launch(error),
        })?;
    validate_version_output(program, output)
}

fn validate_version_output(
    program: &'static str,
    output: Output,
) -> Result<(), RustToolchainError> {
    if output.status.success() {
        return Ok(());
    }
    Err(RustToolchainError {
        program,
        failure: ToolchainFailure::Exit {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;

    #[test]
    fn missing_program_explains_rust_setup() {
        let error = check_program("ntsc-missing-rust-toolchain-fixture").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Rust modules require"));
        assert!(message.contains("https://rustup.rs"));
        assert!(message.contains("rustup default stable"));
        assert!(message.contains("standard library do not need Rust"));
        assert!(error.source().is_some());
    }

    #[test]
    fn unusable_rustup_shim_preserves_its_error() {
        let error = validate_version_output(
            "cargo",
            Output {
                status: ExitStatus::from_raw(1),
                stdout: Vec::new(),
                stderr: b"rustup: no default toolchain configured\n".to_vec(),
            },
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("cargo --version"));
        assert!(message.contains("no default toolchain configured"));
        assert!(message.contains("rustup default stable"));
        assert!(error.source().is_none());
    }

    #[test]
    fn working_version_commands_are_accepted() {
        for program in ["rustc", "cargo"] {
            validate_version_output(
                program,
                Output {
                    status: ExitStatus::from_raw(0),
                    stdout: b"version output\n".to_vec(),
                    stderr: Vec::new(),
                },
            )
            .unwrap();
        }
    }
}
