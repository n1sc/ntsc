//! Parser for `neutron.toml` — the Neutron project configuration file.
//!
//! Format is TOML with a `[package]` section:
//! ```toml
//! [package]
//! entry = "src/main.nt"
//! output = "my-project"
//! ```

use std::fmt;

pub mod aliases;
pub mod modules;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildConfig {
    /// Target triple for the compiled binary (e.g. `"x86_64-unknown-linux-gnu"`).
    pub target: String,

    /// Path to the entry source file (e.g. `"src/main.nt"`).
    pub entry: String,

    /// Name of the output binary.
    pub output: String,

    /// Native libraries required by `extern "C"` declarations, without the
    /// platform-specific linker prefix (for example `m` becomes `-lm`).
    pub ffi_libraries: Vec<String>,

    /// Project-relative or absolute directories searched for FFI libraries.
    /// Relative paths are resolved from the project directory by the CLI.
    pub ffi_search_paths: Vec<String>,

    /// Whether the experimental Rust-crate import feature (`use @crate`)
    /// is enabled. Defaults to `false`; a `[experimental]` table with
    /// `rust_modules = true` opts the project in.
    pub experimental_rust_modules: bool,
}

/// A single error encountered while parsing `neutron.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    pub message: String,
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "neutron.toml:{}:{}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for BuildError {}

/// Parse a `neutron.toml` source string into a [`BuildConfig`].
///
/// Returns all errors found during parsing (not just the first).
pub fn parse(source: &str) -> Result<BuildConfig, Vec<BuildError>> {
    let mut errors = Vec::new();

    let table: toml::Value = match source.parse() {
        Ok(v) => v,
        Err(e) => {
            errors.push(BuildError {
                message: format!("TOML syntax error: {e}"),
                line: 0,
                column: 0,
            });
            return Err(errors);
        }
    };

    let package = match table.get("package") {
        Some(toml::Value::Table(t)) => t,
        _ => {
            errors.push(BuildError {
                message: "missing `[package]` section".to_string(),
                line: 0,
                column: 0,
            });
            return Err(errors);
        }
    };

    let target = package
        .get("target")
        .map(|_| extract_string(package, "target", &mut errors))
        .unwrap_or_else(|| Some(host_triple().to_string()));
    let entry = extract_string(package, "entry", &mut errors);
    let output = extract_string(package, "output", &mut errors);
    let ffi_libraries = extract_string_array(table.get("ffi"), "libraries", &mut errors);
    let ffi_search_paths = extract_string_array(table.get("ffi"), "search_paths", &mut errors);
    let experimental_rust_modules =
        extract_bool(table.get("experimental"), "rust_modules", &mut errors);

    if errors.is_empty() {
        Ok(BuildConfig {
            target: target.expect("checked above"),
            entry: entry.expect("checked above"),
            output: output.expect("checked above"),
            ffi_libraries,
            ffi_search_paths,
            experimental_rust_modules,
        })
    } else {
        Err(errors)
    }
}

/// LLVM target triple for the host platform used when a project omits
/// `[package].target`.
pub fn host_triple() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "aarch64-pc-windows-msvc"
    } else {
        "unknown-unknown-unknown"
    }
}

fn extract_string(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
    errors: &mut Vec<BuildError>,
) -> Option<String> {
    match table.get(key) {
        Some(toml::Value::String(s)) => Some(s.clone()),
        Some(_) => {
            errors.push(BuildError {
                message: format!("`{key}` must be a string"),
                line: 0,
                column: 0,
            });
            None
        }
        None => {
            errors.push(BuildError {
                message: format!("missing required key `{key}` in [package]"),
                line: 0,
                column: 0,
            });
            None
        }
    }
}

fn extract_string_array(
    table: Option<&toml::Value>,
    key: &str,
    errors: &mut Vec<BuildError>,
) -> Vec<String> {
    let Some(table) = table else {
        return Vec::new();
    };
    let Some(value) = table.get(key) else {
        return Vec::new();
    };
    let Some(values) = value.as_array() else {
        errors.push(BuildError {
            message: format!("`ffi.{key}` must be an array of strings"),
            line: 0,
            column: 0,
        });
        return Vec::new();
    };
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        if let Some(value) = value.as_str() {
            result.push(value.to_string());
        } else {
            errors.push(BuildError {
                message: format!("every `ffi.{key}` entry must be a string"),
                line: 0,
                column: 0,
            });
        }
    }
    result
}

fn extract_bool(table: Option<&toml::Value>, key: &str, errors: &mut Vec<BuildError>) -> bool {
    let Some(table) = table else {
        return false;
    };
    match table.get(key) {
        Some(toml::Value::Boolean(value)) => *value,
        Some(_) => {
            errors.push(BuildError {
                message: format!("`experimental.{key}` must be a boolean"),
                line: 0,
                column: 0,
            });
            false
        }
        None => false,
    }
}

/// Whether the program imports Rust crates through the experimental
/// `use @crate` form (whole-module or per-symbol selective imports).
pub fn uses_rust_crates(program: &ntsc_ast::stmt::Program) -> bool {
    program.statements.iter().any(|stmt| match stmt {
        ntsc_ast::stmt::Stmt::Use {
            is_rust,
            imported_is_rust,
            ..
        } => *is_rust || imported_is_rust.iter().any(|is_rust| *is_rust),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_config() {
        let src = r#"
[package]
target = "x86_64-unknown-linux-gnu"
entry = "src/main.nt"
output = "my-project"
[ffi]
search_paths = ["native"]
libraries = ["m"]
"#;
        let config = parse(src).expect("should parse successfully");
        assert_eq!(config.target, "x86_64-unknown-linux-gnu");
        assert_eq!(config.entry, "src/main.nt");
        assert_eq!(config.output, "my-project");
        assert_eq!(config.ffi_libraries, vec!["m"]);
        assert_eq!(config.ffi_search_paths, vec!["native"]);
    }

    #[test]
    fn valid_with_comments() {
        let src = r#"
# Build configuration
[package]
target = "aarch64-apple-darwin"
entry = "src/main.nt"
output = "cool-app"
"#;
        let config = parse(src).expect("should parse successfully");
        assert_eq!(config.target, "aarch64-apple-darwin");
        assert_eq!(config.entry, "src/main.nt");
        assert_eq!(config.output, "cool-app");
    }

    #[test]
    fn missing_package_section() {
        let src = "target = \"x86_64-unknown-linux-gnu\"\n";
        let errs = parse(src).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing `[package]`"))
        );
    }

    #[test]
    fn omitted_target_defaults_to_host() {
        let src = "[package]\nentry = \"src/main.nt\"\noutput = \"my-project\"\n";
        let config = parse(src).expect("target should default to host");
        assert_eq!(config.target, host_triple());
    }

    #[test]
    fn missing_entry() {
        let src = "[package]\ntarget = \"x86_64-unknown-linux-gnu\"\noutput = \"my-project\"\n";
        let errs = parse(src).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing required key `entry`"))
        );
    }

    #[test]
    fn missing_output() {
        let src = "[package]\ntarget = \"x86_64-unknown-linux-gnu\"\nentry = \"src/main.nt\"\n";
        let errs = parse(src).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing required key `output`"))
        );
    }

    #[test]
    fn all_missing() {
        let src = "[package]\n";
        let errs = parse(src).unwrap_err();
        assert_eq!(errs.len(), 2);
    }

    #[test]
    fn invalid_toml() {
        let src = "not valid toml {{{";
        let errs = parse(src).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("TOML syntax error")));
    }

    #[test]
    fn wrong_type() {
        let src = "[package]\ntarget = 123\nentry = \"src/main.nt\"\noutput = \"p\"\n";
        let errs = parse(src).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("must be a string")));
    }

    #[test]
    fn experimental_rust_modules_defaults_to_false() {
        let src = "[package]\nentry = \"src/main.nt\"\noutput = \"p\"\n";
        let config = parse(src).expect("config should parse");
        assert!(!config.experimental_rust_modules);
    }

    #[test]
    fn experimental_rust_modules_opt_in() {
        let src = r#"
[package]
entry = "src/main.nt"
output = "p"
[experimental]
rust_modules = true
"#;
        let config = parse(src).expect("config should parse");
        assert!(config.experimental_rust_modules);
    }

    #[test]
    fn experimental_rust_modules_wrong_type() {
        let src = r#"
[package]
entry = "src/main.nt"
output = "p"
[experimental]
rust_modules = "yes"
"#;
        let errs = parse(src).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message == "`experimental.rust_modules` must be a boolean")
        );
    }

    #[test]
    fn uses_rust_crates_detects_at_crate_imports() {
        let tokenize_and_parse = |source: &str| {
            let tokens = ntsc_lexer::tokenize(source);
            ntsc_parser::parse(&tokens).expect("source should parse")
        };
        let rust_program = tokenize_and_parse("use @serde_json\nfun main() {}\n");
        assert!(uses_rust_crates(&rust_program));

        let selective_program =
            tokenize_and_parse("use (@json) = from @serde_json\nfun main() {}\n");
        assert!(uses_rust_crates(&selective_program));

        let plain_program = tokenize_and_parse("use sys\nfun main() { sys.print(1) }\n");
        assert!(!uses_rust_crates(&plain_program));
    }
}
