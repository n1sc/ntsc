mod docs;
mod ui;

mod docs_data {
    include!(concat!(env!("OUT_DIR"), "/docs_data.rs"));
}

use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use inkwell::OptimizationLevel;
use ntsc_ast::stmt::{Program, Stmt};
use ntsc_codegen::SUMMARY_MARKER;
use ntsc_diag::{DiagConfig, Diagnostic, SourceBuffer, SourceMap, Writer, diagnostics_to_json};

// The compiler's version is the crate version, so --version can never
// drift from Cargo.toml.
pub const NTSC_VERSION: &str = env!("CARGO_PKG_VERSION");

// ANSI escape codes for the CLI's own output. Follows the same colour
// conventions as ntsc-diag: CLICOLOR_FORCE, NO_COLOR, CLICOLOR=0, TERM=dumb.
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[1;36m";
const GREEN: &str = "\x1b[1;32m";
const RESET: &str = "\x1b[0m";

/// Whether ANSI colours should be emitted for a given stream.
fn colour_on(stream_is_tty: bool) -> bool {
    let force = env::var("CLICOLOR_FORCE")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    let no_color = env::var("NO_COLOR").map(|v| !v.is_empty()).unwrap_or(false);
    let clicolor_off = env::var("CLICOLOR").map(|v| v == "0").unwrap_or(false);
    let dumb_term = env::var("TERM").map(|v| v == "dumb").unwrap_or(false);
    force || (!no_color && !clicolor_off && !dumb_term && stream_is_tty)
}

/// Wrap text in an ANSI escape code when colour is enabled.
fn paint(text: &str, code: &str, colour: bool) -> String {
    if colour {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// One row of the command reference. Shared by --help, ntsc help, and the
/// interactive landing screen so the three can never drift apart.
pub(crate) struct CommandDoc {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
    pub details: &'static [&'static str],
    pub options: &'static [(&'static str, &'static str)],
}

pub(crate) const COMMANDS: &[CommandDoc] = &[
    CommandDoc {
        name: "init",
        usage: "ntsc init [name]",
        summary: "Scaffold a new project",
        details: &[
            "Creates neutron.toml and src/main.nt.",
            "With no name, scaffolds in the current directory.",
            "Refuses to overwrite an existing project.",
        ],
        options: &[],
    },
    CommandDoc {
        name: "build",
        usage: "ntsc build [--release] [--json]",
        summary: "Compile the project",
        details: &[
            "Reads neutron.toml, loads the module closure, compiles, and links.",
            "Writes to build/debug/ or build/release/.",
        ],
        options: &[
            ("--release", "Optimize with LLVM"),
            ("--json", "Emit the build result and diagnostics as JSON"),
        ],
    },
    CommandDoc {
        name: "test",
        usage: "ntsc test [--release] [--json]",
        summary: "Run every test block",
        details: &[
            "Builds with the test harness in place of main.",
            "Exits non-zero if any test fails.",
        ],
        options: &[
            ("--release", "Optimize with LLVM"),
            ("--json", "Emit the test summary and diagnostics as JSON"),
        ],
    },
    CommandDoc {
        name: "run",
        usage: "ntsc run [--release] [--json]",
        summary: "Build and execute",
        details: &["Builds the project, then runs the resulting binary."],
        options: &[
            ("--release", "Optimize with LLVM"),
            ("--json", "Emit diagnostics as JSON"),
        ],
    },
    CommandDoc {
        name: "clean",
        usage: "ntsc clean [--cache] [--tmp] [--dry-run] [--max-age <days>]",
        summary: "Remove build output or caches",
        details: &[
            "With no flags, removes build/.",
            "--cache prunes global Rust adaptor caches.",
            "--tmp removes stale /tmp/ntsc_* scratch trees.",
        ],
        options: &[
            ("--cache", "Prune global adaptor caches"),
            ("--tmp", "Remove stale compiler scratch trees"),
            ("--dry-run", "Show what would be removed"),
            ("--max-age <days>", "Only prune entries older than <days>"),
            ("--force-rebuild", "Drop caches even when they are fresh"),
        ],
    },
    CommandDoc {
        name: "watch",
        usage: "ntsc watch [--release] [--json]",
        summary: "Rebuild when sources change",
        details: &["Polls the module closure and neutron.toml every 400 ms."],
        options: &[
            ("--release", "Optimize with LLVM"),
            ("--json", "Emit diagnostics as JSON"),
        ],
    },
    CommandDoc {
        name: "graph",
        usage: "ntsc graph",
        summary: "Print the module graph as DOT",
        details: &["Writes a Graphviz digraph of file imports to stdout."],
        options: &[],
    },
    CommandDoc {
        name: "pkg",
        usage: "ntsc pkg <command> [args]",
        summary: "Run the package manager",
        details: &[
            "Forwards arguments to ntsc-pkg.",
            "Commands: install, remove, update, list, search, help.",
        ],
        options: &[],
    },
    CommandDoc {
        name: "cargo",
        usage: "ntsc cargo <command> [args]",
        summary: "Manage Rust crate dependencies",
        details: &["Commands: install, remove, list, update, features, doc, coverage, gc."],
        options: &[],
    },
    CommandDoc {
        name: "lsp",
        usage: "ntsc lsp",
        summary: "Start the language server",
        details: &["Speaks LSP over stdio for editor integration."],
        options: &[],
    },
    CommandDoc {
        name: "docs",
        usage: "ntsc docs [page]",
        summary: "Browse the documentation",
        details: &[
            "Reads docs/ from the checkout when available, otherwise the embedded copy.",
            "With a page name or path, opens that page.",
            "When stdout is not a terminal, prints the index or the page body.",
        ],
        options: &[],
    },
    CommandDoc {
        name: "version",
        usage: "ntsc version | --version | -V",
        summary: "Print detailed version information",
        details: &["Shows the version, LLVM revision, host triple, build profile, and git commit."],
        options: &[],
    },
    CommandDoc {
        name: "help",
        usage: "ntsc help [command]",
        summary: "Show help for a command",
        details: &["With no command, prints the full command reference."],
        options: &[],
    },
];

pub(crate) const GLOBAL_FLAGS: &[(&str, &str)] = &[
    ("--release", "Build with LLVM aggressive optimization"),
    ("--debug", "Build with no optimization (default)"),
    ("--json", "Emit diagnostics and results as JSON lines"),
    ("--help, -h", "Print help, optionally for one command"),
    ("--version, -V", "Print detailed version info and exit"),
];

const OPTION_NAMES: &[&str] = &[
    "--cache",
    "--debug",
    "--dry-run",
    "--force-rebuild",
    "--help",
    "--json",
    "--max-age",
    "--release",
    "--short",
    "--tmp",
    "--version",
];

fn main() {
    #[cfg(unix)]
    reset_sigpipe();

    if env::args_os().len() == 1 {
        if ui::interactive_available() && ui::run_landing().is_ok() {
            return;
        }
        let mut out = io::stderr().lock();
        let _ = print_usage(&mut out, colour_on(io::stderr().is_terminal()));
        std::process::exit(1);
    }

    let (command, opts) = match parse_args() {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("error: {message}");
            let mut out = io::stderr().lock();
            let _ = print_usage(&mut out, colour_on(io::stderr().is_terminal()));
            std::process::exit(1);
        }
    };

    let result = match command {
        Command::Help(target) => print_help(target.as_deref()),
        Command::Docs(page) => cmd_docs(page.as_deref()),
        Command::Version => {
            print_version();
            Ok(())
        }
        Command::Init(name) => cmd_init(name.as_deref()).map(|_| ()),
        Command::Build => cmd_build(opts.release, false, opts.json).map(|_| ()),
        Command::Test => cmd_test(opts.release, opts.json).map(|_| ()),
        Command::Run => cmd_run(opts.release, opts.json).map(|_| ()),
        Command::Clean(args) => cmd_clean(&args),
        Command::Watch => cmd_watch(opts.release, opts.json),
        Command::Graph => cmd_graph().map(|_| ()),
        Command::Pkg(args) => cmd_pkg(&args),
        Command::Cargo(args) => cmd_cargo(&args),
        Command::Lsp => {
            std::process::exit(ntsc_lsp::run_stdio());
        }
    };

    if let Err(e) = result {
        report(e, opts.json);
        std::process::exit(1);
    }
}

/// Rust ignores SIGPIPE by default, which turns a piped ntsc run into a
/// panic on the first failed write. Restore the conventional behaviour.
#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: signal is async-signal-safe and SIG_DFL is a valid handler.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Command-line options shared by every subcommand.
#[derive(Clone, Copy, Default)]
struct CliOptions {
    release: bool,
    json: bool,
}

/// An error that failed at the CLI level. Either a batch of renderable
/// diagnostics (with their source files) or a plain message for
/// infrastructure failures that have no source location.
enum CliError {
    Diagnostics {
        diags: Vec<Diagnostic>,
        sources: SourceMap,
    },
    Plain(String),
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self::Plain(message)
    }
}

impl From<Box<dyn std::error::Error>> for CliError {
    fn from(error: Box<dyn std::error::Error>) -> Self {
        Self::Plain(error.to_string())
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::Plain(error.to_string())
    }
}

impl From<ntsc_codegen::CodegenError> for CliError {
    fn from(error: ntsc_codegen::CodegenError) -> Self {
        Self::Diagnostics {
            diags: error.into_diagnostics(),
            sources: SourceMap::new(),
        }
    }
}

/// Print a CLI error: diagnostics through the writer, plain messages as a
/// styled panel (or plain text when the terminal cannot render it).
///
/// With --json, emit a machine-readable JSON document on stdout instead.
fn report(error: CliError, json: bool) {
    match error {
        CliError::Diagnostics { diags, sources } => {
            if json {
                println!("{}", diagnostics_to_json(&diags));
            } else {
                let writer = Writer::new(DiagConfig::from_env());
                writer.emit_all(&diags, Some(&sources));
            }
        }
        CliError::Plain(msg) => {
            if json {
                println!("{}", diagnostics_to_json(&[Diagnostic::error(msg)]));
            } else {
                ui::report_plain_error(&msg);
            }
        }
    }
}

enum Command {
    Help(Option<String>),
    Docs(Option<String>),
    Version,
    Init(Option<String>),
    Build,
    Test,
    Run,
    Clean(Vec<String>),
    Watch,
    Graph,
    Pkg(Vec<String>),
    Cargo(Vec<String>),
    Lsp,
}

/// Parse the command line, accepting global flags (--release, --debug,
/// --json, --help, --version) before or after the subcommand.
fn parse_args() -> Result<(Command, CliOptions), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut opts = CliOptions::default();
    let mut command: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut forwarded: Vec<String> = Vec::new();

    for arg in args {
        if let Some(name) = command.as_deref() {
            if matches!(name, "pkg" | "cargo") {
                forwarded.push(arg);
                continue;
            }
            if name == "clean" {
                if matches!(arg.as_str(), "--help" | "-h") {
                    return Ok((Command::Help(Some(name.to_string())), opts));
                }
                forwarded.push(arg);
                continue;
            }
        }

        match arg.as_str() {
            "--release" => opts.release = true,
            "--debug" => opts.release = false,
            "--json" => opts.json = true,
            "--help" | "-h" => return Ok((Command::Help(command.clone()), opts)),
            "--version" | "-V" => return Ok((Command::Version, opts)),
            _ if arg.starts_with('-') => return Err(unknown_option(&arg)),
            _ => match command.as_deref() {
                None => {
                    if is_command(&arg) || arg == "help" {
                        command = Some(arg);
                    } else {
                        return Err(unknown_command(&arg));
                    }
                }
                Some("init") | Some("docs") if positional.is_empty() => positional.push(arg),
                Some("help") if positional.len() < 2 => positional.push(arg),
                Some(_) => return Err(format!("unexpected argument '{arg}'")),
            },
        }
    }

    let name = command.ok_or_else(|| "missing command".to_string())?;
    let command = match name.as_str() {
        "help" => {
            if positional.len() > 1 {
                return Err("help takes at most one command name".to_string());
            }
            Command::Help(positional.into_iter().next())
        }
        "version" => {
            if !positional.is_empty() {
                return Err("version takes no arguments".to_string());
            }
            Command::Version
        }
        "init" => Command::Init(positional.into_iter().next()),
        "docs" => Command::Docs(positional.into_iter().next()),
        "build" => Command::Build,
        "test" => Command::Test,
        "run" => Command::Run,
        "clean" => Command::Clean(forwarded),
        "watch" => Command::Watch,
        "graph" => Command::Graph,
        "pkg" => Command::Pkg(forwarded),
        "cargo" => Command::Cargo(forwarded),
        "lsp" => Command::Lsp,
        _ => return Err(unknown_command(&name)),
    };
    Ok((command, opts))
}

fn is_command(name: &str) -> bool {
    matches!(
        name,
        "init"
            | "build"
            | "test"
            | "run"
            | "clean"
            | "watch"
            | "graph"
            | "pkg"
            | "cargo"
            | "lsp"
            | "docs"
            | "version"
    )
}

fn unknown_command(name: &str) -> String {
    match closest(name, COMMANDS.iter().map(|doc| doc.name)) {
        Some(suggestion) => format!("unknown command '{name}'\ndid you mean 'ntsc {suggestion}'?"),
        None => format!("unknown command '{name}'"),
    }
}

fn unknown_option(option: &str) -> String {
    match closest(option, OPTION_NAMES.iter().copied()) {
        Some(suggestion) => format!("unknown option '{option}'\ndid you mean '{suggestion}'?"),
        None => format!("unknown option '{option}'"),
    }
}

/// Pick the candidate within a small edit distance of the input.
fn closest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = if input.chars().count() <= 4 { 1 } else { 2 };
    candidates
        .into_iter()
        .map(|candidate| (levenshtein(input, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

fn levenshtein(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, left_char) in left.chars().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != *right_char);
            current[column + 1] = (previous[column + 1] + 1)
                .min(current[column] + 1)
                .min(previous[column] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn print_help(target: Option<&str>) -> Result<(), CliError> {
    let mut out = io::stdout().lock();
    let colour = colour_on(io::stdout().is_terminal());
    match target {
        Some(name) => {
            if !print_command_help(&mut out, name, colour)? {
                return Err(CliError::Plain(unknown_command(name)));
            }
        }
        None => print_usage(&mut out, colour)?,
    }
    Ok(())
}

fn print_usage(out: &mut dyn Write, colour: bool) -> io::Result<()> {
    let bold = |text: &str| paint(text, BOLD, colour);
    let cell = |name: &str| paint(&format!("{name:<20}"), CYAN, colour);

    writeln!(
        out,
        "ntsc — Neutron Type-Safe Compiler {}",
        bold(NTSC_VERSION)
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "A statically typed, memory-safe systems language compiled to native"
    )?;
    writeln!(out, "binaries through LLVM.")?;
    writeln!(out)?;
    writeln!(out, "Usage: ntsc [options] <command> [args]")?;
    writeln!(out)?;
    writeln!(out, "{}", bold("Commands:"))?;
    for doc in COMMANDS {
        let name = if doc.name == "init" {
            "init [name]"
        } else {
            doc.name
        };
        writeln!(out, "  {}{}", cell(name), doc.summary)?;
    }
    writeln!(out)?;
    writeln!(out, "{}", bold("Options:"))?;
    for (flag, description) in GLOBAL_FLAGS {
        writeln!(out, "  {}{}", cell(flag), description)?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "Run ntsc help <command> for details, or just ntsc for the interactive screen."
    )?;
    Ok(())
}

fn print_command_help(out: &mut dyn Write, name: &str, colour: bool) -> io::Result<bool> {
    let Some(doc) = COMMANDS.iter().find(|doc| doc.name == name) else {
        return Ok(false);
    };
    let bold = |text: &str| paint(text, BOLD, colour);

    writeln!(out, "{}", bold(doc.summary))?;
    writeln!(out)?;
    writeln!(out, "Usage: {}", paint(doc.usage, CYAN, colour))?;
    if !doc.details.is_empty() {
        writeln!(out)?;
        for detail in doc.details {
            writeln!(out, "  {detail}")?;
        }
    }
    if !doc.options.is_empty() {
        writeln!(out)?;
        writeln!(out, "{}", bold("Options:"))?;
        for (flag, description) in doc.options {
            writeln!(
                out,
                "  {}{}",
                paint(&format!("{flag:<22}"), CYAN, colour),
                description
            )?;
        }
    }
    Ok(true)
}

/// Print a detailed version banner: version, LLVM revision, host triple,
/// build profile, and (when inside a checkout) the git commit.
fn print_version() {
    let colour = colour_on(std::io::stdout().is_terminal());
    println!(
        "ntsc {} — Neutron Type-Safe Compiler",
        paint(NTSC_VERSION, BOLD, colour)
    );
    println!();
    println!("LLVM   {}", ntsc_codegen::llvm_version());
    println!("Host   {}", ntsc_codegen::host_triple());
    println!("Build ID   {}", env!("NTSC_BUILD_ID"));
    println!("Build date {}", env!("NTSC_BUILD_DATE"));
    println!(
        "Build  {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    if let Some(commit) = git_commit() {
        println!("Commit {commit}");
    }
}

/// The current commit hash of the workspace checkout, when the running
/// binary can be traced back to one. Best effort: `None` for released
/// binaries.
fn git_commit() -> Option<String> {
    let dir = find_rewrite_dir_from_exe()?;
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?;
    let hash = hash.trim();
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum BuildMode {
    Debug,
    Release,
}

impl BuildMode {
    fn new(release: bool) -> Self {
        if release { Self::Release } else { Self::Debug }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    fn opt_level(self) -> OptimizationLevel {
        match self {
            Self::Debug => OptimizationLevel::None,
            Self::Release => OptimizationLevel::Aggressive,
        }
    }
}

/// Open the built-in documentation browser, or print docs when stdout is
/// not a terminal.
fn cmd_docs(page: Option<&str>) -> Result<(), CliError> {
    let pages = docs::load_docs();
    if pages.is_empty() {
        return Err(CliError::Plain(
            "no documentation pages were found in this build".into(),
        ));
    }

    let index = match page {
        Some(query) => match ui::docs::find_doc(&pages, query) {
            Some(index) => Some(index),
            None => {
                return Err(CliError::Plain(format!(
                    "no documentation page matches '{query}'"
                )));
            }
        },
        None => None,
    };

    if ui::interactive_available() {
        return ui::docs::run_docs(pages, index).map_err(CliError::from);
    }

    let mut out = io::stdout().lock();
    match index {
        Some(index) => out.write_all(pages[index].body.as_bytes())?,
        None => {
            for doc in &pages {
                writeln!(out, "{:<44} {}", doc.title, doc.path)?;
            }
        }
    }
    Ok(())
}

/// Scaffold a new project with exactly two files: neutron.toml and
/// src/main.nt.
///
/// With no name the project is scaffolded in the current directory; with a
/// name it is created in ./<name>. Existing projects are never overwritten.
fn cmd_init(project_name: Option<&str>) -> Result<(), CliError> {
    let cwd = env::current_dir()?;
    let project_dir = match project_name {
        Some(name) => {
            let target = Path::new(name);
            if name.is_empty() || target == Path::new(".") {
                cwd.clone()
            } else {
                cwd.join(target)
            }
        }
        None => cwd.clone(),
    };

    let name = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("my-project")
        .to_string();

    let manifest_path = project_dir.join("neutron.toml");
    let main_path = project_dir.join("src").join("main.nt");
    if manifest_path.exists() || main_path.exists() {
        return Err(CliError::Plain(format!(
            "refusing to overwrite the existing project in {}",
            project_dir.display()
        )));
    }

    fs::create_dir_all(project_dir.join("src"))?;

    let neutron_toml = format!(
        "[package]\n\
         entry = \"src/main.nt\"\n\
         output = \"{}\"\n",
        ntsc_codegen::with_executable_extension(&name),
    );
    fs::write(&manifest_path, &neutron_toml)?;

    let main_nt = "fun main() {\n    say(\"Hello, World!\")\n}\n";
    fs::write(&main_path, main_nt)?;

    println!("Created project '{name}' in {}", project_dir.display());
    println!("  {}", display_rel(&cwd, &manifest_path));
    println!("  {}", display_rel(&cwd, &main_path));

    Ok(())
}

/// Build the current project: read `neutron.toml`, load modules, compile,
/// link.
///
/// In test mode the user `main` is replaced by the test harness.
fn cmd_build(release: bool, test_mode: bool, json: bool) -> Result<PathBuf, CliError> {
    let (cwd, config) = load_project()?;
    let mode = BuildMode::new(release);
    let entry_path = cwd.join(&config.entry);

    // Resolve and parse the module closure (in parallel).
    let load_start = Instant::now();
    let loaded = ntsc_build::modules::load_program(&entry_path).map_err(load_error)?;
    let load_time = load_start.elapsed();

    // Experimental Rust-crate imports are opt-in: without the toggle the
    // build stops before resolution, adaptor generation, or codegen run.
    if !config.experimental_rust_modules && ntsc_build::uses_rust_crates(&loaded.program) {
        return Err(CliError::Plain(
            "Rust crate imports (`use @crate`) are an experimental feature and are disabled \
             by default. Opt in with:\n\n\
             [experimental]\n\
             rust_modules = true"
                .to_string(),
        ));
    }

    // Run name resolution up front so undefined-name errors get the
    // `NTSC-E0101` code (and any "did you mean" suggestions).
    if let Err(errors) = ntsc_typeck::resolve_program(&loaded.program) {
        return Err(resolve_error(errors, &loaded));
    }

    // Lint warnings are non-fatal: report them, then continue the build.
    let warnings = ntsc_typeck::lint_program(&loaded.program);
    if !warnings.is_empty() {
        emit_warnings(warnings, &loaded, json);
    }

    // Create the output directory.
    let out_dir = cwd.join("build").join(mode.dir_name());
    fs::create_dir_all(&out_dir)?;

    if !json {
        let colour = colour_on(std::io::stdout().is_terminal());
        let count = loaded.modules.len();
        let noun = if count == 1 { "module" } else { "modules" };
        println!("  {} {count} {noun}:", paint("Compiling", CYAN, colour));
        for module in &loaded.modules {
            println!(
                "    {} ({:.1}ms)",
                display_rel(&cwd, &module.path),
                module.parse_duration.as_secs_f64() * 1000.0
            );
        }
    }

    // Rust-crate dependencies: resolve, fetch, and emit every adaptor
    // *before* codegen so the export manifests exist — codegen reads them
    // to type Rust-call returns and enable method syntax on object
    // handles. The archives are built after codegen (only linking needs
    // them), through the same cached call.
    let manifest = ntsc_cargo::manifest::Manifest::read(&cwd.join("neutron.toml"))
        .map_err(|e| CliError::Plain(e.to_string()))?;
    if !manifest.rust_deps.is_empty() {
        let cache = ntsc_cargo::cache::CacheRoot::from_env();
        let resolution = ntsc_cargo::resolve::resolve_cached(&cache, &cwd, &manifest)
            .map_err(|e| CliError::Plain(e.to_string()))?;
        let colour = colour_on(std::io::stdout().is_terminal());
        let total = resolution.deps.len();
        if !json && total > 0 {
            let noun = if total == 1 { "adaptor" } else { "adaptors" };
            println!("  {} {total} {noun}:", paint("Generating", CYAN, colour));
        }
        for (index, dep) in resolution.deps.iter().enumerate() {
            // Announce the crate before the slow fetch + public-API scan, so
            // a large dependency (serenity et al.) never looks like a hang.
            if !json {
                println!(
                    "    [{:>width$}/{total}] {} v{}",
                    index + 1,
                    paint(&dep.name, CYAN, colour),
                    dep.version,
                    width = total.to_string().len(),
                );
            }
            ntsc_cargo::resolve::materialize_dep(
                &cache,
                &cwd,
                &manifest,
                dep,
                &resolution.registry_dir,
            )
            .map_err(|e| CliError::Plain(e.to_string()))?;
        }
    }

    // Compile source → object file. Rust-call result types come from the
    // export manifests generated by `ntsc cargo install`/`ntsc build`.
    let codegen_start = Instant::now();
    ntsc_codegen::emit::set_rust_export_types(&cwd);
    let obj_path = ntsc_codegen::compile_program(
        &loaded.program,
        &config.target,
        mode.opt_level(),
        &config.output,
        &out_dir,
        test_mode,
    )
    .map_err(|e| codegen_error(e, &loaded))?;
    let codegen_time = codegen_start.elapsed();

    // Find the runtime library and link → binary.
    // Only the stdlib modules actually imported by the program are built
    // into the runtime (the monolithic `full` archive is ~39M release;
    // a `hello` with no heavy imports is ~2-3M). This is the "only build
    // what is used" path.
    let needed_features = detect_runtime_features(&loaded.program);
    let runtime_lib = find_runtime_lib(&cwd, &needed_features, release)?;

    // Rust-crate dependencies: make sure every [rust-deps] crate is
    // resolved, fetched, and its adaptor built, then link the compiled
    // adaptor archives beside the runtime.
    let adaptor_artifacts = rust_dep_archives(&cwd, release)?;
    let mut native_link_args = adaptor_artifacts.native_link_args.clone();
    native_link_args.extend(config.ffi_search_paths.iter().map(|search_path| {
        let path = Path::new(search_path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        #[cfg(target_os = "windows")]
        {
            format!("/LIBPATH:{}", path.display())
        }
        #[cfg(not(target_os = "windows"))]
        {
            format!("-L{}", path.display())
        }
    }));
    native_link_args.extend(config.ffi_libraries.iter().map(|library| {
        #[cfg(target_os = "windows")]
        {
            format!("/DEFAULTLIB:{library}")
        }
        #[cfg(not(target_os = "windows"))]
        {
            format!("-l{library}")
        }
    }));
    native_link_args.sort();
    native_link_args.dedup();
    let output_path = out_dir.join(ntsc_codegen::with_executable_extension(&config.output));
    let link_start = Instant::now();
    ntsc_codegen::link_binary_with_extras(
        &obj_path,
        &runtime_lib,
        &adaptor_artifacts.archives,
        &native_link_args,
        &output_path,
    )?;
    let link_time = link_start.elapsed();

    if !json {
        let colour = colour_on(std::io::stdout().is_terminal());
        println!(
            "  {} load {:.1}ms, codegen {:.1}ms, link {:.1}ms",
            paint("Timing", CYAN, colour),
            load_time.as_secs_f64() * 1000.0,
            codegen_time.as_secs_f64() * 1000.0,
            link_time.as_secs_f64() * 1000.0
        );
        println!(
            "  {} {mode:?} build at {}",
            paint("Finished", GREEN, colour),
            output_path.display()
        );
    }

    if json {
        println!(
            "{{\"success\":true,\"output\":\"{}\",\"mode\":\"{}\"}}",
            json_escape(&output_path.display().to_string()),
            mode.dir_name()
        );
    }
    Ok(output_path)
}

/// Convert a module-load failure into diagnostics, attaching the source
/// text of any file that produced parse errors so snippets can be
/// rendered.
fn load_error(error: ntsc_build::modules::ModuleLoadError) -> CliError {
    let diags = error.into_diagnostics();
    let mut sources = SourceMap::new();
    for diag in &diags {
        if let Some(path) = diag.file_path.as_deref()
            && let Ok(text) = fs::read_to_string(path)
        {
            sources.add(SourceBuffer::new(&text, path));
        }
    }
    CliError::Diagnostics { diags, sources }
}

/// Render lint warnings, attributing each span to its source file via
/// the module provenance recorded during load.
fn emit_warnings(
    warnings: Vec<ntsc_typeck::Warning>,
    loaded: &ntsc_build::modules::ModuleLoadResult,
    json: bool,
) {
    let diags: Vec<Diagnostic> = warnings
        .into_iter()
        .map(|warning| {
            let mut diag = Diagnostic::from(&warning);
            if let Some((path, base)) = loaded.localize(warning.span) {
                diag.file_path = Some(path.display().to_string());
                for label in &mut diag.labels {
                    label.span.start = label.span.start.saturating_sub(base);
                    label.span.end = label.span.end.saturating_sub(base);
                }
            }
            diag
        })
        .collect();
    if json {
        println!("{}", diagnostics_to_json(&diags));
    } else {
        let writer = Writer::new(DiagConfig::from_env());
        writer.emit_all(&diags, Some(&loaded.sources));
    }
}

/// Convert a name-resolution failure into diagnostics, attributing each
/// error to its source file via the module provenance recorded during
/// load.
fn resolve_error(
    errors: Vec<ntsc_typeck::ResolveError>,
    loaded: &ntsc_build::modules::ModuleLoadResult,
) -> CliError {
    let diags = errors
        .into_iter()
        .map(|error| {
            let mut diag = Diagnostic::from(&error);
            if diag.file_path.is_none()
                && let Some((path, base)) = loaded.localize(error.span)
            {
                diag.file_path = Some(path.display().to_string());
                for label in &mut diag.labels {
                    label.span.start = label.span.start.saturating_sub(base);
                    label.span.end = label.span.end.saturating_sub(base);
                }
            }
            diag
        })
        .collect();
    CliError::Diagnostics {
        diags,
        sources: loaded.sources.clone(),
    }
}

/// Convert a codegen failure into diagnostics. Span-carrying diagnostics
/// (parse/type errors) are attributed to their source file via the module
/// provenance recorded during load, and their spans rebased to
/// file-local byte coordinates so source snippets can be rendered.
fn codegen_error(
    error: ntsc_codegen::CodegenError,
    loaded: &ntsc_build::modules::ModuleLoadResult,
) -> CliError {
    let diags = error
        .into_diagnostics()
        .into_iter()
        .map(|mut diag| {
            if diag.file_path.is_none()
                && let Some(span) = diag.labels.first().map(|l| l.span)
                && let Some((path, base)) = loaded.localize(span)
            {
                diag.file_path = Some(path.display().to_string());
                for label in &mut diag.labels {
                    label.span.start = label.span.start.saturating_sub(base);
                    label.span.end = label.span.end.saturating_sub(base);
                }
            }
            diag
        })
        .collect();
    CliError::Diagnostics {
        diags,
        sources: loaded.sources.clone(),
    }
}

/// Build in test mode, link, and run every `test` block. Exits non-zero
/// if any test fails.
fn cmd_test(release: bool, json: bool) -> Result<PathBuf, CliError> {
    let output_path = cmd_build(release, true, json)?;

    let mut child = std::process::Command::new(&output_path)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Plain(format!("failed to run '{}': {e}", output_path.display())))?;

    // The generated harness reports its totals on a __NTSC_SUMMARY__ line
    // (see emit_test_harness). That marker is an internal protocol between
    // codegen and this command, so it is consumed here and re-rendered rather
    // than shown to the user. Human-readable output is streamed line by line
    // so test progress still appears as it happens; with --json, stdout stays
    // machine-readable and only the summary object is printed.
    let mut summary = None;
    if let Some(stdout) = child.stdout.take() {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            let line =
                line.map_err(|e| CliError::Plain(format!("failed to read test output: {e}")))?;
            match line.strip_prefix(SUMMARY_MARKER) {
                Some(totals) => summary = Some(totals.trim().to_string()),
                None if json => {}
                None => println!("{line}"),
            }
        }
    }

    let status = child
        .wait()
        .map_err(|e| CliError::Plain(format!("failed to run '{}': {e}", output_path.display())))?;

    let totals = summary.unwrap_or_else(|| "no tests ran".to_string());
    if json {
        println!(
            "{{\"success\":{},\"summary\":\"{}\"}}",
            status.success(),
            json_escape(&totals)
        );
    } else {
        let colour = colour_on(std::io::stdout().is_terminal());
        let label = if status.success() { GREEN } else { BOLD };
        println!("  {} {totals}", paint("Summary", label, colour));
    }

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(output_path)
}

/// Build and run the project.
fn cmd_run(release: bool, json: bool) -> Result<PathBuf, CliError> {
    let output_path = cmd_build(release, false, json)?;

    if !json {
        let colour = colour_on(std::io::stdout().is_terminal());
        println!(
            "  {} {}...",
            paint("Running", CYAN, colour),
            output_path.display()
        );
    }
    let status = std::process::Command::new(&output_path)
        .status()
        .map_err(|e| CliError::Plain(format!("failed to run `{}`: {e}", output_path.display())))?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(output_path)
}

/// Remove project output, global adaptor caches, or Neutron scratch trees.
fn cmd_clean(args: &[String]) -> Result<(), CliError> {
    let mut clean_cache = false;
    let mut clean_tmp = false;
    let mut dry_run = false;
    let mut force_rebuild = false;
    let mut max_age = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--cache" => clean_cache = true,
            "--tmp" => clean_tmp = true,
            "--dry-run" => dry_run = true,
            "--force-rebuild" => force_rebuild = true,
            "--max-age" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| CliError::Plain("--max-age requires a number of days".into()))?;
                max_age =
                    Some(value.parse::<u64>().map_err(|_| {
                        CliError::Plain("--max-age requires a number of days".into())
                    })?);
            }
            other => return Err(CliError::Plain(format!("unknown clean option '{other}'"))),
        }
        index += 1;
    }

    if max_age.is_some() && !clean_cache {
        return Err(CliError::Plain("--max-age requires --cache".into()));
    }
    if force_rebuild && !clean_cache {
        return Err(CliError::Plain("--force-rebuild requires --cache".into()));
    }

    if clean_cache {
        let cache = ntsc_cargo::cache::CacheRoot::from_env();
        ntsc_cargo::cache::gc(&cache, dry_run, max_age, force_rebuild)
            .map_err(|e| CliError::Plain(e.to_string()))?;
        if !dry_run {
            println!(
                "Cleaned Neutron Rust caches under {}",
                cache.path().display()
            );
        }
    }
    if clean_tmp {
        clean_ntsc_tmp(dry_run)?;
    }
    if clean_cache || clean_tmp {
        return Ok(());
    }

    let cwd = env::current_dir()?;
    let build_dir = cwd.join("build");
    if !build_dir.exists() {
        if !dry_run {
            println!("Nothing to remove: {} does not exist", build_dir.display());
        }
        return Ok(());
    }
    if dry_run {
        println!("would remove {}", build_dir.display());
    } else {
        fs::remove_dir_all(&build_dir)?;
        println!("Removed {}", build_dir.display());
    }
    Ok(())
}

fn clean_ntsc_tmp(dry_run: bool) -> Result<(), CliError> {
    let tmp = Path::new("/tmp");
    let entries = fs::read_dir(tmp)?;
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !name.starts_with("ntsc_") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::now());
        if modified >= cutoff {
            continue;
        }
        if dry_run {
            println!("would remove {}", path.display());
        } else if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
        removed += 1;
    }
    if !dry_run {
        println!("Removed {removed} stale Neutron temporary trees");
    }
    Ok(())
}

/// Rebuild whenever the entry, an imported module, or `neutron.toml`
/// changes.
fn cmd_watch(release: bool, json: bool) -> Result<(), CliError> {
    let (cwd, config) = load_project()?;

    let rebuild = || {
        if let Err(e) = cmd_build(release, false, json) {
            report(e, json);
        }
    };

    let mut last_signature = project_signature(&cwd, &config)?;
    println!("Watching for changes (Ctrl-C to stop)...");
    rebuild();

    loop {
        std::thread::sleep(Duration::from_millis(400));
        match project_signature(&cwd, &config) {
            Ok(signature) if signature != last_signature => {
                println!("  Change detected, rebuilding...");
                last_signature = signature;
                rebuild();
            }
            Err(e) => {
                report(e, json);
                std::thread::sleep(Duration::from_secs(1));
            }
            Ok(_) => {}
        }
    }
}

/// Print the module dependency graph in DOT format.
fn cmd_graph() -> Result<(), CliError> {
    let (cwd, config) = load_project()?;
    let graph = ntsc_build::modules::discover(&cwd.join(&config.entry)).map_err(load_error)?;

    println!("digraph ntsc_modules {{");
    for (importer, importee) in &graph.edges {
        println!(
            "  \"{}\" -> \"{}\";",
            display_rel(&cwd, importer),
            display_rel(&cwd, importee)
        );
    }
    println!("}}");
    Ok(())
}

/// Manage Rust-crate dependencies: `ntsc cargo install <crate>` adds a
/// crate to `[rust-deps]`, resolves it through the real cargo, and emits
/// the adaptor that `ntsc build` links in.
fn cmd_cargo(args: &[String]) -> Result<(), CliError> {
    let cwd = env::current_dir()?;
    ntsc_cargo::run(args, &cwd).map_err(|e| CliError::Plain(e.to_string()))
}

/// Resolve, fetch, and build the project's `[rust-deps]` adaptors.
///
/// A project with no `[rust-deps]` section (the overwhelmingly common
/// case) skips the whole cargo dance: the manifest is read, found empty,
/// and no archives are produced.
fn rust_dep_archives(
    project_dir: &Path,
    release: bool,
) -> Result<ntsc_cargo::build::AdaptorArtifacts, CliError> {
    let manifest_path = project_dir.join("neutron.toml");
    let manifest = ntsc_cargo::manifest::Manifest::read(&manifest_path)
        .map_err(|e| CliError::Plain(e.to_string()))?;
    if manifest.rust_deps.is_empty() {
        return Ok(ntsc_cargo::build::AdaptorArtifacts::default());
    }

    let cache = ntsc_cargo::cache::CacheRoot::from_env();
    // Adaptors were already resolved, fetched, and emitted earlier in
    // `cmd_build` — before codegen, so the export manifests exist. This step
    // only compiles those generated crates into linkable archives; the cached
    // adaptors keep the whole build fast on rebuild.
    ntsc_cargo::build::build_adaptors_with_native_links(&cache, project_dir, release)
        .map_err(|e| CliError::Plain(e.to_string()))
}

/// Run the package-manager executable installed beside `ntsc`.
fn cmd_pkg(args: &[String]) -> Result<(), CliError> {
    let current_exe = env::current_exe()?;
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| CliError::Plain("cannot locate the ntsc executable directory".into()))?;
    let pkg_name = ntsc_codegen::with_executable_extension("ntsc-pkg");
    let pkg = exe_dir.join(&pkg_name);

    if !pkg.is_file() {
        return Err(CliError::Plain(format!(
            "cannot find the package manager beside ntsc: {}\n\
             reinstall ntsc so ntsc-pkg is installed beside the compiler",
            pkg.display()
        )));
    }

    let status = std::process::Command::new(&pkg).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// Snapshot the modification times of every file in the module closure
/// plus `neutron.toml`. Used by watch to detect changes (including new
/// imports).
fn project_signature(
    cwd: &Path,
    config: &ntsc_build::BuildConfig,
) -> Result<Vec<(PathBuf, std::time::SystemTime)>, CliError> {
    let graph = ntsc_build::modules::discover(&cwd.join(&config.entry)).map_err(load_error)?;
    let mut signature = Vec::new();
    for path in &graph.files {
        let mtime = fs::metadata(path)?.modified()?;
        signature.push((path.clone(), mtime));
    }
    let neutron_toml = cwd.join("neutron.toml");
    let mtime = fs::metadata(&neutron_toml)?.modified()?;
    signature.push((neutron_toml, mtime));
    Ok(signature)
}

/// Read `neutron.toml` from the current directory.
fn load_project() -> Result<(PathBuf, ntsc_build::BuildConfig), CliError> {
    let cwd = env::current_dir()?;

    let build_path = cwd.join("neutron.toml");
    let build_src = fs::read_to_string(&build_path)
        .map_err(|e| CliError::Plain(format!("cannot read neutron.toml: {e}")))?;
    let config = ntsc_build::parse(&build_src).map_err(|errors| {
        let msgs: Vec<_> = errors.iter().map(|e| e.to_string()).collect();
        CliError::Plain(format!("neutron.toml errors:\n  {}", msgs.join("\n  ")))
    })?;

    Ok((cwd, config))
}

/// Display a canonical path relative to `base` when possible.
fn display_rel(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn detect_runtime_features(program: &Program) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut used = BTreeSet::new();
    for stmt in &program.statements {
        if let Stmt::Use {
            library,
            is_file_path: false,
            is_rust: false,
            ..
        } = stmt
        {
            used.insert(library.lexeme().to_string());
        }
    }
    let mut features = Vec::new();
    let has = |name: &str| used.contains(name);
    if has("regex") || has("glob") {
        features.push("regex".to_string());
    }
    if has("http") {
        features.push("http".to_string());
    }
    if has("archive") {
        features.push("archive".to_string());
    }
    if has("crypto") || has("hash") || has("encoding") {
        features.push("crypto".to_string());
    }
    features
}

/// Locate the runtime static library.
///
/// Only the stdlib modules actually imported by the program are built
/// into the runtime (`--no-default-features --features <needed>`). A
/// `hello` with no heavy imports links a ~2-3M core archive instead of
/// the full ~39M one, and `cargo` still caches per feature set.
fn find_runtime_lib(
    project_dir: &Path,
    needed_features: &[String],
    release: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let lib_name = ntsc_codegen::runtime_lib_name();

    let is_full_needed = {
        let mut s = needed_features.to_vec();
        s.sort();
        s == vec![
            "archive".to_string(),
            "crypto".to_string(),
            "http".to_string(),
            "regex".to_string(),
        ]
    };
    if is_full_needed && let Some(runtime) = find_installed_runtime() {
        return Ok(runtime);
    }

    let rewrite_dir = match find_rewrite_dir(project_dir)?.or_else(find_rewrite_dir_from_exe) {
        Some(dir) if dir.exists() => dir,
        // An installed binary with a shipped runtime archive can still
        // build projects outside the workspace: the archive carries every
        // stdlib module, so any feature set links against it.
        _ => {
            return find_installed_runtime()
                .ok_or_else(|| runtime_not_found_error(lib_name).into());
        }
    };

    let profile_dir = if release { "release" } else { "debug" };
    let is_full = is_full_needed;

    // Reuse the workspace's already-built full archive for the common
    // `cargo test`/GATE path where every heavy module is needed and the
    // archive is already on disk. This avoids rebuilding the 39M full
    // runtime for every `ntsc build` inside the workspace.
    if is_full {
        let candidates = [
            project_dir.join("target").join(profile_dir).join(lib_name),
            rewrite_dir.join("target").join(profile_dir).join(lib_name),
        ];
        for candidate in &candidates {
            if candidate.exists() {
                let dominated = needed_features.is_empty()
                    && candidate
                        .metadata()
                        .map(|m| m.len() > 20_000_000)
                        .unwrap_or(false);
                if !dominated {
                    return Ok(candidate.clone());
                }
            }
        }
    }

    // Per-feature cache so `hello` (no heavy deps) and `http` programs
    // don't clobber each other's `libntsc_runtime.a`. Cargo's fingerprint
    // already separates them, but the output file is the same path, so we
    // isolate by `target-dir`.
    let mut feature_key = needed_features.to_vec();
    feature_key.sort();
    let key = if feature_key.is_empty() {
        "core".to_string()
    } else {
        feature_key.join(",")
    };
    let hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        key.hash(&mut h);
        format!("{:x}", h.finish())
    };
    let target_dir = rewrite_dir
        .join("target")
        .join(format!("ntsc-runtime-{}-{}", profile_dir, hash));

    let mut args = vec![
        "build".to_string(),
        "-p".to_string(),
        "ntsc-runtime".to_string(),
    ];
    if release {
        args.push("--release".to_string());
    }
    if feature_key.is_empty() {
        args.push("--no-default-features".to_string());
    } else if is_full {
        // `default = ["full"]` already builds everything; no extra flags
        // needed, but keep the isolated target-dir for cache hygiene.
    } else {
        args.push("--no-default-features".to_string());
        args.push("--features".to_string());
        args.push(key.clone());
    }
    args.push("--target-dir".to_string());
    args.push(target_dir.display().to_string());

    println!(
        "  Building runtime ({})...",
        if feature_key.is_empty() {
            "core".to_string()
        } else {
            key.clone()
        }
    );
    let status = std::process::Command::new("cargo")
        .args(&args)
        .current_dir(&rewrite_dir)
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;

    if !status.success() {
        return Err("failed to build ntsc-runtime".into());
    }

    let built = target_dir.join(profile_dir).join(lib_name);
    if built.exists() {
        return Ok(built);
    }

    Err(format!("cannot find {lib_name} after building").into())
}

/// Locate a runtime archive staged by an installer, relative to the
/// running executable. Returns the first candidate that exists.
///
/// `current_exe` resolves symlinks on Linux (via `/proc/self/exe`), so a
/// tarball whose `bin/ntsc` is symlinked onto `PATH` still resolves its
/// sibling `lib/ntsc` directory correctly.
fn find_installed_runtime() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let lib_name = ntsc_codegen::runtime_lib_name();
    exe_relative_runtime_candidates(exe_dir, lib_name)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// The runtime-archive locations an installed `ntsc` searches, relative
/// to its executable directory, in priority order:
///
/// 1. `<exe_dir>/<lib>` — Windows (`.msi` and tarball), the macOS `.app`
///    bundle, and a dev `target/{debug,release}` build.
/// 2. `<exe_dir>/../lib/ntsc/<lib>` — the `.deb`, Arch, and Unix tarball
///    prefix layout (`bin/` + `lib/ntsc/`).
/// 3. `<exe_dir>/../lib64/ntsc/<lib>` — the `.rpm` layout: `rpmbuild`
///    expands `%{_libdir}` to `/usr/lib64` on x86_64, so Fedora installs
///    land there.
///
/// Pure and filesystem-free so the ordering is unit-tested directly.
fn exe_relative_runtime_candidates(exe_dir: &Path, lib_name: &str) -> Vec<PathBuf> {
    vec![
        exe_dir.join(lib_name),
        exe_dir.join("..").join("lib").join("ntsc").join(lib_name),
        exe_dir.join("..").join("lib64").join("ntsc").join(lib_name),
    ]
}

/// Error shown when no runtime archive can be found and no workspace is
/// available to build one — i.e. an installed binary whose installer
/// failed to stage the runtime. Names the archive and the prefix layout
/// it is expected in rather than the misleading "cannot locate the NTSC
/// workspace".
fn runtime_not_found_error(lib_name: &str) -> String {
    let searched = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map(|exe_dir| {
            exe_relative_runtime_candidates(&exe_dir, lib_name)
                .iter()
                .map(|candidate| format!("\n  {}", candidate.display()))
                .collect::<String>()
        })
        .unwrap_or_default();
    format!(
        "cannot find the NTSC runtime library ({lib_name}).\n\
         Searched next to the executable:{searched}\n\
         An installed ntsc expects the runtime at <prefix>/lib/ntsc/{lib_name}; \
         reinstall from a package that ships it, or run inside the NTSC workspace \
         so it can be built."
    )
}

/// Walk up from `from` to find the rewrite workspace root.
fn find_rewrite_dir(from: &Path) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let mut dir = from.to_path_buf();
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").exists() {
            return Ok(Some(dir));
        }
        if !dir.pop() {
            break;
        }
    }
    Ok(None)
}

/// Walk up from the `ntsc` executable to find the rewrite workspace
/// root.
fn find_rewrite_dir_from_exe() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    find_rewrite_dir(&exe).ok().flatten()
}

/// Escape a string for embedding in a JSON string literal.
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_candidates_are_ordered_beside_then_lib_then_lib64() {
        let exe_dir = Path::new("/opt/ntsc/bin");
        let got = exe_relative_runtime_candidates(exe_dir, "libntsc_runtime.a");

        assert_eq!(
            got,
            vec![
                PathBuf::from("/opt/ntsc/bin/libntsc_runtime.a"),
                PathBuf::from("/opt/ntsc/bin/../lib/ntsc/libntsc_runtime.a"),
                PathBuf::from("/opt/ntsc/bin/../lib64/ntsc/libntsc_runtime.a"),
            ],
            "beside-exe must win (Windows/.msi/macOS), then the .deb/Arch \
             prefix, then the .rpm lib64 prefix"
        );
    }

    #[test]
    fn runtime_candidates_use_the_given_lib_name() {
        // The archive name follows the host toolchain (ntsc_runtime.lib
        // under MSVC), so the helper must never hardcode it.
        let got = exe_relative_runtime_candidates(Path::new("/p/bin"), "ntsc_runtime.lib");
        assert!(got.iter().all(|p| p.ends_with("ntsc_runtime.lib")));
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn runtime_not_found_error_names_the_library_and_prefix() {
        let msg = runtime_not_found_error("libntsc_runtime.a");
        assert!(msg.contains("libntsc_runtime.a"));
        assert!(msg.contains("<prefix>/lib/ntsc/"));
        // Must not resurrect the misleading workspace-only wording.
        assert!(!msg.contains("cannot locate the NTSC workspace"));
    }
    #[test]
    fn closest_suggests_near_misses() {
        let commands = COMMANDS.iter().map(|doc| doc.name);
        assert_eq!(closest("buidl", commands.clone()), Some("build"));
        assert_eq!(closest("doc", commands.clone()), Some("docs"));
        assert_eq!(closest("qwerty", commands), None);
        assert_eq!(
            closest("--dryrun", OPTION_NAMES.iter().copied()),
            Some("--dry-run")
        );
    }

    #[test]
    fn json_escape_handles_quotes_and_controls() {
        assert_eq!(json_escape("a\"b\\c\n\t"), "a\\\"b\\\\c\\n\\t");
    }
}
