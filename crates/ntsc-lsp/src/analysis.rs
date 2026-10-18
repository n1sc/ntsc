//! Single-file analysis for the language server.
//!
//! [`analyze`] lexes, parses (with error recovery), and indexes one buffer:
//! it collects symbol definitions, resolves references against lexical
//! scopes, records call sites, and runs name resolution, type checking, and
//! linting for diagnostics. All later LSP queries (hover, definition,
//! completion, …) read the resulting [`FileAnalysis`].

use std::collections::HashMap;

use ntsc_ast::expr::{Expr, FunctionParam};
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{Program, Stmt};
use ntsc_ast::token::{Token, TokenKind};
use ntsc_ast::types::TypeAnnotation;
use ntsc_parser::ParseError;
use ntsc_typeck::{ResolveError, TypeError, Warning};

/// The kind of a defined symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    AsyncFunction,
    Class,
    Method,
    Field,
    Variable,
    Parameter,
    Enum,
    EnumMember,
    Trait,
    TypeAlias,
    Test,
    Module,
}

impl SymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::AsyncFunction => "async function",
            Self::Class => "class",
            Self::Method => "method",
            Self::Field => "field",
            Self::Variable => "variable",
            Self::Parameter => "parameter",
            Self::Enum => "enum",
            Self::EnumMember => "enum member",
            Self::Trait => "trait",
            Self::TypeAlias => "type alias",
            Self::Test => "test",
            Self::Module => "module",
        }
    }
}

/// A single definition in the file.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Span of the defining name token.
    pub name_span: Span,
    /// Human-readable signature, e.g. `fun add(int a, int b) -> int`.
    pub detail: String,
    /// Enclosing class/trait name for members.
    pub container: Option<String>,
    /// Byte range in which this definition is visible; globals cover the
    /// whole file.
    pub scope_range: (usize, usize),
    /// Owning symbol (class for members, function for parameters).
    pub parent: Option<usize>,
    /// Parameter list for functions and methods: `(name, type)`.
    pub params: Vec<(String, String)>,
    /// Declared return type, if any.
    pub return_type: Option<String>,
    /// Byte range of the symbol's body (functions, methods, tests, classes).
    pub body_range: Option<(usize, usize)>,
    /// Comment block directly above the declaration, if any. Surfaced in
    /// hover and completion.
    pub docs: String,
}

/// A use of a name: either a definition site or a reference to one.
#[derive(Debug, Clone)]
pub struct Reference {
    pub name: String,
    pub span: Span,
    /// Resolved definition, if the name could be bound lexically.
    pub target: Option<usize>,
    pub is_definition: bool,
    /// Whether this is a member access (`object.name`).
    pub is_member: bool,
}

/// A function-call site, used for signature help and inlay hints.
#[derive(Debug, Clone)]
pub struct CallSite {
    pub callee_name: Option<String>,
    pub paren_span: Span,
    pub arg_spans: Vec<Span>,
    pub full_span: Span,
}

/// Everything the server knows about one buffer revision.
#[derive(Debug, Clone)]
pub struct FileAnalysis {
    pub text: String,
    pub tokens: Vec<Token>,
    pub program: Option<Program>,
    pub parse_errors: Vec<ParseError>,
    pub resolve_errors: Vec<ResolveError>,
    pub type_errors: Vec<TypeError>,
    pub warnings: Vec<Warning>,
    pub symbols: Vec<Symbol>,
    pub references: Vec<Reference>,
    pub calls: Vec<CallSite>,
    /// Byte ranges of every statement and expression subtree, innermost
    /// last; backs `textDocument/selectionRange`.
    pub struct_ranges: Vec<(usize, usize)>,
}

impl FileAnalysis {
    /// The name and span of the identifier (or member) at `offset`, if any.
    pub fn identifier_at(&self, offset: usize) -> Option<(&str, Span, bool)> {
        let mut best: Option<&Reference> = None;
        for reference in &self.references {
            if reference.span.start <= offset && offset <= reference.span.end {
                let width = reference.span.end.saturating_sub(reference.span.start);
                let is_better = best.is_none_or(|current| {
                    width < current.span.end.saturating_sub(current.span.start)
                });
                if is_better {
                    best = Some(reference);
                }
            }
        }
        if let Some(reference) = best {
            return Some((reference.name.as_str(), reference.span, reference.is_member));
        }
        for token in &self.tokens {
            if token.span.start <= offset
                && offset <= token.span.end
                && matches!(token.kind, TokenKind::Identifier(_))
            {
                return Some((token.lexeme(), token.span, false));
            }
        }
        None
    }

    /// The keyword under `offset`, if any. Keywords are separate token kinds
    /// rather than identifiers, so [`Self::identifier_at`] skips them; hover
    /// still documents them.
    pub fn keyword_at(&self, offset: usize) -> Option<&'static str> {
        self.tokens
            .iter()
            .find(|token| token.span.start <= offset && offset <= token.span.end)
            .and_then(|token| token.kind.keyword_lexeme())
    }

    /// Resolve the definition targeted by the identifier at `offset`.
    pub fn definition_at(&self, offset: usize) -> Option<&Symbol> {
        let (name, _, is_member) = self.identifier_at(offset)?;
        let reference = self.references.iter().find(|reference| {
            reference.span.start <= offset && offset <= reference.span.end && reference.name == name
        });
        if let Some(target) = reference.and_then(|reference| reference.target) {
            return self.symbols.get(target);
        }
        if is_member {
            return self.member_by_name(name);
        }
        self.symbols.iter().find(|symbol| symbol.name == name)
    }

    /// All member (method/field/enum-member) symbols with `name`.
    pub fn members_by_name(&self, name: &str) -> Vec<&Symbol> {
        self.symbols
            .iter()
            .filter(|symbol| {
                symbol.name == name
                    && matches!(
                        symbol.kind,
                        SymbolKind::Method | SymbolKind::Field | SymbolKind::EnumMember
                    )
            })
            .collect()
    }

    fn member_by_name(&self, name: &str) -> Option<&Symbol> {
        self.members_by_name(name).into_iter().next()
    }

    /// Every reference resolving to `symbol_idx`, plus the definition itself.
    pub fn references_to(&self, symbol_idx: usize) -> Vec<Span> {
        let mut spans = Vec::new();
        if let Some(symbol) = self.symbols.get(symbol_idx) {
            spans.push(symbol.name_span);
        }
        for reference in &self.references {
            if reference.target == Some(symbol_idx) && !reference.is_definition {
                spans.push(reference.span);
            }
        }
        spans
    }

    /// Symbols visible at `offset`: globals plus locals whose scope covers
    /// the position and which are defined before it.
    pub fn visible_symbols_at(&self, offset: usize) -> Vec<&Symbol> {
        self.symbols
            .iter()
            .filter(|symbol| {
                let (start, end) = symbol.scope_range;
                let is_global = start == 0 && end == usize::MAX;
                if is_global {
                    return true;
                }
                start <= offset && offset <= end && symbol.name_span.start <= offset
            })
            .collect()
    }

    /// Innermost function/method/test whose body contains `offset`.
    pub fn enclosing_function(&self, offset: usize) -> Option<&Symbol> {
        let mut best: Option<&Symbol> = None;
        for symbol in &self.symbols {
            let is_callable = matches!(
                symbol.kind,
                SymbolKind::Function
                    | SymbolKind::AsyncFunction
                    | SymbolKind::Method
                    | SymbolKind::Test
            );
            if !is_callable {
                continue;
            }
            if let Some((start, end)) = symbol.body_range
                && start <= offset
                && offset <= end
            {
                let width = end.saturating_sub(start);
                let is_better = best.is_none_or(|current| {
                    let (current_start, current_end) =
                        current.body_range.unwrap_or((0, usize::MAX));
                    width < current_end.saturating_sub(current_start)
                });
                if is_better {
                    best = Some(symbol);
                }
            }
        }
        best
    }

    /// Selection chain at `offset`: structural ranges containing it,
    /// smallest first.
    pub fn selection_chain(&self, offset: usize) -> Vec<(usize, usize)> {
        let mut chain: Vec<(usize, usize)> = self
            .struct_ranges
            .iter()
            .copied()
            .filter(|(start, end)| *start <= offset && offset <= *end && *start < *end)
            .collect();
        chain.sort_by_key(|(start, end)| end.saturating_sub(*start));
        chain.dedup();
        chain
    }

    /// Innermost call whose parenthesis range contains `offset`.
    pub fn enclosing_call(&self, offset: usize) -> Option<&CallSite> {
        let mut best: Option<&CallSite> = None;
        for call in &self.calls {
            let span = call.paren_span;
            if is_real_span(span) && span.start <= offset {
                let width = call.full_span.end.saturating_sub(call.full_span.start);
                let is_better = best.is_none_or(|current: &CallSite| {
                    width
                        < current
                            .full_span
                            .end
                            .saturating_sub(current.full_span.start)
                });
                if is_better {
                    best = Some(call);
                }
            }
        }
        best
    }

    /// Function or method symbol with `name` visible at `offset`.
    pub fn callable_at(&self, name: &str, offset: usize) -> Option<&Symbol> {
        self.visible_symbols_at(offset).into_iter().find(|symbol| {
            symbol.name == name
                && matches!(
                    symbol.kind,
                    SymbolKind::Function | SymbolKind::AsyncFunction | SymbolKind::Method
                )
        })
    }
}

/// Analyze one buffer revision.
pub fn analyze(text: &str) -> FileAnalysis {
    let tokens = ntsc_lexer::tokenize(text);
    let (program, parse_errors) = ntsc_parser::parse_recovering(&tokens);

    let mut resolve_errors = Vec::new();
    let mut type_errors = Vec::new();
    let mut warnings = Vec::new();
    if parse_errors.is_empty() {
        if let Err(errors) = ntsc_typeck::resolve_program(&program) {
            resolve_errors = errors;
        }
        if resolve_errors.is_empty() {
            if let Err(errors) = ntsc_typeck::check_program(&program) {
                type_errors = errors;
            }
            warnings = ntsc_typeck::lint_program(&program);
        }
    } else if !program.statements.is_empty() {
        // Still lint the recovered prefix so unused-variable hints survive
        // while the user is typing; resolve/type errors on a partial tree
        // would be mostly noise, so they stay suppressed until the file
        // parses cleanly.
        warnings = ntsc_typeck::lint_program(&program);
    }

    let mut indexer = Indexer {
        comments: collect_comment_runs(text),
        ..Indexer::default()
    };
    indexer.index_program(&program);

    FileAnalysis {
        text: text.to_string(),
        tokens,
        program: Some(program),
        parse_errors,
        resolve_errors,
        type_errors,
        warnings,
        symbols: indexer.symbols,
        references: indexer.references,
        calls: indexer.calls,
        struct_ranges: indexer.struct_ranges,
    }
}

fn is_real_span(span: Span) -> bool {
    span.start != 0 || span.end != 0
}

fn contains(span: Span, offset: usize) -> bool {
    is_real_span(span) && span.start <= offset && offset <= span.end
}

/// Render a type annotation as source-level text.
pub fn type_to_string(ty: &TypeAnnotation) -> String {
    match ty {
        TypeAnnotation::Int => "int".to_string(),
        TypeAnnotation::Float => "float".to_string(),
        TypeAnnotation::String => "string".to_string(),
        TypeAnnotation::Bool => "bool".to_string(),
        TypeAnnotation::Array(None) => "array".to_string(),
        TypeAnnotation::Array(Some(inner)) => format!("array[{}]", type_to_string(inner)),
        TypeAnnotation::Object => "object".to_string(),
        TypeAnnotation::Option(inner) => format!("option[{}]", type_to_string(inner)),
        TypeAnnotation::Result { ok, err } => {
            format!("result[{}, {}]", type_to_string(ok), type_to_string(err))
        }
        TypeAnnotation::View(inner, true) => format!("view mut {}", type_to_string(inner)),
        TypeAnnotation::View(inner, false) => format!("view {}", type_to_string(inner)),
        TypeAnnotation::Shared(inner) => format!("shared {}", type_to_string(inner)),
        TypeAnnotation::Any => "any".to_string(),
        TypeAnnotation::Pointer => "pointer".to_string(),
        TypeAnnotation::Slice(None) => "slice".to_string(),
        TypeAnnotation::Slice(Some(inner)) => format!("slice[{}]", type_to_string(inner)),
        TypeAnnotation::Own(inner) => format!("own {}", type_to_string(inner)),
        TypeAnnotation::Ref(inner, true) => format!("&mut {}", type_to_string(inner)),
        TypeAnnotation::Ref(inner, false) => format!("&{}", type_to_string(inner)),
        TypeAnnotation::RawPointer(inner, true) => {
            format!("*mut {}", type_to_string(inner))
        }
        TypeAnnotation::RawPointer(inner, false) => {
            format!("*const {}", type_to_string(inner))
        }
        TypeAnnotation::Named(token) => token.lexeme().to_string(),
        TypeAnnotation::ImplTrait(token) => format!("impl {}", token.lexeme()),
        TypeAnnotation::Dyn(token) => format!("dyn {}", token.lexeme()),
        TypeAnnotation::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(type_to_string).collect();
            format!("({})", inner.join(", "))
        }
        TypeAnnotation::Chan(inner) => format!("chan[{}]", type_to_string(inner)),
    }
}

fn param_to_string(param: &FunctionParam) -> String {
    match &param.type_annotation {
        Some(ty) => format!("{} {}", type_to_string(ty), param.name.lexeme()),
        None => param.name.lexeme().to_string(),
    }
}

fn function_detail(
    keyword: &str,
    name: &str,
    params: &[FunctionParam],
    return_type: Option<&ntsc_ast::types::ReturnType>,
) -> String {
    let params: Vec<String> = params.iter().map(param_to_string).collect();
    let mut detail = format!("{keyword} {}({})", name, params.join(", "));
    if let Some(return_type) = return_type {
        detail.push_str(&format!(" -> {}", type_to_string(&return_type.ty)));
    }
    detail
}

// ── Indexer ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct ScopeFrame {
    start: usize,
    end: usize,
    owner: Option<usize>,
    definitions: HashMap<String, usize>,
}

struct Indexer {
    symbols: Vec<Symbol>,
    references: Vec<Reference>,
    calls: Vec<CallSite>,
    struct_ranges: Vec<(usize, usize)>,
    scopes: Vec<ScopeFrame>,
    current_class: Option<String>,
    current_class_symbol: Option<usize>,
    /// Comment runs as `(end offset, end line, text)`, used to attach docs
    /// to the declaration that follows them.
    comments: Vec<(usize, u32, String)>,
}

impl Default for Indexer {
    fn default() -> Self {
        Self {
            symbols: Vec::new(),
            references: Vec::new(),
            calls: Vec::new(),
            struct_ranges: Vec::new(),
            scopes: vec![ScopeFrame {
                start: 0,
                end: usize::MAX,
                owner: None,
                definitions: HashMap::new(),
            }],
            current_class: None,
            current_class_symbol: None,
            comments: Vec::new(),
        }
    }
}

/// Extract runs of adjacent comment lines from the source: the lexer
/// discards comments, so doc blocks have to be read from the text.
///
/// Returns `(end offset, end line, text)` per run. String literals are
/// skipped so a `//` inside a string is not mistaken for a comment.
fn collect_comment_runs(text: &str) -> Vec<(usize, u32, String)> {
    let bytes = text.as_bytes();
    let mut runs: Vec<(usize, u32, String)> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut run_end = 0usize;
    let mut run_line = 0u32;
    let mut line = 1u32;
    let mut index = 0usize;

    // Record the pending run, if any, and start a fresh one.
    macro_rules! flush {
        () => {
            if !current.is_empty() {
                runs.push((run_end, run_line, current.join("\n")));
                current.clear();
            }
        };
    }

    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'\n' => {
                line += 1;
                index += 1;
            }
            b'"' | b'\'' => {
                // Raw strings (`r"..."`) behave the same for our purpose.
                let quote = byte;
                index += 1;
                while index < bytes.len() && bytes[index] != quote && bytes[index] != b'\n' {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                let start = index + 2;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'\n' {
                    end += 1;
                }
                if !current.is_empty() && line > run_line + 1 {
                    flush!();
                }
                current.push(text[start..end].trim_start().to_string());
                run_end = end;
                run_line = line;
                index = end;
            }
            b'#' if bytes.get(index + 1) == Some(&b'{') => {
                let start = index + 2;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'}' {
                    if bytes[end] == b'\n' {
                        line += 1;
                    }
                    end += 1;
                }
                let end = (end + 1).min(bytes.len());
                if !current.is_empty() && line > run_line + 1 {
                    flush!();
                }
                let body = &text[start..end.saturating_sub(1).max(start)];
                current.push(
                    body.lines()
                        .map(|line| line.trim_start().trim_start_matches('#'))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                run_end = end;
                run_line = line;
                index = end;
            }
            _ => index += 1,
        }
    }
    flush!();
    runs
}

impl Indexer {
    /// The doc block immediately above `span`, if any.
    fn docs_above(&self, span: Span) -> String {
        if !is_real_span(span) {
            return String::new();
        }
        self.comments
            .iter()
            .rev()
            .find(|(end, line, _)| *end <= span.start && *line + 1 >= span.line)
            .map(|(_, _, text)| text.clone())
            .unwrap_or_default()
    }

    fn index_program(&mut self, program: &Program) {
        for stmt in &program.statements {
            self.declare_top_level(stmt);
        }
        for stmt in &program.statements {
            self.walk_stmt(stmt, None);
        }
    }

    /// First pass: bind top-level function/class/enum names (plus module
    /// aliases) so forward references resolve, mirroring the compiler's
    /// name-resolution order.
    fn declare_top_level(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Function {
                name,
                params,
                return_type,
                ..
            } => {
                self.declare_global(
                    name,
                    SymbolKind::Function,
                    function_detail("fun", name.lexeme(), params, return_type.as_ref()),
                    None,
                );
            }
            Stmt::AsyncFunction {
                name,
                params,
                return_type,
                ..
            } => {
                self.declare_global(
                    name,
                    SymbolKind::AsyncFunction,
                    function_detail("async fun", name.lexeme(), params, return_type.as_ref()),
                    None,
                );
            }
            Stmt::Class { name, .. } => {
                self.declare_global(
                    name,
                    SymbolKind::Class,
                    format!("class {}", name.lexeme()),
                    None,
                );
            }
            Stmt::Enum { name, members, .. } => {
                self.declare_global(
                    name,
                    SymbolKind::Enum,
                    format!("enum {}", name.lexeme()),
                    None,
                );
                for member in members {
                    self.declare_global(
                        &member.name,
                        SymbolKind::EnumMember,
                        format!("{}.{}", name.lexeme(), member.name.lexeme()),
                        None,
                    );
                }
            }
            Stmt::Trait { name, .. } => {
                self.declare_global(
                    name,
                    SymbolKind::Trait,
                    format!("trait {}", name.lexeme()),
                    None,
                );
            }
            Stmt::TypeAlias { name, target, .. } => {
                self.declare_global(
                    name,
                    SymbolKind::TypeAlias,
                    format!("type {} = {}", name.lexeme(), type_to_string(target)),
                    None,
                );
            }
            // A top-level `var` is a global: Neutron merges every module
            // into one flat namespace, so it is visible to sibling files
            // that import this one. Indexing it here is what keeps
            // cross-file references from looking undefined.
            Stmt::Var {
                name,
                type_annotation,
                is_static,
                is_const,
                ..
            } => {
                let keyword = match (is_static, is_const) {
                    (true, true) => "static const var",
                    (true, false) => "static var",
                    _ => "var",
                };
                let detail = match type_annotation {
                    Some(ty) => format!("{keyword} {}: {}", name.lexeme(), type_to_string(ty)),
                    None => format!("{keyword} {}", name.lexeme()),
                };
                self.declare_global(name, SymbolKind::Variable, detail, None);
            }
            Stmt::Test { name, .. } => {
                self.declare_global(
                    name,
                    SymbolKind::Test,
                    format!("test {}", name.lexeme()),
                    None,
                );
            }
            Stmt::Use {
                library,
                is_file_path,
                alias,
                imported_symbols,
                ..
            } => {
                if imported_symbols.is_empty() {
                    if *is_file_path {
                        if let Some(alias) = alias {
                            self.declare_global(
                                alias,
                                SymbolKind::Module,
                                format!("use \"{}\" as {}", library.lexeme(), alias.lexeme()),
                                None,
                            );
                        }
                    } else if let Some(alias) = alias {
                        self.declare_global(
                            alias,
                            SymbolKind::Module,
                            format!("use {} as {}", library.lexeme(), alias.lexeme()),
                            None,
                        );
                    } else {
                        self.declare_global(
                            library,
                            SymbolKind::Module,
                            format!("use {}", library.lexeme()),
                            None,
                        );
                    }
                } else {
                    for symbol in imported_symbols {
                        self.declare_global(
                            symbol,
                            SymbolKind::Variable,
                            format!("use {} from {}", symbol.lexeme(), library.lexeme()),
                            None,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn declare_global(
        &mut self,
        name_token: &Token,
        kind: SymbolKind,
        detail: String,
        container: Option<String>,
    ) {
        self.declare(
            name_token,
            kind,
            detail,
            container,
            (0, usize::MAX),
            None,
            &[],
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn declare(
        &mut self,
        name_token: &Token,
        kind: SymbolKind,
        detail: String,
        container: Option<String>,
        scope_range: (usize, usize),
        parent: Option<usize>,
        params: &[(String, String)],
        return_type: Option<String>,
    ) -> usize {
        let idx = self.symbols.len();
        let name = name_token.lexeme().to_string();
        let docs = self.docs_above(name_token.span);
        self.symbols.push(Symbol {
            name: name.clone(),
            kind,
            name_span: name_token.span,
            detail,
            container,
            scope_range,
            parent,
            params: params.to_vec(),
            return_type,
            body_range: None,
            docs,
        });
        if is_real_span(name_token.span) {
            self.references.push(Reference {
                name,
                span: name_token.span,
                target: Some(idx),
                is_definition: true,
                is_member: matches!(kind, SymbolKind::Method | SymbolKind::Field),
            });
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope
                .definitions
                .entry(name_token.lexeme().to_string())
                .or_insert(idx);
        }
        idx
    }

    fn resolve(&mut self, name: &str, span: Span, is_member: bool) -> Option<usize> {
        if !is_real_span(span) {
            return None;
        }
        let target = if is_member {
            self.symbols.iter().position(|symbol| {
                symbol.name == name
                    && matches!(
                        symbol.kind,
                        SymbolKind::Method | SymbolKind::Field | SymbolKind::EnumMember
                    )
            })
        } else {
            self.scopes
                .iter()
                .rev()
                .find_map(|scope| scope.definitions.get(name).copied())
        };
        self.references.push(Reference {
            name: name.to_string(),
            span,
            target,
            is_definition: false,
            is_member,
        });
        target
    }

    fn push_scope(&mut self, range: (usize, usize), owner: Option<usize>) {
        self.scopes.push(ScopeFrame {
            start: range.0,
            end: range.1,
            owner,
            definitions: HashMap::new(),
        });
    }

    fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            let _ = self.scopes.pop();
        }
    }

    fn current_range(&self) -> (usize, usize) {
        if let Some(scope) = self.scopes.last() {
            (scope.start, scope.end)
        } else {
            (0, usize::MAX)
        }
    }

    fn current_owner(&self) -> Option<usize> {
        self.scopes.last().and_then(|scope| scope.owner)
    }

    // ── Statements ──

    fn walk_stmt(&mut self, stmt: &Stmt, in_class: Option<&str>) {
        let subtree = stmt_subtree_range(stmt);
        if subtree.0 < subtree.1 {
            self.struct_ranges.push(subtree);
        }
        match stmt {
            Stmt::Expression { expression } => self.walk_expr(expression),
            Stmt::Say { expression, .. } => self.walk_expr(expression),
            Stmt::Throw { value } => self.walk_expr(value),
            Stmt::Var {
                name,
                type_annotation,
                initializer,
                is_const,
                ..
            } => {
                if let Some(initializer) = initializer {
                    self.walk_expr(initializer);
                }
                if let Some(ty) = type_annotation {
                    self.walk_type(ty);
                }
                if let Some(class_name) = in_class {
                    let detail = match type_annotation {
                        Some(ty) => {
                            format!("{}.{}: {}", class_name, name.lexeme(), type_to_string(ty))
                        }
                        None => format!("{}.{}", class_name, name.lexeme()),
                    };
                    self.declare(
                        name,
                        SymbolKind::Field,
                        detail,
                        Some(class_name.to_string()),
                        self.current_range(),
                        self.current_class_symbol,
                        &[],
                        None,
                    );
                } else {
                    let detail = match type_annotation {
                        Some(ty) => format!("var {}: {}", name.lexeme(), type_to_string(ty)),
                        None => format!("var {}", name.lexeme()),
                    };
                    let kind_detail = if *is_const {
                        format!("const {detail}")
                    } else {
                        detail
                    };
                    let parent = self.current_owner();
                    self.declare(
                        name,
                        SymbolKind::Variable,
                        kind_detail,
                        None,
                        self.current_range(),
                        parent,
                        &[],
                        None,
                    );
                }
            }
            Stmt::Destructure {
                names, initializer, ..
            } => {
                self.walk_expr(initializer);
                let parent = self.current_owner();
                let range = self.current_range();
                for name in names {
                    self.declare(
                        name,
                        SymbolKind::Variable,
                        format!("var {}", name.lexeme()),
                        None,
                        range,
                        parent,
                        &[],
                        None,
                    );
                }
            }
            Stmt::Block {
                statements,
                open_span,
                close_span,
            } => {
                let range = block_range(*open_span, *close_span, statements);
                self.push_scope(range, None);
                for stmt in statements {
                    self.walk_stmt(stmt, in_class);
                }
                self.pop_scope();
            }
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_stmt(then_branch, in_class);
                for branch in elif_branches {
                    self.walk_expr(&branch.condition);
                    self.walk_stmt(&branch.body, in_class);
                }
                if let Some(else_branch) = else_branch {
                    self.walk_stmt(else_branch, in_class);
                }
            }
            Stmt::While { condition, body } | Stmt::DoWhile { condition, body } => {
                self.walk_expr(condition);
                self.walk_stmt(body, in_class);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                let range = stmt_subtree_range(stmt);
                self.push_scope(range, None);
                if let Some(init) = init {
                    self.walk_stmt(init, in_class);
                }
                if let Some(condition) = condition {
                    self.walk_expr(condition);
                }
                if let Some(update) = update {
                    self.walk_expr(update);
                }
                self.walk_stmt(body, in_class);
                self.pop_scope();
            }
            Stmt::ForIn {
                variable,
                iterable,
                body,
            }
            | Stmt::ForAwait {
                variable,
                producer: iterable,
                body,
            }
            | Stmt::ChanRecvFor {
                variable,
                channel: iterable,
                body,
            } => {
                self.walk_expr(iterable);
                let range = stmt_subtree_range(stmt);
                self.push_scope(range, None);
                let range = self.current_range();
                let parent = self.current_owner();
                self.declare(
                    variable,
                    SymbolKind::Variable,
                    format!("var {}", variable.lexeme()),
                    None,
                    range,
                    parent,
                    &[],
                    None,
                );
                self.walk_stmt(body, in_class);
                self.pop_scope();
            }
            Stmt::Go { call, block, .. } => {
                self.walk_expr(call);
                if let Some(block) = block {
                    let range = stmts_range(block);
                    self.push_scope(range, None);
                    for stmt in block {
                        self.walk_stmt(stmt, in_class);
                    }
                    self.pop_scope();
                }
            }
            Stmt::Function {
                name,
                params,
                return_type,
                body,
                ..
            }
            | Stmt::AsyncFunction {
                name,
                params,
                return_type,
                body,
            } => {
                let is_async = matches!(stmt, Stmt::AsyncFunction { .. });
                let kind = if in_class.is_some() {
                    SymbolKind::Method
                } else if is_async {
                    SymbolKind::AsyncFunction
                } else {
                    SymbolKind::Function
                };
                let keyword = if in_class.is_some() {
                    "fun"
                } else if is_async {
                    "async fun"
                } else {
                    "fun"
                };
                let detail = function_detail(keyword, name.lexeme(), params, return_type.as_ref());
                let param_info: Vec<(String, String)> = params
                    .iter()
                    .map(|param| {
                        (
                            param.name.lexeme().to_string(),
                            param
                                .type_annotation
                                .as_ref()
                                .map_or_else(|| "any".to_string(), type_to_string),
                        )
                    })
                    .collect();
                let return_info = return_type
                    .as_ref()
                    .map(|return_type| type_to_string(&return_type.ty));
                // The declaration already exists for top-level items; nested
                // functions and methods are declared here.
                let body_range = stmts_range(body);
                let symbol_idx = if in_class.is_some() {
                    let class_name = in_class.map(str::to_string);
                    Some(self.declare(
                        name,
                        kind,
                        detail,
                        class_name,
                        self.current_range(),
                        self.current_class_symbol,
                        &param_info,
                        return_info,
                    ))
                } else if self.lookup(name.lexeme()).is_none() {
                    Some(self.declare(
                        name,
                        kind,
                        detail,
                        None,
                        self.current_range(),
                        self.current_owner(),
                        &param_info,
                        return_info,
                    ))
                } else {
                    // Enrich the forward-declared global with its signature.
                    let idx = self.lookup(name.lexeme());
                    if let Some(idx) = idx
                        && let Some(symbol) = self.symbols.get_mut(idx)
                    {
                        symbol.params = param_info.clone();
                        symbol.return_type = return_info.clone();
                    }
                    idx
                };
                if let Some(idx) = symbol_idx
                    && let Some(symbol) = self.symbols.get_mut(idx)
                {
                    symbol.body_range = Some(body_range);
                }
                let range = body_range;
                self.push_scope(range, symbol_idx);
                for param in params {
                    if let Some(ty) = &param.type_annotation {
                        self.walk_type(ty);
                    }
                    let detail = match &param.type_annotation {
                        Some(ty) => format!("{}: {}", param.name.lexeme(), type_to_string(ty)),
                        None => param.name.lexeme().to_string(),
                    };
                    self.declare(
                        &param.name,
                        SymbolKind::Parameter,
                        detail,
                        None,
                        range,
                        symbol_idx,
                        &[],
                        None,
                    );
                }
                for stmt in body {
                    self.walk_stmt(stmt, None);
                }
                self.pop_scope();
            }
            Stmt::ExternFunction {
                name,
                params,
                return_type,
                ..
            } => {
                for param in params {
                    if let Some(ty) = &param.type_annotation {
                        self.walk_type(ty);
                    }
                }
                if let Some(return_type) = return_type {
                    self.walk_type(&return_type.ty);
                }
                if self.lookup(name.lexeme()).is_none() {
                    let param_info = params
                        .iter()
                        .map(|param| {
                            (
                                param.name.lexeme().to_string(),
                                param
                                    .type_annotation
                                    .as_ref()
                                    .map_or_else(|| "any".to_string(), type_to_string),
                            )
                        })
                        .collect::<Vec<_>>();
                    let return_info = return_type
                        .as_ref()
                        .map(|return_type| type_to_string(&return_type.ty));
                    let detail =
                        function_detail("extern fun", name.lexeme(), params, return_type.as_ref());
                    self.declare(
                        name,
                        SymbolKind::Function,
                        detail,
                        None,
                        self.current_range(),
                        self.current_owner(),
                        &param_info,
                        return_info,
                    );
                }
            }
            Stmt::ExternBlock { functions, .. } => {
                for function in functions {
                    self.walk_stmt(function, in_class);
                }
            }
            Stmt::Return { value } => {
                if let Some(value) = value {
                    self.walk_expr(value);
                }
            }
            Stmt::Class {
                name, parent, body, ..
            } => {
                if let Some(parent) = parent {
                    self.resolve(parent.lexeme(), parent.span, false);
                }
                let class_idx = self.lookup(name.lexeme());
                if let Some(idx) = class_idx
                    && let Some(symbol) = self.symbols.get_mut(idx)
                {
                    symbol.body_range = Some(stmts_range(body));
                }
                let previous_class = self.current_class.clone();
                let previous_symbol = self.current_class_symbol;
                self.current_class = Some(name.lexeme().to_string());
                self.current_class_symbol = class_idx;
                let range = stmts_range(body);
                self.push_scope(range, class_idx);
                // `this` is always in scope inside a class body.
                if let Some(scope) = self.scopes.last_mut()
                    && let Some(idx) = class_idx
                {
                    scope.definitions.entry("this".to_string()).or_insert(idx);
                }
                for member in body {
                    if let Stmt::Function { name, .. } | Stmt::AsyncFunction { name, .. } = member {
                        let _ = name;
                    }
                    self.walk_stmt(member, Some(name.lexeme()));
                }
                self.pop_scope();
                self.current_class = previous_class;
                self.current_class_symbol = previous_symbol;
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => {
                self.walk_expr(expression);
                for case in cases {
                    match &case.pattern {
                        Some(pattern) => {
                            let range = stmt_subtree_range(&case.body);
                            self.push_scope(range, None);
                            if let Some(binding) = &pattern.binding {
                                let range = self.current_range();
                                let parent = self.current_owner();
                                self.declare(
                                    binding,
                                    SymbolKind::Variable,
                                    format!("var {}", binding.lexeme()),
                                    None,
                                    range,
                                    parent,
                                    &[],
                                    None,
                                );
                            }
                            self.walk_stmt(&case.body, in_class);
                            self.pop_scope();
                        }
                        None => {
                            self.walk_expr(&case.value);
                            if let Some(guard) = &case.guard {
                                self.walk_expr(guard);
                            }
                            self.walk_stmt(&case.body, in_class);
                        }
                    }
                }
                if let Some(default_case) = default_case {
                    self.walk_stmt(default_case, in_class);
                }
            }
            Stmt::Try {
                try_block,
                catch_var,
                catch_block,
                finally_block,
            } => {
                self.walk_stmt(try_block, in_class);
                if let Some(catch_block) = catch_block {
                    let range = stmt_subtree_range(catch_block);
                    self.push_scope(range, None);
                    if let Some(catch_var) = catch_var {
                        let range = self.current_range();
                        let parent = self.current_owner();
                        self.declare(
                            catch_var,
                            SymbolKind::Variable,
                            format!("var {}", catch_var.lexeme()),
                            None,
                            range,
                            parent,
                            &[],
                            None,
                        );
                    }
                    self.walk_stmt(catch_block, in_class);
                    self.pop_scope();
                }
                if let Some(finally_block) = finally_block {
                    self.walk_stmt(finally_block, in_class);
                }
            }
            Stmt::Retry {
                count,
                body,
                catch_var,
                catch_block,
            } => {
                self.walk_expr(count);
                self.walk_stmt(body, in_class);
                if let Some(catch_block) = catch_block {
                    let range = stmt_subtree_range(catch_block);
                    self.push_scope(range, None);
                    if let Some(catch_var) = catch_var {
                        let range = self.current_range();
                        let parent = self.current_owner();
                        self.declare(
                            catch_var,
                            SymbolKind::Variable,
                            format!("var {}", catch_var.lexeme()),
                            None,
                            range,
                            parent,
                            &[],
                            None,
                        );
                    }
                    self.walk_stmt(catch_block, in_class);
                    self.pop_scope();
                }
            }
            Stmt::Unsafe { body } => self.walk_stmt(body, in_class),
            Stmt::Quiet { body, .. } => self.walk_stmt(body, in_class),
            Stmt::Use { .. } | Stmt::Enum { .. } | Stmt::TypeAlias { .. } => {}
            Stmt::Trait {
                name,
                parents,
                methods,
                ..
            } => {
                for parent in parents {
                    self.resolve(parent.lexeme(), parent.span, false);
                }
                let trait_name = name.lexeme().to_string();
                let trait_idx = self.lookup(&trait_name);
                for method in methods {
                    self.walk_stmt(method, Some(&trait_name));
                }
                let _ = trait_idx;
            }
            Stmt::Impl {
                trait_name,
                type_name,
                body,
            } => {
                self.resolve(trait_name.lexeme(), trait_name.span, false);
                self.resolve(type_name.lexeme(), type_name.span, false);
                for member in body {
                    self.walk_stmt(member, None);
                }
            }
            Stmt::Test { name, body } => {
                if let Some(idx) = self.lookup(name.lexeme())
                    && let Some(symbol) = self.symbols.get_mut(idx)
                {
                    symbol.body_range = Some(stmts_range(body));
                }
                let range = stmts_range(body);
                self.push_scope(range, None);
                for stmt in body {
                    self.walk_stmt(stmt, None);
                }
                self.pop_scope();
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.definitions.get(name).copied())
    }

    // ── Expressions ──

    fn walk_expr(&mut self, expr: &Expr) {
        let span = expr.span();
        if is_real_span(span) && span.start < span.end {
            self.struct_ranges.push((span.start, span.end));
        }
        match expr {
            Expr::Literal { .. } => {}
            Expr::Variable { name } => {
                let lexeme = name.lexeme().trim_start_matches('@');
                self.resolve(lexeme, name.span, false);
            }
            Expr::This { keyword } => {
                self.resolve("this", keyword.span, false);
            }
            Expr::Assign { name, value } => {
                self.resolve(name.lexeme(), name.span, false);
                self.walk_expr(value);
            }
            Expr::Unary { right, .. } | Expr::Spread { value: right, .. } => {
                self.walk_expr(right);
            }
            Expr::PostfixUnary { left, .. } => self.walk_expr(left),
            Expr::Grouping { expression, .. } => self.walk_expr(expression),
            Expr::Binary { left, right, .. }
            | Expr::IndexGet {
                object: left,
                index: right,
            } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            Expr::Ternary {
                condition,
                then_branch,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_expr(then_branch);
                self.walk_expr(else_branch);
            }
            Expr::Call {
                callee,
                paren,
                arguments,
            } => {
                let callee_name = match callee.as_ref() {
                    Expr::Variable { name } => Some(name.lexeme().to_string()),
                    Expr::Member { property, .. } | Expr::OptionalMember { property, .. } => {
                        Some(property.lexeme().to_string())
                    }
                    _ => None,
                };
                self.calls.push(CallSite {
                    callee_name,
                    paren_span: *paren,
                    arg_spans: arguments.iter().map(Expr::span).collect(),
                    full_span: expr.span(),
                });
                self.walk_expr(callee);
                for argument in arguments {
                    self.walk_expr(argument);
                }
            }
            Expr::Await {
                callee, arguments, ..
            } => {
                self.walk_expr(callee);
                for argument in arguments {
                    self.walk_expr(argument);
                }
            }
            Expr::AsyncBlock { body, .. } => {
                let range = stmts_range(body);
                self.push_scope(range, None);
                for stmt in body {
                    self.walk_stmt(stmt, None);
                }
                self.pop_scope();
            }
            Expr::ChanSend { channel, value, .. } => {
                self.walk_expr(channel);
                self.walk_expr(value);
            }
            Expr::ChanRecv {
                receiver, channel, ..
            } => {
                self.walk_expr(channel);
                let range = self.current_range();
                let parent = self.current_owner();
                self.declare(
                    receiver,
                    SymbolKind::Variable,
                    format!("var {}", receiver.lexeme()),
                    None,
                    range,
                    parent,
                    &[],
                    None,
                );
            }
            Expr::Close { channel, .. } => self.walk_expr(channel),
            Expr::Member { object, property } | Expr::OptionalMember { object, property } => {
                self.walk_expr(object);
                self.resolve(property.lexeme(), property.span, true);
            }
            Expr::View { target, .. }
            | Expr::Borrow { target, .. }
            | Expr::RawDeref { target, .. }
            | Expr::Copy {
                expression: target, ..
            }
            | Expr::Propagate { value: target, .. } => self.walk_expr(target),
            Expr::RawDerefSet { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            Expr::IndexSet {
                object,
                index,
                value,
            } => {
                self.walk_expr(object);
                self.walk_expr(index);
                self.walk_expr(value);
            }
            Expr::MemberSet {
                object,
                property,
                value,
            } => {
                self.walk_expr(object);
                self.resolve(property.lexeme(), property.span, true);
                self.walk_expr(value);
            }
            Expr::Lambda {
                params, body, span, ..
            } => {
                for param in params {
                    if let Some(ty) = &param.type_annotation {
                        self.walk_type(ty);
                    }
                }
                let range = if is_real_span(*span) {
                    (span.start, span.end)
                } else {
                    stmts_range(body)
                };
                self.push_scope(range, None);
                for param in params {
                    let detail = match &param.type_annotation {
                        Some(ty) => format!("{}: {}", param.name.lexeme(), type_to_string(ty)),
                        None => param.name.lexeme().to_string(),
                    };
                    self.declare(
                        &param.name,
                        SymbolKind::Parameter,
                        detail,
                        None,
                        range,
                        None,
                        &[],
                        None,
                    );
                }
                for stmt in body {
                    self.walk_stmt(stmt, None);
                }
                self.pop_scope();
            }
            Expr::ObjectLiteral { properties, .. } => {
                for property in properties {
                    self.walk_expr(&property.value);
                }
            }
            Expr::ArrayLiteral { elements, .. } => {
                for element in elements {
                    self.walk_expr(element);
                }
            }
            Expr::StructLiteral {
                class_name,
                fields,
                update,
                ..
            } => {
                self.resolve(class_name.lexeme(), class_name.span, false);
                for field in fields {
                    self.walk_expr(&field.value);
                }
                if let Some(update) = update {
                    self.walk_expr(update);
                }
            }
            Expr::TupleLiteral { elements, .. } => {
                for element in elements {
                    self.walk_expr(element);
                }
            }
            Expr::TupleIndex { object, .. } => self.walk_expr(object),
        }
    }

    fn walk_type(&mut self, ty: &TypeAnnotation) {
        match ty {
            TypeAnnotation::Named(token)
            | TypeAnnotation::ImplTrait(token)
            | TypeAnnotation::Dyn(token) => {
                self.resolve(token.lexeme(), token.span, false);
            }
            TypeAnnotation::Array(Some(inner))
            | TypeAnnotation::Option(inner)
            | TypeAnnotation::Slice(Some(inner))
            | TypeAnnotation::Own(inner)
            | TypeAnnotation::Shared(inner)
            | TypeAnnotation::View(inner, _)
            | TypeAnnotation::Ref(inner, _)
            | TypeAnnotation::RawPointer(inner, _)
            | TypeAnnotation::Chan(inner) => self.walk_type(inner),
            TypeAnnotation::Result { ok, err } => {
                self.walk_type(ok);
                self.walk_type(err);
            }
            TypeAnnotation::Tuple(items) => {
                for item in items {
                    self.walk_type(item);
                }
            }
            _ => {}
        }
    }
}

// ── Span ranges ─────────────────────────────────────────────────────────────

fn span_union(range: (usize, usize), span: Span) -> (usize, usize) {
    if !is_real_span(span) {
        return range;
    }
    (range.0.min(span.start), range.1.max(span.end))
}

fn block_range(open: Span, close: Span, statements: &[Stmt]) -> (usize, usize) {
    let mut range = (usize::MAX, 0);
    range = span_union(range, open);
    range = span_union(range, close);
    for stmt in statements {
        let sub = stmt_subtree_range(stmt);
        range = (range.0.min(sub.0), range.1.max(sub.1));
    }
    if range.0 == usize::MAX {
        (0, usize::MAX)
    } else {
        range
    }
}

fn stmts_range(statements: &[Stmt]) -> (usize, usize) {
    let mut range = (usize::MAX, 0);
    for stmt in statements {
        let sub = stmt_subtree_range(stmt);
        range = (range.0.min(sub.0), range.1.max(sub.1));
    }
    if range.0 == usize::MAX {
        (0, usize::MAX)
    } else {
        range
    }
}

fn stmt_subtree_range(stmt: &Stmt) -> (usize, usize) {
    let mut spans = Vec::new();
    collect_stmt_spans(stmt, &mut spans);
    let mut range = (usize::MAX, 0);
    for span in spans {
        range = span_union(range, span);
    }
    if range.0 == usize::MAX { (0, 0) } else { range }
}

fn collect_stmt_spans(stmt: &Stmt, out: &mut Vec<Span>) {
    match stmt {
        Stmt::Expression { expression } | Stmt::Throw { value: expression } => {
            collect_expr_spans(expression, out);
        }
        Stmt::Say {
            expression,
            keyword_span,
        } => {
            out.push(*keyword_span);
            collect_expr_spans(expression, out);
        }
        Stmt::Var {
            name,
            type_annotation,
            initializer,
            ..
        } => {
            out.push(name.span);
            if let Some(ty) = type_annotation {
                collect_type_spans(ty, out);
            }
            if let Some(initializer) = initializer {
                collect_expr_spans(initializer, out);
            }
        }
        Stmt::Block {
            statements,
            open_span,
            close_span,
        } => {
            out.push(*open_span);
            out.push(*close_span);
            for stmt in statements {
                collect_stmt_spans(stmt, out);
            }
        }
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            collect_expr_spans(condition, out);
            collect_stmt_spans(then_branch, out);
            for branch in elif_branches {
                out.push(branch.elif_span);
                collect_expr_spans(&branch.condition, out);
                collect_stmt_spans(&branch.body, out);
            }
            if let Some(else_branch) = else_branch {
                collect_stmt_spans(else_branch, out);
            }
        }
        Stmt::While { condition, body } | Stmt::DoWhile { condition, body } => {
            collect_expr_spans(condition, out);
            collect_stmt_spans(body, out);
        }
        Stmt::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(init) = init {
                collect_stmt_spans(init, out);
            }
            if let Some(condition) = condition {
                collect_expr_spans(condition, out);
            }
            if let Some(update) = update {
                collect_expr_spans(update, out);
            }
            collect_stmt_spans(body, out);
        }
        Stmt::ForIn {
            variable,
            iterable,
            body,
        }
        | Stmt::ForAwait {
            variable,
            producer: iterable,
            body,
        }
        | Stmt::ChanRecvFor {
            variable,
            channel: iterable,
            body,
        } => {
            out.push(variable.span);
            collect_expr_spans(iterable, out);
            collect_stmt_spans(body, out);
        }
        Stmt::Go {
            call,
            block,
            keyword_span,
        } => {
            out.push(*keyword_span);
            collect_expr_spans(call, out);
            if let Some(block) = block {
                for stmt in block {
                    collect_stmt_spans(stmt, out);
                }
            }
        }
        Stmt::Function {
            name,
            params,
            return_type,
            body,
            ..
        }
        | Stmt::AsyncFunction {
            name,
            params,
            return_type,
            body,
        } => {
            out.push(name.span);
            for param in params {
                out.push(param.name.span);
                if let Some(ty) = &param.type_annotation {
                    collect_type_spans(ty, out);
                }
            }
            if let Some(return_type) = return_type {
                out.push(return_type.arrow_span);
                collect_type_spans(&return_type.ty, out);
            }
            for stmt in body {
                collect_stmt_spans(stmt, out);
            }
        }
        Stmt::ExternFunction {
            abi,
            name,
            params,
            return_type,
            ..
        } => {
            out.push(abi.span);
            out.push(name.span);
            for param in params {
                out.push(param.name.span);
                if let Some(ty) = &param.type_annotation {
                    collect_type_spans(ty, out);
                }
            }
            if let Some(return_type) = return_type {
                out.push(return_type.arrow_span);
                collect_type_spans(&return_type.ty, out);
            }
        }
        Stmt::ExternBlock { abi, functions } => {
            out.push(abi.span);
            for function in functions {
                collect_stmt_spans(function, out);
            }
        }
        Stmt::Return { value } => {
            if let Some(value) = value {
                collect_expr_spans(value, out);
            }
        }
        Stmt::Class {
            name, parent, body, ..
        } => {
            out.push(name.span);
            if let Some(parent) = parent {
                out.push(parent.span);
            }
            for stmt in body {
                collect_stmt_spans(stmt, out);
            }
        }
        Stmt::Break { span } | Stmt::Continue { span } => out.push(*span),
        Stmt::Match {
            expression,
            cases,
            default_case,
        } => {
            collect_expr_spans(expression, out);
            for case in cases {
                out.push(case.case_span);
                collect_expr_spans(&case.value, out);
                if let Some(guard) = &case.guard {
                    collect_expr_spans(guard, out);
                }
                collect_stmt_spans(&case.body, out);
            }
            if let Some(default_case) = default_case {
                collect_stmt_spans(default_case, out);
            }
        }
        Stmt::Try {
            try_block,
            catch_var,
            catch_block,
            finally_block,
        } => {
            collect_stmt_spans(try_block, out);
            if let Some(catch_var) = catch_var {
                out.push(catch_var.span);
            }
            if let Some(catch_block) = catch_block {
                collect_stmt_spans(catch_block, out);
            }
            if let Some(finally_block) = finally_block {
                collect_stmt_spans(finally_block, out);
            }
        }
        Stmt::Retry {
            count,
            body,
            catch_var,
            catch_block,
        } => {
            collect_expr_spans(count, out);
            collect_stmt_spans(body, out);
            if let Some(catch_var) = catch_var {
                out.push(catch_var.span);
            }
            if let Some(catch_block) = catch_block {
                collect_stmt_spans(catch_block, out);
            }
        }
        Stmt::Unsafe { body } | Stmt::Quiet { body, .. } => collect_stmt_spans(body, out),
        Stmt::Destructure {
            names, initializer, ..
        } => {
            for name in names {
                out.push(name.span);
            }
            collect_expr_spans(initializer, out);
        }
        Stmt::Use {
            library,
            imported_symbols,
            alias,
            ..
        } => {
            out.push(library.span);
            for symbol in imported_symbols {
                out.push(symbol.span);
            }
            if let Some(alias) = alias {
                out.push(alias.span);
            }
        }
        Stmt::Enum { name, members, .. } => {
            out.push(name.span);
            for member in members {
                out.push(member.name.span);
                if let Some(value) = &member.value {
                    collect_expr_spans(value, out);
                }
            }
        }
        Stmt::TypeAlias { name, target, .. } => {
            out.push(name.span);
            collect_type_spans(target, out);
        }
        Stmt::Trait {
            name,
            parents,
            associated_types,
            methods,
        } => {
            out.push(name.span);
            for parent in parents {
                out.push(parent.span);
            }
            for associated in associated_types {
                out.push(associated.span);
            }
            for method in methods {
                collect_stmt_spans(method, out);
            }
        }
        Stmt::Impl {
            trait_name,
            type_name,
            body,
        } => {
            out.push(trait_name.span);
            out.push(type_name.span);
            for member in body {
                collect_stmt_spans(member, out);
            }
        }
        Stmt::Test { name, body } => {
            out.push(name.span);
            for stmt in body {
                collect_stmt_spans(stmt, out);
            }
        }
    }
}

fn collect_expr_spans(expr: &Expr, out: &mut Vec<Span>) {
    out.push(expr.span());
    match expr {
        Expr::Literal { .. }
        | Expr::Variable { .. }
        | Expr::This { .. }
        | Expr::TupleIndex { .. } => {}
        Expr::Binary { left, right, .. }
        | Expr::IndexGet {
            object: left,
            index: right,
        } => {
            collect_expr_spans(left, out);
            collect_expr_spans(right, out);
        }
        Expr::Unary { right, .. }
        | Expr::Spread { value: right, .. }
        | Expr::Grouping {
            expression: right, ..
        }
        | Expr::Copy {
            expression: right, ..
        }
        | Expr::Propagate { value: right, .. } => collect_expr_spans(right, out),
        Expr::PostfixUnary { left, .. } => collect_expr_spans(left, out),
        Expr::Member { object, .. } | Expr::OptionalMember { object, .. } => {
            collect_expr_spans(object, out);
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::Await {
            callee, arguments, ..
        } => {
            collect_expr_spans(callee, out);
            for argument in arguments {
                collect_expr_spans(argument, out);
            }
        }
        Expr::Assign { value, .. } => collect_expr_spans(value, out),
        Expr::IndexSet {
            object,
            index,
            value,
        } => {
            collect_expr_spans(object, out);
            collect_expr_spans(index, out);
            collect_expr_spans(value, out);
        }
        Expr::MemberSet { object, value, .. } => {
            collect_expr_spans(object, out);
            collect_expr_spans(value, out);
        }
        Expr::Lambda { body, .. } => {
            for stmt in body {
                collect_stmt_spans(stmt, out);
            }
        }
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_expr_spans(condition, out);
            collect_expr_spans(then_branch, out);
            collect_expr_spans(else_branch, out);
        }
        Expr::ObjectLiteral { properties, .. } => {
            for property in properties {
                collect_expr_spans(&property.value, out);
            }
        }
        Expr::ArrayLiteral { elements, .. } | Expr::TupleLiteral { elements, .. } => {
            for element in elements {
                collect_expr_spans(element, out);
            }
        }
        Expr::AsyncBlock { body, .. } => {
            for stmt in body {
                collect_stmt_spans(stmt, out);
            }
        }
        Expr::View { target, .. }
        | Expr::Borrow { target, .. }
        | Expr::RawDeref { target, .. }
        | Expr::Close {
            channel: target, ..
        } => collect_expr_spans(target, out),
        Expr::RawDerefSet { target, value, .. }
        | Expr::ChanSend {
            channel: target,
            value,
            ..
        } => {
            collect_expr_spans(target, out);
            collect_expr_spans(value, out);
        }
        Expr::StructLiteral { fields, update, .. } => {
            for field in fields {
                collect_expr_spans(&field.value, out);
            }
            if let Some(update) = update {
                collect_expr_spans(update, out);
            }
        }
        Expr::ChanRecv { channel, .. } => collect_expr_spans(channel, out),
    }
}

fn collect_type_spans(ty: &TypeAnnotation, out: &mut Vec<Span>) {
    match ty {
        TypeAnnotation::Named(token)
        | TypeAnnotation::ImplTrait(token)
        | TypeAnnotation::Dyn(token) => out.push(token.span),
        TypeAnnotation::Array(Some(inner))
        | TypeAnnotation::Option(inner)
        | TypeAnnotation::Slice(Some(inner))
        | TypeAnnotation::Own(inner)
        | TypeAnnotation::Shared(inner)
        | TypeAnnotation::View(inner, _)
        | TypeAnnotation::Ref(inner, _)
        | TypeAnnotation::RawPointer(inner, _)
        | TypeAnnotation::Chan(inner) => collect_type_spans(inner, out),
        TypeAnnotation::Result { ok, err } => {
            collect_type_spans(ok, out);
            collect_type_spans(err, out);
        }
        TypeAnnotation::Tuple(items) => {
            for item in items {
                collect_type_spans(item, out);
            }
        }
        _ => {}
    }
}

pub fn span_contains(span: Span, offset: usize) -> bool {
    contains(span, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "fun add(int a, int b) -> int {\n    return a + b\n}\n\nfun main() {\n    var total = add(1, 2)\n    say(total)\n}\n";

    #[test]
    fn indexes_functions_and_resolves_calls() {
        let analysis = analyze(SAMPLE);
        assert!(analysis.parse_errors.is_empty());
        assert!(analysis.symbols.iter().any(|symbol| symbol.name == "add"));
        let call = analysis
            .calls
            .iter()
            .find(|call| call.callee_name.as_deref() == Some("add"))
            .expect("call site recorded");
        assert_eq!(call.arg_spans.len(), 2);
        let offset = call.paren_span.start + 1;
        let enclosing = analysis.enclosing_call(offset).expect("enclosing call");
        assert_eq!(enclosing.callee_name.as_deref(), Some("add"));
        let target = analysis.callable_at("add", offset).expect("callable");
        assert_eq!(target.params.len(), 2);
    }

    #[test]
    fn keyword_at_reports_keyword_tokens() {
        let analysis = analyze(SAMPLE);
        let offset = SAMPLE.find("say(total)").expect("sample contains say") + 1;
        // `say` is a keyword token, not an identifier, so hover can only
        // describe it through the token under the cursor.
        assert_eq!(analysis.identifier_at(offset), None);
        assert_eq!(analysis.keyword_at(offset), Some("say"));
        let identifier = SAMPLE.find("total)").expect("sample contains total") + 1;
        assert_eq!(analysis.keyword_at(identifier), None);
    }

    #[test]
    fn recovers_symbols_when_parse_fails() {
        let analysis = analyze("fun broken( {\nfun good() {\n    say(\"hi\")\n}\n");
        assert!(!analysis.parse_errors.is_empty());
        assert!(analysis.symbols.iter().any(|symbol| symbol.name == "good"));
    }

    #[test]
    fn resolves_locals_and_members() {
        let source = "class Point {\n    var int x\n    fun get() -> int {\n        return this.x\n    }\n}\n";
        let analysis = analyze(source);
        assert!(analysis.symbols.iter().any(|symbol| symbol.name == "Point"));
        assert!(
            analysis
                .symbols
                .iter()
                .any(|symbol| symbol.name == "x" && symbol.kind == SymbolKind::Field)
        );
    }

    #[test]
    fn reports_undefined_names() {
        let analysis = analyze("fun main() {\n    say(missing)\n}\n");
        assert!(!analysis.resolve_errors.is_empty() || !analysis.type_errors.is_empty());
    }

    #[test]
    fn selection_chain_nests_smallest_first() {
        let analysis = analyze(SAMPLE);
        let offset = SAMPLE.find("a + b").expect("sample contains expression") + 2;
        let chain = analysis.selection_chain(offset);
        assert!(chain.len() >= 2, "expected nested ranges, got {chain:?}");
        let widths: Vec<usize> = chain
            .iter()
            .map(|(start, end)| end.saturating_sub(*start))
            .collect();
        let mut sorted = widths.clone();
        sorted.sort();
        assert_eq!(widths, sorted);
        // The smallest range is exactly the binary expression.
        assert_eq!(&SAMPLE[chain[0].0..chain[0].1], "a + b");
    }

    #[test]
    fn enclosing_function_finds_body_owner() {
        let analysis = analyze(SAMPLE);
        let offset = SAMPLE.find("say(total)").expect("sample contains call");
        let function = analysis
            .enclosing_function(offset)
            .expect("enclosing function");
        assert_eq!(function.name, "main");
        let inner = SAMPLE.find("a + b").expect("sample contains expression");
        let top = analysis.enclosing_function(inner).expect("add body offset");
        assert_eq!(top.name, "add");
    }

    /// Neutron merges modules into one flat namespace, so a top-level
    /// `var` is a global: it must be indexed, or every other file that
    /// uses it reports a bogus "undefined name".
    #[test]
    fn indexes_top_level_vars_as_globals() {
        let analysis = analyze("var int MAX_ITEMS = 100\nstatic var counter = 0\n");
        for name in ["MAX_ITEMS", "counter"] {
            let symbol = analysis
                .symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .unwrap_or_else(|| panic!("{name} should be indexed as a global"));
            assert_eq!(symbol.kind, SymbolKind::Variable);
            assert_eq!(symbol.scope_range, (0, usize::MAX), "{name} must be global");
        }
    }

    #[test]
    fn attaches_doc_comments_to_declarations() {
        let source = "// Doubles a value.\n// Second line.\nfun twice(int v) -> int {\n    return v * 2\n}\n";
        let analysis = analyze(source);
        let symbol = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "twice")
            .expect("function indexed");
        assert_eq!(symbol.docs, "Doubles a value.\nSecond line.");
    }

    #[test]
    fn ignores_comment_markers_inside_strings() {
        let source = "fun main() {\n    // real doc\n    say(\"// not a comment\")\n}\n";
        let analysis = analyze(source);
        let main = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "main")
            .expect("function indexed");
        assert!(
            !main.docs.contains("not a comment"),
            "string contents must not be read as docs: {:?}",
            main.docs
        );
    }
}
