//! Namespace a module's top-level symbols under an import alias, and scope
//! `private` declarations to their own file.
//!
//! `use "file.nt" as arm` turns every top-level declaration in `file.nt`
//! into `arm::<name>`. References to those symbols *inside* the file are
//! rewritten to the namespaced form so the merged (flat) program stays
//! consistent: external callers reach the symbols through `arm.name()`, and
//! the file's own unqualified calls keep working because they are rewritten
//! in place.
//!
//! Private declarations are renamed to a module-scoped internal name instead,
//! so the same flat program cannot reach them from another file. While
//! rewriting, free references to another module's private names are reported.
//!
//! References that must NOT be rewritten:
//! - local variables, parameters, loop/catch/lambda bindings (they can shadow),
//! - `this` and member/field access (`obj.x`, `obj.method()`),
//! - stdlib module names and symbols imported from other files,
//! - the identifiers inside `use` statements themselves.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ntsc_ast::expr::Expr;
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{Stmt, declaration_name};
use ntsc_ast::token::Token;
use ntsc_ast::types::TypeAnnotation;
use ntsc_diag::{Diagnostic, Label, codes};

/// Names of top-level declarations that free identifiers may reference.
pub fn top_level_names(statements: &[Stmt]) -> HashSet<String> {
    let mut names = HashSet::new();
    for stmt in statements {
        match stmt {
            Stmt::Function { name, .. }
            | Stmt::AsyncFunction { name, .. }
            | Stmt::Class { name, .. }
            | Stmt::Enum { name, .. }
            | Stmt::TypeAlias { name, .. }
            | Stmt::Trait { name, .. } => {
                names.insert(name.lexeme().to_string());
            }
            Stmt::Var {
                name,
                is_static: true,
                ..
            } => {
                names.insert(name.lexeme().to_string());
            }
            _ => {}
        }
    }
    names
}

/// Program-wide visibility information a module is rewritten against.
pub struct Visibility<'a> {
    /// The module being rewritten.
    pub path: &'a Path,

    /// Aliases bound in this module: alias name -> imported module.
    pub aliases: &'a HashMap<String, PathBuf>,

    /// Private names declared by each module.
    pub private: &'a HashMap<PathBuf, HashSet<String>>,

    /// The module that declares each private name (first in load order).
    pub private_owners: &'a HashMap<String, PathBuf>,

    /// Every public top-level name in the program. A name that is public
    /// anywhere is reachable unqualified, so it never triggers a privacy
    /// error.
    pub public_names: &'a HashSet<String>,

    /// Names this module imports selectively. The import itself is reported
    /// when it is private, so later uses of the name are not reported again.
    pub imported: &'a HashSet<String>,

    /// Human-readable name of each module, used in diagnostics.
    pub display_names: &'a HashMap<PathBuf, String>,

    /// Byte offset added to this module's spans during the merge. Error
    /// spans are shifted back by it so they stay file-local.
    pub span_base: usize,
}

/// Remove `private` wrappers from a module's top-level statements, returning
/// the bare declarations and the set of names they make file-local.
pub fn split_visibility(statements: Vec<Stmt>) -> (Vec<Stmt>, HashSet<String>) {
    let mut private = HashSet::new();
    let statements = statements
        .into_iter()
        .map(|stmt| {
            let Stmt::Private { statement } = stmt else {
                return stmt;
            };
            if let Some(name) = declaration_name(&statement) {
                private.insert(name.lexeme().to_string());
            }
            *statement
        })
        .collect();
    (statements, private)
}

/// The internal prefix private symbols are renamed under. It contains `$`,
/// which no Neutron identifier can, so it can never collide with user code.
const PRIVATE_PREFIX: &str = "$private";

/// Compute the renames applied to one module's own top-level names: private
/// symbols get a module-scoped internal name, public symbols of an aliased
/// module get the alias namespace.
pub fn renames(
    own: &HashSet<String>,
    private: &HashSet<String>,
    alias: Option<&str>,
    index: usize,
) -> HashMap<String, String> {
    own.iter()
        .filter_map(|name| {
            if private.contains(name) {
                Some((name.clone(), format!("{PRIVATE_PREFIX}{index}::{name}")))
            } else {
                alias.map(|alias| (name.clone(), format!("{alias}::{name}")))
            }
        })
        .collect()
}

/// Rewrite `statements` (a module's top-level body) so its own symbols are
/// namespaced, returning the new statements and any visibility errors.
pub fn namespaced(
    statements: Vec<Stmt>,
    renames: &HashMap<String, String>,
    own: &HashSet<String>,
    visibility: &Visibility<'_>,
) -> (Vec<Stmt>, Vec<Diagnostic>) {
    let mut rewriter = Rewriter {
        renames,
        own,
        scopes: vec![HashSet::new()],
        visibility,
        errors: Vec::new(),
    };
    let statements = rewriter.stmts(statements);
    (statements, rewriter.errors)
}

struct Rewriter<'a, 'v> {
    renames: &'a HashMap<String, String>,
    own: &'a HashSet<String>,
    /// Lexical binding scopes; the innermost is last.
    scopes: Vec<HashSet<String>>,
    visibility: &'v Visibility<'a>,
    errors: Vec<Diagnostic>,
}

impl Rewriter<'_, '_> {
    fn is_shadowed(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|scope| scope.contains(name))
    }

    fn id_token(&mut self, tok: &Token, bind: bool) -> Token {
        if !bind {
            self.check_private_reference(tok);
        }
        let mut out = tok.clone();
        if !bind
            && !self.is_shadowed(tok.lexeme())
            && let Some(renamed) = self.renames.get(tok.lexeme())
        {
            out.kind = ntsc_ast::token::TokenKind::Identifier(renamed.clone());
        }
        out
    }

    /// Report a free identifier that names another module's private symbol.
    /// The module's own declarations and locals are allowed, and a name that
    /// is public somewhere stays reachable through the flat namespace.
    fn check_private_reference(&mut self, tok: &Token) {
        let name = tok.lexeme();
        if self.own.contains(name)
            || self.is_shadowed(name)
            || self.visibility.public_names.contains(name)
            || self.visibility.imported.contains(name)
        {
            return;
        }
        let Some(owner) = self.visibility.private_owners.get(name) else {
            return;
        };
        if owner == self.visibility.path {
            return;
        }
        let display = self.display(owner);
        self.errors.push(
            Diagnostic::error(format!("`{name}` is private to `{display}`"))
                .with_code(codes::BUILD)
                .with_file(self.visibility.path.display().to_string())
                .with_label(Label::primary(
                    self.local_span(tok.span),
                    format!("not visible outside `{display}`"),
                ))
                .with_help(format!(
                    "remove `private` from `{name}` in `{display}` to export it"
                )),
        );
    }

    /// Report `alias.private_name`, which names a symbol through the alias of
    /// the module that keeps it private.
    fn check_alias_member(&mut self, object: &Expr, property: &Token) {
        let Expr::Variable { name } = object else {
            return;
        };
        if self.is_shadowed(name.lexeme()) {
            return;
        }
        let Some(target) = self.visibility.aliases.get(name.lexeme()) else {
            return;
        };
        let Some(private) = self.visibility.private.get(target) else {
            return;
        };
        if !private.contains(property.lexeme()) {
            return;
        }
        let display = self.display(target);
        self.errors.push(
            Diagnostic::error(format!("`{}` is private to `{display}`", property.lexeme()))
                .with_code(codes::BUILD)
                .with_file(self.visibility.path.display().to_string())
                .with_label(Label::primary(
                    self.local_span(property.span),
                    format!("not visible outside `{display}`"),
                ))
                .with_help(format!(
                    "remove `private` from `{}` in `{display}` to export it",
                    property.lexeme()
                )),
        );
    }

    fn display(&self, path: &Path) -> String {
        self.visibility
            .display_names
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.display().to_string())
    }

    fn local_span(&self, span: Span) -> Span {
        Span::new(
            span.start.saturating_sub(self.visibility.span_base),
            span.end.saturating_sub(self.visibility.span_base),
            span.line,
            span.column,
        )
    }

    // ── statements ─────────────────────────────────────────────────

    fn stmts(&mut self, list: Vec<Stmt>) -> Vec<Stmt> {
        list.into_iter().map(|s| self.stmt_push(s)).collect()
    }

    fn stmt_push(&mut self, stmt: Stmt) -> Stmt {
        self.scopes.push(HashSet::new());
        let out = self.stmt(stmt);
        self.scopes.pop();
        out
    }

    fn stmt(&mut self, stmt: Stmt) -> Stmt {
        match stmt {
            Stmt::Expression { expression } => Stmt::Expression {
                expression: self.expr(expression),
            },
            Stmt::Say {
                expression,
                keyword_span,
            } => Stmt::Say {
                expression: self.expr(expression),
                keyword_span,
            },
            Stmt::Var {
                mut name,
                type_annotation,
                initializer,
                is_static,
                is_const,
                view,
            } => {
                name = self.id_token(&name, !is_static);
                Stmt::Var {
                    name,
                    type_annotation: type_annotation.map(|t| self.ty(t)),
                    initializer: initializer.map(|e| self.expr(e)),
                    is_static,
                    is_const,
                    view,
                }
            }
            Stmt::Block {
                statements,
                open_span,
                close_span,
            } => Stmt::Block {
                statements: self.stmts(statements),
                open_span,
                close_span,
            },
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => Stmt::If {
                condition: self.expr(condition),
                then_branch: Box::new(self.stmt_push(*then_branch)),
                elif_branches: elif_branches
                    .into_iter()
                    .map(|b| ntsc_ast::stmt::ElifBranch {
                        condition: self.expr(b.condition),
                        body: Box::new(self.stmt_push(*b.body)),
                        elif_span: b.elif_span,
                    })
                    .collect(),
                else_branch: else_branch.map(|b| Box::new(self.stmt_push(*b))),
            },
            Stmt::While { condition, body } => Stmt::While {
                condition: self.expr(condition),
                body: Box::new(self.stmt_push(*body)),
            },
            Stmt::DoWhile { body, condition } => Stmt::DoWhile {
                body: Box::new(self.stmt_push(*body)),
                condition: self.expr(condition),
            },
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => Stmt::For {
                init: init.map(|i| Box::new(self.stmt_push(*i))),
                condition: condition.map(|c| self.expr(c)),
                update: update.map(|u| self.expr(u)),
                body: Box::new(self.stmt_push(*body)),
            },
            Stmt::ForIn {
                mut variable,
                iterable,
                body,
            } => {
                variable = self.id_token(&variable, true);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(variable.lexeme().to_string());
                Stmt::ForIn {
                    variable,
                    iterable: self.expr(iterable),
                    body: Box::new(self.stmt(*body)),
                }
            }
            Stmt::ForAwait {
                mut variable,
                producer,
                body,
            } => {
                variable = self.id_token(&variable, true);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(variable.lexeme().to_string());
                Stmt::ForAwait {
                    variable,
                    producer: self.expr(producer),
                    body: Box::new(self.stmt(*body)),
                }
            }
            Stmt::Function {
                mut name,
                generic_params,
                params,
                return_type,
                body,
            } => {
                name = self.id_token(&name, false);
                Stmt::Function {
                    name,
                    generic_params,
                    params: self.params(params),
                    return_type: return_type.map(|r| ntsc_ast::types::ReturnType {
                        ty: self.ty(r.ty),
                        arrow_span: r.arrow_span,
                    }),
                    body: self.stmts(body),
                }
            }
            Stmt::AsyncFunction {
                mut name,
                params,
                return_type,
                body,
            } => {
                name = self.id_token(&name, false);
                Stmt::AsyncFunction {
                    name,
                    params: self.params(params),
                    return_type: return_type.map(|r| ntsc_ast::types::ReturnType {
                        ty: self.ty(r.ty),
                        arrow_span: r.arrow_span,
                    }),
                    body: self.stmts(body),
                }
            }
            Stmt::ExternFunction {
                abi,
                mut name,
                params,
                return_type,
                c_style,
            } => {
                name = self.id_token(&name, false);
                Stmt::ExternFunction {
                    abi,
                    name,
                    params: self.params(params),
                    return_type: return_type.map(|r| ntsc_ast::types::ReturnType {
                        ty: self.ty(r.ty),
                        arrow_span: r.arrow_span,
                    }),
                    c_style,
                }
            }
            Stmt::ExternBlock { abi, functions } => Stmt::ExternBlock {
                abi,
                functions: functions.into_iter().map(|f| self.stmt(f)).collect(),
            },
            Stmt::Return { value } => Stmt::Return {
                value: value.map(|v| self.expr(v)),
            },
            Stmt::Class {
                mut name,
                generic_params,
                parent,
                mut body,
            } => {
                name = self.id_token(&name, false);
                body = self.stmts(body);
                Stmt::Class {
                    name,
                    generic_params,
                    parent: parent.map(|p| self.id_token2(p)),
                    body,
                }
            }
            Stmt::Break { span } => Stmt::Break { span },
            Stmt::Continue { span } => Stmt::Continue { span },
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => Stmt::Match {
                expression: self.expr(expression),
                cases: cases
                    .into_iter()
                    .map(|c| {
                        let mut pat = c.pattern;
                        if let Some(p) = &mut pat {
                            // Binders are locals: they must not be rewritten
                            // as module-level aliases.
                            for binding in &p.bindings {
                                self.scopes
                                    .last_mut()
                                    .unwrap()
                                    .insert(binding.lexeme().to_string());
                            }
                        }
                        ntsc_ast::stmt::MatchCase {
                            value: self.expr(c.value),
                            pattern: pat.map(|p| ntsc_ast::stmt::MatchPattern {
                                qualifier: p.qualifier.as_ref().map(|q| self.id_token(q, false)),
                                variant: self.id_token(&p.variant, false),
                                bindings: p
                                    .bindings
                                    .into_iter()
                                    .map(|b| self.id_token(&b, true))
                                    .collect(),
                            }),
                            guard: c.guard.map(|g| self.expr(g)),
                            body: self.stmt_push(c.body),
                            case_span: c.case_span,
                        }
                    })
                    .collect(),
                default_case: default_case.map(|d| Box::new(self.stmt_push(*d))),
            },
            Stmt::Try {
                try_block,
                mut catch_var,
                catch_block,
                finally_block,
            } => {
                if let Some(v) = &catch_var {
                    self.scopes
                        .last_mut()
                        .unwrap()
                        .insert(v.lexeme().to_string());
                }
                catch_var = catch_var.map(|v| self.id_token(&v, true));
                Stmt::Try {
                    try_block: Box::new(self.stmt_push(*try_block)),
                    catch_var,
                    catch_block: catch_block.map(|b| Box::new(self.stmt_push(*b))),
                    finally_block: finally_block.map(|b| Box::new(self.stmt_push(*b))),
                }
            }
            Stmt::Throw { value } => Stmt::Throw {
                value: self.expr(value),
            },
            Stmt::Retry {
                count,
                body,
                mut catch_var,
                catch_block,
            } => {
                if let Some(v) = &catch_var {
                    self.scopes
                        .last_mut()
                        .unwrap()
                        .insert(v.lexeme().to_string());
                }
                catch_var = catch_var.map(|v| self.id_token(&v, true));
                Stmt::Retry {
                    count: self.expr(count),
                    body: Box::new(self.stmt_push(*body)),
                    catch_var,
                    catch_block: catch_block.map(|b| Box::new(self.stmt_push(*b))),
                }
            }
            Stmt::Unsafe { body } => Stmt::Unsafe {
                body: Box::new(self.stmt_push(*body)),
            },
            Stmt::Defer { keyword, statement } => Stmt::Defer {
                keyword: self.id_token(&keyword, false),
                statement: Box::new(self.stmt_push(*statement)),
            },
            Stmt::Quiet { suppressed, body } => Stmt::Quiet {
                suppressed,
                body: Box::new(self.stmt_push(*body)),
            },
            Stmt::Destructure {
                is_array,
                is_tuple,
                mut names,
                keys,
                initializer,
            } => {
                names = names
                    .into_iter()
                    .map(|n| {
                        let t = self.id_token(&n, true);
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(t.lexeme().to_string());
                        t
                    })
                    .collect();
                Stmt::Destructure {
                    is_array,
                    is_tuple,
                    names,
                    keys,
                    initializer: self.expr(initializer),
                }
            }
            // A `use` statement is left untouched; rename applies only to the
            // module's own declarations.
            other @ Stmt::Use { .. } => other,
            // Already stripped by `split_visibility`; recurse defensively.
            Stmt::Private { statement } => Stmt::Private {
                statement: Box::new(self.stmt_push(*statement)),
            },
            Stmt::Enum {
                mut name,
                generic_params,
                members,
            } => {
                name = self.id_token(&name, false);
                Stmt::Enum {
                    name,
                    generic_params,
                    members: members
                        .into_iter()
                        .map(|m| ntsc_ast::stmt::EnumMember {
                            name: m.name,
                            value: m.value.map(|v| self.expr(v)),
                            data_types: m.data_types.into_iter().map(|t| self.ty(t)).collect(),
                        })
                        .collect(),
                }
            }
            Stmt::TypeAlias {
                mut name,
                generic_params,
                target,
            } => {
                name = self.id_token(&name, false);
                Stmt::TypeAlias {
                    name,
                    generic_params,
                    target: self.ty(target),
                }
            }
            Stmt::Trait {
                mut name,
                parents,
                associated_types,
                methods,
            } => {
                name = self.id_token(&name, false);
                Stmt::Trait {
                    name,
                    parents: parents
                        .into_iter()
                        .map(|p| self.id_token(&p, false))
                        .collect(),
                    associated_types: associated_types
                        .into_iter()
                        .map(|p| self.id_token(&p, false))
                        .collect(),
                    methods: methods.into_iter().map(|m| self.stmt(m)).collect(),
                }
            }
            Stmt::Impl {
                trait_name,
                type_name,
                body,
            } => Stmt::Impl {
                trait_name: self.id_token(&trait_name, false),
                type_name: self.id_token(&type_name, false),
                body: body.into_iter().map(|m| self.stmt(m)).collect(),
            },
            Stmt::Test { mut name, body } => {
                name = self.id_token(&name, true);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(name.lexeme().to_string());
                Stmt::Test {
                    name,
                    body: self.stmts(body),
                }
            }
            Stmt::ChanRecvFor {
                mut variable,
                channel,
                body,
            } => {
                variable = self.id_token(&variable, true);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(variable.lexeme().to_string());
                Stmt::ChanRecvFor {
                    variable,
                    channel: self.expr(channel),
                    body: Box::new(self.stmt(*body)),
                }
            }
            Stmt::Go {
                call,
                block,
                keyword_span,
            } => Stmt::Go {
                call: self.expr(call),
                block: block.map(|b| self.stmts(b)),
                keyword_span,
            },
        }
    }

    fn id_token2(&mut self, tok: Token) -> Token {
        self.id_token(&tok, false)
    }

    fn params(
        &mut self,
        params: Vec<ntsc_ast::expr::FunctionParam>,
    ) -> Vec<ntsc_ast::expr::FunctionParam> {
        params
            .into_iter()
            .map(|p| {
                let name = self.id_token(&p.name, true);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(name.lexeme().to_string());
                ntsc_ast::expr::FunctionParam {
                    name,
                    type_annotation: p.type_annotation.map(|t| self.ty(t)),
                }
            })
            .collect()
    }

    fn ty(&mut self, ty: TypeAnnotation) -> TypeAnnotation {
        match ty {
            TypeAnnotation::Named(t) => TypeAnnotation::Named(self.id_token2(t)),
            TypeAnnotation::Array(inner) => {
                TypeAnnotation::Array(inner.map(|i| Box::new(self.ty(*i))))
            }
            TypeAnnotation::Option(inner) => TypeAnnotation::Option(Box::new(self.ty(*inner))),
            TypeAnnotation::Result { ok, err } => TypeAnnotation::Result {
                ok: Box::new(self.ty(*ok)),
                err: Box::new(self.ty(*err)),
            },
            TypeAnnotation::View(inner, m) => TypeAnnotation::View(Box::new(self.ty(*inner)), m),
            TypeAnnotation::Shared(inner) => TypeAnnotation::Shared(Box::new(self.ty(*inner))),
            TypeAnnotation::Slice(inner) => {
                TypeAnnotation::Slice(inner.map(|i| Box::new(self.ty(*i))))
            }
            TypeAnnotation::Own(inner) => TypeAnnotation::Own(Box::new(self.ty(*inner))),
            TypeAnnotation::Ref(inner, m) => TypeAnnotation::Ref(Box::new(self.ty(*inner)), m),
            TypeAnnotation::RawPointer(inner, m) => {
                TypeAnnotation::RawPointer(Box::new(self.ty(*inner)), m)
            }
            TypeAnnotation::ImplTrait(t) => TypeAnnotation::ImplTrait(self.id_token2(t)),
            TypeAnnotation::Dyn(t) => TypeAnnotation::Dyn(self.id_token2(t)),
            TypeAnnotation::Tuple(inner) => {
                TypeAnnotation::Tuple(inner.into_iter().map(|t| self.ty(t)).collect())
            }
            other => other,
        }
    }

    // ── expressions ────────────────────────────────────────────────

    fn expr(&mut self, expr: Expr) -> Expr {
        match expr {
            Expr::Literal { value, span } => Expr::Literal { value, span },
            Expr::Variable { name } => Expr::Variable {
                name: self.id_token(&name, false),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new(self.expr(*left)),
                op,
                right: Box::new(self.expr(*right)),
            },
            Expr::Unary { op, right } => Expr::Unary {
                op,
                right: Box::new(self.expr(*right)),
            },
            Expr::PostfixUnary { op, left } => Expr::PostfixUnary {
                op,
                left: Box::new(self.expr(*left)),
            },
            Expr::Grouping {
                expression,
                open_span,
                close_span,
            } => Expr::Grouping {
                expression: Box::new(self.expr(*expression)),
                open_span,
                close_span,
            },
            Expr::Member { object, property } => {
                self.check_alias_member(&object, &property);
                Expr::Member {
                    object: Box::new(self.member_object(*object)),
                    property: self.member_property(property),
                }
            }
            Expr::OptionalMember { object, property } => {
                self.check_alias_member(&object, &property);
                Expr::OptionalMember {
                    object: Box::new(self.member_object(*object)),
                    property: self.member_property(property),
                }
            }
            Expr::Call {
                callee,
                paren,
                arguments,
            } => Expr::Call {
                callee: Box::new(self.expr(*callee)),
                paren,
                arguments: arguments.into_iter().map(|a| self.expr(a)).collect(),
            },
            Expr::Assign { name, value } => Expr::Assign {
                name: self.id_token(&name, false),
                value: Box::new(self.expr(*value)),
            },
            Expr::IndexGet { object, index } => Expr::IndexGet {
                object: Box::new(self.expr(*object)),
                index: Box::new(self.expr(*index)),
            },
            Expr::IndexSet {
                object,
                index,
                value,
            } => Expr::IndexSet {
                object: Box::new(self.expr(*object)),
                index: Box::new(self.expr(*index)),
                value: Box::new(self.expr(*value)),
            },
            Expr::MemberSet {
                object,
                property,
                value,
            } => Expr::MemberSet {
                object: Box::new(self.member_object(*object)),
                property: self.member_property(property),
                value: Box::new(self.expr(*value)),
            },
            Expr::This { keyword } => Expr::This { keyword },
            Expr::Super { keyword } => Expr::Super { keyword },
            Expr::Lambda {
                params,
                return_type,
                body,
                span,
            } => {
                self.scopes.push(HashSet::new());
                let params = self.params(params);
                let return_type = return_type.map(|r| ntsc_ast::types::ReturnType {
                    ty: self.ty(r.ty),
                    arrow_span: r.arrow_span,
                });
                let body = self.stmts(body);
                self.scopes.pop();
                Expr::Lambda {
                    params,
                    return_type,
                    body,
                    span,
                }
            }
            Expr::Ternary {
                condition,
                then_branch,
                else_branch,
            } => Expr::Ternary {
                condition: Box::new(self.expr(*condition)),
                then_branch: Box::new(self.expr(*then_branch)),
                else_branch: Box::new(self.expr(*else_branch)),
            },
            Expr::Spread { value, op_span } => Expr::Spread {
                value: Box::new(self.expr(*value)),
                op_span,
            },
            Expr::ObjectLiteral { properties, span } => Expr::ObjectLiteral {
                properties: properties
                    .into_iter()
                    .map(|p| ntsc_ast::expr::ObjectProperty {
                        key: p.key,
                        value: self.expr(p.value),
                        key_span: p.key_span,
                    })
                    .collect(),
                span,
            },
            Expr::ArrayLiteral { elements, span } => Expr::ArrayLiteral {
                elements: elements.into_iter().map(|e| self.expr(e)).collect(),
                span,
            },
            Expr::Await {
                callee,
                arguments,
                span,
            } => Expr::Await {
                callee: Box::new(self.expr(*callee)),
                arguments: arguments.into_iter().map(|a| self.expr(a)).collect(),
                span,
            },
            Expr::AsyncBlock {
                body,
                return_type,
                span,
            } => Expr::AsyncBlock {
                body: self.stmts(body),
                return_type: return_type.map(|r| ntsc_ast::types::ReturnType {
                    ty: self.ty(r.ty),
                    arrow_span: r.arrow_span,
                }),
                span,
            },
            Expr::View {
                target,
                mutable,
                keyword,
            } => Expr::View {
                target: Box::new(self.expr(*target)),
                mutable,
                keyword,
            },
            Expr::Copy {
                expression,
                keyword,
            } => Expr::Copy {
                expression: Box::new(self.expr(*expression)),
                keyword,
            },
            Expr::Borrow {
                target,
                mutable,
                keyword,
            } => Expr::Borrow {
                target: Box::new(self.expr(*target)),
                mutable,
                keyword,
            },
            Expr::RawDeref { target, star } => Expr::RawDeref {
                target: Box::new(self.expr(*target)),
                star,
            },
            Expr::RawDerefSet {
                target,
                value,
                star,
            } => Expr::RawDerefSet {
                target: Box::new(self.expr(*target)),
                value: Box::new(self.expr(*value)),
                star,
            },
            Expr::StructLiteral {
                class_name,
                fields,
                update,
                span,
            } => Expr::StructLiteral {
                class_name: self.id_token(&class_name, false),
                fields: fields
                    .into_iter()
                    .map(|p| ntsc_ast::expr::ObjectProperty {
                        key: p.key,
                        value: self.expr(p.value),
                        key_span: p.key_span,
                    })
                    .collect(),
                update: update.map(|u| Box::new(self.expr(*u))),
                span,
            },
            Expr::Propagate {
                value,
                question_span,
            } => Expr::Propagate {
                value: Box::new(self.expr(*value)),
                question_span,
            },
            Expr::TupleLiteral { elements, span } => Expr::TupleLiteral {
                elements: elements.into_iter().map(|e| self.expr(e)).collect(),
                span,
            },
            Expr::TupleIndex {
                object,
                index,
                dot_span,
            } => Expr::TupleIndex {
                object: Box::new(self.expr(*object)),
                index,
                dot_span,
            },
            Expr::ChanSend {
                channel,
                value,
                op_span,
            } => Expr::ChanSend {
                channel: Box::new(self.expr(*channel)),
                value: Box::new(self.expr(*value)),
                op_span,
            },
            Expr::ChanRecv {
                receiver,
                channel,
                op_span,
            } => Expr::ChanRecv {
                receiver: self.id_token(&receiver, true),
                channel: Box::new(self.expr(*channel)),
                op_span,
            },
            Expr::Close { channel, keyword } => Expr::Close {
                channel: Box::new(self.expr(*channel)),
                keyword,
            },
        }
    }

    /// The object of a member access: an identifier here is a *value*
    /// (module, field, or local), never the module's own free symbol, so it
    /// must not be renamed.
    fn member_object(&mut self, expr: Expr) -> Expr {
        self.expr(expr)
    }

    /// A member property is a field/method name, never renamed.
    fn member_property(&self, prop: Token) -> Token {
        prop
    }
}
