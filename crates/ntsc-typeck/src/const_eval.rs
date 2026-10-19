//! Compile-time constant expression evaluator.
//!
//! Evaluates `static const` initializers at build time. Supports integer and
//! float arithmetic, unary operators, references to earlier constants, and
//! calls to pure functions (functions whose body is a single `return expr`).

use std::collections::HashMap;

use ntsc_ast::expr::{Expr, LiteralValue};
use ntsc_ast::token::TokenKind;

/// A compile-time evaluated constant value.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
}

impl ConstValue {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ConstValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ConstValue::Float(v) => Some(*v),
            ConstValue::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ConstValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ConstValue::String(v) => Some(v),
            _ => None,
        }
    }
}

/// Statement type used for function bodies during const evaluation.
pub type BodyStmt = ntsc_ast::stmt::Stmt;

fn is_const_binary_op(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::EqualEqual
            | TokenKind::BangEqual
            | TokenKind::Less
            | TokenKind::LessEqual
            | TokenKind::Greater
            | TokenKind::GreaterEqual
    )
}

/// Compile-time constant evaluator.
pub struct ConstEvaluator {
    /// Already-evaluated constants (name → value).
    pub(crate) constants: HashMap<String, ConstValue>,
    /// Function bodies for pure-function evaluation at build time.
    pub(crate) fn_bodies: HashMap<String, Vec<BodyStmt>>,
    /// Function parameter names in declaration order.
    pub(crate) fn_params: HashMap<String, Vec<String>>,
    /// Functions currently being evaluated (for cycle detection).
    evaluating: HashMap<String, ntsc_ast::span::Span>,
}

impl ConstEvaluator {
    pub fn new() -> Self {
        Self {
            constants: HashMap::new(),
            fn_bodies: HashMap::new(),
            fn_params: HashMap::new(),
            evaluating: HashMap::new(),
        }
    }

    /// Returns `true` when `expr` is a compile-time constant expression:
    /// literals, unary `-`, parenthesized constants, references to earlier
    /// `static const` variables, binary arithmetic on constants, and calls
    /// to pure functions with constant arguments.
    pub fn is_constant_expr(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Literal { .. } => true,
            Expr::Unary { op, right } if matches!(op.kind, TokenKind::Minus) => {
                self.is_constant_expr(right)
            }
            Expr::Grouping { expression, .. } => self.is_constant_expr(expression),
            Expr::Binary { left, op, right } => {
                is_const_binary_op(&op.kind)
                    && self.is_constant_expr(left)
                    && self.is_constant_expr(right)
            }
            Expr::Variable { name } => self.constants.contains_key(name.lexeme()),
            Expr::Call {
                callee, arguments, ..
            } => {
                let fn_name = match callee.as_ref() {
                    Expr::Variable { name } => name.lexeme(),
                    _ => return false,
                };
                self.fn_bodies.get(fn_name).is_some_and(|body| {
                    is_pure_function(body) && arguments.iter().all(|a| self.is_constant_expr(a))
                })
            }
            _ => false,
        }
    }

    /// Evaluate a constant expression. Returns `None` when the expression
    /// cannot be reduced at compile time.
    pub fn eval(&mut self, expr: &Expr) -> Option<ConstValue> {
        match expr {
            Expr::Literal { value, .. } => match value {
                LiteralValue::Number(n) => {
                    if n.contains('.') {
                        n.parse::<f64>().ok().map(ConstValue::Float)
                    } else {
                        ntsc_ast::expr::parse_int_literal(n).map(ConstValue::Int)
                    }
                }
                LiteralValue::Bool(b) => Some(ConstValue::Bool(*b)),
                LiteralValue::String(s) => Some(ConstValue::String(s.clone())),
                LiteralValue::Nil => None,
            },
            Expr::Unary { op, right, .. } if matches!(op.kind, TokenKind::Minus) => {
                let val = self.eval(right)?;
                match val {
                    ConstValue::Int(v) => Some(ConstValue::Int(-v)),
                    ConstValue::Float(v) => Some(ConstValue::Float(-v)),
                    _ => None,
                }
            }
            Expr::Grouping { expression, .. } => self.eval(expression),
            Expr::Binary {
                left, op, right, ..
            } => self.eval_binary(left, &op.kind, right),
            Expr::Variable { name } => self.constants.get(name.lexeme()).cloned(),
            Expr::Call {
                callee, arguments, ..
            } => self.eval_call(callee, arguments),
            _ => None,
        }
    }

    fn eval_binary(
        &mut self,
        left: &Expr,
        op_kind: &TokenKind,
        right: &Expr,
    ) -> Option<ConstValue> {
        let l = self.eval(left)?;
        let r = self.eval(right)?;

        match (op_kind, &l, &r) {
            // Comparison operators (work on matching types).
            (TokenKind::EqualEqual, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Bool(a == b))
            }
            (TokenKind::BangEqual, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Bool(a != b))
            }
            (TokenKind::Less, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Bool(a < b))
            }
            (TokenKind::LessEqual, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Bool(a <= b))
            }
            (TokenKind::Greater, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Bool(a > b))
            }
            (TokenKind::GreaterEqual, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Bool(a >= b))
            }
            (TokenKind::EqualEqual, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Bool(a == b))
            }
            (TokenKind::BangEqual, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Bool(a != b))
            }
            (TokenKind::Less, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Bool(a < b))
            }
            (TokenKind::LessEqual, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Bool(a <= b))
            }
            (TokenKind::Greater, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Bool(a > b))
            }
            (TokenKind::GreaterEqual, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Bool(a >= b))
            }
            (TokenKind::EqualEqual, ConstValue::Bool(a), ConstValue::Bool(b)) => {
                Some(ConstValue::Bool(a == b))
            }
            (TokenKind::BangEqual, ConstValue::Bool(a), ConstValue::Bool(b)) => {
                Some(ConstValue::Bool(a != b))
            }
            (TokenKind::EqualEqual, ConstValue::String(a), ConstValue::String(b)) => {
                Some(ConstValue::Bool(a == b))
            }
            (TokenKind::BangEqual, ConstValue::String(a), ConstValue::String(b)) => {
                Some(ConstValue::Bool(a != b))
            }

            // Arithmetic operators on ints.
            (TokenKind::Plus, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Int(a + b))
            }
            (TokenKind::Minus, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Int(a - b))
            }
            (TokenKind::Star, ConstValue::Int(a), ConstValue::Int(b)) => {
                Some(ConstValue::Int(a * b))
            }
            (TokenKind::Slash, ConstValue::Int(a), ConstValue::Int(b)) => {
                if *b == 0 {
                    None
                } else {
                    Some(ConstValue::Int(a / b))
                }
            }
            (TokenKind::Percent, ConstValue::Int(a), ConstValue::Int(b)) => {
                if *b == 0 {
                    None
                } else {
                    Some(ConstValue::Int(a % b))
                }
            }

            // Arithmetic operators on floats.
            (TokenKind::Plus, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Float(a + b))
            }
            (TokenKind::Minus, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Float(a - b))
            }
            (TokenKind::Star, ConstValue::Float(a), ConstValue::Float(b)) => {
                Some(ConstValue::Float(a * b))
            }
            (TokenKind::Slash, ConstValue::Float(a), ConstValue::Float(b)) => {
                if *b == 0.0 {
                    None
                } else {
                    Some(ConstValue::Float(a / b))
                }
            }
            (TokenKind::Percent, ConstValue::Float(a), ConstValue::Float(b)) => {
                if *b == 0.0 {
                    None
                } else {
                    Some(ConstValue::Float(a % b))
                }
            }

            // Mixed int/float arithmetic: promote to float.
            (TokenKind::Plus, ConstValue::Int(a), ConstValue::Float(b)) => {
                Some(ConstValue::Float(*a as f64 + b))
            }
            (TokenKind::Plus, ConstValue::Float(a), ConstValue::Int(b)) => {
                Some(ConstValue::Float(a + *b as f64))
            }
            (TokenKind::Minus, ConstValue::Int(a), ConstValue::Float(b)) => {
                Some(ConstValue::Float(*a as f64 - b))
            }
            (TokenKind::Minus, ConstValue::Float(a), ConstValue::Int(b)) => {
                Some(ConstValue::Float(a - *b as f64))
            }
            (TokenKind::Star, ConstValue::Int(a), ConstValue::Float(b)) => {
                Some(ConstValue::Float(*a as f64 * b))
            }
            (TokenKind::Star, ConstValue::Float(a), ConstValue::Int(b)) => {
                Some(ConstValue::Float(a * *b as f64))
            }
            (TokenKind::Slash, ConstValue::Int(a), ConstValue::Float(b)) => {
                if *b == 0.0 {
                    None
                } else {
                    Some(ConstValue::Float(*a as f64 / b))
                }
            }
            (TokenKind::Slash, ConstValue::Float(a), ConstValue::Int(b)) => {
                if *b == 0 {
                    None
                } else {
                    Some(ConstValue::Float(a / *b as f64))
                }
            }
            (TokenKind::Percent, ConstValue::Int(a), ConstValue::Float(b)) => {
                if *b == 0.0 {
                    None
                } else {
                    Some(ConstValue::Float(*a as f64 % b))
                }
            }
            (TokenKind::Percent, ConstValue::Float(a), ConstValue::Int(b)) => {
                if *b == 0 {
                    None
                } else {
                    Some(ConstValue::Float(a % *b as f64))
                }
            }

            _ => None,
        }
    }

    fn eval_call(&mut self, callee: &Expr, arguments: &[Expr]) -> Option<ConstValue> {
        let fn_name = match callee {
            Expr::Variable { name } => name.lexeme(),
            _ => return None,
        };

        let body = self.fn_bodies.get(fn_name)?.clone();

        if !is_pure_function(&body) {
            return None;
        }

        let param_names = self.fn_params.get(fn_name)?.clone();

        if param_names.len() != arguments.len() {
            return None;
        }

        // Evaluate all arguments first.
        let arg_vals: Vec<ConstValue> = arguments
            .iter()
            .map(|a| self.eval(a))
            .collect::<Option<_>>()?;

        // Build local scope with parameter bindings.
        let mut local_constants = self.constants.clone();
        for (name, val) in param_names.iter().zip(arg_vals) {
            local_constants.insert(name.clone(), val);
        }

        let mut inner = ConstEvaluator {
            constants: local_constants,
            fn_bodies: self.fn_bodies.clone(),
            fn_params: self.fn_params.clone(),
            evaluating: self.evaluating.clone(),
        };

        // Cycle detection.
        if inner.evaluating.contains_key(fn_name) {
            return None;
        }
        inner
            .evaluating
            .insert(fn_name.to_string(), ntsc_ast::span::Span::dummy());

        let result = inner.eval_body(&body);
        self.evaluating = inner.evaluating;
        result
    }

    fn eval_body(&mut self, body: &[BodyStmt]) -> Option<ConstValue> {
        for stmt in body {
            if let BodyStmt::Return {
                value: Some(ret_expr),
            } = stmt
            {
                return self.eval(ret_expr);
            }
        }
        None
    }
}

/// A function is pure for const-evaluation purposes when its body is a
/// single `return` statement (no side effects, no control flow).
fn is_pure_function(body: &[BodyStmt]) -> bool {
    if body.len() != 1 {
        return false;
    }
    matches!(&body[0], BodyStmt::Return { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntsc_ast::span::Span;
    use ntsc_ast::token::Token;

    fn lit_int(n: i64) -> Expr {
        Expr::Literal {
            value: LiteralValue::Number(n.to_string()),
            span: Span::dummy(),
        }
    }

    fn lit_float(f: f64) -> Expr {
        Expr::Literal {
            value: LiteralValue::Number(f.to_string()),
            span: Span::dummy(),
        }
    }

    fn lit_bool(b: bool) -> Expr {
        Expr::Literal {
            value: LiteralValue::Bool(b),
            span: Span::dummy(),
        }
    }

    fn var(name: &str) -> Expr {
        Expr::Variable {
            name: Token::new(TokenKind::Identifier(name.into()), Span::dummy()),
        }
    }

    fn tok(kind: TokenKind) -> Token {
        Token::new(kind, Span::dummy())
    }

    fn binop(left: Expr, op: TokenKind, right: Expr) -> Expr {
        Expr::Binary {
            left: Box::new(left),
            op: tok(op),
            right: Box::new(right),
        }
    }

    fn neg(e: Expr) -> Expr {
        Expr::Unary {
            op: tok(TokenKind::Minus),
            right: Box::new(e),
        }
    }

    fn grouping(e: Expr) -> Expr {
        Expr::Grouping {
            expression: Box::new(e),
            open_span: Span::dummy(),
            close_span: Span::dummy(),
        }
    }

    #[test]
    fn eval_int_literal() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(ev.eval(&lit_int(42)), Some(ConstValue::Int(42)));
    }

    #[test]
    fn eval_float_literal() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(ev.eval(&lit_float(2.71)), Some(ConstValue::Float(2.71)));
    }

    #[test]
    fn eval_bool_literal() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(ev.eval(&lit_bool(true)), Some(ConstValue::Bool(true)));
    }

    #[test]
    fn eval_negation() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(ev.eval(&neg(lit_int(5))), Some(ConstValue::Int(-5)));
    }

    #[test]
    fn eval_grouping() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(ev.eval(&grouping(lit_int(7))), Some(ConstValue::Int(7)));
    }

    #[test]
    fn eval_binary_add() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(
            ev.eval(&binop(lit_int(3), TokenKind::Plus, lit_int(4))),
            Some(ConstValue::Int(7))
        );
    }

    #[test]
    fn eval_binary_mul() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(
            ev.eval(&binop(lit_int(6), TokenKind::Star, lit_int(7))),
            Some(ConstValue::Int(42))
        );
    }

    #[test]
    fn eval_nested_binary() {
        let mut ev = ConstEvaluator::new();
        let expr = binop(
            grouping(binop(lit_int(2), TokenKind::Plus, lit_int(3))),
            TokenKind::Star,
            lit_int(4),
        );
        assert_eq!(ev.eval(&expr), Some(ConstValue::Int(20)));
    }

    #[test]
    fn eval_mixed_float_int() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(
            ev.eval(&binop(lit_float(2.5), TokenKind::Plus, lit_int(3))),
            Some(ConstValue::Float(5.5))
        );
    }

    #[test]
    fn eval_variable_reference() {
        let mut ev = ConstEvaluator::new();
        ev.constants.insert("X".into(), ConstValue::Int(10));
        assert_eq!(ev.eval(&var("X")), Some(ConstValue::Int(10)));
    }

    #[test]
    fn eval_comparison() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(
            ev.eval(&binop(lit_int(3), TokenKind::Less, lit_int(5))),
            Some(ConstValue::Bool(true))
        );
    }

    #[test]
    fn eval_division_by_zero() {
        let mut ev = ConstEvaluator::new();
        assert_eq!(
            ev.eval(&binop(lit_int(1), TokenKind::Slash, lit_int(0))),
            None
        );
    }

    #[test]
    fn is_const_expr_binary() {
        let mut ev = ConstEvaluator::new();
        ev.constants.insert("X".into(), ConstValue::Int(1));
        assert!(ev.is_constant_expr(&binop(var("X"), TokenKind::Plus, lit_int(2))));
    }

    #[test]
    fn is_const_expr_variable() {
        let mut ev = ConstEvaluator::new();
        ev.constants.insert("X".into(), ConstValue::Int(1));
        assert!(ev.is_constant_expr(&var("X")));
    }

    #[test]
    fn is_const_expr_undefined_variable() {
        let ev = ConstEvaluator::new();
        assert!(!ev.is_constant_expr(&var("X")));
    }
}
