use crate::expr::Expr;
use crate::expr::FunctionParam;
use crate::expr::ReturnTypeAnnotation;
use crate::span::Span;
use crate::token::Token;
use crate::types::{TypeAnnotation, ViewMutability};

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Expression {
        expression: Expr,
    },

    Say {
        expression: Expr,
        keyword_span: Span,
    },

    Var {
        name: Token,
        type_annotation: Option<TypeAnnotation>,
        initializer: Option<Expr>,
        is_static: bool,
        is_const: bool,

        /// `Some` for `view var` / `view mut var` borrow declarations.
        view: Option<ViewMutability>,
    },

    Block {
        statements: Vec<Stmt>,
        open_span: Span,
        close_span: Span,
    },

    If {
        condition: Expr,
        then_branch: Box<Stmt>,
        elif_branches: Vec<ElifBranch>,
        else_branch: Option<Box<Stmt>>,
    },

    While {
        condition: Expr,
        body: Box<Stmt>,
    },

    DoWhile {
        body: Box<Stmt>,
        condition: Expr,
    },

    For {
        init: Option<Box<Stmt>>,
        condition: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },

    ForIn {
        variable: Token,
        iterable: Expr,
        body: Box<Stmt>,
    },

    /// `for await x in producer { ... }` — consume an async stream.
    ForAwait {
        variable: Token,
        producer: Expr,
        body: Box<Stmt>,
    },

    /// `for v in chan { ... }` — receive from a channel until it is closed
    /// (each iteration binds the received element to `variable`).
    ChanRecvFor {
        variable: Token,
        channel: Expr,
        body: Box<Stmt>,
    },

    /// `go worker(args)` / `go { ... }` — spawn a virtual goroutine on the
    /// task scheduler, running the call (or the inline `block`) concurrently.
    Go {
        call: Expr,
        block: Option<Vec<Stmt>>,
        keyword_span: Span,
    },

    Function {
        name: Token,
        generic_params: Vec<GenericParam>,
        params: Vec<FunctionParam>,
        return_type: Option<ReturnTypeAnnotation>,
        body: Vec<Stmt>,
    },

    /// `extern "C" fun name(params) [-> Type]` or a C-style declaration
    /// inside `extern "C" { ... }`.
    ExternFunction {
        abi: Token,
        name: Token,
        params: Vec<FunctionParam>,
        return_type: Option<ReturnTypeAnnotation>,
        /// C-style declarations use ABI-width primitive aliases such as
        /// `c_int` and `c_char`; the Neutron spelling uses ordinary types.
        c_style: bool,
    },

    /// `extern "C" { ... }` containing C-style declarations.
    ExternBlock {
        abi: Token,
        functions: Vec<Stmt>,
    },

    /// `async fun name(params) [-> Type] { body }` — lowered to a poll-based
    /// state machine (see docs/async-rfc.md).
    AsyncFunction {
        name: Token,
        params: Vec<FunctionParam>,
        return_type: Option<ReturnTypeAnnotation>,
        body: Vec<Stmt>,
    },

    Return {
        value: Option<Expr>,
    },

    Class {
        name: Token,
        generic_params: Vec<GenericParam>,
        parent: Option<Token>,
        body: Vec<Stmt>,
    },

    Break {
        span: Span,
    },

    Continue {
        span: Span,
    },

    Match {
        expression: Expr,
        cases: Vec<MatchCase>,
        default_case: Option<Box<Stmt>>,
    },

    Try {
        try_block: Box<Stmt>,
        catch_var: Option<Token>,
        catch_block: Option<Box<Stmt>>,
        finally_block: Option<Box<Stmt>>,
    },

    Throw {
        value: Expr,
    },

    Retry {
        count: Expr,
        body: Box<Stmt>,
        catch_var: Option<Token>,
        catch_block: Option<Box<Stmt>>,
    },

    /// `unsafe { body }` — opts out of the language's default safety in `body`.
    Unsafe {
        body: Box<Stmt>,
    },

    /// `quiet [name, ...] body` — suppress lint warnings for `body`; an empty
    /// `suppressed` list silences every warning.
    Quiet {
        suppressed: Vec<String>,
        body: Box<Stmt>,
    },

    /// `var [a, b] = expr` / `var {x, y} = expr` / `var (a, b) = expr`.
    /// In object destructuring, `keys` hold the source keys and `names`
    /// the binding variables (aliases). `is_tuple` is set for positional
    /// tuple destructuring.
    Destructure {
        is_array: bool,
        is_tuple: bool,
        names: Vec<Token>,
        keys: Vec<String>,
        initializer: Expr,
    },

    Use {
        library: Token,
        is_file_path: bool,
        /// Whether `library` was prefixed with `@` (a Rust crate from `[rust-deps]`).
        is_rust: bool,
        imported_symbols: Vec<Token>,
        /// Parallel to `imported_symbols`: whether each imported name had `@`.
        imported_is_rust: Vec<bool>,
        alias: Option<Token>,
    },

    Enum {
        name: Token,
        generic_params: Vec<GenericParam>,
        members: Vec<EnumMember>,
    },

    TypeAlias {
        name: Token,
        generic_params: Vec<GenericParam>,
        target: TypeAnnotation,
    },

    /// `trait Printable { fun format() -> string }` — a statically checked
    /// method contract. Trait declarations do not generate runtime values.
    /// A method with a non-empty body is a default implementation: impls
    /// that omit the method inherit it. `parents` lists the supertraits
    /// declared after `:`; every implementor of this trait is also an
    /// implementor of each parent.
    Trait {
        name: Token,
        parents: Vec<Token>,
        associated_types: Vec<Token>,
        methods: Vec<Stmt>,
    },

    /// `impl Printable for User { fun format() -> string { ... } }` — a
    /// compile-time implementation used by generic trait bounds.
    Impl {
        trait_name: Token,
        type_name: Token,
        body: Vec<Stmt>,
    },

    /// `test name { body }` — compiled to `test_<name>` and discovered by the
    /// `ntsc test` runner.
    Test {
        name: Token,
        body: Vec<Stmt>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElifBranch {
    pub condition: Expr,
    pub body: Box<Stmt>,
    pub elif_span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchCase {
    pub value: Expr,
    /// Present when the arm head is a variant pattern like `Ok(v)` or
    /// `Err(e)`: the arm destructures an enum-like value and binds its
    /// payload to `binding` for the arm body. `value` still holds the raw
    /// parsed call form so generic AST consumers keep working, but checking
    /// and lowering branch on this field instead.
    pub pattern: Option<MatchPattern>,
    pub guard: Option<Expr>,
    pub body: Stmt,
    pub case_span: Span,
}

/// A destructuring match arm head: `VariantName` or `VariantName(binder)`.
/// A `_` binder matches the variant while ignoring its payload.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchPattern {
    pub variant: Token,
    pub binding: Option<Token>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumMember {
    pub name: Token,
    pub value: Option<Expr>,
    /// Associated data types for enum variants with data, e.g.
    /// `enum Shape { Circle(float), Rect(float, float) }`.
    pub data_types: Vec<crate::types::TypeAnnotation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: Token,
    pub bounds: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub statements: Vec<Stmt>,
}
