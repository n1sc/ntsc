//! Name resolution performed before type checking.

use std::collections::HashSet;

use ntsc_ast::expr::Expr;
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{Program, Stmt};

/// A name-resolution error with a source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    pub message: String,
    pub span: Span,

    /// A close-enough spelling that the user may have intended, if any.
    pub suggestion: Option<String>,
}

/// Names of the builtin stdlib modules available via explicit `use`.
#[allow(dead_code)]
pub const BUILTIN_MODULES: &[&str] = &[
    "archive",
    "arrays",
    "async",
    "collections",
    "csv",
    "crypto",
    "encoding",
    "fmt",
    "glob",
    "hash",
    "http",
    "io",
    "json",
    "math",
    "memory",
    "paths",
    "slices",
    "net",
    "os",
    "process",
    "random",
    "regex",
    "sort",
    "strings",
    "sys",
    "term",
    "testing",
    "time",
    "toml",
    "yaml",
];

/// Resolve all declarations and references in a program.
pub fn resolve_program(program: &Program) -> Result<(), Vec<ResolveError>> {
    let mut resolver = Resolver::new();
    resolver.resolve_program(program);
    if resolver.errors.is_empty() {
        Ok(())
    } else {
        Err(resolver.errors)
    }
}

struct Resolver {
    scopes: Vec<HashSet<String>>,
    errors: Vec<ResolveError>,
}

impl Resolver {
    fn new() -> Self {
        let mut global = HashSet::new();
        global.insert("say".into());
        global.insert("alloc".into());
        global.insert("wait_any".into());
        global.insert("wait_all".into());

        // Result constructors are builtins, like `say` and `alloc`.
        global.insert("Ok".into());
        global.insert("Err".into());

        // The wildcard pattern `_` is always in scope in match cases.
        global.insert("_".into());

        // `async` is a reserved keyword and cannot be imported with `use`, so
        // it stays always in scope.
        global.insert("async".into());

        // `chan` — the virtual-task channel constructor namespace.
        global.insert("chan".into());

        Self {
            scopes: vec![global],
            errors: Vec::new(),
        }
    }

    fn resolve_program(&mut self, program: &Program) {
        for statement in &program.statements {
            match statement {
                Stmt::Function { name, .. }
                | Stmt::AsyncFunction { name, .. }
                | Stmt::ExternFunction { name, .. }
                | Stmt::Class { name, .. }
                | Stmt::Test { name, .. } => {
                    self.define(name.lexeme(), name.span);
                }

                Stmt::ExternBlock { functions, .. } => {
                    for function in functions {
                        if let Stmt::ExternFunction { name, .. } = function {
                            self.define(name.lexeme(), name.span);
                        }
                    }
                }

                Stmt::Enum { name, members, .. } => {
                    self.define(name.lexeme(), name.span);
                    // Enum members act as int constants and may be
                    // referenced bare (`case North`, `say(North)`).
                    for member in members {
                        self.define(member.name.lexeme(), member.name.span);
                    }
                }
                _ => {}
            }
        }
        for statement in &program.statements {
            self.resolve_statement(statement);
        }
    }

    fn resolve_statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Expression { expression } | Stmt::Throw { value: expression } => {
                self.resolve_expression(expression);
            }
            Stmt::Say { expression, .. } => self.resolve_expression(expression),
            Stmt::Var {
                name, initializer, ..
            } => {
                if let Some(initializer) = initializer {
                    self.resolve_expression(initializer);
                }
                self.define(name.lexeme(), name.span);
            }
            Stmt::Destructure {
                names, initializer, ..
            } => {
                self.resolve_expression(initializer);
                for name in names {
                    self.define(name.lexeme(), name.span);
                }
            }
            Stmt::Block { statements, .. } => self.in_scope(|resolver| {
                for statement in statements {
                    resolver.resolve_statement(statement);
                }
            }),
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => {
                self.resolve_expression(condition);
                self.resolve_statement(then_branch);
                for branch in elif_branches {
                    self.resolve_expression(&branch.condition);
                    self.resolve_statement(&branch.body);
                }
                if let Some(branch) = else_branch {
                    self.resolve_statement(branch);
                }
            }
            Stmt::While { condition, body } | Stmt::DoWhile { condition, body } => {
                self.resolve_expression(condition);
                self.resolve_statement(body);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => self.in_scope(|resolver| {
                if let Some(init) = init {
                    resolver.resolve_statement(init);
                }
                if let Some(condition) = condition {
                    resolver.resolve_expression(condition);
                }
                if let Some(update) = update {
                    resolver.resolve_expression(update);
                }
                resolver.resolve_statement(body);
            }),
            Stmt::ForIn {
                variable,
                iterable,
                body,
            } => {
                self.resolve_expression(iterable);
                self.in_scope(|resolver| {
                    resolver.define(variable.lexeme(), variable.span);
                    resolver.resolve_statement(body);
                });
            }
            Stmt::ForAwait {
                variable,
                producer,
                body,
            } => {
                self.resolve_expression(producer);
                self.in_scope(|resolver| {
                    resolver.define(variable.lexeme(), variable.span);
                    resolver.resolve_statement(body);
                });
            }
            Stmt::ChanRecvFor {
                variable,
                channel,
                body,
            } => {
                self.resolve_expression(channel);
                self.in_scope(|resolver| {
                    resolver.define(variable.lexeme(), variable.span);
                    resolver.resolve_statement(body);
                });
            }
            Stmt::Go { call, block, .. } => {
                self.resolve_expression(call);
                if let Some(block) = block {
                    self.in_scope(|resolver| {
                        for statement in block {
                            resolver.resolve_statement(statement);
                        }
                    });
                }
            }
            Stmt::Function { params, body, .. } => self.resolve_function(params, body),
            Stmt::AsyncFunction { params, body, .. } => self.resolve_function(params, body),
            Stmt::ExternFunction { .. } | Stmt::ExternBlock { .. } => {}
            Stmt::Test { body, .. } => self.resolve_function(&[], body),
            Stmt::Return { value } => {
                if let Some(value) = value {
                    self.resolve_expression(value);
                }
            }
            Stmt::Class {
                name, parent, body, ..
            } => {
                if let Some(parent) = parent
                    && !self
                        .scopes
                        .iter()
                        .rev()
                        .any(|scope| scope.contains(parent.lexeme()))
                {
                    self.errors.push(ResolveError {
                        message: format!("parent class `{}` is not defined", parent.lexeme()),
                        span: parent.span,
                        suggestion: closest_match(
                            parent.lexeme(),
                            self.scopes.iter().flatten().map(|s| s.as_str()),
                        ),
                    });
                }
                self.in_scope(|resolver| {
                    resolver.define("this", name.span);
                    for member in body {
                        if let Stmt::Function { name, .. } = member {
                            resolver.define(name.lexeme(), name.span);
                        }
                    }
                    for member in body {
                        resolver.resolve_statement(member);
                    }
                });
            }
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => {
                self.resolve_expression(expression);
                for case in cases {
                    match &case.pattern {
                        // The binder scopes to the arm body; the raw call
                        // form in `value` is not resolved as an expression.
                        Some(pattern) => {
                            if let Some(binding) = &pattern.binding {
                                self.in_scope(|resolver| {
                                    resolver.define(binding.lexeme(), binding.span);
                                    resolver.resolve_statement(&case.body);
                                });
                            } else {
                                self.resolve_statement(&case.body);
                            }
                        }
                        None => {
                            self.resolve_expression(&case.value);
                            if let Some(guard) = &case.guard {
                                self.resolve_expression(guard);
                            }
                            self.resolve_statement(&case.body);
                        }
                    }
                }
                if let Some(default_case) = default_case {
                    self.resolve_statement(default_case);
                }
            }
            Stmt::Try {
                try_block,
                catch_var,
                catch_block,
                finally_block,
            } => {
                self.resolve_statement(try_block);
                if let Some(catch_block) = catch_block {
                    self.in_scope(|resolver| {
                        if let Some(catch_var) = catch_var {
                            resolver.define(catch_var.lexeme(), catch_var.span);
                        }
                        resolver.resolve_statement(catch_block);
                    });
                }
                if let Some(finally_block) = finally_block {
                    self.resolve_statement(finally_block);
                }
            }
            Stmt::Retry {
                count,
                body,
                catch_var,
                catch_block,
            } => {
                self.resolve_expression(count);
                self.resolve_statement(body);
                if let Some(catch_block) = catch_block {
                    self.in_scope(|resolver| {
                        if let Some(catch_var) = catch_var {
                            resolver.define(catch_var.lexeme(), catch_var.span);
                        }
                        resolver.resolve_statement(catch_block);
                    });
                }
            }
            Stmt::Unsafe { body } => self.resolve_statement(body),
            Stmt::Quiet { body, .. } => self.resolve_statement(body),
            Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Enum { .. }
            | Stmt::TypeAlias { .. }
            | Stmt::Trait { .. }
            | Stmt::Impl { .. } => {}
            Stmt::Use {
                library,
                is_file_path: false,
                alias,
                imported_symbols,
                ..
            } if imported_symbols.is_empty() => {
                // Stdlib imports are idempotent: crossing file boundaries, the
                // same module may be re-imported without conflicting. `use
                // strings as s` binds the alias `s` instead of the module name.
                // Rust crate imports (`use @serde_json`) behave identically for
                // name resolution – they bind the crate name or its alias.
                let name = alias
                    .as_ref()
                    .map_or_else(|| library.lexeme(), |a| a.lexeme());
                if !self.scopes.iter().any(|scope| scope.contains(name)) {
                    self.define(name, library.span);
                }
            }
            // `use "file.nt" as arm` binds the namespace name `arm`; the
            // module's symbols are referenced through `arm.name`.
            Stmt::Use {
                is_file_path: true,
                alias: Some(alias),
                imported_symbols,
                ..
            } if imported_symbols.is_empty() => {
                let name = alias.lexeme();
                if !self.scopes.iter().any(|scope| scope.contains(name)) {
                    self.define(name, alias.span);
                }
            }
            Stmt::Use {
                imported_symbols, ..
            } if !imported_symbols.is_empty() => {
                for symbol in imported_symbols {
                    let name = symbol.lexeme();
                    if !self.scopes.iter().any(|scope| scope.contains(name)) {
                        self.define(name, symbol.span);
                    }
                }
            }
            Stmt::Use { .. } => {}
        }
    }

    fn resolve_function(&mut self, params: &[ntsc_ast::expr::FunctionParam], body: &[Stmt]) {
        self.in_scope(|resolver| {
            for param in params {
                resolver.define(param.name.lexeme(), param.name.span);
            }
            for statement in body {
                resolver.resolve_statement(statement);
            }
        });
    }

    fn resolve_expression(&mut self, expression: &Expr) {
        match expression {
            Expr::Literal { .. } => {}
            Expr::This { keyword } => self.require("this", keyword.span),
            Expr::Variable { name } => {
                let lex = name.lexeme().trim_start_matches('@');
                self.require(lex, name.span)
            }
            Expr::Assign { name, value } => {
                self.require(name.lexeme(), name.span);
                self.resolve_expression(value);
            }
            Expr::Unary { right, .. } | Expr::Spread { value: right, .. } => {
                self.resolve_expression(right)
            }
            Expr::PostfixUnary { left, .. } => self.resolve_expression(left),
            Expr::Grouping { expression, .. } => self.resolve_expression(expression),
            Expr::Binary { left, right, .. }
            | Expr::IndexGet {
                object: left,
                index: right,
            } => {
                self.resolve_expression(left);
                self.resolve_expression(right);
            }
            Expr::Ternary {
                condition,
                then_branch,
                else_branch,
            } => {
                self.resolve_expression(condition);
                self.resolve_expression(then_branch);
                self.resolve_expression(else_branch);
            }
            Expr::Call {
                callee, arguments, ..
            } => {
                self.resolve_expression(callee);
                for argument in arguments {
                    self.resolve_expression(argument);
                }
            }
            Expr::Await {
                callee, arguments, ..
            } => {
                self.resolve_expression(callee);
                for argument in arguments {
                    self.resolve_expression(argument);
                }
            }
            Expr::AsyncBlock { body, .. } => {
                for stmt in body {
                    self.resolve_statement(stmt);
                }
            }
            Expr::ChanSend { channel, value, .. } => {
                self.resolve_expression(channel);
                self.resolve_expression(value);
            }
            Expr::ChanRecv {
                receiver, channel, ..
            } => {
                self.resolve_expression(channel);
                self.define(receiver.lexeme(), receiver.span);
            }
            Expr::Close { channel, .. } => self.resolve_expression(channel),
            Expr::Member { object, .. } | Expr::OptionalMember { object, .. } => {
                self.resolve_expression(object);
            }
            Expr::View { target, .. } => self.resolve_expression(target),
            Expr::Borrow { target, .. } | Expr::RawDeref { target, .. } => {
                self.resolve_expression(target)
            }
            Expr::RawDerefSet { target, value, .. } => {
                self.resolve_expression(target);
                self.resolve_expression(value);
            }
            Expr::Copy { expression, .. } => self.resolve_expression(expression),
            Expr::Propagate { value, .. } => self.resolve_expression(value),
            Expr::IndexSet {
                object,
                index,
                value,
            } => {
                self.resolve_expression(object);
                self.resolve_expression(index);
                self.resolve_expression(value);
            }
            Expr::MemberSet { object, value, .. } => {
                self.resolve_expression(object);
                self.resolve_expression(value);
            }
            Expr::Lambda { params, body, .. } => self.resolve_function(params, body),
            Expr::ObjectLiteral { properties, .. } => {
                for property in properties {
                    self.resolve_expression(&property.value);
                }
            }
            Expr::ArrayLiteral { elements, .. } => {
                for element in elements {
                    self.resolve_expression(element);
                }
            }
            Expr::StructLiteral {
                class_name,
                fields,
                update,
                ..
            } => {
                self.require(class_name.lexeme(), class_name.span);
                for field in fields {
                    self.resolve_expression(&field.value);
                }
                if let Some(update) = update {
                    self.resolve_expression(update);
                }
            }
            Expr::TupleLiteral { elements, .. } => {
                for element in elements {
                    self.resolve_expression(element);
                }
            }
            Expr::TupleIndex { object, .. } => {
                self.resolve_expression(object);
            }
        }
    }

    fn in_scope(&mut self, operation: impl FnOnce(&mut Self)) {
        self.scopes.push(HashSet::new());
        operation(self);
        let _ = self.scopes.pop();
    }

    fn define(&mut self, name: &str, span: Span) {
        if let Some(scope) = self.scopes.last_mut()
            && !scope.insert(name.to_owned())
        {
            // The result constructors start as globals; a user definition of
            // the same name (e.g. an `Ok` enum variant) shadows the builtin
            // instead of conflicting with it.
            let shadows_builtin = matches!(name, "Ok" | "Err") && self.scopes.len() == 1;
            if !shadows_builtin {
                self.errors.push(ResolveError {
                    message: format!("`{name}` is already defined in this scope"),
                    span,
                    suggestion: None,
                });
            }
        }
    }

    fn require(&mut self, name: &str, span: Span) {
        if !self.scopes.iter().rev().any(|scope| scope.contains(name)) {
            self.errors.push(ResolveError {
                message: format!("undefined name `{name}`"),
                span,
                suggestion: closest_match(name, self.scopes.iter().flatten().map(|s| s.as_str())),
            });
        }
    }
}

/// Edit distance between two strings (used for typo suggestions).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur.push((prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Return the closest candidate to `target`, if it is close enough to be a
/// typo. Shared by every diagnostic that can offer a "did you mean" hint.
pub(crate) fn closest_match<'a>(
    target: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        if candidate == target {
            continue;
        }
        let distance = levenshtein(target, candidate);
        if best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, candidate));
        }
    }
    best.and_then(|(distance, candidate)| {
        let allowed = distance <= 2 || distance * 2 <= target.chars().count().max(2);
        allowed.then(|| candidate.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntsc_lexer::tokenize;
    use ntsc_parser::parse;

    fn resolve_source(source: &str) -> Result<(), Vec<ResolveError>> {
        let tokens = tokenize(source);
        let program = parse(&tokens).map_err(|errors| {
            errors
                .into_iter()
                .map(|error| ResolveError {
                    message: error.message,
                    span: error.span,
                    suggestion: None,
                })
                .collect::<Vec<_>>()
        })?;
        resolve_program(&program)
    }

    #[test]
    fn resolves_forward_function_and_class_names() {
        assert!(resolve_source("fun main() -> int { return make() }\nfun make() -> int { return 1 }\nclass Person { }").is_ok());
    }

    #[test]
    fn reports_undefined_names_before_type_checking() {
        let errors = resolve_source("fun main() -> int { return missing }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("undefined name `missing`"))
        );
    }

    #[test]
    fn suggests_close_spelling_for_typos() {
        let errors = resolve_source("fun main() { var count = 0\n    say(coun) }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.suggestion.as_deref() == Some("count")),
            "expected a suggestion for `coun`, got: {errors:#?}"
        );
    }

    #[test]
    fn no_suggestion_for_unrelated_name() {
        let errors = resolve_source("fun main() { var count = 0\n    say(zebra) }").unwrap_err();
        assert!(
            errors.iter().all(|error| error.suggestion.is_none()),
            "`zebra` is too far from `count` for a suggestion, got: {errors:#?}"
        );
    }
}
