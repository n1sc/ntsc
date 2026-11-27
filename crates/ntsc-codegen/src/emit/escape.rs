//! Escape analysis for stack allocation of local class instances.

use super::*;

/// How an expression's value is consumed by its parent node.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ExprCtx {
    /// The value is used as data (an argument, operand, return value, ...).
    Data,

    /// The value is dereferenced further (the object of `Member`/`Index`/...).
    Base,
}

/// Collect every local `var x = ClassName()` that could be stack-allocated:
/// a zero-argument construction of a class without an initializer method.
pub(crate) fn collect_stack_candidates(
    body: &[Stmt],
    candidates: &mut HashSet<String>,
    module: &Module<'_>,
) {
    for stmt in body {
        if let Stmt::Var {
            name,
            type_annotation,
            initializer,
            is_static,
            is_const: _,
            view,
        } = stmt
        {
            if *is_static || type_annotation.is_some() || view.is_some() {
                continue;
            }
            let Some(init) = initializer else {
                continue;
            };
            let eligible = match init {
                Expr::Call {
                    callee, arguments, ..
                } if arguments.is_empty() => {
                    matches!(callee.as_ref(), Expr::Variable { name: ctor }
                        if module.get_struct_type(ctor.lexeme()).is_some()
                            && module
                                .get_function(&format!("{}.init", ctor.lexeme()))
                                .is_none())
                }
                _ => false,
            };
            if eligible {
                candidates.insert(name.lexeme().to_string());
            }
        }
    }
}

pub(crate) fn expr_mentions_var(expr: &Expr, var: &str) -> bool {
    match expr {
        Expr::Variable { name } => name.lexeme() == var,
        Expr::Literal { .. } | Expr::This { .. } | Expr::Super { .. } => false,
        Expr::Binary { left, right, .. } => {
            expr_mentions_var(left, var) || expr_mentions_var(right, var)
        }
        Expr::Unary { right, .. } | Expr::Spread { value: right, .. } => {
            expr_mentions_var(right, var)
        }
        Expr::PostfixUnary { left, .. } => expr_mentions_var(left, var),
        Expr::Grouping { expression, .. } => expr_mentions_var(expression, var),
        Expr::Member { object, .. } | Expr::OptionalMember { object, .. } => {
            expr_mentions_var(object, var)
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::Await {
            callee, arguments, ..
        } => expr_mentions_var(callee, var) || arguments.iter().any(|a| expr_mentions_var(a, var)),
        Expr::AsyncBlock { body, .. } => body.iter().any(|s| stmt_mentions_var(s, var)),
        Expr::Assign { name, value } => name.lexeme() == var || expr_mentions_var(value, var),
        Expr::IndexGet { object, index } => {
            expr_mentions_var(object, var) || expr_mentions_var(index, var)
        }
        Expr::IndexSet {
            object,
            index,
            value,
        } => {
            expr_mentions_var(object, var)
                || expr_mentions_var(index, var)
                || expr_mentions_var(value, var)
        }
        Expr::MemberSet { object, value, .. } => {
            expr_mentions_var(object, var) || expr_mentions_var(value, var)
        }
        Expr::Lambda { body, .. } => body.iter().any(|s| stmt_mentions_var(s, var)),
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_mentions_var(condition, var)
                || expr_mentions_var(then_branch, var)
                || expr_mentions_var(else_branch, var)
        }
        Expr::ObjectLiteral { properties, .. } => {
            properties.iter().any(|p| expr_mentions_var(&p.value, var))
        }
        Expr::ArrayLiteral { elements, .. } => elements.iter().any(|e| expr_mentions_var(e, var)),
        Expr::View { target, .. } => expr_mentions_var(target, var),
        Expr::Copy { expression, .. } => expr_mentions_var(expression, var),
        Expr::Propagate { value, .. } => expr_mentions_var(value, var),
        Expr::Borrow { target, .. } | Expr::RawDeref { target, .. } => {
            expr_mentions_var(target, var)
        }
        Expr::RawDerefSet { target, value, .. } => {
            expr_mentions_var(target, var) || expr_mentions_var(value, var)
        }
        Expr::StructLiteral {
            class_name: _,
            fields,
            update,
            ..
        } => {
            fields.iter().any(|p| expr_mentions_var(&p.value, var))
                || update.as_ref().is_some_and(|u| expr_mentions_var(u, var))
        }
        Expr::TupleLiteral { elements, .. } => elements.iter().any(|e| expr_mentions_var(e, var)),
        Expr::TupleIndex { object, .. } => expr_mentions_var(object, var),
        Expr::ChanSend { channel, value, .. } => {
            expr_mentions_var(channel, var) || expr_mentions_var(value, var)
        }
        Expr::ChanRecv { channel, .. } => expr_mentions_var(channel, var),
        Expr::Close { channel, .. } => expr_mentions_var(channel, var),
    }
}

pub(crate) fn stmt_mentions_var(stmt: &Stmt, var: &str) -> bool {
    match stmt {
        Stmt::Private { statement } => stmt_mentions_var(statement, var),
        Stmt::Expression { expression } | Stmt::Say { expression, .. } => {
            expr_mentions_var(expression, var)
        }
        Stmt::Var {
            initializer: Some(init),
            ..
        } => expr_mentions_var(init, var),
        Stmt::Var { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Function { .. }
        | Stmt::AsyncFunction { .. }
        | Stmt::ExternFunction { .. }
        | Stmt::ExternBlock { .. }
        | Stmt::Class { .. }
        | Stmt::Use { .. }
        | Stmt::Enum { .. }
        | Stmt::Test { .. }
        | Stmt::Trait { .. }
        | Stmt::Impl { .. } => false,
        Stmt::Block { statements, .. } => statements.iter().any(|s| stmt_mentions_var(s, var)),
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            expr_mentions_var(condition, var)
                || stmt_mentions_var(then_branch, var)
                || elif_branches
                    .iter()
                    .any(|e| stmt_mentions_var(&e.body, var))
                || else_branch
                    .as_deref()
                    .is_some_and(|s| stmt_mentions_var(s, var))
        }
        Stmt::While { condition, body } | Stmt::DoWhile { body, condition } => {
            expr_mentions_var(condition, var) || stmt_mentions_var(body, var)
        }
        Stmt::For {
            init,
            condition,
            update,
            body,
        } => {
            init.as_deref().is_some_and(|s| stmt_mentions_var(s, var))
                || condition
                    .as_ref()
                    .is_some_and(|e| expr_mentions_var(e, var))
                || update.as_ref().is_some_and(|e| expr_mentions_var(e, var))
                || stmt_mentions_var(body, var)
        }
        Stmt::ForIn { iterable, body, .. } => {
            expr_mentions_var(iterable, var) || stmt_mentions_var(body, var)
        }
        Stmt::ForAwait { producer, body, .. } => {
            expr_mentions_var(producer, var) || stmt_mentions_var(body, var)
        }
        Stmt::Return { value } => value.as_ref().is_some_and(|v| expr_mentions_var(v, var)),
        Stmt::Throw { value } => expr_mentions_var(value, var),
        Stmt::Match {
            expression,
            cases,
            default_case,
        } => {
            expr_mentions_var(expression, var)
                || cases.iter().any(|c| stmt_mentions_var(&c.body, var))
                || default_case
                    .as_deref()
                    .is_some_and(|s| stmt_mentions_var(s, var))
        }
        Stmt::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            stmt_mentions_var(try_block, var)
                || catch_block
                    .as_deref()
                    .is_some_and(|s| stmt_mentions_var(s, var))
                || finally_block
                    .as_deref()
                    .is_some_and(|s| stmt_mentions_var(s, var))
        }
        Stmt::Retry {
            count,
            body,
            catch_block,
            ..
        } => {
            expr_mentions_var(count, var)
                || stmt_mentions_var(body, var)
                || catch_block
                    .as_deref()
                    .is_some_and(|s| stmt_mentions_var(s, var))
        }
        Stmt::Unsafe { body } => stmt_mentions_var(body, var),
        Stmt::Quiet { body, .. } => stmt_mentions_var(body, var),
        Stmt::Defer { statement, .. } => stmt_mentions_var(statement, var),
        Stmt::Destructure { initializer, .. } => expr_mentions_var(initializer, var),
        Stmt::TypeAlias { .. } => false,
        Stmt::ChanRecvFor { channel, body, .. } => {
            expr_mentions_var(channel, var) || stmt_mentions_var(body, var)
        }
        Stmt::Go { call, block, .. } => {
            expr_mentions_var(call, var)
                || block
                    .as_ref()
                    .is_some_and(|stmts| stmts.iter().any(|s| stmt_mentions_var(s, var)))
        }
    }
}

pub(crate) fn expr_uses_var_safely(expr: &Expr, var: &str, ctx: ExprCtx) -> bool {
    // Expressions that never reference `var` cannot leak it; skip the
    // context-sensitive rules for unrelated subtrees.
    if !expr_mentions_var(expr, var) {
        return true;
    }
    match expr {
        // A bare reference is only safe as a single-level base.
        Expr::Variable { name: _ } => ctx == ExprCtx::Base,
        Expr::Literal { .. } | Expr::This { .. } | Expr::Super { .. } => true,
        // Grouping an object base is treated as an escape (conservative).
        Expr::Grouping { expression, .. } => expr_uses_var_safely(expression, var, ExprCtx::Data),
        // `var.field`: the field value may be used anywhere but not
        // dereferenced again (no member chains) — a chain could hand out an
        // interior address.
        Expr::Member { object, .. } | Expr::OptionalMember { object, .. } => {
            expr_uses_var_safely(object, var, ExprCtx::Base) && ctx == ExprCtx::Data
        }
        // `var[i]` — safe, unless chained further (see `Member`).
        Expr::IndexGet { object, index } => {
            expr_uses_var_safely(object, var, ExprCtx::Base)
                && expr_uses_var_safely(index, var, ExprCtx::Data)
                && ctx == ExprCtx::Data
        }
        // `var[i] = v` and `var.field = v` write a slot; they never leak
        // the address.
        Expr::IndexSet {
            object,
            index,
            value,
        } => {
            expr_uses_var_safely(object, var, ExprCtx::Base)
                && expr_uses_var_safely(index, var, ExprCtx::Data)
                && expr_uses_var_safely(value, var, ExprCtx::Data)
        }
        Expr::MemberSet { object, value, .. } => {
            expr_uses_var_safely(object, var, ExprCtx::Base)
                && expr_uses_var_safely(value, var, ExprCtx::Data)
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            // A method can return or store its receiver.
            expr_uses_var_safely(callee, var, ExprCtx::Data)
                && !matches!(callee.as_ref(), Expr::Member { object, .. } if expr_mentions_var(object, var))
                && arguments
                    .iter()
                    .all(|arg| expr_uses_var_safely(arg, var, ExprCtx::Data))
        }
        Expr::Await {
            callee, arguments, ..
        } => {
            expr_uses_var_safely(callee, var, ExprCtx::Base)
                && arguments
                    .iter()
                    .all(|a| expr_uses_var_safely(a, var, ExprCtx::Data))
        }
        Expr::AsyncBlock { body, .. } => body.iter().all(|s| stmt_uses_var_safely(s, var)),
        Expr::Assign { name, value } => {
            name.lexeme() != var && expr_uses_var_safely(value, var, ExprCtx::Data)
        }
        Expr::Binary { left, right, .. } => {
            expr_uses_var_safely(left, var, ExprCtx::Data)
                && expr_uses_var_safely(right, var, ExprCtx::Data)
        }
        Expr::Unary { right, .. } | Expr::Spread { value: right, .. } => {
            expr_uses_var_safely(right, var, ExprCtx::Data)
        }
        Expr::PostfixUnary { left, .. } => expr_uses_var_safely(left, var, ExprCtx::Data),
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_uses_var_safely(condition, var, ExprCtx::Data)
                && expr_uses_var_safely(then_branch, var, ExprCtx::Data)
                && expr_uses_var_safely(else_branch, var, ExprCtx::Data)
        }
        Expr::ObjectLiteral { properties, .. } => properties
            .iter()
            .all(|p| expr_uses_var_safely(&p.value, var, ExprCtx::Data)),
        Expr::ArrayLiteral { elements, .. } => elements
            .iter()
            .all(|e| expr_uses_var_safely(e, var, ExprCtx::Data)),
        Expr::Lambda { body, .. } => !body.iter().any(|s| stmt_mentions_var(s, var)),
        Expr::View { target, .. } => expr_uses_var_safely(target, var, ExprCtx::Data),
        Expr::Copy { expression, .. } => expr_uses_var_safely(expression, var, ExprCtx::Base),
        Expr::Propagate { value, .. } => expr_uses_var_safely(value, var, ExprCtx::Data),
        Expr::Borrow { target, .. } | Expr::RawDeref { target, .. } => {
            expr_uses_var_safely(target, var, ExprCtx::Data)
        }
        Expr::RawDerefSet { target, value, .. } => {
            expr_uses_var_safely(target, var, ExprCtx::Data)
                && expr_uses_var_safely(value, var, ExprCtx::Data)
        }
        Expr::StructLiteral {
            class_name: _,
            fields,
            update,
            ..
        } => {
            fields
                .iter()
                .all(|p| expr_uses_var_safely(&p.value, var, ExprCtx::Data))
                // A `..base` update deep-copies the base's fields and never
                // aliases the base, so it reads like `copy(base)`.
                && update
                    .as_ref()
                    .is_none_or(|u| expr_uses_var_safely(u, var, ExprCtx::Base))
        }
        Expr::TupleLiteral { elements, .. } => elements
            .iter()
            .all(|e| expr_uses_var_safely(e, var, ExprCtx::Data)),
        Expr::TupleIndex { object, .. } => expr_uses_var_safely(object, var, ExprCtx::Base),
        // Channel operations are not yet supported by codegen; conservatively
        // refuse to classify them as safe.
        Expr::ChanSend { .. } | Expr::ChanRecv { .. } | Expr::Close { .. } => false,
    }
}

pub(crate) fn stmt_uses_var_safely(stmt: &Stmt, var: &str) -> bool {
    match stmt {
        Stmt::Private { statement } => stmt_uses_var_safely(statement, var),
        Stmt::Expression { expression } | Stmt::Say { expression, .. } => {
            expr_uses_var_safely(expression, var, ExprCtx::Data)
        }
        Stmt::Return { value } => value
            .as_ref()
            .is_none_or(|v| expr_uses_var_safely(v, var, ExprCtx::Data)),
        Stmt::Throw { value } => expr_uses_var_safely(value, var, ExprCtx::Data),
        Stmt::Var {
            initializer: Some(init),
            ..
        } => expr_uses_var_safely(init, var, ExprCtx::Data),
        Stmt::Var { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Function { .. }
        | Stmt::AsyncFunction { .. }
        | Stmt::ExternFunction { .. }
        | Stmt::ExternBlock { .. }
        | Stmt::Class { .. }
        | Stmt::Use { .. }
        | Stmt::Enum { .. }
        | Stmt::TypeAlias { .. }
        | Stmt::Test { .. }
        | Stmt::Trait { .. }
        | Stmt::Impl { .. } => true,
        Stmt::Block { statements, .. } => statements.iter().all(|s| stmt_uses_var_safely(s, var)),
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            expr_uses_var_safely(condition, var, ExprCtx::Data)
                && stmt_uses_var_safely(then_branch, var)
                && elif_branches.iter().all(|e| {
                    expr_uses_var_safely(&e.condition, var, ExprCtx::Data)
                        && stmt_uses_var_safely(&e.body, var)
                })
                && else_branch
                    .as_deref()
                    .is_none_or(|s| stmt_uses_var_safely(s, var))
        }
        Stmt::While { condition, body } => {
            expr_uses_var_safely(condition, var, ExprCtx::Data) && stmt_uses_var_safely(body, var)
        }
        Stmt::DoWhile { body, condition } => {
            stmt_uses_var_safely(body, var) && expr_uses_var_safely(condition, var, ExprCtx::Data)
        }
        Stmt::For {
            init,
            condition,
            update,
            body,
        } => {
            init.as_deref().is_none_or(|s| stmt_uses_var_safely(s, var))
                && condition
                    .as_ref()
                    .is_none_or(|e| expr_uses_var_safely(e, var, ExprCtx::Data))
                && update
                    .as_ref()
                    .is_none_or(|e| expr_uses_var_safely(e, var, ExprCtx::Data))
                && stmt_uses_var_safely(body, var)
        }
        Stmt::ForIn { iterable, body, .. } => {
            expr_uses_var_safely(iterable, var, ExprCtx::Data) && stmt_uses_var_safely(body, var)
        }
        Stmt::ForAwait { producer, body, .. } => {
            expr_uses_var_safely(producer, var, ExprCtx::Data) && stmt_uses_var_safely(body, var)
        }
        Stmt::Match {
            expression,
            cases,
            default_case,
        } => {
            expr_uses_var_safely(expression, var, ExprCtx::Data)
                && cases.iter().all(|c| {
                    expr_uses_var_safely(&c.value, var, ExprCtx::Data)
                        && c.guard
                            .as_ref()
                            .is_none_or(|g| expr_uses_var_safely(g, var, ExprCtx::Data))
                        && stmt_uses_var_safely(&c.body, var)
                })
                && default_case
                    .as_deref()
                    .is_none_or(|s| stmt_uses_var_safely(s, var))
        }
        Stmt::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            stmt_uses_var_safely(try_block, var)
                && catch_block
                    .as_deref()
                    .is_none_or(|s| stmt_uses_var_safely(s, var))
                && finally_block
                    .as_deref()
                    .is_none_or(|s| stmt_uses_var_safely(s, var))
        }
        Stmt::Retry {
            count,
            body,
            catch_block,
            ..
        } => {
            expr_uses_var_safely(count, var, ExprCtx::Data)
                && stmt_uses_var_safely(body, var)
                && catch_block
                    .as_deref()
                    .is_none_or(|s| stmt_uses_var_safely(s, var))
        }
        Stmt::Unsafe { body } => stmt_uses_var_safely(body, var),
        Stmt::Quiet { body, .. } => stmt_uses_var_safely(body, var),
        Stmt::Defer { statement, .. } => stmt_uses_var_safely(statement, var),
        Stmt::Destructure { initializer, .. } => {
            expr_uses_var_safely(initializer, var, ExprCtx::Data)
        }
        // Goroutines and channel iteration are not yet supported by codegen;
        // conservatively refuse to classify them as safe.
        Stmt::ChanRecvFor { .. } | Stmt::Go { .. } => false,
    }
}

/// Allocate an instance on the stack only when every use stays in this frame.
pub(crate) fn analyze_stack_allocatable(body: &[Stmt], module: &Module<'_>) -> HashSet<String> {
    let mut candidates = HashSet::new();
    collect_stack_candidates(body, &mut candidates, module);
    candidates.retain(|name| body.iter().all(|stmt| stmt_uses_var_safely(stmt, name)));
    candidates
}
