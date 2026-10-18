//! Byte-offset translation for merged programs: each module's spans are
//! relative to its own source, so merging makes ranges overlap. Shifting by a
//! per-module base keeps them unique; line/column are never shifted.

use crate::expr::{Expr, FunctionParam, ObjectProperty};
use crate::span::Span;
use crate::stmt::{ElifBranch, EnumMember, GenericParam, MatchCase, MatchPattern, Program, Stmt};
use crate::token::Token;
use crate::types::{ReturnType, TypeAnnotation};

impl Program {
    /// Add `base` to every span's byte offsets (dummy spans untouched);
    /// line/column numbers are preserved.
    pub fn shift_spans(&mut self, base: usize) {
        for stmt in &mut self.statements {
            shift_stmt(stmt, base);
        }
    }
}

fn shift_span(span: &mut Span, base: usize) {
    // Dummy spans are a (0, 0) sentinel and stay put; a real token can never
    // occupy an empty byte range.
    if span.start != 0 || span.end != 0 {
        span.start += base;
        span.end += base;
    }
}

fn shift_token(token: &mut Token, base: usize) {
    shift_span(&mut token.span, base);
}

fn shift_type(ty: &mut TypeAnnotation, base: usize) {
    match ty {
        TypeAnnotation::Named(token) => shift_token(token, base),
        TypeAnnotation::Array(Some(inner)) => shift_type(inner, base),
        TypeAnnotation::Option(inner) => shift_type(inner, base),
        _ => {}
    }
}

fn shift_return(return_type: &mut ReturnType, base: usize) {
    shift_type(&mut return_type.ty, base);
    shift_span(&mut return_type.arrow_span, base);
}

fn shift_param(param: &mut FunctionParam, base: usize) {
    shift_token(&mut param.name, base);
    if let Some(ty) = &mut param.type_annotation {
        shift_type(ty, base);
    }
}

fn shift_generic_param(param: &mut GenericParam, base: usize) {
    shift_token(&mut param.name, base);
    for bound in &mut param.bounds {
        shift_token(bound, base);
    }
}

fn shift_expr(expr: &mut Expr, base: usize) {
    match expr {
        Expr::Literal { span, .. } => shift_span(span, base),
        Expr::Variable { name } => shift_token(name, base),
        Expr::Binary { left, op, right } => {
            shift_expr(left, base);
            shift_token(op, base);
            shift_expr(right, base);
        }
        Expr::Unary { op, right } => {
            shift_token(op, base);
            shift_expr(right, base);
        }
        Expr::PostfixUnary { op, left } => {
            shift_token(op, base);
            shift_expr(left, base);
        }
        Expr::Grouping {
            expression,
            open_span,
            close_span,
        } => {
            shift_expr(expression, base);
            shift_span(open_span, base);
            shift_span(close_span, base);
        }
        Expr::Member { object, property } => {
            shift_expr(object, base);
            shift_token(property, base);
        }
        Expr::OptionalMember { object, property } => {
            shift_expr(object, base);
            shift_token(property, base);
        }
        Expr::Call {
            callee,
            paren,
            arguments,
        } => {
            shift_expr(callee, base);
            shift_span(paren, base);
            for argument in arguments {
                shift_expr(argument, base);
            }
        }
        Expr::Assign { name, value } => {
            shift_token(name, base);
            shift_expr(value, base);
        }
        Expr::IndexGet { object, index } => {
            shift_expr(object, base);
            shift_expr(index, base);
        }
        Expr::IndexSet {
            object,
            index,
            value,
        } => {
            shift_expr(object, base);
            shift_expr(index, base);
            shift_expr(value, base);
        }
        Expr::MemberSet {
            object,
            property,
            value,
        } => {
            shift_expr(object, base);
            shift_token(property, base);
            shift_expr(value, base);
        }
        Expr::This { keyword } => shift_token(keyword, base),
        Expr::Lambda {
            params,
            return_type,
            body,
            span,
        } => {
            for param in params {
                shift_param(param, base);
            }
            if let Some(return_type) = return_type {
                shift_return(return_type, base);
            }
            for stmt in body {
                shift_stmt(stmt, base);
            }
            shift_span(span, base);
        }
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => {
            shift_expr(condition, base);
            shift_expr(then_branch, base);
            shift_expr(else_branch, base);
        }
        Expr::Spread { value, op_span } => {
            shift_expr(value, base);
            shift_span(op_span, base);
        }
        Expr::ObjectLiteral { properties, span } => {
            for ObjectProperty {
                key_span, value, ..
            } in properties
            {
                shift_span(key_span, base);
                shift_expr(value, base);
            }
            shift_span(span, base);
        }
        Expr::ArrayLiteral { elements, span } => {
            for element in elements {
                shift_expr(element, base);
            }
            shift_span(span, base);
        }
        Expr::Await {
            callee,
            arguments,
            span,
        } => {
            shift_expr(callee, base);
            for argument in arguments {
                shift_expr(argument, base);
            }
            shift_span(span, base);
        }
        Expr::View {
            target, keyword, ..
        } => {
            shift_expr(target, base);
            shift_span(keyword, base);
        }
        Expr::Copy {
            expression,
            keyword,
        } => {
            shift_expr(expression, base);
            shift_span(keyword, base);
        }
        Expr::Borrow {
            target, keyword, ..
        } => {
            shift_expr(target, base);
            shift_span(keyword, base);
        }
        Expr::RawDeref { target, star } => {
            shift_expr(target, base);
            shift_span(star, base);
        }
        Expr::RawDerefSet {
            target,
            value,
            star,
        } => {
            shift_expr(target, base);
            shift_expr(value, base);
            shift_span(star, base);
        }
        Expr::StructLiteral {
            class_name,
            fields,
            update,
            span,
        } => {
            shift_token(class_name, base);
            for prop in fields {
                shift_span(&mut prop.key_span, base);
                shift_expr(&mut prop.value, base);
            }
            if let Some(update) = update {
                shift_expr(update, base);
            }
            shift_span(span, base);
        }
        Expr::Propagate {
            value,
            question_span,
        } => {
            shift_expr(value, base);
            shift_span(question_span, base);
        }
        Expr::AsyncBlock {
            body,
            return_type,
            span,
        } => {
            for stmt in body {
                shift_stmt(stmt, base);
            }
            if let Some(rt) = return_type {
                shift_return(rt, base);
            }
            shift_span(span, base);
        }
        Expr::TupleLiteral { elements, span } => {
            for element in elements {
                shift_expr(element, base);
            }
            shift_span(span, base);
        }
        Expr::TupleIndex {
            object, dot_span, ..
        } => {
            shift_expr(object, base);
            shift_span(dot_span, base);
        }
        Expr::ChanSend {
            channel,
            value,
            op_span,
        } => {
            shift_expr(channel, base);
            shift_expr(value, base);
            shift_span(op_span, base);
        }
        Expr::ChanRecv {
            receiver,
            channel,
            op_span,
        } => {
            shift_token(receiver, base);
            shift_expr(channel, base);
            shift_span(op_span, base);
        }
        Expr::Close { channel, keyword } => {
            shift_expr(channel, base);
            shift_span(keyword, base);
        }
    }
}

fn shift_stmt(stmt: &mut Stmt, base: usize) {
    match stmt {
        Stmt::Expression { expression } => shift_expr(expression, base),
        Stmt::Say {
            expression,
            keyword_span,
        } => {
            shift_expr(expression, base);
            shift_span(keyword_span, base);
        }
        Stmt::Var {
            name,
            type_annotation,
            initializer,
            ..
        } => {
            shift_token(name, base);
            if let Some(ty) = type_annotation {
                shift_type(ty, base);
            }
            if let Some(initializer) = initializer {
                shift_expr(initializer, base);
            }
        }
        Stmt::Block {
            statements,
            open_span,
            close_span,
        } => {
            for stmt in statements {
                shift_stmt(stmt, base);
            }
            shift_span(open_span, base);
            shift_span(close_span, base);
        }
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            shift_expr(condition, base);
            shift_stmt(then_branch, base);
            for ElifBranch {
                condition,
                body,
                elif_span,
            } in elif_branches
            {
                shift_expr(condition, base);
                shift_stmt(body, base);
                shift_span(elif_span, base);
            }
            if let Some(else_branch) = else_branch {
                shift_stmt(else_branch, base);
            }
        }
        Stmt::While { condition, body } => {
            shift_expr(condition, base);
            shift_stmt(body, base);
        }
        Stmt::DoWhile { body, condition } => {
            shift_stmt(body, base);
            shift_expr(condition, base);
        }
        Stmt::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(init) = init {
                shift_stmt(init, base);
            }
            if let Some(condition) = condition {
                shift_expr(condition, base);
            }
            if let Some(update) = update {
                shift_expr(update, base);
            }
            shift_stmt(body, base);
        }
        Stmt::ForIn {
            variable,
            iterable,
            body,
        } => {
            shift_token(variable, base);
            shift_expr(iterable, base);
            shift_stmt(body, base);
        }
        Stmt::ForAwait {
            variable,
            producer,
            body,
        } => {
            shift_token(variable, base);
            shift_expr(producer, base);
            shift_stmt(body, base);
        }
        Stmt::ChanRecvFor {
            variable,
            channel,
            body,
        } => {
            shift_token(variable, base);
            shift_expr(channel, base);
            shift_stmt(body, base);
        }
        Stmt::Go {
            call,
            block,
            keyword_span,
        } => {
            shift_expr(call, base);
            if let Some(block) = block {
                for stmt in block {
                    shift_stmt(stmt, base);
                }
            }
            shift_span(keyword_span, base);
        }
        Stmt::Function {
            generic_params,
            name,
            params,
            return_type,
            body,
        } => {
            for param in generic_params {
                shift_generic_param(param, base);
            }
            shift_token(name, base);
            for param in params {
                shift_param(param, base);
            }
            if let Some(return_type) = return_type {
                shift_return(return_type, base);
            }
            for stmt in body {
                shift_stmt(stmt, base);
            }
        }
        Stmt::AsyncFunction {
            name,
            params,
            return_type,
            body,
        } => {
            shift_token(name, base);
            for param in params {
                shift_param(param, base);
            }
            if let Some(return_type) = return_type {
                shift_return(return_type, base);
            }
            for stmt in body {
                shift_stmt(stmt, base);
            }
        }
        Stmt::ExternFunction {
            abi,
            name,
            params,
            return_type,
            ..
        } => {
            shift_token(abi, base);
            shift_token(name, base);
            for param in params {
                shift_param(param, base);
            }
            if let Some(return_type) = return_type {
                shift_return(return_type, base);
            }
        }
        Stmt::ExternBlock { abi, functions } => {
            shift_token(abi, base);
            for function in functions {
                shift_stmt(function, base);
            }
        }
        Stmt::Return { value } => {
            if let Some(value) = value {
                shift_expr(value, base);
            }
        }
        Stmt::Class {
            name,
            generic_params,
            parent,
            body,
        } => {
            shift_token(name, base);
            for param in generic_params {
                shift_generic_param(param, base);
            }
            if let Some(parent) = parent {
                shift_token(parent, base);
            }
            for stmt in body {
                shift_stmt(stmt, base);
            }
        }
        Stmt::Break { span } => shift_span(span, base),
        Stmt::Continue { span } => shift_span(span, base),
        Stmt::Match {
            expression,
            cases,
            default_case,
        } => {
            shift_expr(expression, base);
            for MatchCase {
                value,
                pattern,
                guard,
                body,
                case_span,
            } in cases
            {
                shift_expr(value, base);
                if let Some(MatchPattern { variant, binding }) = pattern {
                    shift_span(&mut variant.span, base);
                    if let Some(binding) = binding {
                        shift_span(&mut binding.span, base);
                    }
                }
                if let Some(guard) = guard {
                    shift_expr(guard, base);
                }
                shift_stmt(body, base);
                shift_span(case_span, base);
            }
            if let Some(default_case) = default_case {
                shift_stmt(default_case, base);
            }
        }
        Stmt::Try {
            try_block,
            catch_var,
            catch_block,
            finally_block,
        } => {
            shift_stmt(try_block, base);
            if let Some(catch_var) = catch_var {
                shift_token(catch_var, base);
            }
            if let Some(catch_block) = catch_block {
                shift_stmt(catch_block, base);
            }
            if let Some(finally_block) = finally_block {
                shift_stmt(finally_block, base);
            }
        }
        Stmt::Throw { value } => shift_expr(value, base),
        Stmt::Retry {
            count,
            body,
            catch_var,
            catch_block,
        } => {
            shift_expr(count, base);
            shift_stmt(body, base);
            if let Some(catch_var) = catch_var {
                shift_token(catch_var, base);
            }
            if let Some(catch_block) = catch_block {
                shift_stmt(catch_block, base);
            }
        }
        Stmt::Unsafe { body } => shift_stmt(body, base),
        Stmt::Quiet { body, .. } => shift_stmt(body, base),
        Stmt::Destructure {
            names, initializer, ..
        } => {
            for name in names {
                shift_token(name, base);
            }
            shift_expr(initializer, base);
        }
        Stmt::Use {
            library,
            imported_symbols,
            alias,
            ..
        } => {
            shift_token(library, base);
            for symbol in imported_symbols {
                shift_token(symbol, base);
            }
            if let Some(alias) = alias {
                shift_token(alias, base);
            }
        }
        Stmt::Enum {
            name,
            generic_params,
            members,
        } => {
            shift_token(name, base);
            for param in generic_params {
                shift_generic_param(param, base);
            }
            for EnumMember {
                name,
                value,
                data_types: _,
            } in members
            {
                shift_token(name, base);
                if let Some(value) = value {
                    shift_expr(value, base);
                }
            }
        }
        Stmt::TypeAlias {
            name,
            generic_params,
            target,
        } => {
            shift_token(name, base);
            for param in generic_params {
                shift_generic_param(param, base);
            }
            shift_type(target, base);
        }
        Stmt::Trait {
            name,
            parents,
            associated_types,
            methods,
        } => {
            shift_token(name, base);
            for parent in parents {
                shift_token(parent, base);
            }
            for associated_type in associated_types {
                shift_token(associated_type, base);
            }
            for method in methods {
                shift_stmt(method, base);
            }
        }
        Stmt::Impl {
            trait_name,
            type_name,
            body,
        } => {
            shift_token(trait_name, base);
            shift_token(type_name, base);
            for member in body {
                shift_stmt(member, base);
            }
        }
        Stmt::Test { name, body } => {
            shift_token(name, base);
            for stmt in body {
                shift_stmt(stmt, base);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::LiteralValue;

    fn token(kind: crate::token::TokenKind, start: usize, end: usize) -> Token {
        Token::new(kind, Span::new(start, end, 1, 1))
    }

    #[test]
    fn shifts_nested_statement_spans() {
        let mut program = Program {
            statements: vec![Stmt::Var {
                name: token(crate::token::TokenKind::Identifier("x".into()), 4, 5),
                type_annotation: None,
                initializer: Some(Expr::Literal {
                    value: LiteralValue::Number("1".into()),
                    span: Span::new(8, 9, 1, 9),
                }),
                is_static: false,
                is_const: false,
                view: None,
            }],
        };
        program.shift_spans(100);
        match &program.statements[0] {
            Stmt::Var {
                name, initializer, ..
            } => {
                assert_eq!(name.span, Span::new(104, 105, 1, 1));
                assert_eq!(
                    initializer.as_ref().unwrap().span(),
                    Span::new(108, 109, 1, 9)
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn leaves_dummy_spans_untouched() {
        let mut program = Program {
            statements: vec![Stmt::Expression {
                expression: Expr::Literal {
                    value: LiteralValue::String(String::new()),
                    span: Span::dummy(),
                },
            }],
        };
        program.shift_spans(50);
        match &program.statements[0] {
            Stmt::Expression { expression } => {
                assert_eq!(expression.span(), Span::dummy());
            }
            _ => unreachable!(),
        }
    }
}
