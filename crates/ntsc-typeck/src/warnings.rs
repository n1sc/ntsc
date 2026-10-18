//! Lint warnings pass.
//!
//! Implements the `unused_variable` warning: a local variable declared with
//! `var`, destructuring, or a `for ... in` loop that is never read or written
//! is reported when its scope ends.
//!
//! Suppression via `quiet` is lexical and local:
//!
//! - `quiet body` silences the warning for every declaration made inside
//!   `body`; `quiet [unused_variable] body` silences only the listed lints.
//! - Suppression never crosses function, lambda, or class boundaries: a
//!   nested body always lints its own locals, and there is no global pile of
//!   allowed names that would silence the same name elsewhere.

use std::collections::HashMap;

use ntsc_ast::expr::Expr;
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{Program, Stmt};

/// The single lint implemented so far.
pub(crate) const LINT_UNUSED_VARIABLE: &str = "unused_variable";

/// A lint warning with source location and the suppressible lint name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// The `quiet [name]` list entry that silences this warning.
    pub lint: &'static str,
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.span, self.message)
    }
}

impl std::error::Error for Warning {}

/// Check `program` for lint warnings, ordered by source position.
pub fn lint_program(program: &Program) -> Vec<Warning> {
    let mut linter = Linter::new();
    linter.check_program(program);
    linter.warnings
}

/// A tracked local variable declaration.
#[derive(Debug)]
struct Decl {
    span: Span,
    suppressed: bool,
    used: bool,
}

/// Walks the AST collecting `unused_variable` warnings.
struct Linter {
    /// Stack of local scopes; the innermost scope is last.
    scopes: Vec<HashMap<String, Decl>>,

    /// `quiet` suppression currently active for the subtree being walked.
    /// Reset to `false` when entering a nested function/class body.
    suppress_unused: bool,
    warnings: Vec<Warning>,
}

impl Linter {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            suppress_unused: false,
            warnings: Vec::new(),
        }
    }

    fn check_program(&mut self, program: &Program) {
        for statement in &program.statements {
            self.check_stmt(statement);
        }
        self.end_scope();
        self.warnings.sort_by_key(|w| (w.span.start, w.span.end));
    }

    /// Register a local variable declaration, honoring the current
    /// suppression level. `_` is conventionally ignored and never warned about.
    fn declare(&mut self, name: &str, span: Span) {
        if name == "_" {
            return;
        }
        let scope = self
            .scopes
            .last_mut()
            .expect("linter always keeps the global scope");
        scope.insert(
            name.to_string(),
            Decl {
                span,
                suppressed: self.suppress_unused,
                used: false,
            },
        );
    }

    /// Mark the innermost visible declaration of `name` as used. Searches
    /// outward so references from nested functions count as uses of outer
    /// variables (captures).
    fn mark_used(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(decl) = scope.get_mut(name) {
                decl.used = true;
                return;
            }
        }
    }

    /// Pop the innermost scope, reporting every unsuppressed unused variable.
    fn end_scope(&mut self) {
        if let Some(scope) = self.scopes.pop() {
            for (name, decl) in scope {
                if !decl.used && !decl.suppressed {
                    self.warnings.push(Warning {
                        lint: LINT_UNUSED_VARIABLE,
                        message: format!("unused variable `{name}`"),
                        span: decl.span,
                    });
                }
            }
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Var {
                name, initializer, ..
            } => {
                if let Some(init) = initializer {
                    self.check_expr(init);
                }
                self.declare(name.lexeme(), name.span);
            }
            Stmt::Destructure {
                names, initializer, ..
            } => {
                self.check_expr(initializer);
                for name in names {
                    self.declare(name.lexeme(), name.span);
                }
            }
            Stmt::Expression { expression } => self.check_expr(expression),
            Stmt::Say { expression, .. } => self.check_expr(expression),
            Stmt::Block { statements, .. } => {
                self.scopes.push(HashMap::new());
                for statement in statements {
                    self.check_stmt(statement);
                }
                self.end_scope();
            }
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => {
                self.check_expr(condition);
                self.check_stmt(then_branch);
                for branch in elif_branches {
                    self.check_expr(&branch.condition);
                    self.check_stmt(&branch.body);
                }
                if let Some(else_branch) = else_branch {
                    self.check_stmt(else_branch);
                }
            }
            Stmt::While { condition, body } => {
                self.check_expr(condition);
                self.check_stmt(body);
            }
            Stmt::DoWhile { body, condition } => {
                self.check_stmt(body);
                self.check_expr(condition);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                self.scopes.push(HashMap::new());
                if let Some(init) = init {
                    self.check_stmt(init);
                }
                if let Some(condition) = condition {
                    self.check_expr(condition);
                }
                if let Some(update) = update {
                    self.check_expr(update);
                }
                self.check_stmt(body);
                self.end_scope();
            }
            Stmt::ForIn {
                variable,
                iterable,
                body,
            } => {
                self.check_expr(iterable);
                self.scopes.push(HashMap::new());
                self.declare(variable.lexeme(), variable.span);
                self.check_stmt(body);
                self.end_scope();
            }
            Stmt::ForAwait {
                variable,
                producer,
                body,
            } => {
                self.check_expr(producer);
                self.scopes.push(HashMap::new());
                self.declare(variable.lexeme(), variable.span);
                self.check_stmt(body);
                self.end_scope();
            }
            Stmt::Function { body, .. }
            | Stmt::AsyncFunction { body, .. }
            | Stmt::Test { body, .. } => {
                // Nested bodies lint their own locals: suppression does
                // not leak in from the enclosing body.
                let saved = self.suppress_unused;
                self.suppress_unused = false;
                self.scopes.push(HashMap::new());
                for statement in body {
                    self.check_stmt(statement);
                }
                self.end_scope();
                self.suppress_unused = saved;
            }
            Stmt::Return { value } => {
                if let Some(value) = value {
                    self.check_expr(value);
                }
            }
            Stmt::Class { body, .. } => {
                let saved = self.suppress_unused;
                self.suppress_unused = false;
                self.scopes.push(HashMap::new());
                for member in body {
                    match member {
                        // Field declarations are not local variables; only
                        // their initializers are walked.
                        Stmt::Var { initializer, .. } => {
                            if let Some(init) = initializer {
                                self.check_expr(init);
                            }
                        }
                        Stmt::Destructure { initializer, .. } => {
                            self.check_expr(initializer);
                        }
                        _ => self.check_stmt(member),
                    }
                }
                self.end_scope();
                self.suppress_unused = saved;
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => {
                self.check_expr(expression);
                for case in cases {
                    self.check_expr(&case.value);
                    if let Some(guard) = &case.guard {
                        self.check_expr(guard);
                    }
                    self.check_stmt(&case.body);
                }
                if let Some(default_case) = default_case {
                    self.check_stmt(default_case);
                }
            }
            Stmt::Try {
                try_block,
                catch_var,
                catch_block,
                finally_block,
                ..
            } => {
                self.check_stmt(try_block);
                if let Some(catch_block) = catch_block {
                    self.scopes.push(HashMap::new());
                    if let Some(var) = catch_var {
                        self.declare(var.lexeme(), var.span);
                    }
                    self.check_stmt(catch_block);
                    self.end_scope();
                }
                if let Some(finally_block) = finally_block {
                    self.check_stmt(finally_block);
                }
            }
            Stmt::Throw { value } => self.check_expr(value),
            Stmt::Retry {
                count,
                body,
                catch_var,
                catch_block,
                ..
            } => {
                self.check_expr(count);
                self.check_stmt(body);
                if let Some(catch_block) = catch_block {
                    self.scopes.push(HashMap::new());
                    if let Some(var) = catch_var {
                        self.declare(var.lexeme(), var.span);
                    }
                    self.check_stmt(catch_block);
                    self.end_scope();
                }
            }
            Stmt::Unsafe { body } => self.check_stmt(body),
            Stmt::Quiet { suppressed, body } => {
                let applies = suppressed.is_empty()
                    || suppressed.iter().any(|name| name == LINT_UNUSED_VARIABLE);
                let saved = self.suppress_unused;
                if applies {
                    self.suppress_unused = true;
                }
                self.check_stmt(body);
                self.suppress_unused = saved;
            }
            Stmt::Use { .. }
            | Stmt::Enum { .. }
            | Stmt::TypeAlias { .. }
            | Stmt::Trait { .. }
            | Stmt::Impl { .. }
            | Stmt::ExternFunction { .. }
            | Stmt::ExternBlock { .. } => {}
            Stmt::ChanRecvFor {
                variable,
                channel,
                body,
            } => {
                self.check_expr(channel);
                self.scopes.push(HashMap::new());
                self.declare(variable.lexeme(), variable.span);
                self.check_stmt(body);
                self.end_scope();
            }
            Stmt::Go { call, block, .. } => {
                self.check_expr(call);
                if let Some(block) = block {
                    self.scopes.push(HashMap::new());
                    for statement in block {
                        self.check_stmt(statement);
                    }
                    self.end_scope();
                }
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal { .. } => {}
            Expr::Variable { name } => self.mark_used(name.lexeme()),
            Expr::Binary { left, right, .. } => {
                self.check_expr(left);
                self.check_expr(right);
            }
            Expr::Unary { right, .. } => self.check_expr(right),
            Expr::PostfixUnary { left, .. } => self.check_expr(left),
            Expr::Grouping { expression, .. } => self.check_expr(expression),
            Expr::Member { object, .. } | Expr::OptionalMember { object, .. } => {
                self.check_expr(object);
            }
            Expr::Call {
                callee, arguments, ..
            } => {
                self.check_expr(callee);
                for argument in arguments {
                    self.check_expr(argument);
                }
            }
            Expr::Assign { name, value } => {
                self.mark_used(name.lexeme());
                self.check_expr(value);
            }
            Expr::IndexGet { object, index } => {
                self.check_expr(object);
                self.check_expr(index);
            }
            Expr::IndexSet {
                object,
                index,
                value,
            } => {
                self.check_expr(object);
                self.check_expr(index);
                self.check_expr(value);
            }
            Expr::MemberSet { object, value, .. } => {
                self.check_expr(object);
                self.check_expr(value);
            }
            Expr::This { .. } => {}
            Expr::Lambda { body, .. } => {
                let saved = self.suppress_unused;
                self.suppress_unused = false;
                self.scopes.push(HashMap::new());
                for statement in body {
                    self.check_stmt(statement);
                }
                self.end_scope();
                self.suppress_unused = saved;
            }
            Expr::Ternary {
                condition,
                then_branch,
                else_branch,
            } => {
                self.check_expr(condition);
                self.check_expr(then_branch);
                self.check_expr(else_branch);
            }
            Expr::Spread { value, .. } => self.check_expr(value),
            Expr::ObjectLiteral { properties, .. } => {
                for property in properties {
                    self.check_expr(&property.value);
                }
            }
            Expr::ArrayLiteral { elements, .. } => {
                for element in elements {
                    self.check_expr(element);
                }
            }
            Expr::Await {
                callee, arguments, ..
            } => {
                self.check_expr(callee);
                for argument in arguments {
                    self.check_expr(argument);
                }
            }
            Expr::AsyncBlock { body, .. } => {
                self.scopes.push(HashMap::new());
                for stmt in body {
                    self.check_stmt(stmt);
                }
                self.end_scope();
            }
            Expr::View { target, .. } => self.check_expr(target),
            Expr::Borrow { target, .. } | Expr::RawDeref { target, .. } => self.check_expr(target),
            Expr::RawDerefSet { target, value, .. } => {
                self.check_expr(target);
                self.check_expr(value);
            }
            Expr::Copy { expression, .. } => self.check_expr(expression),
            Expr::Propagate { value, .. } => self.check_expr(value),
            Expr::StructLiteral {
                class_name,
                fields,
                update,
                ..
            } => {
                self.mark_used(class_name.lexeme());
                for field in fields {
                    self.check_expr(&field.value);
                }
                if let Some(update) = update {
                    self.check_expr(update);
                }
            }
            Expr::TupleLiteral { elements, .. } => {
                for element in elements {
                    self.check_expr(element);
                }
            }
            Expr::TupleIndex { object, .. } => {
                self.check_expr(object);
            }
            Expr::ChanSend { channel, value, .. } => {
                self.check_expr(channel);
                self.check_expr(value);
            }
            Expr::ChanRecv {
                receiver, channel, ..
            } => {
                self.declare(receiver.lexeme(), receiver.span);
                self.check_expr(channel);
            }
            Expr::Close { channel, .. } => self.check_expr(channel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntsc_lexer::tokenize;
    use ntsc_parser::parse;

    fn lint_source(source: &str) -> Vec<Warning> {
        let tokens = tokenize(source);
        let program = parse(&tokens).expect("test source should parse");
        lint_program(&program)
    }

    fn messages(warnings: &[Warning]) -> Vec<&str> {
        warnings.iter().map(|w| w.message.as_str()).collect()
    }

    #[test]
    fn unused_local_is_reported() {
        let warnings = lint_source("fun f() { var x = 1 }");
        assert_eq!(messages(&warnings), vec!["unused variable `x`"]);
    }

    #[test]
    fn used_local_is_not_reported() {
        assert!(lint_source("fun f() { var x = 1\nsay(x) }").is_empty());
    }

    #[test]
    fn global_unused_is_reported() {
        let warnings = lint_source("var x = 1");
        assert_eq!(messages(&warnings), vec!["unused variable `x`"]);
    }

    #[test]
    fn quiet_var_suppresses_warning() {
        let warnings = lint_source("fun f() { quiet [unused_variable] var x = 1 }");
        assert!(warnings.is_empty());
    }

    #[test]
    fn quiet_block_suppresses_all_locals() {
        let source = "fun f() {\n    quiet {\n        var a = 1\n        var b = 2\n    }\n}";
        assert!(lint_source(source).is_empty());
    }

    #[test]
    fn quiet_is_lexical_and_does_not_leak() {
        let source = "fun f() {\n    quiet { var x = 1 }\n    var y = 2\n}";
        let warnings = lint_source(source);
        assert_eq!(messages(&warnings), vec!["unused variable `y`"]);
    }

    #[test]
    fn quiet_does_not_cross_function_boundaries() {
        let source = "fun f() {\n    quiet {\n        fun g() { var y = 1 }\n    }\n}";
        let warnings = lint_source(source);
        assert_eq!(messages(&warnings), vec!["unused variable `y`"]);
    }

    #[test]
    fn named_suppression_only_matches_listed_lints() {
        let source = "fun f() { quiet [other_lint] var x = 1 }";
        let warnings = lint_source(source);
        assert_eq!(messages(&warnings), vec!["unused variable `x`"]);
    }

    #[test]
    fn shadowing_reports_inner_not_outer() {
        let source = "fun f() {\n    var x = 1\n    {\n        var x = 2\n        say(x)\n    }\n}";
        let warnings = lint_source(source);
        assert_eq!(messages(&warnings), vec!["unused variable `x`"]);
    }

    #[test]
    fn captured_by_nested_function_counts_as_used() {
        let source = "fun f() {\n    var x = 1\n    fun g() { say(x) }\n}";
        assert!(lint_source(source).is_empty());
    }

    #[test]
    fn assignment_counts_as_use() {
        assert!(lint_source("fun f() { var x = 1\nx = 2 }").is_empty());
    }

    #[test]
    fn unused_loop_variable_is_reported() {
        let source = "fun f() {\n    for (var x in [1, 2, 3]) { say(1) }\n}";
        let warnings = lint_source(source);
        assert_eq!(messages(&warnings), vec!["unused variable `x`"]);
    }

    #[test]
    fn warnings_are_sorted_by_position() {
        let source = "fun f() {\n    var z = 1\n    var a = 2\n    var m = 3\n}";
        let warnings = lint_source(source);
        assert_eq!(
            messages(&warnings),
            vec![
                "unused variable `z`",
                "unused variable `a`",
                "unused variable `m`"
            ]
        );
    }
}
