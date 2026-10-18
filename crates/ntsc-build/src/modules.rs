//! Multi-module loading: resolves `use "file.nt"` imports into a single
//! merged program.
//!
//! Files are discovered by walking the `use`-file closure of the entry
//! source, then parsed in parallel. The resulting ASTs are merged (imported
//! modules first, deduplicated by canonical path) so the rest of the compiler
//! sees one flat program — the language keeps a single namespace.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ntsc_ast::expr::Expr;
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{Program, Stmt};
use ntsc_ast::token::TokenKind;
use ntsc_ast::types::TypeAnnotation;
use ntsc_diag::Diagnostic;
use ntsc_diag::SourceBuffer;
use ntsc_diag::codes;

/// Errors that can occur while loading a module closure.
#[derive(Debug)]
pub enum ModuleLoadError {
    /// The file could not be read.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A file failed to parse. Errors carry their original spans, so they
    /// can be rendered against the file's source.
    Parse {
        path: PathBuf,
        errors: Vec<ntsc_parser::ParseError>,
    },

    /// A module imports itself, directly or transitively.
    Cycle { path: PathBuf, chain: Vec<String> },

    /// A file import resolves outside the project root.
    EscapesProjectRoot { path: PathBuf, import: String },

    /// An imported file could not be resolved against the parsed closure.
    Internal(String),
}

impl ModuleLoadError {
    /// Convert into one or more ready-to-render diagnostics.
    ///
    /// A `Parse` failure yields one diagnostic per parse error, each
    /// attached to its source file. Other failures yield a single
    /// diagnostic with no span.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        match self {
            Self::Parse { path, errors } => errors
                .into_iter()
                .map(|error| Diagnostic::from(&error).with_file(path.display().to_string()))
                .collect(),
            Self::Io { path, source } => vec![
                Diagnostic::error(format!("cannot read `{}`: {source}", path.display()))
                    .with_code(codes::BUILD),
            ],
            Self::Cycle { path, chain } => vec![
                Diagnostic::error(format!(
                    "import cycle detected for `{}`:\n  {}",
                    path.display(),
                    chain.join(" -> ")
                ))
                .with_code(codes::BUILD),
            ],
            Self::EscapesProjectRoot { path, import } => vec![
                Diagnostic::error(format!(
                    "file import `{import}` in `{}` resolves outside the project root",
                    path.display()
                ))
                .with_code(codes::BUILD),
            ],
            Self::Internal(msg) => vec![Diagnostic::error(msg).with_code(codes::BUILD)],
        }
    }
}

impl std::fmt::Display for ModuleLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "cannot read `{}`: {source}", path.display()),
            Self::Parse { path, errors } => write!(
                f,
                "parse errors in `{}`:\n  {}",
                path.display(),
                errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ),
            Self::Cycle { path, chain } => write!(
                f,
                "import cycle detected for `{}`:\n  {}",
                path.display(),
                chain.join(" -> ")
            ),
            Self::EscapesProjectRoot { path, import } => write!(
                f,
                "file import `{import}` in `{}` resolves outside the project root",
                path.display()
            ),
            Self::Internal(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ModuleLoadError {}

/// The `use`-import dependency graph of an entry source.
#[derive(Debug, Clone)]
pub struct ModuleGraph {
    /// Entry source path (canonical).
    pub entry: PathBuf,

    /// All files in the closure, in discovery order (imports before
    /// importer).
    pub files: Vec<PathBuf>,

    /// Edges `(importer, importee)`, all canonical.
    pub edges: Vec<(PathBuf, PathBuf)>,

    /// Namespace alias of each file that is imported with `use "F" as name`.
    /// Files imported bare (or the entry) are absent.
    pub aliases: HashMap<PathBuf, String>,
}

/// Information about one compiled module.
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    /// Canonical path of the module file.
    pub path: PathBuf,

    /// Wall time spent lexing and parsing this module.
    pub parse_duration: Duration,
}

/// The merged program plus per-module build information.
#[derive(Debug)]
pub struct ModuleLoadResult {
    pub program: Program,
    pub modules: Vec<ModuleInfo>,
    pub graph: ModuleGraph,

    /// Source text of every file in the closure, keyed by canonical path.
    pub sources: ntsc_diag::SourceMap,

    /// Source file of each top-level statement in `program`, in order.
    origins: Vec<PathBuf>,

    /// Byte range of each top-level statement in `program`, in order.
    ranges: Vec<(usize, usize)>,

    /// Byte shift applied to each top-level statement's span, in order.
    bases: Vec<usize>,
}

impl ModuleLoadResult {
    /// Index of the most specific (smallest) top-level statement whose
    /// merged byte range contains `span`, together with that range's
    /// width.
    fn span_index(&self, span: Span) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        for (i, (min, max)) in self.ranges.iter().enumerate() {
            if span.start >= *min && span.end <= *max {
                let width = max.saturating_sub(*min);
                if best.is_none_or(|(_, best_width)| width < best_width) {
                    best = Some((i, width));
                }
            }
        }
        best
    }

    /// The source file that a span belongs to, when it can be determined.
    ///
    /// Attribution walks the top-level statements of the merged program
    /// and finds the one whose byte range contains the span. Merged
    /// ranges are globally unique because every module's spans were
    /// shifted by a base during merge.
    pub fn file_for_span(&self, span: Span) -> Option<&Path> {
        self.span_index(span)
            .map(|(i, _)| self.origins[i].as_path())
    }

    /// Attribute a merged span to its source file, returning the file and
    /// the byte base that was added to its spans during merge.
    ///
    /// Subtracting the base from a merged span yields the file-local byte
    /// coordinates; line and column numbers are already file-local and
    /// need no adjustment.
    pub fn localize(&self, span: Span) -> Option<(PathBuf, usize)> {
        let (i, _) = self.span_index(span)?;
        Some((self.origins[i].clone(), self.bases[i]))
    }
}

/// Resolve the `use`-file closure of `entry`, parsing every file in
/// parallel and merging the ASTs into one program.
pub fn load_program(entry: &Path) -> Result<ModuleLoadResult, ModuleLoadError> {
    let graph = discover(entry)?;

    // Parse all files concurrently; each module is independent once the
    // import graph is known.
    let mut modules = std::thread::scope(|scope| {
        let handles: Vec<_> = graph
            .files
            .iter()
            .map(|path| {
                scope.spawn(move || {
                    let start = Instant::now();
                    let parsed = parse_file(path);
                    (path.clone(), parsed, start.elapsed())
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|handle| {
                let (path, parsed, parse_duration) = handle.join().expect("parser thread panicked");
                parsed.map(|(source, program)| ParsedModule {
                    path,
                    source,
                    program,
                    parse_duration,
                })
            })
            .collect::<Result<Vec<_>, _>>()
    })?;

    let (program, origins, bases) = merge(&graph, &mut modules)?;

    let mut sources = ntsc_diag::SourceMap::new();
    for module in &modules {
        sources.add(SourceBuffer::new(
            &module.source,
            module.path.display().to_string(),
        ));
    }

    let ranges = program.statements.iter().map(stmt_byte_range).collect();

    let infos = modules
        .into_iter()
        .map(|m| ModuleInfo {
            path: m.path,
            parse_duration: m.parse_duration,
        })
        .collect();

    Ok(ModuleLoadResult {
        program,
        modules: infos,
        graph,
        sources,
        origins,
        ranges,
        bases,
    })
}

/// Walk the `use`-file imports of `entry`, producing the dependency
/// graph.
///
/// Imports are located by scanning tokens for `use "<path>"` (a quoted
/// string names a file; an identifier such as `use process` names a
/// stdlib module and is ignored here). The returned file order is
/// post-order DFS: every import comes before its importer, so merging in
/// this order puts library code ahead of entry code.
///
/// The project root is the entry file's directory: every resolved import
/// must stay inside it, so no file can reach outside the project through
/// `../`.
pub fn discover(entry: &Path) -> Result<ModuleGraph, ModuleLoadError> {
    let mut files = Vec::new();
    let mut edges = Vec::new();
    let mut aliases = HashMap::new();
    let mut seen = HashSet::new();
    let mut stack = Vec::new();

    let entry_canon = canonicalize(entry)?;
    let root = entry_canon
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(entry_canon.clone());
    visit(
        &entry_canon,
        &root,
        &mut files,
        &mut edges,
        &mut aliases,
        &mut seen,
        &mut stack,
    )?;

    Ok(ModuleGraph {
        entry: entry_canon,
        files,
        edges,
        aliases,
    })
}

fn visit(
    path: &Path,
    root: &Path,
    files: &mut Vec<PathBuf>,
    edges: &mut Vec<(PathBuf, PathBuf)>,
    aliases: &mut HashMap<PathBuf, String>,
    seen: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
) -> Result<(), ModuleLoadError> {
    if !seen.insert(path.to_path_buf()) {
        return Ok(());
    }
    stack.push(path.to_path_buf());

    for (import, alias) in file_imports(path, root)? {
        edges.push((path.to_path_buf(), import.clone()));
        if let Some(alias) = alias {
            aliases.insert(import.clone(), alias);
        }
        if stack.contains(&import) {
            let mut chain: Vec<String> = stack.iter().map(|p| p.display().to_string()).collect();
            chain.push(import.display().to_string());
            return Err(ModuleLoadError::Cycle {
                path: path.to_path_buf(),
                chain,
            });
        }
        visit(&import, root, files, edges, aliases, seen, stack)?;
    }

    stack.pop();
    files.push(path.to_path_buf());
    Ok(())
}

fn canonicalize(path: &Path) -> Result<PathBuf, ModuleLoadError> {
    fs::canonicalize(path).map_err(|e| ModuleLoadError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Lex a file and return the canonical paths of every `use "<path>"`
/// import, along with the namespace alias if the import names one with
/// `... as X`.
///
/// A string literal after `use` (or after `from` in a selective import)
/// names a file; an `as` immediately after it names the import alias.
fn file_imports(
    path: &Path,
    root: &Path,
) -> Result<Vec<(PathBuf, Option<String>)>, ModuleLoadError> {
    let source = fs::read_to_string(path).map_err(|e| ModuleLoadError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let tokens = ntsc_lexer::tokenize(&source);
    let parent = path.parent().unwrap_or(Path::new("."));

    let mut imports = Vec::new();
    let mut tokens = tokens.iter().peekable();
    while let Some(token) = tokens.next() {
        if token.kind != TokenKind::Use {
            continue;
        }
        // Find the first string literal on this statement; there is
        // exactly one, and it is the file path. Stop at the terminator so
        // a string on a later statement is never misread as this import.
        let mut import: Option<String> = None;
        let mut alias: Option<String> = None;
        loop {
            match tokens.next() {
                Some(t) if matches!(t.kind, TokenKind::StringLiteral(_)) => {
                    import = Some(t.lexeme().to_string());
                }
                Some(t) if t.kind == TokenKind::As => {
                    if let Some(a) = tokens.next() {
                        alias = Some(a.lexeme().to_string());
                    }
                }
                Some(t) if t.kind == TokenKind::Semicolon || t.kind == TokenKind::Newline => break,
                Some(_) => {}
                None => break,
            }
        }
        if let Some(import) = import {
            imports.push((resolve_import(root, parent, &import)?, alias));
        }
    }
    Ok(imports)
}

/// Resolve a `use "path"` import against the importing file's directory.
///
/// The `.nt` extension is inferred when the path has none, matching how
/// the parser reports the imported name. The resolved path must stay
/// inside `root`: a file cannot reach outside the project through `../`.
fn resolve_import(root: &Path, parent: &Path, import: &str) -> Result<PathBuf, ModuleLoadError> {
    let candidate = if import.ends_with(".nt") {
        parent.join(import)
    } else {
        parent.join(format!("{import}.nt"))
    };
    let canonical = canonicalize(&candidate)?;
    if !canonical.starts_with(root) {
        return Err(ModuleLoadError::EscapesProjectRoot {
            path: canonical,
            import: import.to_string(),
        });
    }
    Ok(canonical)
}

fn parse_file(path: &Path) -> Result<(String, Program), ModuleLoadError> {
    let source = fs::read_to_string(path).map_err(|e| ModuleLoadError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let tokens = ntsc_lexer::tokenize(&source);
    let program = ntsc_parser::parse(&tokens).map_err(|errors| ModuleLoadError::Parse {
        path: path.to_path_buf(),
        errors,
    })?;
    Ok((source, program))
}

struct ParsedModule {
    path: PathBuf,
    source: String,
    program: Program,
    parse_duration: Duration,
}

/// Merge the parsed modules into one program.
///
/// `graph.files` is post-order (imports before importer), so each
/// module's statements are simply appended in that order and file-import
/// `use` statements are dropped — their content already appears earlier
/// in the list. Deduplication happened during discovery.
///
/// Every module's byte offsets are shifted by a cumulative base so that
/// merged spans are globally unique across files (line/column numbers
/// stay file-local). Returns the merged program, the source file of every
/// merged top-level statement, and the per-module shift base of each
/// statement.
fn merge(
    graph: &ModuleGraph,
    modules: &mut [ParsedModule],
) -> Result<(Program, Vec<PathBuf>, Vec<usize>), ModuleLoadError> {
    let mut by_path: HashMap<PathBuf, &mut ParsedModule> =
        modules.iter_mut().map(|m| (m.path.clone(), m)).collect();

    let mut statements = Vec::new();
    let mut origins = Vec::new();
    let mut bases = Vec::new();
    let mut base = 0usize;
    for path in &graph.files {
        let module = by_path.get_mut(path).ok_or_else(|| {
            ModuleLoadError::Internal(format!(
                "discovered module `{}` was not parsed",
                path.display()
            ))
        })?;
        module.program.shift_spans(base);

        // A file imported with `use "F" as arm` has its own symbols namespaced
        // under `arm::`; bare imports keep their global names.
        if let Some(alias) = graph.aliases.get(path) {
            let own = crate::aliases::top_level_names(&module.program.statements);
            module.program.statements = crate::aliases::namespaced(
                std::mem::take(&mut module.program.statements),
                alias,
                &own,
            );
        }

        for stmt in &module.program.statements {
            // Bare file imports are dropped (their content is already part of
            // the flat program); aliased imports are preserved so the resolver
            // learns the namespace name.
            let keep = match stmt {
                Stmt::Use {
                    is_file_path: true,
                    alias: Some(_),
                    ..
                } => true,
                Stmt::Use {
                    is_file_path: true, ..
                } => false,
                _ => true,
            };
            if keep {
                statements.push(stmt.clone());
                origins.push(path.clone());
                bases.push(base);
            }
        }
        base += module.source.len() + 1;
    }

    Ok((Program { statements }, origins, bases))
}

/// Byte range `(start, end)` covered by a top-level statement subtree.
///
/// Computed from the statement's own token spans and its nested
/// expressions and statements. Only real spans are counted
/// (dummy/placeholder spans are ignored). Used to attribute error spans
/// to source files.
fn stmt_byte_range(stmt: &Stmt) -> (usize, usize) {
    use Stmt::*;

    let add_span = |span: Span, (mut lo, mut hi): (usize, usize)| -> (usize, usize) {
        if span.start == 0 && span.end == 0 {
            return (lo, hi);
        }
        lo = lo.min(span.start);
        hi = hi.max(span.end);
        (lo, hi)
    };

    let add_expr = |e: &Expr, (mut lo, mut hi): (usize, usize)| -> (usize, usize) {
        let (s, en) = (e.span().start, e.span().end);
        if s == 0 && en == 0 {
            return (lo, hi);
        }
        lo = lo.min(s);
        hi = hi.max(en);
        (lo, hi)
    };

    let add_sub = |s: &Stmt, (mut lo, mut hi): (usize, usize)| -> (usize, usize) {
        let (s1, e1) = stmt_byte_range(s);
        lo = lo.min(s1);
        hi = hi.max(e1);
        (lo, hi)
    };

    let add_ty = |t: &TypeAnnotation, (mut lo, mut hi): (usize, usize)| -> (usize, usize) {
        if let Some(s) = ta_span(t) {
            lo = lo.min(s.start);
            hi = hi.max(s.end);
        }
        (lo, hi)
    };

    let mut acc: (usize, usize) = (usize::MAX, 0);

    acc = match stmt {
        Expression { expression } => add_expr(expression, acc),
        Say {
            expression,
            keyword_span,
        } => {
            acc = add_span(*keyword_span, acc);
            add_expr(expression, acc)
        }
        Var {
            name,
            type_annotation,
            initializer,
            ..
        } => {
            acc = add_span(name.span, acc);
            if let Some(ta) = type_annotation {
                acc = add_ty(ta, acc);
            }
            if let Some(initializer) = initializer {
                acc = add_expr(initializer, acc);
            }
            acc
        }
        Block {
            statements,
            open_span,
            close_span,
        } => {
            acc = add_span(*open_span, acc);
            acc = add_span(*close_span, acc);
            for s in statements {
                acc = add_sub(s, acc);
            }
            acc
        }
        If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            acc = add_expr(condition, acc);
            acc = add_sub(then_branch, acc);
            for branch in elif_branches {
                acc = add_span(branch.elif_span, acc);
                acc = add_expr(&branch.condition, acc);
                acc = add_sub(&branch.body, acc);
            }
            if let Some(else_branch) = else_branch {
                acc = add_sub(else_branch, acc);
            }
            acc
        }
        While { condition, body } => {
            acc = add_expr(condition, acc);
            add_sub(body, acc)
        }
        DoWhile { body, condition } => {
            acc = add_sub(body, acc);
            add_expr(condition, acc)
        }
        For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(init) = init {
                acc = add_sub(init, acc);
            }
            if let Some(condition) = condition {
                acc = add_expr(condition, acc);
            }
            if let Some(update) = update {
                acc = add_expr(update, acc);
            }
            add_sub(body, acc)
        }
        ForIn {
            variable,
            iterable,
            body,
        } => {
            acc = add_span(variable.span, acc);
            acc = add_expr(iterable, acc);
            add_sub(body, acc)
        }
        ForAwait {
            variable,
            producer,
            body,
        } => {
            acc = add_span(variable.span, acc);
            acc = add_expr(producer, acc);
            add_sub(body, acc)
        }
        Function {
            name,
            params,
            return_type,
            body,
            ..
        }
        | AsyncFunction {
            name,
            params,
            return_type,
            body,
        } => {
            acc = add_span(name.span, acc);
            for param in params {
                acc = add_span(param.name.span, acc);
                if let Some(ta) = &param.type_annotation {
                    acc = add_ty(ta, acc);
                }
            }
            if let Some(rt) = return_type {
                acc = add_span(rt.arrow_span, acc);
                acc = add_ty(&rt.ty, acc);
            }
            for s in body {
                acc = add_sub(s, acc);
            }
            acc
        }
        ExternFunction {
            abi,
            name,
            params,
            return_type,
            ..
        } => {
            acc = add_span(abi.span, acc);
            acc = add_span(name.span, acc);
            for param in params {
                acc = add_span(param.name.span, acc);
                if let Some(ta) = &param.type_annotation {
                    acc = add_ty(ta, acc);
                }
            }
            if let Some(rt) = return_type {
                acc = add_span(rt.arrow_span, acc);
                acc = add_ty(&rt.ty, acc);
            }
            acc
        }
        ExternBlock { abi, functions } => {
            acc = add_span(abi.span, acc);
            for function in functions {
                acc = add_sub(function, acc);
            }
            acc
        }
        Return { value } => {
            if let Some(value) = value {
                acc = add_expr(value, acc);
            }
            acc
        }
        Class {
            name, parent, body, ..
        } => {
            acc = add_span(name.span, acc);
            if let Some(parent) = parent {
                acc = add_span(parent.span, acc);
            }
            for s in body {
                acc = add_sub(s, acc);
            }
            acc
        }
        Break { span } | Continue { span } => add_span(*span, acc),
        Match {
            expression,
            cases,
            default_case,
        } => {
            acc = add_expr(expression, acc);
            for case in cases {
                acc = add_span(case.case_span, acc);
                acc = add_expr(&case.value, acc);
                if let Some(guard) = &case.guard {
                    acc = add_expr(guard, acc);
                }
                acc = add_sub(&case.body, acc);
            }
            if let Some(default_case) = default_case {
                acc = add_sub(default_case, acc);
            }
            acc
        }
        Try {
            try_block,
            catch_var,
            catch_block,
            finally_block,
        } => {
            acc = add_sub(try_block, acc);
            if let Some(catch_var) = catch_var {
                acc = add_span(catch_var.span, acc);
            }
            if let Some(catch_block) = catch_block {
                acc = add_sub(catch_block, acc);
            }
            if let Some(finally_block) = finally_block {
                acc = add_sub(finally_block, acc);
            }
            acc
        }
        Throw { value } => add_expr(value, acc),
        Retry {
            count,
            body,
            catch_var,
            catch_block,
        } => {
            acc = add_expr(count, acc);
            acc = add_sub(body, acc);
            if let Some(catch_var) = catch_var {
                acc = add_span(catch_var.span, acc);
            }
            if let Some(catch_block) = catch_block {
                acc = add_sub(catch_block, acc);
            }
            acc
        }
        Unsafe { body } => add_sub(body, acc),
        Quiet { body, .. } => add_sub(body, acc),
        Destructure {
            names, initializer, ..
        } => {
            for name in names {
                acc = add_span(name.span, acc);
            }
            add_expr(initializer, acc)
        }
        Use {
            library,
            imported_symbols,
            alias,
            ..
        } => {
            acc = add_span(library.span, acc);
            for symbol in imported_symbols {
                acc = add_span(symbol.span, acc);
            }
            if let Some(alias) = alias {
                acc = add_span(alias.span, acc);
            }
            acc
        }
        Enum { name, members, .. } => {
            acc = add_span(name.span, acc);
            for member in members {
                acc = add_span(member.name.span, acc);
                if let Some(value) = &member.value {
                    acc = add_expr(value, acc);
                }
            }
            acc
        }
        TypeAlias {
            name,
            generic_params,
            target,
        } => {
            acc = add_span(name.span, acc);
            for param in generic_params {
                acc = add_span(param.name.span, acc);
                for bound in &param.bounds {
                    acc = add_span(bound.span, acc);
                }
            }
            add_ty(target, acc)
        }
        Test { name, body } => {
            acc = add_span(name.span, acc);
            for s in body {
                acc = add_sub(s, acc);
            }
            acc
        }
        Trait {
            name,
            parents,
            associated_types,
            methods,
        } => {
            acc = add_span(name.span, acc);
            for parent in parents {
                acc = add_span(parent.span, acc);
            }
            for associated_type in associated_types {
                acc = add_span(associated_type.span, acc);
            }
            for method in methods {
                acc = add_sub(method, acc);
            }
            acc
        }
        Impl {
            trait_name,
            type_name,
            body,
        } => {
            acc = add_span(trait_name.span, acc);
            acc = add_span(type_name.span, acc);
            for method in body {
                acc = add_sub(method, acc);
            }
            acc
        }
        ChanRecvFor {
            variable,
            channel,
            body,
        } => {
            acc = add_span(variable.span, acc);
            acc = add_expr(channel, acc);
            add_sub(body, acc)
        }
        Go {
            call,
            block,
            keyword_span,
        } => {
            acc = add_span(*keyword_span, acc);
            acc = add_expr(call, acc);
            if let Some(block) = block {
                for s in block {
                    acc = add_sub(s, acc);
                }
            }
            acc
        }
    };

    if acc.0 == usize::MAX { (0, 0) } else { acc }
}

/// The real source span of a type annotation, if it names a token.
fn ta_span(ty: &TypeAnnotation) -> Option<Span> {
    match ty {
        TypeAnnotation::Named(token) => Some(token.span),
        TypeAnnotation::Array(Some(inner)) => ta_span(inner),
        TypeAnnotation::Option(inner) => ta_span(inner),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestProject {
        dir: PathBuf,
    }

    impl TestProject {
        fn new() -> Self {
            static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("ntsc_modules_test_{}_{id}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("src")).unwrap();
            Self { dir }
        }

        fn write(&self, rel: &str, contents: &str) -> PathBuf {
            let path = self.dir.join(rel);
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn loads_transitive_closure() {
        let project = TestProject::new();
        project.write(
            "src/main.nt",
            "use \"lib.nt\"\n\nfun main() {\n    say(value())\n}\n",
        );
        project.write(
            "src/lib.nt",
            "use \"util.nt\"\n\nfun value() -> int {\n    return 42;\n}\n",
        );
        project.write("src/util.nt", "fun value() -> int {\n    return 42;\n}\n");

        let result = load_program(&project.dir.join("src/main.nt")).expect("loads");
        assert_eq!(result.modules.len(), 3);

        // Post-order: util, lib, then the entry.
        let canon = |rel: &str| project.dir.join(rel).canonicalize().unwrap();
        assert_eq!(result.graph.files[0], canon("src/util.nt"));
        assert_eq!(result.graph.files[1], canon("src/lib.nt"));
        assert_eq!(result.graph.files[2], canon("src/main.nt"));
    }

    #[test]
    fn deduplicates_shared_modules() {
        let project = TestProject::new();
        project.write(
            "src/main.nt",
            "use \"a.nt\"\nuse \"b.nt\"\n\nfun main() {\n    say(a_thing() + b_thing())\n}\n",
        );
        project.write(
            "src/a.nt",
            "use \"util.nt\"\n\nfun a_thing() -> int {\n    return util_value();\n}\n",
        );
        project.write(
            "src/b.nt",
            "use \"util.nt\"\n\nfun b_thing() -> int {\n    return util_value();\n}\n",
        );
        project.write(
            "src/util.nt",
            "fun util_value() -> int {\n    return 1;\n}\n",
        );

        let result = load_program(&project.dir.join("src/main.nt")).expect("loads");
        assert_eq!(result.modules.len(), 4);
        let util = project.dir.join("src/util.nt").canonicalize().unwrap();
        let count = result.graph.files.iter().filter(|p| **p == util).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn detects_import_cycles() {
        let project = TestProject::new();
        project.write("src/main.nt", "use \"a.nt\"\n\nfun main() {}\n");
        project.write("src/a.nt", "use \"b.nt\"\n\nfun a() {}\n");
        project.write("src/b.nt", "use \"a.nt\"\n\nfun b() {}\n");

        let err = load_program(&project.dir.join("src/main.nt")).unwrap_err();
        assert!(
            matches!(err, ModuleLoadError::Cycle { .. }),
            "expected cycle: {err}"
        );
    }

    #[test]
    fn reports_missing_import() {
        let project = TestProject::new();
        project.write("src/main.nt", "use \"nope.nt\"\n\nfun main() {}\n");

        let err = load_program(&project.dir.join("src/main.nt")).unwrap_err();
        assert!(
            matches!(err, ModuleLoadError::Io { .. }),
            "expected io: {err}"
        );
    }

    #[test]
    fn ignores_builtin_imports() {
        let project = TestProject::new();
        project.write(
            "src/main.nt",
            "use process\n\nfun main() {\n    say(\"hi\")\n}\n",
        );

        let result = load_program(&project.dir.join("src/main.nt")).expect("loads");
        assert_eq!(result.modules.len(), 1);
    }

    #[test]
    fn infers_nt_extension() {
        let project = TestProject::new();
        project.write("src/main.nt", "use \"lib\"\n\nfun main() {}\n");
        project.write("src/lib.nt", "fun answer() -> int {\n    return 42;\n}\n");

        let result = load_program(&project.dir.join("src/main.nt")).expect("loads");
        let lib = project.dir.join("src/lib.nt").canonicalize().unwrap();
        assert_eq!(result.modules.len(), 2);
        assert!(result.graph.files.contains(&lib));
    }

    #[test]
    fn rejects_imports_outside_project_root() {
        let project = TestProject::new();
        project.write("src/main.nt", "use \"../outside.nt\"\n\nfun main() {}\n");
        project.write("outside.nt", "fun hidden() {}\n");

        let err = load_program(&project.dir.join("src/main.nt")).unwrap_err();
        assert!(
            matches!(err, ModuleLoadError::EscapesProjectRoot { .. }),
            "expected escape rejection: {err}"
        );
    }

    #[test]
    fn merges_imported_statements_in_place() {
        let project = TestProject::new();
        project.write(
            "src/main.nt",
            "use \"lib.nt\"\n\nfun main() {\n    say(answer())\n}\n",
        );
        project.write("src/lib.nt", "fun answer() -> int {\n    return 42;\n}\n");

        let result = load_program(&project.dir.join("src/main.nt")).expect("loads");
        // The import is replaced by the library's statements; no `use`
        // remains.
        let merged: Vec<String> = result
            .program
            .statements
            .iter()
            .map(|s| match s {
                Stmt::Function { name, .. } => format!("fn {}", name.lexeme()),
                Stmt::Use { .. } => "use".to_string(),
                _ => "other".to_string(),
            })
            .collect();

        assert_eq!(merged, vec!["fn answer".to_string(), "fn main".to_string()]);
    }

    #[test]
    fn sources_map_covers_all_modules() {
        let project = TestProject::new();
        project.write("src/main.nt", "use \"lib.nt\"\n\nfun main() {}\n");
        project.write("src/lib.nt", "fun answer() -> int {\n    return 42;\n}\n");

        let result = load_program(&project.dir.join("src/main.nt")).expect("loads");
        assert_eq!(result.sources.len(), 2);
        for module in &result.modules {
            let display = module.path.display().to_string();
            assert!(result.sources.get(&display).is_some(), "missing {display}");
        }
    }

    #[test]
    fn attributes_span_to_imported_file() {
        let project = TestProject::new();
        project.write(
            "src/main.nt",
            "use \"lib.nt\"\n\nfun main() {\n    say(util_answer())\n}\n",
        );
        project.write(
            "src/lib.nt",
            "fun util_answer() -> int {\n    return 40 + 2;\n}\n",
        );

        let result = load_program(&project.dir.join("src/main.nt")).expect("loads");
        let lib = project.dir.join("src/lib.nt").canonicalize().unwrap();
        let main = project.dir.join("src/main.nt").canonicalize().unwrap();

        let mut lib_span = Span::new(0, 0, 1, 1);
        let mut main_span = Span::new(0, 0, 1, 1);
        for (stmt, file) in result.program.statements.iter().zip(&result.origins) {
            if let Stmt::Function { name, .. } = stmt {
                let (start, end) = stmt_byte_range(stmt);
                let span = Span::new(start, end, 1, 1);
                if name.lexeme() == "util_answer" {
                    lib_span = span;
                    assert_eq!(file, &lib);
                } else if name.lexeme() == "main" {
                    main_span = span;
                    assert_eq!(file, &main);
                }
            }
        }
        assert_eq!(result.file_for_span(lib_span), Some(lib.as_path()));
        assert_eq!(result.file_for_span(main_span), Some(main.as_path()));
    }

    /// Regression: merged byte ranges used to stay file-local, so
    /// top-level statements from different files could occupy overlapping
    /// byte ranges and a span could be attributed to the wrong file.
    /// Per-module shift bases make merged ranges globally unique.
    ///
    /// l.nt's `"oops"` literal spans bytes [13, 19) (line 2 col 13);
    /// main.nt's line-2 `say("hi")` statement spans bytes [11, 20) in its
    /// own coordinates — an overlapping range that used to steal the
    /// attribution.
    #[test]
    fn attributes_spans_that_collide_across_files() {
        let project = TestProject::new();
        project.write("src/main.nt", "use \"l.nt\"\nsay(\"hi\")\n");
        project.write("src/l.nt", "\nvar int x = \"oops\"\n");

        let result = load_program(&project.dir.join("src/main.nt")).expect("loads");
        let lib = project.dir.join("src/l.nt").canonicalize().unwrap();
        let main = project.dir.join("src/main.nt").canonicalize().unwrap();

        let mut oops_span = Span::dummy();
        let mut say_span = Span::dummy();
        for stmt in &result.program.statements {
            match stmt {
                Stmt::Var {
                    initializer:
                        Some(Expr::Literal {
                            value: ntsc_ast::expr::LiteralValue::String(s),
                            span,
                        }),
                    ..
                } if s == "oops" => oops_span = *span,
                Stmt::Say {
                    expression:
                        Expr::Literal {
                            value: ntsc_ast::expr::LiteralValue::String(s),
                            span,
                        },
                    ..
                } if s == "hi" => say_span = *span,
                _ => {}
            }
        }
        assert_ne!(oops_span, Span::dummy());
        assert_ne!(say_span, Span::dummy());

        assert_eq!(oops_span, Span::new(13, 19, 2, 13));

        // l.nt is merged first (base 0), so it keeps its file-local
        // offsets. The lib span must be attributed to lib, not to main's
        // colliding line.
        assert_eq!(result.file_for_span(oops_span), Some(lib.as_path()));
        assert_eq!(result.file_for_span(say_span), Some(main.as_path()));

        // Rebase back to file-local byte coordinates.
        assert_eq!(result.localize(oops_span), Some((lib, 0)));
        let (path, base) = result.localize(say_span).expect("say span localizes");
        assert_eq!(path, main);
        assert_eq!(base, 21);
        assert_eq!(say_span.start.saturating_sub(base), 15);
        assert_eq!(say_span.end.saturating_sub(base), 19);
    }

    #[test]
    fn parse_errors_become_diagnostics() {
        let project = TestProject::new();
        project.write("src/main.nt", "fun main() {\n    say(\"hi\"\n}\n");

        let err = load_program(&project.dir.join("src/main.nt")).unwrap_err();
        let diags = err.into_diagnostics();
        assert!(!diags.is_empty());
        let parse = diags.first().unwrap();
        assert_eq!(parse.code.as_deref(), Some(codes::PARSE));
        assert_eq!(parse.labels.len(), 1);
        assert!(parse.file_path.is_some());
    }
}
