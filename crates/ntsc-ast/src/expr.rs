use crate::span::Span;
use crate::token::Token;
use crate::types::TypeAnnotation;

#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Nil,

    Bool(bool),

    /// Number literal kept as text so formatting round-trips losslessly.
    Number(String),

    /// String literal. Interpolation is split by the lexer and reassembled by
    /// the parser.
    String(String),
}

/// Parse integer literal text, honoring the `0x`, `0o`, and `0b` radix
/// prefixes the lexer accepts. `None` for floats and text outside any
/// accepted number form.
pub fn parse_int_literal(text: &str) -> Option<i64> {
    let (radix, digits) = if let Some(rest) = text.strip_prefix("0x") {
        (16, rest)
    } else if let Some(rest) = text.strip_prefix("0o") {
        (8, rest)
    } else if let Some(rest) = text.strip_prefix("0b") {
        (2, rest)
    } else {
        (10, text)
    };
    i64::from_str_radix(digits, radix).ok()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal {
        value: LiteralValue,
        span: Span,
    },

    Variable {
        name: Token,
    },

    Binary {
        left: Box<Expr>,
        op: Token,
        right: Box<Expr>,
    },

    Unary {
        op: Token,
        right: Box<Expr>,
    },

    PostfixUnary {
        op: Token,
        left: Box<Expr>,
    },

    Grouping {
        expression: Box<Expr>,
        open_span: Span,
        close_span: Span,
    },

    Member {
        object: Box<Expr>,
        property: Token,
    },

    OptionalMember {
        object: Box<Expr>,
        property: Token,
    },

    Call {
        callee: Box<Expr>,
        paren: Span,
        arguments: Vec<Expr>,
    },

    Assign {
        name: Token,
        value: Box<Expr>,
    },

    IndexGet {
        object: Box<Expr>,
        index: Box<Expr>,
    },

    IndexSet {
        object: Box<Expr>,
        index: Box<Expr>,
        value: Box<Expr>,
    },

    MemberSet {
        object: Box<Expr>,
        property: Token,
        value: Box<Expr>,
    },

    This {
        keyword: Token,
    },

    Lambda {
        params: Vec<FunctionParam>,
        return_type: Option<ReturnTypeAnnotation>,
        body: Vec<crate::stmt::Stmt>,
        span: Span,
    },

    Ternary {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },

    Spread {
        value: Box<Expr>,
        op_span: Span,
    },

    ObjectLiteral {
        properties: Vec<ObjectProperty>,
        span: Span,
    },

    ArrayLiteral {
        elements: Vec<Expr>,
        span: Span,
    },

    /// `await async_fn(args)` — suspends the async state machine until the
    /// callee's future completes, then yields the callee's return value.
    Await {
        callee: Box<Expr>,
        arguments: Vec<Expr>,
        span: Span,
    },

    /// `async { ... }` — inline async block that compiles to an anonymous
    /// future. Can be passed to `wait_any`/`wait_all` or awaited directly.
    AsyncBlock {
        body: Vec<crate::stmt::Stmt>,
        return_type: Option<ReturnTypeAnnotation>,
        span: Span,
    },

    /// `view target` / `view mut target` — block-scoped, non-owning view of a
    /// value; `mutable` marks the exclusive writable form.
    View {
        target: Box<Expr>,
        mutable: bool,
        keyword: Span,
    },

    /// `copy(expr)` — owned deep copy of a heap value; the source is untouched.
    Copy {
        expression: Box<Expr>,
        keyword: Span,
    },

    /// `&value` / `&mut value` — checked borrow creation.
    Borrow {
        target: Box<Expr>,
        mutable: bool,
        keyword: Span,
    },

    /// `*raw` — raw pointer dereference; requires an unsafe context.
    RawDeref {
        target: Box<Expr>,
        star: Span,
    },

    RawDerefSet {
        target: Box<Expr>,
        value: Box<Expr>,
        star: Span,
    },

    /// `ClassName { field: val, ... }` — typed struct literal that desugars to
    /// a constructor call followed by field assignments. `update` is the
    /// `..base` expression whose remaining fields fill in the unset ones.
    StructLiteral {
        class_name: Token,
        fields: Vec<ObjectProperty>,
        update: Option<Box<Expr>>,
        span: Span,
    },

    /// `expr?` — propagate an `Err` out of a result-returning function; on
    /// `Ok` yields the payload.
    Propagate {
        value: Box<Expr>,
        question_span: Span,
    },

    /// `(e1, e2, ...)` — a fixed-size, heterogeneous product type.
    TupleLiteral {
        elements: Vec<Expr>,
        span: Span,
    },

    /// `t.N` — numeric index into a tuple.
    TupleIndex {
        object: Box<Expr>,
        index: usize,
        dot_span: Span,
    },

    /// `chan << value` — send `value` into the channel (moves the value; the
    /// sender can no longer use it). Parks the goroutine when the channel is
    /// full.
    ChanSend {
        channel: Box<Expr>,
        value: Box<Expr>,
        op_span: Span,
    },

    /// `value >> chan` — receive the next element from the channel into
    /// `receiver` (owned by the receiver; frees once at scope exit). The
    /// expression's value is the received element.
    ChanRecv {
        receiver: Token,
        channel: Box<Expr>,
        op_span: Span,
    },

    /// `close(chan)` — close a channel for further sends; receivers drain
    /// remaining buffered elements then see the zero value.
    Close {
        channel: Box<Expr>,
        keyword: Span,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectProperty {
    pub key: String,
    pub value: Expr,
    pub key_span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionParam {
    pub name: Token,
    pub type_annotation: Option<TypeAnnotation>,
}

pub type ReturnTypeAnnotation = crate::types::ReturnType;

impl Expr {
    /// Best-effort span; synthetic for derived nodes (span of the operands).
    pub fn span(&self) -> Span {
        match self {
            Expr::Literal { span, .. } => *span,
            Expr::Variable { name } => name.span,
            Expr::Binary { left, right, .. } => left.span().to(right.span()),
            Expr::Unary { op, right } => op.span.to(right.span()),
            Expr::PostfixUnary { op, left } => left.span().to(op.span),
            Expr::Grouping {
                open_span,
                close_span,
                ..
            } => open_span.to(*close_span),
            Expr::Member { object, property } => object.span().to(property.span),
            Expr::OptionalMember { object, property } => object.span().to(property.span),
            Expr::Call {
                callee, arguments, ..
            } => {
                let end = arguments.last().map_or(callee.span(), |a| a.span());
                callee.span().to(end)
            }
            Expr::Assign { name, value } => name.span.to(value.span()),
            Expr::IndexGet { object, index } => object.span().to(index.span()),
            Expr::IndexSet { object, index, .. } => object.span().to(index.span()),
            Expr::MemberSet {
                object, property, ..
            } => object.span().to(property.span),
            Expr::This { keyword } => keyword.span,
            Expr::Lambda { span, .. } => *span,
            Expr::Ternary {
                condition,
                else_branch,
                ..
            } => condition.span().to(else_branch.span()),
            Expr::Spread { op_span, value } => op_span.to(value.span()),
            Expr::ObjectLiteral { span, .. } => *span,
            Expr::ArrayLiteral { span, .. } => *span,
            Expr::Await { span, .. } => *span,
            Expr::View {
                target, keyword, ..
            } => keyword.to(target.span()),
            Expr::Copy {
                keyword,
                expression,
            } => keyword.to(expression.span()),
            Expr::Borrow {
                target, keyword, ..
            } => keyword.to(target.span()),
            Expr::RawDeref { target, star } => star.to(target.span()),
            Expr::RawDerefSet {
                target,
                value,
                star,
            } => star.to(value.span()).to(target.span()),
            Expr::StructLiteral { span, .. } => *span,
            Expr::Propagate {
                value,
                question_span,
            } => value.span().to(*question_span),
            Expr::AsyncBlock { span, .. } => *span,
            Expr::TupleLiteral { span, .. } => *span,
            Expr::TupleIndex {
                object, dot_span, ..
            } => object.span().to(*dot_span),
            Expr::ChanSend {
                channel,
                value,
                op_span,
            } => channel.span().to(value.span()).to(*op_span),
            Expr::ChanRecv {
                receiver,
                channel,
                op_span,
            } => receiver.span.to(channel.span()).to(*op_span),
            Expr::Close { channel, keyword } => keyword.to(channel.span()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_int_literal;

    #[test]
    fn parses_radix_prefixes() {
        assert_eq!(parse_int_literal("0xff"), Some(255));
        assert_eq!(parse_int_literal("0o17"), Some(15));
        assert_eq!(parse_int_literal("0b1010"), Some(10));
        assert_eq!(parse_int_literal("42"), Some(42));
    }

    #[test]
    fn rejects_floats_and_malformed_text() {
        assert_eq!(parse_int_literal("3.14"), None);
        assert_eq!(parse_int_literal("0x"), None);
        assert_eq!(parse_int_literal(""), None);
    }
}
