use ntsc_ast::expr::{Expr, FunctionParam, LiteralValue, ObjectProperty, ReturnTypeAnnotation};
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{ElifBranch, EnumMember, GenericParam, MatchCase, Stmt};
use ntsc_ast::token::{Token, TokenKind};
use ntsc_ast::types::{ReturnType, TypeAnnotation, ViewMutability};

mod diag;

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error at {}: {}", self.span, self.message)
    }
}

impl std::error::Error for ParseError {}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Whether `kind` is an operator token that may be used as an overloaded
/// method name inside a class body (`fun +(view Vec other) -> Vec`).
fn is_operator_token(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::Bang
            | TokenKind::EqualEqual
            | TokenKind::BangEqual
            | TokenKind::Less
            | TokenKind::LessEqual
            | TokenKind::Greater
            | TokenKind::GreaterEqual
    )
}

/// Map an operator token kind to its source-level lexeme, used when
/// synthesising an `Identifier(lexeme)` token for an operator method name.
fn operator_token_lexeme(kind: &TokenKind) -> &'static str {
    match kind {
        TokenKind::Plus => "+",
        TokenKind::Minus => "-",
        TokenKind::Star => "*",
        TokenKind::Slash => "/",
        TokenKind::Percent => "%",
        TokenKind::Bang => "!",
        TokenKind::EqualEqual => "==",
        TokenKind::BangEqual => "!=",
        TokenKind::Less => "<",
        TokenKind::LessEqual => "<=",
        TokenKind::Greater => ">",
        TokenKind::GreaterEqual => ">=",
        _ => "",
    }
}

fn token_debug_name(kind: &TokenKind) -> &'static str {
    match kind {
        TokenKind::LeftParen => "(",
        TokenKind::RightParen => ")",
        TokenKind::LeftBrace => "{",
        TokenKind::RightBrace => "}",
        TokenKind::LeftBracket => "[",
        TokenKind::RightBracket => "]",
        TokenKind::Comma => ",",
        TokenKind::Dot => ".",
        TokenKind::Minus => "-",
        TokenKind::Plus => "+",
        TokenKind::Semicolon => ";",
        TokenKind::Slash => "/",
        TokenKind::Star => "*",
        TokenKind::Colon => ":",
        TokenKind::Percent => "%",
        TokenKind::Ampersand => "&",
        TokenKind::Pipe => "|",
        TokenKind::Caret => "^",
        TokenKind::Tilde => "~",
        TokenKind::Question => "?",
        TokenKind::Bang => "!",
        TokenKind::BangEqual => "!=",
        TokenKind::Equal => "=",
        TokenKind::EqualEqual => "==",
        TokenKind::Greater => ">",
        TokenKind::GreaterEqual => ">=",
        TokenKind::GreaterGreater => ">>",
        TokenKind::Less => "<",
        TokenKind::LessEqual => "<=",
        TokenKind::LessLess => "<<",
        TokenKind::LessPipe => "<|",
        TokenKind::PipeGreater => "|>",
        TokenKind::PlusPlus => "++",
        TokenKind::MinusMinus => "--",
        TokenKind::AndSym => "&&",
        TokenKind::OrSym => "||",
        TokenKind::Arrow => "=>",
        TokenKind::ReturnArrow => "->",
        TokenKind::DotDotDot => "...",
        TokenKind::DotDot => "..",
        TokenKind::QuestionDot => "?.",
        TokenKind::Identifier(_) => "identifier",
        TokenKind::StringLiteral(_) => "string literal",
        TokenKind::StringSegment(_) => "string segment",
        TokenKind::NumberLiteral(_) => "number literal",
        TokenKind::Newline => "newline",
        TokenKind::And => "and",
        TokenKind::Class => "class",
        TokenKind::Else => "else",
        TokenKind::Elif => "elif",
        TokenKind::False => "false",
        TokenKind::Fun => "fun",
        TokenKind::For => "for",
        TokenKind::If => "if",
        TokenKind::Nil => "nil",
        TokenKind::Or => "or",
        TokenKind::Say => "say",
        TokenKind::Return => "return",
        TokenKind::Static => "static",
        TokenKind::Super => "super",
        TokenKind::This => "this",
        TokenKind::True => "true",
        TokenKind::Var => "var",
        TokenKind::While => "while",
        TokenKind::Do => "do",
        TokenKind::Break => "break",
        TokenKind::Continue => "continue",
        TokenKind::Match => "match",
        TokenKind::Case => "case",
        TokenKind::Default => "default",
        TokenKind::Try => "try",
        TokenKind::Catch => "catch",
        TokenKind::Finally => "finally",
        TokenKind::Throw => "throw",
        TokenKind::Retry => "retry",
        TokenKind::Unsafe => "unsafe",
        TokenKind::Quiet => "quiet",
        TokenKind::Enum => "enum",
        TokenKind::Type => "type",
        TokenKind::Trait => "trait",
        TokenKind::Impl => "impl",
        TokenKind::In => "in",
        TokenKind::Use => "use",
        TokenKind::From => "from",
        TokenKind::As => "as",
        TokenKind::Test => "test",
        TokenKind::Async => "async",
        TokenKind::Await => "await",
        TokenKind::Extern => "extern",
        TokenKind::TypeInt => "int",
        TokenKind::TypeFloat => "float",
        TokenKind::TypeString => "string",
        TokenKind::TypeBool => "bool",
        TokenKind::TypeArray => "array",
        TokenKind::TypeObject => "object",
        TokenKind::TypeOption => "option",
        TokenKind::TypeResult => "result",
        TokenKind::TypeAny => "any",
        TokenKind::TypePointer => "pointer",
        TokenKind::TypeSlice => "slice",
        TokenKind::View => "view",
        TokenKind::Mut => "mut",
        TokenKind::Shared => "shared",
        TokenKind::Copy => "copy",
        TokenKind::Own => "own",
        TokenKind::Go => "go",
        TokenKind::Chan => "chan",
        TokenKind::Close => "close",
        TokenKind::At => "@",
        TokenKind::Eof => "end of file",
    }
}

/// Sentinel EOF token returned by `peek`/`peek_at` past the end of the token
/// stream; its span is a dummy 1:1 position.
const EOF_TOKEN: Token = Token {
    kind: TokenKind::Eof,
    span: Span {
        start: 0,
        end: 0,
        line: 1,
        column: 1,
    },
};

// ── Parser ──────────────────────────────────────────────────────────────

pub struct Parser<'src> {
    tokens: &'src [Token],
    position: usize,
    errors: Vec<ParseError>,
}

impl<'src> Parser<'src> {
    pub fn new(tokens: &'src [Token]) -> Self {
        Self {
            tokens,
            position: 0,
            errors: Vec::new(),
        }
    }

    pub fn parse(tokens: &'src [Token]) -> Result<ntsc_ast::stmt::Program, Vec<ParseError>> {
        let (program, errors) = Self::parse_recovering(tokens);
        if errors.is_empty() {
            Ok(program)
        } else {
            Err(errors)
        }
    }

    /// Parse `tokens`, returning the successfully parsed statements together
    /// with every error encountered. Error recovery skips to the next
    /// statement boundary, so the returned program is a usable prefix of the
    /// file. Used by the language server, which needs symbols and scopes
    /// even while the buffer contains errors.
    pub fn parse_recovering(tokens: &'src [Token]) -> (ntsc_ast::stmt::Program, Vec<ParseError>) {
        let mut parser = Self::new(tokens);
        let mut statements = Vec::new();

        while !parser.is_at_end() {
            if parser.check(&TokenKind::Newline) {
                parser.advance();
                continue;
            }
            match parser.parse_statement() {
                Ok(stmt) => statements.push(stmt),
                Err(err) => {
                    parser.errors.push(err);
                    parser.synchronize();
                }
            }
        }

        (ntsc_ast::stmt::Program { statements }, parser.errors)
    }

    // ── Token navigation ────────────────────────────────────────────

    fn peek(&self) -> &Token {
        self.tokens.get(self.position).unwrap_or(&EOF_TOKEN)
    }

    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    fn consume(&mut self, kind: &TokenKind) -> Option<Token> {
        if self.check(kind) {
            Some(self.advance())
        } else {
            None
        }
    }

    /// Consume the next token if it is the given keyword as an identifier
    /// (e.g. `const` which is not a reserved keyword token).
    fn consume_keyword(&mut self, text: &str) -> bool {
        if self.check_ident(text) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_ident(&mut self, message: &str) -> Result<Token, ParseError> {
        if matches!(&self.peek().kind, TokenKind::Identifier(_)) {
            Ok(self.advance())
        } else {
            Err(ParseError {
                message: message.to_string(),
                span: self.peek().span,
            })
        }
    }

    /// Parse a member/property name after `.` or `?.`. Keywords are accepted as
    /// property names (e.g. `random.int`) and normalized to identifier tokens
    /// so the rest of the pipeline sees the source text via `Token::lexeme()`.
    fn parse_member_name(&mut self) -> Result<Token, ParseError> {
        let property = self.peek().clone();
        if let TokenKind::Identifier(_) = property.kind {
            self.advance();
            return Ok(property);
        }
        if let Some(lexeme) = property.kind.keyword_lexeme() {
            self.advance();
            return Ok(Token::new(
                TokenKind::Identifier(lexeme.to_string()),
                property.span,
            ));
        }
        Err(ParseError {
            message: "expected property name after '.'".to_string(),
            span: property.span,
        })
    }

    fn is_at_end(&self) -> bool {
        self.peek().kind == TokenKind::Eof
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.position].clone();
        self.position += 1;
        tok
    }

    fn expect(&mut self, kind: TokenKind, message: &str) -> Result<Token, ParseError> {
        if self.check(&kind) {
            Ok(self.advance())
        } else {
            Err(ParseError {
                message: message.to_string(),
                span: self.peek().span,
            })
        }
    }

    // ── ASI ─────────────────────────────────────────────────────────

    fn consume_terminator(&mut self) -> Option<Span> {
        if let Some(tok) = self.consume(&TokenKind::Semicolon) {
            return Some(tok.span);
        }
        if self.check(&TokenKind::Newline) && !self.next_is_continuation() {
            let tok = self.advance();
            return Some(tok.span);
        }
        if self.is_at_end() {
            return Some(self.peek().span);
        }
        None
    }

    fn next_is_continuation(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Dot
                | TokenKind::QuestionDot
                | TokenKind::LeftParen
                | TokenKind::LeftBracket
                | TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::Equal
                | TokenKind::EqualEqual
                | TokenKind::BangEqual
                | TokenKind::Greater
                | TokenKind::GreaterEqual
                | TokenKind::Less
                | TokenKind::LessEqual
                | TokenKind::AndSym
                | TokenKind::OrSym
                | TokenKind::Ampersand
                | TokenKind::Pipe
                | TokenKind::Caret
                | TokenKind::Tilde
                | TokenKind::Bang
                | TokenKind::PlusPlus
                | TokenKind::MinusMinus
                | TokenKind::DotDotDot
                | TokenKind::Semicolon
        )
    }

    fn skip_newlines(&mut self) {
        while self.check(&TokenKind::Newline) {
            self.advance();
        }
    }

    fn synchronize(&mut self) {
        while !self.is_at_end() {
            if self.check(&TokenKind::Newline) || self.check(&TokenKind::Semicolon) {
                self.advance();
                return;
            }
            if matches!(
                self.peek().kind,
                TokenKind::Class
                    | TokenKind::Fun
                    | TokenKind::Async
                    | TokenKind::Var
                    | TokenKind::Static
                    | TokenKind::For
                    | TokenKind::If
                    | TokenKind::While
                    | TokenKind::Return
                    | TokenKind::Match
                    | TokenKind::Try
                    | TokenKind::Throw
                    | TokenKind::Unsafe
                    | TokenKind::Quiet
                    | TokenKind::Enum
                    | TokenKind::Type
                    | TokenKind::Use
                    | TokenKind::Extern
                    | TokenKind::Test
            ) {
                return;
            }
            self.advance();
        }
    }

    // ── Statements ──────────────────────────────────────────────────

    fn parse_statement(&mut self) -> Result<Stmt, ParseError> {
        while self.check(&TokenKind::Newline) {
            self.advance();
        }

        match &self.peek().kind {
            TokenKind::Var => self.parse_var_declaration(false),
            TokenKind::Static => self.parse_static_var(),
            TokenKind::Shared => self.parse_shared_var_declaration(),
            TokenKind::View => {
                // `view var` / `view mut var` declare a borrow variable;
                // otherwise `view` is a unary prefix expression.
                if self.peek_at(1) == &TokenKind::Var
                    || (self.peek_at(1) == &TokenKind::Mut && self.peek_at(2) == &TokenKind::Var)
                {
                    self.parse_view_var_declaration()
                } else {
                    self.parse_expression_statement()
                }
            }
            TokenKind::Fun => {
                // `fun name(...)` is a declaration; `fun(...)` is a lambda expression.
                // Operator tokens (+, -, *, /, %, !) are accepted as method
                // names inside class bodies for operator overloading.
                if (self.peek_at_ident(1) && !self.peek_at_type_keyword(1))
                    || self.peek_at_operator(1)
                {
                    self.parse_function_declaration()
                } else {
                    self.parse_expression_statement()
                }
            }
            TokenKind::Async => {
                self.advance();
                if self.check(&TokenKind::Fun) {
                    self.parse_async_function_declaration()
                } else if self.check(&TokenKind::LeftBrace) || self.check(&TokenKind::ReturnArrow) {
                    let block = self.parse_async_block_body(self.peek().span)?;
                    Ok(Stmt::Expression { expression: block })
                } else {
                    Err(ParseError {
                        message: "expected 'fun', '{', or '->' after 'async'".into(),
                        span: self.peek().span,
                    })
                }
            }
            TokenKind::Go => self.parse_go_statement(),
            TokenKind::Class => self.parse_class_declaration(),
            TokenKind::Enum => self.parse_enum_declaration(),
            TokenKind::Type => self.parse_type_alias_declaration(),
            TokenKind::Trait => self.parse_trait_declaration(),
            TokenKind::Impl => self.parse_impl_declaration(),
            TokenKind::If => self.parse_if_statement(),
            TokenKind::While => self.parse_while_statement(),
            TokenKind::Do => self.parse_do_while_statement(),
            TokenKind::For => self.parse_for_statement(),
            TokenKind::Match => self.parse_match_statement(),
            TokenKind::Try => self.parse_try_statement(),
            TokenKind::Throw => self.parse_throw_statement(),
            TokenKind::Retry => self.parse_retry_statement(),
            TokenKind::Unsafe => self.parse_unsafe_statement(),
            TokenKind::Quiet => self.parse_quiet_statement(),
            TokenKind::Return => self.parse_return_statement(),
            TokenKind::Break => self.parse_break_statement(),
            TokenKind::Continue => self.parse_continue_statement(),
            TokenKind::Use => self.parse_use_statement(),
            TokenKind::Extern => self.parse_extern_declaration(),
            TokenKind::Test => self.parse_test_block(),
            TokenKind::LeftBrace => self.parse_block(),
            TokenKind::Say => self.parse_say_statement(),
            _ => self.parse_expression_statement(),
        }
    }

    fn parse_var_declaration(&mut self, is_static: bool) -> Result<Stmt, ParseError> {
        if is_static {
            self.advance();
            self.expect(TokenKind::Var, "expected 'var' after 'static'")?;
        } else {
            self.advance();
        }
        self.parse_var_declaration_tail(None, is_static, false)
    }

    fn parse_view_var_declaration(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let mutable = self.consume(&TokenKind::Mut).is_some();
        self.expect(TokenKind::Var, "expected 'var' after 'view'")?;
        let view = Some(if mutable {
            ViewMutability::Mutable
        } else {
            ViewMutability::ReadOnly
        });
        self.parse_var_declaration_tail(view, false, false)
    }

    /// Parse `shared T name = expr`. `shared` is only ever a type prefix, so at
    /// statement start it unambiguously begins a variable declaration.
    fn parse_shared_var_declaration(&mut self) -> Result<Stmt, ParseError> {
        let type_annotation = self.parse_type_annotation(true).ok_or_else(|| ParseError {
            message: "expected a heap type after 'shared'".into(),
            span: self.peek().span,
        })?;
        let name = self.expect_ident("expected variable name")?;
        let initializer = if self.consume(&TokenKind::Equal).is_some() {
            Some(self.parse_expression()?)
        } else {
            None
        };
        self.consume_terminator();
        Ok(Stmt::Var {
            name,
            type_annotation: Some(type_annotation),
            initializer,
            is_static: false,
            is_const: false,
            view: None,
        })
    }

    fn parse_var_declaration_tail(
        &mut self,
        view: Option<ViewMutability>,
        is_static: bool,
        is_const: bool,
    ) -> Result<Stmt, ParseError> {
        // Peek ahead: if the current token is `(` and the pattern looks like
        // tuple destructuring (`(ident, ident)` or `(ident)` followed by `=`),
        // handle it before the type annotation parser consumes the `(`.
        if self.peek().kind == TokenKind::LeftParen && !is_static {
            let saved = self.position;
            self.advance(); // `(`
            let mut is_destructure = false;
            if self.peek().kind != TokenKind::RightParen
                && matches!(self.peek().kind, TokenKind::Identifier(_))
            {
                let after_name = self.peek_at(1);
                if matches!(after_name, TokenKind::Comma | TokenKind::RightParen) {
                    // Looks like `(name, ...)` — check if `=` follows after `)`.
                    let saved2 = self.position;
                    // Skip past the name(s) and commas.
                    while self.peek().kind != TokenKind::RightParen
                        && self.peek().kind != TokenKind::Eof
                    {
                        self.advance();
                    }
                    if self.peek().kind == TokenKind::RightParen {
                        self.advance(); // `)`
                        if self.peek().kind == TokenKind::Equal {
                            is_destructure = true;
                        }
                    }
                    self.position = saved2;
                }
            }
            self.position = saved;

            if is_destructure {
                if view.is_some() {
                    return Err(ParseError {
                        message: "cannot destructure a view declaration".into(),
                        span: self.peek().span,
                    });
                }
                return self.parse_tuple_destructure(None);
            }
        }

        let type_annotation = self.parse_declaration_type_annotation();

        if self.check(&TokenKind::LeftBracket) || self.check(&TokenKind::LeftBrace) {
            if view.is_some() {
                return Err(ParseError {
                    message: "cannot destructure a view declaration".into(),
                    span: self.peek().span,
                });
            }
            return self.parse_destructure(type_annotation, is_static);
        }

        let name = self.expect_ident("expected variable name")?;
        let initializer = if self.consume(&TokenKind::Equal).is_some() {
            Some(self.parse_expression()?)
        } else {
            None
        };
        self.consume_terminator();

        Ok(Stmt::Var {
            name,
            type_annotation,
            initializer,
            is_static,
            is_const,
            view,
        })
    }

    fn parse_static_var(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let is_const = self.consume_keyword("const");
        self.expect(TokenKind::Var, "expected 'var' after 'static'")?;
        self.parse_var_declaration_tail(None, true, is_const)
    }

    fn parse_destructure(
        &mut self,
        type_annotation: Option<TypeAnnotation>,
        is_static: bool,
    ) -> Result<Stmt, ParseError> {
        let is_array = self.check(&TokenKind::LeftBracket);
        self.advance();
        let close_kind = if is_array {
            TokenKind::RightBracket
        } else {
            TokenKind::RightBrace
        };

        let mut names = Vec::new();
        let mut keys = Vec::new();

        loop {
            if self.check(&close_kind) {
                break;
            }
            let name = self.expect_ident("expected variable name in destructuring")?;

            if !is_array && self.consume(&TokenKind::Colon).is_some() {
                let alias = self.expect_ident("expected alias name")?;
                keys.push(name.lexeme().to_string());
                names.push(alias);
            } else {
                keys.push(name.lexeme().to_string());
                names.push(name);
            }

            if self.consume(&TokenKind::Comma).is_none() {
                break;
            }
        }

        self.expect(close_kind, "expected closing bracket/brace")?;
        if type_annotation.is_some() || is_static {
            self.consume_terminator();
        } else {
            self.expect(TokenKind::Equal, "expected '=' in destructuring")?;
            let initializer = self.parse_expression()?;
            self.consume_terminator();
            return Ok(Stmt::Destructure {
                is_array,
                is_tuple: false,
                names,
                keys,
                initializer,
            });
        }

        self.expect(TokenKind::Equal, "expected '=' in destructuring")?;
        let initializer = self.parse_expression()?;
        self.consume_terminator();

        Ok(Stmt::Destructure {
            is_array,
            is_tuple: false,
            names,
            keys,
            initializer,
        })
    }

    fn parse_tuple_destructure(
        &mut self,
        type_annotation: Option<TypeAnnotation>,
    ) -> Result<Stmt, ParseError> {
        self.advance(); // consume `(`
        let mut names = Vec::new();
        loop {
            if self.check(&TokenKind::RightParen) {
                break;
            }
            let name = self.expect_ident("expected variable name in tuple destructuring")?;
            names.push(name);
            if self.consume(&TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(
            TokenKind::RightParen,
            "expected ')' after tuple destructuring",
        )?;
        if type_annotation.is_some() {
            self.consume_terminator();
        } else {
            self.expect(TokenKind::Equal, "expected '=' in tuple destructuring")?;
            let initializer = self.parse_expression()?;
            self.consume_terminator();
            return Ok(Stmt::Destructure {
                is_array: false,
                is_tuple: true,
                names,
                keys: vec![],
                initializer,
            });
        }

        self.expect(TokenKind::Equal, "expected '=' in tuple destructuring")?;
        let initializer = self.parse_expression()?;
        self.consume_terminator();

        Ok(Stmt::Destructure {
            is_array: false,
            is_tuple: true,
            names,
            keys: vec![],
            initializer,
        })
    }

    fn parse_say_statement(&mut self) -> Result<Stmt, ParseError> {
        let keyword = self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'say'")?;
        let expression = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')'")?;
        self.consume_terminator();
        Ok(Stmt::Say {
            expression,
            keyword_span: keyword.span,
        })
    }

    fn parse_expression_statement(&mut self) -> Result<Stmt, ParseError> {
        let expression = self.parse_expression()?;
        self.consume_terminator();
        Ok(Stmt::Expression { expression })
    }

    fn parse_block(&mut self) -> Result<Stmt, ParseError> {
        let open = self.advance();
        let mut statements = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            statements.push(self.parse_statement()?);
            self.skip_newlines();
        }
        let close = self.expect(TokenKind::RightBrace, "expected '}'")?;
        Ok(Stmt::Block {
            statements,
            open_span: open.span,
            close_span: close.span,
        })
    }

    fn parse_function_declaration(&mut self) -> Result<Stmt, ParseError> {
        let _fun_token = self.advance();
        let name = self.parse_method_name()?;
        let mut generic_params = self.parse_generic_params()?;
        let (params, return_type) = self.parse_function_signature()?;
        self.parse_where_bounds(&mut generic_params)?;
        self.skip_newlines();
        let body = self.parse_function_body()?;
        Ok(Stmt::Function {
            name,
            generic_params,
            params,
            return_type,
            body,
        })
    }

    /// Parse either the Neutron spelling
    /// `extern "C" fun strlen(*const byte) -> int` or a C-style block such as
    /// `extern "C" { size_t strlen(const char *); }`.
    fn parse_extern_declaration(&mut self) -> Result<Stmt, ParseError> {
        let _extern_token = self.advance();
        let abi = self.expect(
            TokenKind::StringLiteral(String::new()),
            "expected ABI string after `extern`",
        )?;
        if abi.lexeme() != "C" {
            return Err(ParseError {
                message: format!(
                    "unsupported extern ABI `{}`; only `C` is supported",
                    abi.lexeme()
                ),
                span: abi.span,
            });
        }

        if self.consume(&TokenKind::Fun).is_some() {
            return self.parse_extern_function(abi, false);
        }

        self.skip_newlines();
        self.expect(
            TokenKind::LeftBrace,
            "expected `fun` or `{` after extern ABI",
        )?;
        let mut declarations = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            let return_type = self.parse_c_type_annotation()?;
            let name = self.expect_ident("expected C function name")?;
            self.expect(TokenKind::LeftParen, "expected `(` after C function name")?;
            let params = self.parse_c_parameters()?;
            self.expect(TokenKind::RightParen, "expected `)` after C parameters")?;
            self.consume_terminator();
            declarations.push(Stmt::ExternFunction {
                abi: abi.clone(),
                name,
                params,
                return_type: Some(ReturnType {
                    ty: return_type,
                    arrow_span: abi.span,
                }),
                c_style: true,
            });
            self.skip_newlines();
        }
        self.expect(
            TokenKind::RightBrace,
            "expected `}` after extern declarations",
        )?;
        Ok(Stmt::ExternBlock {
            abi,
            functions: declarations,
        })
    }

    fn parse_extern_function(&mut self, abi: Token, c_style: bool) -> Result<Stmt, ParseError> {
        let name = self.expect_ident("expected extern function name")?;
        self.expect(
            TokenKind::LeftParen,
            "expected `(` after extern function name",
        )?;
        let mut params = Vec::new();
        if !self.check(&TokenKind::RightParen) {
            loop {
                let type_annotation =
                    self.parse_declaration_type_annotation()
                        .ok_or_else(|| ParseError {
                            message: "expected extern parameter type".into(),
                            span: self.peek().span,
                        })?;
                let param_name = if self.peek_at_ident(0)
                    && matches!(self.peek_at(1), TokenKind::Comma | TokenKind::RightParen)
                {
                    self.advance()
                } else {
                    Token::new(
                        TokenKind::Identifier(format!("__ffi_arg{}", params.len())),
                        self.peek().span,
                    )
                };
                params.push(FunctionParam {
                    name: param_name,
                    type_annotation: Some(type_annotation),
                });
                if self.consume(&TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.expect(
            TokenKind::RightParen,
            "expected `)` after extern parameters",
        )?;
        let return_type = if self.consume(&TokenKind::ReturnArrow).is_some() {
            let arrow_span = self.peek().span;
            let ty = self.parse_type_annotation(true).ok_or_else(|| ParseError {
                message: "expected extern return type".into(),
                span: self.peek().span,
            })?;
            Some(ReturnType { ty, arrow_span })
        } else {
            None
        };
        self.consume_terminator();
        Ok(Stmt::ExternFunction {
            abi,
            name,
            params,
            return_type,
            c_style,
        })
    }

    fn parse_c_parameters(&mut self) -> Result<Vec<FunctionParam>, ParseError> {
        if self.check(&TokenKind::RightParen) {
            return Ok(Vec::new());
        }
        if self.check_ident("void") && *self.peek_at(1) == TokenKind::RightParen {
            self.advance();
            return Ok(Vec::new());
        }

        let mut params = Vec::new();
        loop {
            let type_annotation = self.parse_c_type_annotation()?;
            let name = if self.peek_at_ident(0)
                && matches!(self.peek_at(1), TokenKind::Comma | TokenKind::RightParen)
            {
                self.advance()
            } else {
                Token::new(
                    TokenKind::Identifier(format!("__ffi_arg{}", params.len())),
                    self.peek().span,
                )
            };
            params.push(FunctionParam {
                name,
                type_annotation: Some(type_annotation),
            });
            if self.consume(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Ok(params)
    }

    fn parse_c_type_annotation(&mut self) -> Result<TypeAnnotation, ParseError> {
        let _const_pointee = self.consume_keyword("const");
        let base = self.advance();
        let base_name = match &base.kind {
            TokenKind::Identifier(name) => name.clone(),
            TokenKind::TypeInt => "int".into(),
            TokenKind::TypeFloat => "float".into(),
            TokenKind::TypeBool => "bool".into(),
            _ => String::new(),
        };
        let annotation_name = match base_name.as_str() {
            "void" => "void".to_string(),
            "char" => "c_char".to_string(),
            "unsigned" => {
                let next = self.expect_c_word("expected C type after `unsigned`")?;
                let next_name = match &next.kind {
                    TokenKind::Identifier(name) => name.as_str(),
                    TokenKind::TypeInt => "int",
                    _ => "",
                };
                match next_name {
                    "char" => "c_uchar",
                    "short" => "c_ushort",
                    "int" => "c_uint",
                    "long" => "c_ulong",
                    other => {
                        return Err(ParseError {
                            message: format!("unsupported C type `unsigned {other}`"),
                            span: next.span,
                        });
                    }
                }
                .to_string()
            }
            "short" => "c_short".to_string(),
            "int" => "c_int".to_string(),
            "long" => {
                if self.check_ident("long") {
                    self.advance();
                    "c_longlong".to_string()
                } else {
                    "c_long".to_string()
                }
            }
            "size_t" => "c_size_t".to_string(),
            "float" => "c_float".to_string(),
            "double" => "c_double".to_string(),
            other => {
                return Err(ParseError {
                    message: format!("unsupported C type `{other}`"),
                    span: base.span,
                });
            }
        };
        let base_annotation = TypeAnnotation::Named(Token::new(
            TokenKind::Identifier(annotation_name.clone()),
            base.span,
        ));
        if self.consume(&TokenKind::Star).is_some() {
            let mutable = !self.consume_keyword("const");
            let pointee = if annotation_name == "c_char" || annotation_name == "void" {
                TypeAnnotation::Named(Token::new(TokenKind::Identifier("byte".into()), base.span))
            } else {
                base_annotation
            };
            Ok(TypeAnnotation::RawPointer(Box::new(pointee), mutable))
        } else {
            Ok(base_annotation)
        }
    }

    fn expect_c_word(&mut self, message: &str) -> Result<Token, ParseError> {
        if self.peek_at_ident(0) || self.peek().kind.is_type_keyword() {
            Ok(self.advance())
        } else {
            Err(ParseError {
                message: message.into(),
                span: self.peek().span,
            })
        }
    }

    /// Parse a method name: either a regular identifier or an operator
    /// token (+, -, *, /, %, !) used for operator overloading. Operator
    /// tokens are converted to `Identifier(lexeme)` so the rest of the
    /// pipeline sees the source text uniformly.
    fn parse_method_name(&mut self) -> Result<Token, ParseError> {
        if let TokenKind::Identifier(_) = self.peek().kind {
            Ok(self.advance())
        } else if is_operator_token(&self.peek().kind) {
            let tok = self.peek().clone();
            let lexeme = operator_token_lexeme(&tok.kind).to_string();
            self.advance();
            Ok(Token::new(TokenKind::Identifier(lexeme), tok.span))
        } else {
            Err(ParseError {
                message: "expected method name".to_string(),
                span: self.peek().span,
            })
        }
    }

    fn parse_generic_params(&mut self) -> Result<Vec<GenericParam>, ParseError> {
        if self.consume(&TokenKind::Less).is_none() {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        loop {
            let name = self.expect_ident("expected generic parameter name")?;
            let mut bounds = Vec::new();
            if self.consume(&TokenKind::Colon).is_some() {
                loop {
                    bounds.push(self.expect_ident("expected trait name after `:`")?);
                    if self.consume(&TokenKind::Plus).is_none() {
                        break;
                    }
                }
            }
            params.push(GenericParam { name, bounds });
            if self.consume(&TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(TokenKind::Greater, "expected `>` after generic parameters")?;
        Ok(params)
    }

    fn parse_where_bounds(
        &mut self,
        generic_params: &mut [GenericParam],
    ) -> Result<(), ParseError> {
        if !self.check_ident("where") {
            return Ok(());
        }
        self.advance();
        loop {
            let parameter = self.expect_ident("expected generic parameter after `where`")?;
            self.expect(TokenKind::Colon, "expected `:` after generic parameter")?;
            let Some(generic) = generic_params
                .iter_mut()
                .find(|generic| generic.name.lexeme() == parameter.lexeme())
            else {
                return Err(ParseError {
                    message: format!(
                        "`{}` is not a generic parameter of this declaration",
                        parameter.lexeme()
                    ),
                    span: parameter.span,
                });
            };
            loop {
                generic
                    .bounds
                    .push(self.expect_ident("expected trait name in `where` clause")?);
                if self.consume(&TokenKind::Plus).is_none() {
                    break;
                }
            }
            if self.consume(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Ok(())
    }

    fn parse_trait_declaration(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let name = self.expect_ident("expected trait name")?;
        let mut parents = Vec::new();
        if self.consume(&TokenKind::Colon).is_some() {
            loop {
                parents.push(self.expect_ident("expected trait name after `:`")?);
                if self.consume(&TokenKind::Plus).is_none() {
                    break;
                }
            }
        }
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "expected `{` after trait name")?;
        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            if self.check(&TokenKind::Type) {
                self.advance();
                associated_types.push(self.expect_ident("expected associated type name")?);
                self.consume_terminator();
                self.skip_newlines();
                continue;
            }
            self.expect(TokenKind::Fun, "expected `fun` in trait")?;
            let method_name = self.expect_ident("expected trait method name")?;
            let (params, return_type) = self.parse_function_signature()?;
            // A `{` after the signature introduces a default implementation;
            // otherwise the method is abstract and only its signature ends
            // with a terminator.
            let body = if self.check(&TokenKind::LeftBrace) {
                self.parse_function_body()?
            } else {
                self.consume_terminator();
                Vec::new()
            };
            methods.push(Stmt::Function {
                name: method_name,
                generic_params: Vec::new(),
                params,
                return_type,
                body,
            });
            self.skip_newlines();
        }
        self.expect(TokenKind::RightBrace, "expected `}` after trait")?;
        Ok(Stmt::Trait {
            name,
            parents,
            associated_types,
            methods,
        })
    }

    fn parse_impl_declaration(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let trait_name = self.expect_ident("expected trait name after `impl`")?;
        self.expect(TokenKind::For, "expected `for` in trait implementation")?;
        let type_name = self.expect_ident("expected type name after `for`")?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "expected `{` after impl target")?;
        let mut body = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            body.push(self.parse_statement()?);
            self.skip_newlines();
        }
        self.expect(TokenKind::RightBrace, "expected `}` after impl")?;
        Ok(Stmt::Impl {
            trait_name,
            type_name,
            body,
        })
    }

    /// `async fun name(params) [-> Type] { body }` — the `async` keyword has
    /// already been consumed by the caller.
    fn parse_async_function_declaration(&mut self) -> Result<Stmt, ParseError> {
        let _fun_token = self.advance();
        let name = self.expect_ident("expected function name")?;
        let (params, return_type) = self.parse_function_signature()?;
        self.skip_newlines();
        let body = self.parse_function_body()?;
        Ok(Stmt::AsyncFunction {
            name,
            params,
            return_type,
            body,
        })
    }

    /// `async { body }` — inline async block that compiles to an anonymous
    /// future.
    fn parse_async_block(&mut self) -> Result<Expr, ParseError> {
        let async_tok = self.advance();
        self.parse_async_block_body(async_tok.span)
    }

    /// Parse the `{ ... }` body of an async block. `async_tok_span` is the
    /// span of the `async` keyword that was already consumed.
    fn parse_async_block_body(&mut self, async_tok_span: Span) -> Result<Expr, ParseError> {
        let return_type = if self.consume(&TokenKind::ReturnArrow).is_some() {
            let arrow_span = self.peek().span;
            let ty = self.parse_type_annotation(true).ok_or_else(|| ParseError {
                message: "expected return type".into(),
                span: self.peek().span,
            })?;
            Some(ReturnType { ty, arrow_span })
        } else {
            None
        };
        self.expect(TokenKind::LeftBrace, "expected '{' after 'async'")?;
        let mut body = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            body.push(self.parse_statement()?);
            self.skip_newlines();
        }
        let close = self.expect(TokenKind::RightBrace, "expected '}'")?;
        let span = async_tok_span.to(close.span);
        Ok(Expr::AsyncBlock {
            body,
            return_type,
            span,
        })
    }

    /// `test name { body }` — consumed at any statement position, but only
    /// top-level test blocks are compiled into the test harness.
    fn parse_test_block(&mut self) -> Result<Stmt, ParseError> {
        let _test_token = self.advance();
        let name = self.expect_ident("expected test name")?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "expected '{' after test name")?;
        let mut body = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            body.push(self.parse_statement()?);
            self.skip_newlines();
        }
        self.expect(TokenKind::RightBrace, "expected '}'")?;
        Ok(Stmt::Test { name, body })
    }

    fn parse_function_signature(
        &mut self,
    ) -> Result<(Vec<FunctionParam>, Option<ReturnTypeAnnotation>), ParseError> {
        self.expect(TokenKind::LeftParen, "expected '(' after function name")?;
        let mut params = Vec::new();

        if !self.check(&TokenKind::RightParen) {
            loop {
                let type_annotation = self.parse_declaration_type_annotation();

                let param_name = self.expect_ident("expected parameter name")?;

                params.push(FunctionParam {
                    name: param_name,
                    type_annotation,
                });
                if self.consume(&TokenKind::Comma).is_none() {
                    break;
                }
            }
        }

        self.expect(TokenKind::RightParen, "expected ')'")?;

        let return_type = if self.consume(&TokenKind::ReturnArrow).is_some() {
            let arrow_span = self.peek().span;
            // `impl Trait` is only meaningful as a return type, so it is
            // recognized here rather than inside `parse_type_annotation`.
            let ty = if self.check(&TokenKind::Impl) {
                self.advance();
                TypeAnnotation::ImplTrait(self.expect_ident("expected trait name after `impl`")?)
            } else {
                self.parse_type_annotation(true).ok_or_else(|| ParseError {
                    message: "expected return type".into(),
                    span: self.peek().span,
                })?
            };
            Some(ReturnType { ty, arrow_span })
        } else {
            None
        };

        Ok((params, return_type))
    }

    /// Neutron writes annotations before names (`var int count`), so a named
    /// type counts as an annotation only when another identifier follows it;
    /// this keeps an untyped declaration like `var count = 1` unambiguous.
    fn parse_declaration_type_annotation(&mut self) -> Option<TypeAnnotation> {
        let named_type = self.peek_at_ident(0)
            && (self.peek_at_ident(1)
                || self.peek_at(1) == &TokenKind::Less
                || (self.peek_at(1) == &TokenKind::Colon
                    && self.peek_at(2) == &TokenKind::Colon
                    && self.peek_at_ident(3)
                    && self.peek_at_ident(4)));
        self.parse_type_annotation(named_type)
    }

    fn parse_type_annotation(&mut self, allow_named: bool) -> Option<TypeAnnotation> {
        if self.peek().kind == TokenKind::Own {
            self.advance();
            let inner = self.parse_type_annotation(true)?;
            return Some(TypeAnnotation::Own(Box::new(inner)));
        }
        if self.peek().kind == TokenKind::Ampersand {
            self.advance();
            let mutable = self.consume(&TokenKind::Mut).is_some();
            let inner = self.parse_type_annotation(true)?;
            return Some(TypeAnnotation::Ref(Box::new(inner), mutable));
        }
        if self.peek().kind == TokenKind::Star {
            self.advance();
            let mutable = if self.consume(&TokenKind::Mut).is_some() {
                true
            } else {
                self.expect_ident("expected `const` or `mut` after `*`")
                    .ok()
                    .filter(|token| token.lexeme() == "const")?;
                false
            };
            let inner = self.parse_type_annotation(true)?;
            return Some(TypeAnnotation::RawPointer(Box::new(inner), mutable));
        }
        if self.peek().kind == TokenKind::View {
            let _view_token = self.advance();
            let mutable = self.consume(&TokenKind::Mut).is_some();
            let inner = self.parse_type_annotation(true)?;
            return Some(TypeAnnotation::View(Box::new(inner), mutable));
        }
        if self.peek().kind == TokenKind::Shared {
            let _shared_token = self.advance();
            let inner = self.parse_type_annotation(true)?;
            return Some(TypeAnnotation::Shared(Box::new(inner)));
        }
        // Tuple type: `(T1, T2, ...)` — detect by `(` followed by a type
        // then `,`. A single `(T)` is just grouping (handled elsewhere).
        if self.peek().kind == TokenKind::LeftParen && *self.peek_at(1) != TokenKind::RightParen {
            let saved = self.position;
            self.advance(); // consume `(`
            let first = self.parse_type_annotation(true);
            match first {
                Some(first_type) if self.check(&TokenKind::Comma) => {
                    let mut types = vec![first_type];
                    while self.consume(&TokenKind::Comma).is_some() {
                        if self.check(&TokenKind::RightParen) {
                            break;
                        }
                        if let Some(ty) = self.parse_type_annotation(true) {
                            types.push(ty);
                        }
                    }
                    self.expect(TokenKind::RightParen, "expected ')' after tuple type")
                        .ok()?;
                    return Some(TypeAnnotation::Tuple(types));
                }
                _ => {}
            }
            // Not a tuple — reset and fall through.
            self.position = saved;
        }
        if self.peek().kind.is_type_keyword() {
            let token = self.advance();
            return match token.kind {
                TokenKind::TypeArray => {
                    let element = if self.consume(&TokenKind::LeftBracket).is_some() {
                        let element = self.parse_type_annotation(true)?;
                        self.expect(
                            TokenKind::RightBracket,
                            "expected ']' after array element type",
                        )
                        .ok()?;
                        Some(Box::new(element))
                    } else {
                        None
                    };
                    Some(TypeAnnotation::Array(element))
                }
                TokenKind::TypeSlice => {
                    let element = if self.consume(&TokenKind::LeftBracket).is_some() {
                        let element = self.parse_type_annotation(true)?;
                        self.expect(
                            TokenKind::RightBracket,
                            "expected ']' after slice element type",
                        )
                        .ok()?;
                        Some(Box::new(element))
                    } else {
                        None
                    };
                    Some(TypeAnnotation::Slice(element))
                }
                TokenKind::TypeOption => {
                    self.expect(TokenKind::LeftBracket, "expected '[' after 'option'")
                        .ok()?;
                    let inner = self.parse_type_annotation(true)?;
                    self.expect(TokenKind::RightBracket, "expected ']' after option type")
                        .ok()?;
                    Some(TypeAnnotation::Option(Box::new(inner)))
                }
                TokenKind::TypeResult => {
                    self.expect(TokenKind::LeftBracket, "expected '[' after 'result'")
                        .ok()?;
                    let ok = self.parse_type_annotation(true)?;
                    self.expect(
                        TokenKind::Comma,
                        "expected ',' between result ok and err types",
                    )
                    .ok()?;
                    let err = self.parse_type_annotation(true)?;
                    self.expect(TokenKind::RightBracket, "expected ']' after result type")
                        .ok()?;
                    Some(TypeAnnotation::Result {
                        ok: Box::new(ok),
                        err: Box::new(err),
                    })
                }
                _ => TypeAnnotation::from_token(&token),
            };
        }
        // `chan[T]` — a virtual-task channel type.
        if self.peek().kind == TokenKind::Chan {
            self.advance();
            self.expect(TokenKind::LeftBracket, "expected '[' after 'chan'")
                .ok()?;
            let element = self.parse_type_annotation(true)?;
            self.expect(
                TokenKind::RightBracket,
                "expected ']' after chan element type",
            )
            .ok()?;
            return Some(TypeAnnotation::Chan(Box::new(element)));
        }
        // `dyn` stays contextual so existing code can still use it as a
        // plain identifier (`var dyn = []`); it begins a trait object only
        // when a type follows it.
        if allow_named
            && self.peek_at_ident(0)
            && self.peek().lexeme() == "dyn"
            && self.peek_at_ident(1)
        {
            self.advance();
            let trait_name = self.advance();
            if self.check(&TokenKind::Less) {
                let arguments = self.parse_type_arguments().ok()?;
                return Some(TypeAnnotation::Dyn(Token::new(
                    TokenKind::Identifier(mangle_applied_type(trait_name.lexeme(), &arguments)),
                    trait_name.span,
                )));
            }
            return Some(TypeAnnotation::Dyn(trait_name));
        }
        if allow_named && self.peek_at_ident(0) {
            let token = self.advance();
            if self.check(&TokenKind::Less) {
                let arguments = self.parse_type_arguments().ok()?;
                return Some(TypeAnnotation::Named(Token::new(
                    TokenKind::Identifier(mangle_applied_type(token.lexeme(), &arguments)),
                    token.span,
                )));
            }
            if self.consume(&TokenKind::Colon).is_some() {
                self.expect(TokenKind::Colon, "expected second `:` in associated type")
                    .ok()?;
                let associated = self.expect_ident("expected associated type name").ok()?;
                let name = format!("{}::{}", token.lexeme(), associated.lexeme());
                return Some(TypeAnnotation::Named(Token::new(
                    TokenKind::Identifier(name),
                    token.span,
                )));
            }
            return TypeAnnotation::from_token(&token);
        }
        None
    }

    fn parse_type_arguments(&mut self) -> Result<Vec<TypeAnnotation>, ParseError> {
        self.expect(TokenKind::Less, "expected `<` before type arguments")?;
        let mut arguments = Vec::new();
        loop {
            arguments.push(self.parse_type_annotation(true).ok_or_else(|| ParseError {
                message: "expected type argument".into(),
                span: self.peek().span,
            })?);
            if self.consume(&TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect(TokenKind::Greater, "expected `>` after type arguments")?;
        Ok(arguments)
    }

    fn parse_function_body(&mut self) -> Result<Vec<Stmt>, ParseError> {
        if self.check(&TokenKind::LeftBrace) {
            let block = self.parse_block()?;
            if let Stmt::Block { statements, .. } = block {
                Ok(statements)
            } else {
                unreachable!()
            }
        } else if self.check(&TokenKind::Arrow) {
            self.advance();
            self.skip_newlines();
            let expr = self.parse_expression()?;
            self.consume_terminator();
            Ok(vec![Stmt::Return { value: Some(expr) }])
        } else {
            self.skip_newlines();
            let expr = self.parse_expression()?;
            self.consume_terminator();
            Ok(vec![Stmt::Return { value: Some(expr) }])
        }
    }

    fn parse_class_declaration(&mut self) -> Result<Stmt, ParseError> {
        let _class_token = self.advance();
        let name = self.expect_ident("expected class name")?;
        let mut generic_params = self.parse_generic_params()?;

        let parent = if self.check_ident("extends") {
            self.advance();
            Some(self.expect_ident("expected parent class name")?)
        } else {
            None
        };
        self.parse_where_bounds(&mut generic_params)?;

        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "expected '{' after class name")?;
        let mut body = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            body.push(self.parse_statement()?);
            self.skip_newlines();
        }
        self.expect(TokenKind::RightBrace, "expected '}'")?;

        Ok(Stmt::Class {
            name,
            generic_params,
            parent,
            body,
        })
    }

    fn check_ident(&self, text: &str) -> bool {
        matches!(&self.peek().kind, TokenKind::Identifier(s) if s == text)
    }

    fn looks_like_generic_call(&self) -> bool {
        if !self.check(&TokenKind::Less) {
            return false;
        }
        let mut depth = 0usize;
        let mut offset = 0usize;
        loop {
            match self.peek_at(offset) {
                TokenKind::Less => depth += 1,
                TokenKind::Greater => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return self.peek_at(offset + 1) == &TokenKind::LeftParen;
                    }
                }
                TokenKind::Eof | TokenKind::Newline | TokenKind::Semicolon => return false,
                _ => {}
            }
            offset += 1;
        }
    }

    fn peek_at_ident(&self, offset: usize) -> bool {
        matches!(self.peek_at(offset), TokenKind::Identifier(_))
    }

    fn peek_at_type_keyword(&self, offset: usize) -> bool {
        self.peek_at(offset).is_type_keyword()
    }

    fn peek_at_operator(&self, offset: usize) -> bool {
        is_operator_token(self.peek_at(offset))
    }

    fn peek_at(&self, offset: usize) -> &TokenKind {
        &self
            .tokens
            .get(self.position + offset)
            .unwrap_or(&EOF_TOKEN)
            .kind
    }

    fn can_start_expression(&self) -> bool {
        matches!(
            &self.peek().kind,
            TokenKind::NumberLiteral(_)
                | TokenKind::StringLiteral(_)
                | TokenKind::StringSegment(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Nil
                | TokenKind::Identifier(_)
                | TokenKind::This
                | TokenKind::Minus
                | TokenKind::Bang
                | TokenKind::Tilde
                | TokenKind::PlusPlus
                | TokenKind::MinusMinus
                | TokenKind::DotDotDot
                | TokenKind::LeftParen
                | TokenKind::LeftBracket
                | TokenKind::LeftBrace
                | TokenKind::Fun
                | TokenKind::TypeInt
                | TokenKind::TypeFloat
                | TokenKind::TypeString
                | TokenKind::TypeBool
                | TokenKind::TypeArray
                | TokenKind::TypeObject
                | TokenKind::TypeAny
                | TokenKind::TypePointer
                | TokenKind::TypeSlice
        )
    }

    fn parse_enum_declaration(&mut self) -> Result<Stmt, ParseError> {
        let _enum_token = self.advance();
        let name = self.expect_ident("expected enum name")?;
        let mut generic_params = self.parse_generic_params()?;
        self.parse_where_bounds(&mut generic_params)?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "expected '{' after enum name")?;
        let mut members = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            let member_name = self.expect_ident("expected enum member name")?;

            // Optional associated data types: `Variant(Type, Type, ...)`
            let data_types = if self.consume(&TokenKind::LeftParen).is_some() {
                let mut types = Vec::new();
                if !self.check(&TokenKind::RightParen) {
                    loop {
                        types.push(self.parse_type_annotation(true).ok_or_else(|| ParseError {
                            message: "expected type in enum variant data".into(),
                            span: self.peek().span,
                        })?);
                        if self.consume(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                }
                self.expect(
                    TokenKind::RightParen,
                    "expected ')' after enum variant data",
                )?;
                types
            } else {
                Vec::new()
            };

            let value = if self.consume(&TokenKind::Equal).is_some() {
                Some(self.parse_expression()?)
            } else {
                None
            };
            members.push(EnumMember {
                name: member_name,
                value,
                data_types,
            });
            self.consume(&TokenKind::Comma);
            self.skip_newlines();
        }
        self.expect(TokenKind::RightBrace, "expected '}'")?;
        Ok(Stmt::Enum {
            name,
            generic_params,
            members,
        })
    }

    fn parse_type_alias_declaration(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let name = self.expect_ident("expected type alias name")?;
        let generic_params = self.parse_generic_params()?;
        self.expect(TokenKind::Equal, "expected `=` after type alias name")?;
        let target = self.parse_type_annotation(true).ok_or_else(|| ParseError {
            message: "expected aliased type after `=`".into(),
            span: self.peek().span,
        })?;
        self.consume_terminator();
        Ok(Stmt::TypeAlias {
            name,
            generic_params,
            target,
        })
    }

    fn parse_if_statement(&mut self) -> Result<Stmt, ParseError> {
        let _if_token = self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'if'")?;
        let condition = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')'")?;
        self.skip_newlines();
        let then_branch = Box::new(self.parse_statement()?);

        let mut elif_branches = Vec::new();
        while self.check(&TokenKind::Elif) {
            let elif_token = self.advance();
            self.expect(TokenKind::LeftParen, "expected '(' after 'elif'")?;
            let elif_condition = self.parse_expression()?;
            self.expect(TokenKind::RightParen, "expected ')'")?;
            self.skip_newlines();
            let elif_body = Box::new(self.parse_statement()?);
            elif_branches.push(ElifBranch {
                condition: elif_condition,
                body: elif_body,
                elif_span: elif_token.span,
            });
        }

        let else_branch = if self.consume(&TokenKind::Else).is_some() {
            self.skip_newlines();
            Some(Box::new(self.parse_statement()?))
        } else {
            None
        };

        Ok(Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        })
    }

    fn parse_while_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'while'")?;
        let condition = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')'")?;
        self.skip_newlines();
        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::While { condition, body })
    }

    fn parse_do_while_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        self.skip_newlines();
        let body = Box::new(self.parse_statement()?);
        self.expect(TokenKind::While, "expected 'while'")?;
        self.expect(TokenKind::LeftParen, "expected '('")?;
        let condition = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')'")?;
        self.consume_terminator();
        Ok(Stmt::DoWhile { body, condition })
    }

    fn parse_for_statement(&mut self) -> Result<Stmt, ParseError> {
        let _for_token = self.advance();

        // `for await x in producer { ... }`
        if self.check(&TokenKind::Await) {
            self.advance();
            let variable = self.expect_ident("expected variable name after 'for await'")?;
            self.expect(TokenKind::In, "expected 'in' after 'for await variable'")?;
            let producer = self.parse_expression()?;
            self.skip_newlines();
            self.expect(TokenKind::LeftBrace, "expected '{' for for-await body")?;
            let mut body = Vec::new();
            self.skip_newlines();
            while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
                body.push(self.parse_statement()?);
                self.skip_newlines();
            }
            self.expect(TokenKind::RightBrace, "expected '}'")?;
            return Ok(Stmt::ForAwait {
                variable,
                producer,
                body: Box::new(Stmt::Block {
                    statements: body,
                    open_span: Span::dummy(),
                    close_span: Span::dummy(),
                }),
            });
        }

        // `for v in chan { ... }` — receive from a channel until it closes.
        if self.peek_at_ident(0) && self.peek_at(1) == &TokenKind::In {
            let variable = self.expect_ident("expected channel element variable name")?;
            self.expect(TokenKind::In, "expected 'in' after 'for variable'")?;
            let channel = self.parse_expression()?;
            self.skip_newlines();
            self.expect(TokenKind::LeftBrace, "expected '{' for channel-for body")?;
            let mut body = Vec::new();
            self.skip_newlines();
            while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
                body.push(self.parse_statement()?);
                self.skip_newlines();
            }
            self.expect(TokenKind::RightBrace, "expected '}'")?;
            return Ok(Stmt::ChanRecvFor {
                variable,
                channel,
                body: Box::new(Stmt::Block {
                    statements: body,
                    open_span: Span::dummy(),
                    close_span: Span::dummy(),
                }),
            });
        }

        if self.check(&TokenKind::LeftParen) {
            // Look ahead for `for (var key in expr)`; rewind to the saved
            // position if it does not match.
            let saved = self.position;
            self.advance();
            if self.check(&TokenKind::Var) {
                self.advance();
                if let TokenKind::Identifier(_) = &self.peek().kind {
                    let _ = self.advance();
                    if self.check(&TokenKind::In) {
                        self.position = saved;
                        return self.parse_for_in_statement();
                    }
                }
            }
            self.position = saved;
        }

        self.expect(TokenKind::LeftParen, "expected '(' after 'for'")?;

        let init = if self.check(&TokenKind::Var) {
            let var_decl = self.parse_var_declaration(false)?;
            Some(Box::new(var_decl))
        } else if !self.check(&TokenKind::Semicolon) && !self.check(&TokenKind::Newline) {
            let expr = self.parse_expression()?;
            self.consume_terminator();
            Some(Box::new(Stmt::Expression { expression: expr }))
        } else {
            self.consume_terminator();
            None
        };

        let condition = if self.check(&TokenKind::Semicolon) || self.check(&TokenKind::Newline) {
            self.consume_terminator();
            None
        } else {
            let cond = self.parse_expression()?;
            self.consume_terminator();
            Some(cond)
        };

        let update = if self.check(&TokenKind::RightParen) {
            None
        } else {
            Some(self.parse_expression()?)
        };

        self.expect(TokenKind::RightParen, "expected ')'")?;
        self.skip_newlines();
        let body = Box::new(self.parse_statement()?);

        Ok(Stmt::For {
            init,
            condition,
            update,
            body,
        })
    }

    fn parse_for_in_statement(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::LeftParen, "expected '('")?;
        self.expect(TokenKind::Var, "expected 'var'")?;
        let variable = self.expect_ident("expected loop variable name")?;
        self.expect(TokenKind::In, "expected 'in'")?;
        let iterable = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')'")?;
        self.skip_newlines();
        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::ForIn {
            variable,
            iterable,
            body,
        })
    }

    fn parse_go_statement(&mut self) -> Result<Stmt, ParseError> {
        let keyword_span = self.advance().span;
        self.skip_newlines();

        if self.check(&TokenKind::LeftBrace) {
            // `go { ... }` — spawn a goroutine running the inline block.
            self.advance();
            let mut body = Vec::new();
            self.skip_newlines();
            while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
                body.push(self.parse_statement()?);
                self.skip_newlines();
            }
            self.expect(TokenKind::RightBrace, "expected '}' after go block")?;
            return Ok(Stmt::Go {
                call: Expr::Literal {
                    value: LiteralValue::Nil,
                    span: keyword_span,
                },
                block: Some(body),
                keyword_span,
            });
        }

        // `go fn(args)` — spawn a goroutine evaluating the expression.
        let call = self.parse_expression()?;
        Ok(Stmt::Go {
            call,
            block: None,
            keyword_span,
        })
    }

    fn parse_return_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let value = if self.check(&TokenKind::Newline) || self.check(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.consume_terminator();
        Ok(Stmt::Return { value })
    }

    fn parse_break_statement(&mut self) -> Result<Stmt, ParseError> {
        let tok = self.advance();
        self.consume_terminator();
        Ok(Stmt::Break { span: tok.span })
    }

    fn parse_continue_statement(&mut self) -> Result<Stmt, ParseError> {
        let tok = self.advance();
        self.consume_terminator();
        Ok(Stmt::Continue { span: tok.span })
    }

    fn parse_match_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'match'")?;
        let expression = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')'")?;
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace, "expected '{'")?;

        let mut cases = Vec::new();
        let mut default_case = None;
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            if self.check(&TokenKind::Default) {
                self.advance();
                self.expect(TokenKind::Arrow, "expected '=>' after 'default'")?;
                let body = self.parse_statement()?;
                default_case = Some(Box::new(body));
            } else {
                let case_token = self.advance();
                let value = self.parse_expression()?;
                // `Ok(v)` / `Err(e)` arm heads become variant patterns that
                // destructure the scrutinee and bind the payload. Only the
                // builtin result variants are recognized syntactically, so
                // ordinary call-valued cases keep their old meaning.
                let pattern = match_pattern_head(&value);
                let guard = if self.check(&TokenKind::If) {
                    self.advance();
                    Some(self.parse_expression()?)
                } else {
                    None
                };
                self.expect(TokenKind::Arrow, "expected '=>'")?;
                let body = self.parse_statement()?;
                cases.push(MatchCase {
                    value,
                    pattern,
                    guard,
                    body,
                    case_span: case_token.span,
                });
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RightBrace, "expected '}'")?;
        Ok(Stmt::Match {
            expression,
            cases,
            default_case,
        })
    }

    fn parse_try_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        self.skip_newlines();
        let try_block = Box::new(self.parse_statement()?);

        let mut catch_var = None;
        let mut catch_block = None;
        if self.consume(&TokenKind::Catch).is_some() {
            self.expect(TokenKind::LeftParen, "expected '(' after 'catch'")?;
            let var = self.expect_ident("expected catch variable name")?;
            self.expect(TokenKind::RightParen, "expected ')'")?;
            self.skip_newlines();
            catch_var = Some(var);
            catch_block = Some(Box::new(self.parse_statement()?));
        }

        let mut finally_block = None;
        if self.consume(&TokenKind::Finally).is_some() {
            self.skip_newlines();
            finally_block = Some(Box::new(self.parse_statement()?));
        }

        Ok(Stmt::Try {
            try_block,
            catch_var,
            catch_block,
            finally_block,
        })
    }

    fn parse_throw_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let value = self.parse_expression()?;
        self.consume_terminator();
        Ok(Stmt::Throw { value })
    }

    fn parse_retry_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let count = self.parse_expression()?;
        self.skip_newlines();
        let body = Box::new(self.parse_statement()?);

        let mut catch_var = None;
        let mut catch_block = None;
        if self.consume(&TokenKind::Catch).is_some() {
            self.expect(TokenKind::LeftParen, "expected '(' after 'catch'")?;
            let var = self.expect_ident("expected catch variable name")?;
            self.expect(TokenKind::RightParen, "expected ')'")?;
            self.skip_newlines();
            catch_var = Some(var);
            catch_block = Some(Box::new(self.parse_statement()?));
        }

        Ok(Stmt::Retry {
            count,
            body,
            catch_var,
            catch_block,
        })
    }

    fn parse_unsafe_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        self.skip_newlines();
        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::Unsafe { body })
    }

    fn parse_quiet_statement(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        self.skip_newlines();

        let mut suppressed = Vec::new();
        if self.consume(&TokenKind::LeftBracket).is_some() {
            if self.check(&TokenKind::RightBracket) {
                return Err(ParseError {
                    message: "expected at least one warning name in `quiet [...]`".into(),
                    span: self.peek().span,
                });
            }
            loop {
                let name = self.expect_ident("expected a warning name")?;
                suppressed.push(name.lexeme().to_string());
                if self.consume(&TokenKind::Comma).is_none() {
                    break;
                }
            }
            self.expect(TokenKind::RightBracket, "expected ']' after warning names")?;
            self.skip_newlines();
        }

        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::Quiet { suppressed, body })
    }

    fn parse_use_statement(&mut self) -> Result<Stmt, ParseError> {
        let _use_token = self.advance();

        if self.check(&TokenKind::LeftParen) {
            return self.parse_selective_use();
        }

        // A quoted string after `use` is a file import (`use "lib.nt"`);
        // an `@` prefix marks a Rust crate (`use @serde_json`);
        // otherwise an identifier names a stdlib module (`use strings`).
        let (library, is_file_path, is_rust) =
            if let TokenKind::StringLiteral(_) = &self.peek().kind {
                let tok = self.advance();
                (
                    Token::new(TokenKind::Identifier(tok.lexeme().to_string()), tok.span),
                    true,
                    false,
                )
            } else if self.check(&TokenKind::At) {
                let _at = self.advance();
                (
                    self.expect_ident("expected crate name after `@`")?,
                    false,
                    true,
                )
            } else {
                (
                    self.expect_ident("expected module name or file path")?,
                    false,
                    false,
                )
            };
        let alias = if self.check(&TokenKind::As) {
            self.advance();
            Some(self.expect_ident("expected alias name")?)
        } else {
            None
        };
        self.consume_terminator();

        Ok(Stmt::Use {
            library,
            is_file_path,
            is_rust,
            imported_symbols: Vec::new(),
            imported_is_rust: Vec::new(),
            alias,
        })
    }

    fn parse_selective_use(&mut self) -> Result<Stmt, ParseError> {
        self.advance();
        let mut imported_symbols = Vec::new();
        let mut imported_is_rust = Vec::new();

        if !self.check(&TokenKind::RightParen) {
            loop {
                let is_rust = self.consume(&TokenKind::At).is_some();
                imported_symbols.push(self.expect_ident("expected imported symbol name")?);
                imported_is_rust.push(is_rust);
                if self.consume(&TokenKind::Comma).is_none() {
                    break;
                }
            }
        }

        self.expect(TokenKind::RightParen, "expected ')'")?;
        self.expect(TokenKind::Equal, "expected '='")?;
        self.expect(TokenKind::From, "expected 'from'")?;

        let (library, is_file_path, is_rust) =
            if let TokenKind::StringLiteral(_) = &self.peek().kind {
                let tok = self.advance();
                (
                    Token::new(TokenKind::Identifier(tok.lexeme().to_string()), tok.span),
                    true,
                    false,
                )
            } else if self.check(&TokenKind::At) {
                let _at = self.advance();
                (
                    self.expect_ident("expected module name after `@`")?,
                    false,
                    true,
                )
            } else {
                (self.expect_ident("expected module name")?, false, false)
            };

        self.consume_terminator();

        Ok(Stmt::Use {
            library,
            is_file_path,
            is_rust,
            imported_symbols,
            imported_is_rust,
            alias: None,
        })
    }

    // ── Expressions (Pratt) ─────────────────────────────────────────

    fn parse_expression(&mut self) -> Result<Expr, ParseError> {
        self.parse_precedence(Precedence::Assignment)
    }

    fn parse_precedence(&mut self, precedence: Precedence) -> Result<Expr, ParseError> {
        let mut left = self.parse_prefix()?;

        while !self.is_at_end() && precedence <= self.infix_precedence() {
            left = self.parse_infix(left)?;
        }

        Ok(left)
    }

    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        let kind = self.peek().kind.clone();
        match kind {
            TokenKind::NumberLiteral(_) => {
                let tok = self.advance();
                let val = tok.lexeme().to_string();
                Ok(Expr::Literal {
                    value: LiteralValue::Number(val),
                    span: tok.span,
                })
            }
            TokenKind::StringLiteral(_) => {
                let tok = self.advance();
                let val = tok.lexeme().to_string();
                Ok(Expr::Literal {
                    value: LiteralValue::String(val),
                    span: tok.span,
                })
            }
            TokenKind::StringSegment(_) => self.parse_string_with_interpolation(),
            TokenKind::True => {
                let tok = self.advance();
                Ok(Expr::Literal {
                    value: LiteralValue::Bool(true),
                    span: tok.span,
                })
            }
            TokenKind::False => {
                let tok = self.advance();
                Ok(Expr::Literal {
                    value: LiteralValue::Bool(false),
                    span: tok.span,
                })
            }
            TokenKind::Nil => {
                let tok = self.advance();
                Ok(Expr::Literal {
                    value: LiteralValue::Nil,
                    span: tok.span,
                })
            }
            TokenKind::Close => {
                // `close(chan)` — close a channel for further sends.
                let keyword = self.advance().span;
                self.expect(TokenKind::LeftParen, "expected '(' after 'close'")?;
                let channel = self.parse_expression()?;
                self.expect(TokenKind::RightParen, "expected ')' after close argument")?;
                Ok(Expr::Close {
                    channel: Box::new(channel),
                    keyword,
                })
            }
            TokenKind::Identifier(_) => {
                let tok = self.advance();
                let tok = if self.looks_like_generic_call() {
                    let arguments = self.parse_type_arguments()?;
                    Token::new(
                        TokenKind::Identifier(mangle_applied_type(tok.lexeme(), &arguments)),
                        tok.span,
                    )
                } else {
                    tok
                };
                // `ClassName { key: val, ... }` — struct literal.
                if self.check(&TokenKind::LeftBrace) && self.lookahead_struct_literal_fields() {
                    return self.parse_struct_literal(tok);
                }
                Ok(Expr::Variable { name: tok })
            }
            TokenKind::This => {
                let tok = self.advance();
                Ok(Expr::This { keyword: tok })
            }
            TokenKind::Minus
            | TokenKind::Bang
            | TokenKind::Tilde
            | TokenKind::PlusPlus
            | TokenKind::MinusMinus => {
                let op = self.advance();
                let right = self.parse_precedence(Precedence::Unary)?;
                Ok(Expr::Unary {
                    op,
                    right: Box::new(right),
                })
            }
            TokenKind::DotDotDot => {
                let op = self.advance();
                let value = self.parse_precedence(Precedence::Unary)?;
                Ok(Expr::Spread {
                    value: Box::new(value),
                    op_span: op.span,
                })
            }
            TokenKind::LeftParen => {
                let open = self.advance();
                let expr = self.parse_expression()?;
                // If followed by a comma, this is a tuple literal `(e1, e2, ...)`.
                if self.check(&TokenKind::Comma) {
                    let mut elements = vec![expr];
                    while self.consume(&TokenKind::Comma).is_some() {
                        if self.check(&TokenKind::RightParen) {
                            break;
                        }
                        elements.push(self.parse_expression()?);
                    }
                    let close = self.expect(TokenKind::RightParen, "expected ')' after tuple")?;
                    return Ok(Expr::TupleLiteral {
                        elements,
                        span: open.span.to(close.span),
                    });
                }
                let close = self.expect(TokenKind::RightParen, "expected ')'")?;
                Ok(Expr::Grouping {
                    expression: Box::new(expr),
                    open_span: open.span,
                    close_span: close.span,
                })
            }
            TokenKind::Await => {
                let await_tok = self.advance();
                if self.check(&TokenKind::Async)
                    && (matches!(self.peek_at(1), TokenKind::LeftBrace)
                        || matches!(self.peek_at(1), TokenKind::ReturnArrow))
                {
                    let block = self.parse_async_block()?;
                    let span = await_tok.span.to(block.span());
                    return Ok(Expr::Await {
                        callee: Box::new(block),
                        arguments: vec![],
                        span,
                    });
                }
                let operand = self.parse_precedence(Precedence::Call)?;
                match operand {
                    Expr::Call {
                        callee, arguments, ..
                    } => {
                        let span = await_tok.span.to(callee.span());
                        Ok(Expr::Await {
                            callee,
                            arguments,
                            span,
                        })
                    }
                    other => Err(ParseError {
                        message: "await requires an async function call".into(),
                        span: other.span(),
                    }),
                }
            }
            TokenKind::Async => {
                if matches!(self.peek_at(1), TokenKind::LeftBrace)
                    || matches!(self.peek_at(1), TokenKind::ReturnArrow)
                {
                    self.parse_async_block()
                } else if matches!(self.peek_at(1), TokenKind::Dot) {
                    let tok = self.advance();
                    Ok(Expr::Variable {
                        name: Token::new(TokenKind::Identifier("async".to_string()), tok.span),
                    })
                } else {
                    Err(ParseError {
                        message: "unexpected 'async' in expression".into(),
                        span: self.peek().span,
                    })
                }
            }
            TokenKind::Chan => {
                // `chan.new(capacity)` in expression position: the keyword
                // names the constructor namespace.
                if matches!(self.peek_at(1), TokenKind::Dot) {
                    let tok = self.advance();
                    Ok(Expr::Variable {
                        name: Token::new(TokenKind::Identifier("chan".to_string()), tok.span),
                    })
                } else {
                    Err(ParseError {
                        message: "unexpected 'chan' in expression; use `chan.new(capacity)`".into(),
                        span: self.peek().span,
                    })
                }
            }
            TokenKind::View => {
                let view_token = self.advance();
                let mutable = self.consume(&TokenKind::Mut).is_some();
                let target = self.parse_precedence(Precedence::Unary)?;
                Ok(Expr::View {
                    target: Box::new(target),
                    mutable,
                    keyword: view_token.span,
                })
            }
            TokenKind::Copy => {
                let copy_token = self.advance();
                self.expect(TokenKind::LeftParen, "expected '(' after `copy`")?;
                let expression = self.parse_expression()?;
                self.expect(TokenKind::RightParen, "expected ')' after copy expression")?;
                Ok(Expr::Copy {
                    expression: Box::new(expression),
                    keyword: copy_token.span,
                })
            }
            TokenKind::Ampersand => {
                let amp = self.advance();
                let mutable = self.consume(&TokenKind::Mut).is_some();
                let target = self.parse_precedence(Precedence::Unary)?;
                Ok(Expr::Borrow {
                    target: Box::new(target),
                    mutable,
                    keyword: amp.span,
                })
            }
            TokenKind::Star => {
                let star = self.advance();
                let target = self.parse_precedence(Precedence::Unary)?;
                Ok(Expr::RawDeref {
                    target: Box::new(target),
                    star: star.span,
                })
            }

            TokenKind::At => {
                let at_span = self.advance().span;
                let name = self.expect_ident("expected crate name after `@`")?;
                let span = at_span.to(name.span);
                let lex = format!("@{}", name.lexeme());
                Ok(Expr::Variable {
                    name: Token::new(TokenKind::Identifier(lex), span),
                })
            }
            TokenKind::LeftBracket => self.parse_array_literal(),
            TokenKind::LeftBrace => self.parse_object_literal(),
            TokenKind::Fun => self.parse_lambda(),
            _ => Err(ParseError {
                message: format!(
                    "unexpected token '{}' in expression",
                    token_debug_name(&kind)
                ),
                span: self.peek().span,
            }),
        }
    }

    fn parse_string_with_interpolation(&mut self) -> Result<Expr, ParseError> {
        let mut parts: Vec<Expr> = Vec::new();

        loop {
            // Literal StringSegment chunks alternate with interpolated expressions.
            while matches!(&self.peek().kind, TokenKind::StringSegment(_)) {
                let text = match &self.peek().kind {
                    TokenKind::StringSegment(t) => t.clone(),
                    _ => unreachable!(),
                };
                let tok = self.advance();
                parts.push(Expr::Literal {
                    value: LiteralValue::String(text),
                    span: tok.span,
                });
            }

            if !self.can_start_expression() {
                break;
            }

            parts.push(self.parse_expression()?);
        }

        if parts.len() == 1 {
            return Ok(parts.remove(0));
        }

        if parts.is_empty() {
            return Ok(Expr::Literal {
                value: LiteralValue::String(String::new()),
                span: Span::dummy(),
            });
        }

        let mut result = parts.remove(0);
        let plus_op = Token::new(TokenKind::Plus, result.span());
        for part in parts {
            result = Expr::Binary {
                left: Box::new(result),
                op: plus_op.clone(),
                right: Box::new(part),
            };
        }

        Ok(result)
    }

    fn parse_array_literal(&mut self) -> Result<Expr, ParseError> {
        let open = self.advance();
        let mut elements = Vec::new();
        if !self.check(&TokenKind::RightBracket) {
            loop {
                elements.push(self.parse_expression()?);
                if self.consume(&TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::RightBracket, "expected ']'")?;
        Ok(Expr::ArrayLiteral {
            elements,
            span: open.span.to(close.span),
        })
    }

    fn parse_object_literal(&mut self) -> Result<Expr, ParseError> {
        let open = self.advance();
        let mut properties = Vec::new();
        self.skip_newlines();
        if !self.check(&TokenKind::RightBrace) {
            loop {
                let key = if let TokenKind::StringLiteral(_) = &self.peek().kind {
                    let tok = self.advance();
                    (tok.lexeme().to_string(), tok.span)
                } else {
                    let tok = self.expect_ident("expected property name")?;
                    (tok.lexeme().to_string(), tok.span)
                };

                self.expect(TokenKind::Colon, "expected ':'")?;
                let value = self.parse_expression()?;
                properties.push(ObjectProperty {
                    key: key.0,
                    value,
                    key_span: key.1,
                });

                if self.consume(&TokenKind::Comma).is_none() {
                    break;
                }
                self.skip_newlines();
            }
        }
        let close = self.expect(TokenKind::RightBrace, "expected '}'")?;
        Ok(Expr::ObjectLiteral {
            properties,
            span: open.span.to(close.span),
        })
    }

    /// Peek ahead (without consuming) to check if the `{` block following an
    /// identifier looks like struct literal fields: `key: expr`, shorthand
    /// `key`, or a `..base` update. Returns `false` for empty braces `{}`.
    fn lookahead_struct_literal_fields(&self) -> bool {
        // The `{` is at offset 0 (current peek). Fields start at offset 1.
        let after_brace = self.peek_at(1);
        match after_brace {
            // `{}` — object literal; `..base` — struct update.
            TokenKind::RightBrace => false,
            TokenKind::DotDot => true,
            TokenKind::Identifier(_) | TokenKind::StringLiteral(_) => {
                // `key:` or `key` (shorthand) — struct field
                matches!(
                    self.peek_at(2),
                    TokenKind::Colon | TokenKind::Comma | TokenKind::RightBrace
                )
            }
            _ => false,
        }
    }

    /// Parse `ClassName { field: val, field2, ... }` as a struct literal.
    /// The class name token has already been consumed.
    fn parse_struct_literal(&mut self, class_name: Token) -> Result<Expr, ParseError> {
        let _open = self.advance(); // `{`
        let span_start = class_name.span;
        let mut fields = Vec::new();
        let mut update = None;
        self.skip_newlines();
        while !self.check(&TokenKind::RightBrace) && !self.is_at_end() {
            if let TokenKind::DotDot = &self.peek().kind {
                // `..base` — fill unset fields from another instance.
                if update.is_some() {
                    return Err(ParseError {
                        span: self.peek().span,
                        message: "struct literal can have only one `..` update".into(),
                    });
                }
                let dot = self.advance();
                self.skip_newlines();
                if self.check(&TokenKind::RightBrace) || self.check(&TokenKind::Comma) {
                    return Err(ParseError {
                        span: dot.span,
                        message: "expected an expression after `..` in struct literal".into(),
                    });
                }
                let base = self.parse_expression()?;
                update = Some(Box::new(base));
            } else {
                let (key, key_span) = if let TokenKind::StringLiteral(_) = &self.peek().kind {
                    let tok = self.advance();
                    (tok.lexeme().to_string(), tok.span)
                } else {
                    let tok = self.expect_ident("expected field name")?;
                    (tok.lexeme().to_string(), tok.span)
                };

                if self.consume(&TokenKind::Colon).is_some() {
                    // Explicit `field: value`
                    let value = self.parse_expression()?;
                    fields.push(ObjectProperty {
                        key: key.clone(),
                        value,
                        key_span,
                    });
                } else {
                    // Shorthand `field` — desugars to `field: field`
                    fields.push(ObjectProperty {
                        key: key.clone(),
                        value: Expr::Variable {
                            name: Token::new(TokenKind::Identifier(key.clone()), key_span),
                        },
                        key_span,
                    });
                }
            }

            if self.consume(&TokenKind::Comma).is_none() {
                break;
            }
            self.skip_newlines();
        }
        let close = self.expect(TokenKind::RightBrace, "expected '}'")?;
        Ok(Expr::StructLiteral {
            class_name,
            fields,
            update,
            span: span_start.to(close.span),
        })
    }

    fn parse_lambda(&mut self) -> Result<Expr, ParseError> {
        let fun_token = self.advance();
        let (params, return_type) = self.parse_function_signature()?;
        self.skip_newlines();
        let body = self.parse_function_body()?;
        Ok(Expr::Lambda {
            params,
            return_type,
            body,
            span: fun_token.span,
        })
    }

    // ── Infix ───────────────────────────────────────────────────────

    fn infix_precedence(&self) -> Precedence {
        match &self.peek().kind {
            TokenKind::Equal => Precedence::Assignment,
            TokenKind::Question => Precedence::Ternary,
            TokenKind::OrSym => Precedence::Or,
            TokenKind::AndSym => Precedence::And,
            TokenKind::EqualEqual | TokenKind::BangEqual => Precedence::Equality,
            TokenKind::Greater
            | TokenKind::GreaterEqual
            | TokenKind::Less
            | TokenKind::LessEqual => Precedence::Comparison,
            TokenKind::Pipe => Precedence::BitwiseOr,
            TokenKind::Caret => Precedence::BitwiseXor,
            TokenKind::Ampersand => Precedence::BitwiseAnd,
            TokenKind::LessLess
            | TokenKind::GreaterGreater
            | TokenKind::LessPipe
            | TokenKind::PipeGreater => Precedence::Shift,
            TokenKind::Plus | TokenKind::Minus => Precedence::Term,
            TokenKind::Star | TokenKind::Slash | TokenKind::Percent => Precedence::Factor,
            TokenKind::PlusPlus | TokenKind::MinusMinus => Precedence::Call,
            TokenKind::Dot
            | TokenKind::QuestionDot
            | TokenKind::LeftParen
            | TokenKind::LeftBracket => Precedence::Call,
            _ => Precedence::None,
        }
    }

    fn parse_infix(&mut self, left: Expr) -> Result<Expr, ParseError> {
        // Read the operator's precedence before `advance()` consumes it and
        // changes `peek()`.
        let op_prec = self.infix_precedence();

        match &self.peek().kind {
            TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::EqualEqual
            | TokenKind::BangEqual
            | TokenKind::Greater
            | TokenKind::GreaterEqual
            | TokenKind::Less
            | TokenKind::LessEqual
            | TokenKind::AndSym
            | TokenKind::OrSym
            | TokenKind::Ampersand
            | TokenKind::Pipe
            | TokenKind::Caret
            | TokenKind::LessLess
            | TokenKind::GreaterGreater => {
                let op = self.advance();
                let right = self.parse_precedence(op_prec.next_higher())?;
                Ok(Expr::Binary {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                })
            }
            // The arrow points where the data flows: `v |> ch` pipes the
            // value into the channel (send); `r <| ch` feeds the receiver
            // from the channel (receive).
            TokenKind::PipeGreater => {
                let op_span = self.advance().span;
                let channel = self.parse_precedence(op_prec.next_higher())?;
                Ok(Expr::ChanSend {
                    channel: Box::new(channel),
                    value: Box::new(left),
                    op_span,
                })
            }
            TokenKind::LessPipe => {
                let op_span = self.advance().span;
                let channel = self.parse_precedence(op_prec.next_higher())?;
                match left {
                    Expr::Variable { name } => Ok(Expr::ChanRecv {
                        receiver: name,
                        channel: Box::new(channel),
                        op_span,
                    }),
                    _ => Err(ParseError {
                        message: "receive target must be a variable".to_string(),
                        span: op_span,
                    }),
                }
            }
            TokenKind::Equal => {
                self.advance();
                let value = self.parse_precedence(Precedence::Assignment)?;
                match left {
                    Expr::Variable { name } => Ok(Expr::Assign {
                        name,
                        value: Box::new(value),
                    }),
                    Expr::Member { object, property } => Ok(Expr::MemberSet {
                        object,
                        property,
                        value: Box::new(value),
                    }),
                    Expr::IndexGet { object, index } => Ok(Expr::IndexSet {
                        object,
                        index,
                        value: Box::new(value),
                    }),
                    Expr::RawDeref { target, star } => Ok(Expr::RawDerefSet {
                        target,
                        value: Box::new(value),
                        star,
                    }),
                    _ => Err(ParseError {
                        message: "invalid assignment target".to_string(),
                        span: self.peek().span,
                    }),
                }
            }
            TokenKind::PlusPlus | TokenKind::MinusMinus => {
                let op = self.advance();
                Ok(Expr::PostfixUnary {
                    op,
                    left: Box::new(left),
                })
            }
            TokenKind::Question => {
                // `?` is ambiguous between a postfix propagate operator
                // (`f()?`) and a ternary condition (`c ? x : y`). Try the
                // ternary tail first; if it does not parse, rewind and treat
                // the `?` as propagation.
                let checkpoint = self.position;
                self.advance();
                let ternary = (|| {
                    let then_branch = self.parse_expression().ok()?;
                    self.expect(TokenKind::Colon, "expected ':' in ternary")
                        .ok()?;
                    let else_branch = self.parse_expression().ok()?;
                    Some(Expr::Ternary {
                        condition: Box::new(left.clone()),
                        then_branch: Box::new(then_branch),
                        else_branch: Box::new(else_branch),
                    })
                })();
                if let Some(ternary) = ternary {
                    Ok(ternary)
                } else {
                    self.position = checkpoint;
                    let question = self.advance();
                    Ok(Expr::Propagate {
                        value: Box::new(left),
                        question_span: question.span,
                    })
                }
            }
            TokenKind::Dot => {
                let dot_token = self.advance();
                // Tuple index: `t.0`, `t.1`
                if let TokenKind::NumberLiteral(ref s) = self.peek().kind
                    && let Ok(idx) = s.parse::<usize>()
                {
                    let num_token = self.advance();
                    return Ok(Expr::TupleIndex {
                        object: Box::new(left),
                        index: idx,
                        dot_span: dot_token.span.to(num_token.span),
                    });
                }
                let property = self.parse_member_name()?;
                Ok(Expr::Member {
                    object: Box::new(left),
                    property,
                })
            }
            TokenKind::QuestionDot => {
                self.advance();
                let property = self.parse_member_name()?;
                Ok(Expr::OptionalMember {
                    object: Box::new(left),
                    property,
                })
            }
            TokenKind::LeftParen => {
                let paren = self.advance();
                let mut arguments = Vec::new();
                if !self.check(&TokenKind::RightParen) {
                    loop {
                        if self.check(&TokenKind::DotDotDot) {
                            let spread_op = self.advance();
                            let spread_val = self.parse_precedence(Precedence::Unary)?;
                            arguments.push(Expr::Spread {
                                value: Box::new(spread_val),
                                op_span: spread_op.span,
                            });
                        } else {
                            arguments.push(self.parse_precedence(Precedence::Assignment)?);
                        }
                        if self.consume(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RightParen, "expected ')'")?;
                Ok(Expr::Call {
                    callee: Box::new(left),
                    paren: paren.span,
                    arguments,
                })
            }
            TokenKind::LeftBracket => {
                self.advance();
                let index = self.parse_expression()?;
                self.expect(TokenKind::RightBracket, "expected ']'")?;
                Ok(Expr::IndexGet {
                    object: Box::new(left),
                    index: Box::new(index),
                })
            }
            _ => unreachable!("parse_infix called with non-infix token"),
        }
    }
}

// ── Precedence ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Precedence {
    None,
    Assignment,
    Ternary,
    Or,
    And,
    Equality,
    Comparison,
    BitwiseOr,
    BitwiseXor,
    BitwiseAnd,
    Shift,
    Term,
    Factor,
    Unary,
    Call,
}

impl Precedence {
    fn next_higher(self) -> Precedence {
        match self {
            Precedence::None => Precedence::Assignment,
            Precedence::Assignment => Precedence::Ternary,
            Precedence::Ternary => Precedence::Or,
            Precedence::Or => Precedence::And,
            Precedence::And => Precedence::Equality,
            Precedence::Equality => Precedence::Comparison,
            Precedence::Comparison => Precedence::BitwiseOr,
            Precedence::BitwiseOr => Precedence::BitwiseXor,
            Precedence::BitwiseXor => Precedence::BitwiseAnd,
            Precedence::BitwiseAnd => Precedence::Shift,
            Precedence::Shift => Precedence::Term,
            Precedence::Term => Precedence::Factor,
            Precedence::Factor => Precedence::Unary,
            Precedence::Unary => Precedence::Call,
            Precedence::Call => Precedence::Call,
        }
    }
}

// ── Public convenience ──────────────────────────────────────────────────

pub fn parse(tokens: &[Token]) -> Result<ntsc_ast::stmt::Program, Vec<ParseError>> {
    Parser::parse(tokens)
}

/// Parse `tokens`, returning the successfully parsed statements together
/// with every error encountered. See [`Parser::parse_recovering`].
pub fn parse_recovering(tokens: &[Token]) -> (ntsc_ast::stmt::Program, Vec<ParseError>) {
    Parser::parse_recovering(tokens)
}

/// Recognize a match arm head of the shape `Ok(binder)` or `Err(binder)` as
/// a variant pattern. The binder may be `_` to ignore the payload. Bare
/// `Ok` / `Err` without arguments stays a plain value case, matching the
/// pre-pattern behavior.
fn match_pattern_head(value: &Expr) -> Option<ntsc_ast::stmt::MatchPattern> {
    let Expr::Call {
        callee, arguments, ..
    } = value
    else {
        return None;
    };
    if arguments.len() != 1 {
        return None;
    }
    let Expr::Variable { name: variant } = &**callee else {
        return None;
    };
    if !matches!(variant.lexeme(), "Ok" | "Err") {
        return None;
    }
    let Some(Expr::Variable { name: binding }) = arguments.first() else {
        return None;
    };
    let binding = if binding.lexeme() == "_" {
        None
    } else {
        Some(binding.clone())
    };
    Some(ntsc_ast::stmt::MatchPattern {
        variant: variant.clone(),
        binding,
    })
}

fn mangle_applied_type(base: &str, arguments: &[TypeAnnotation]) -> String {
    format!(
        "{base}<{}>",
        arguments
            .iter()
            .map(type_source)
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn type_source(annotation: &TypeAnnotation) -> String {
    match annotation {
        TypeAnnotation::Named(token) => token.lexeme().to_string(),
        TypeAnnotation::Int => "int".into(),
        TypeAnnotation::Float => "float".into(),
        TypeAnnotation::String => "string".into(),
        TypeAnnotation::Bool => "bool".into(),
        TypeAnnotation::Array(Some(inner)) => format!("array[{}]", type_source(inner)),
        TypeAnnotation::Option(inner) => format!("option[{}]", type_source(inner)),
        TypeAnnotation::Result { ok, err } => {
            format!("result[{},{}]", type_source(ok), type_source(err))
        }
        TypeAnnotation::Slice(Some(inner)) => format!("slice[{}]", type_source(inner)),
        TypeAnnotation::Shared(inner) => format!("shared {}", type_source(inner)),
        TypeAnnotation::Own(inner) => format!("own {}", type_source(inner)),
        TypeAnnotation::View(inner, mutable) => format!(
            "view {}{}",
            if *mutable { "mut " } else { "" },
            type_source(inner)
        ),
        _ => annotation.label().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident_name(tok: &Token) -> &str {
        tok.lexeme()
    }

    fn parse_source(source: &str) -> Result<ntsc_ast::stmt::Program, Vec<ParseError>> {
        let tokens = ntsc_lexer::tokenize(source);
        parse(&tokens)
    }

    #[test]
    fn parse_error_converts_to_diagnostic() {
        let errs = parse_source("fun main() {\n    say(\"hi\"\n}\n").unwrap_err();
        assert!(!errs.is_empty());
        let diag = ntsc_diag::Diagnostic::from(&errs[0]);
        assert_eq!(diag.severity, ntsc_diag::Severity::Error);
        assert_eq!(diag.code.as_deref(), Some(ntsc_diag::codes::PARSE));
        assert_eq!(diag.labels.len(), 1);
        assert!(diag.labels[0].is_primary);
        assert_eq!(diag.labels[0].span, errs[0].span);
    }

    #[test]
    fn parse_hello_world() {
        let prog = parse_source(r#"say("Hello, World!")"#).unwrap();
        assert_eq!(prog.statements.len(), 1);
        assert!(matches!(&prog.statements[0], Stmt::Say { .. }));
    }

    #[test]
    fn parse_var_declaration() {
        let prog = parse_source("var x = 42").unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                name,
                initializer,
                is_static,
                ..
            } => {
                assert_eq!(ident_name(name), "x");
                assert!(initializer.is_some());
                assert!(!is_static);
            }
            other => panic!("expected Var, got {:?}", other),
        }
    }

    #[test]
    fn parse_typed_var() {
        let prog = parse_source("var int x = 42").unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                type_annotation, ..
            } => {
                assert_eq!(*type_annotation, Some(TypeAnnotation::Int));
            }
            other => panic!("expected Var, got {:?}", other),
        }
    }

    #[test]
    fn parse_generics_and_traits() {
        let source = "trait Printable { fun format() -> string } impl Printable for User { fun format() -> string { return \"user\" } } fun identity<T>(T value) -> T { return value }";
        let program = parse_source(source).expect("generic and trait syntax should parse");
        assert!(matches!(&program.statements[0], Stmt::Trait { .. }));
        assert!(matches!(&program.statements[1], Stmt::Impl { .. }));
        assert!(
            matches!(&program.statements[2], Stmt::Function { generic_params, .. } if generic_params.len() == 1)
        );
    }

    #[test]
    fn parse_associated_types_and_projections() {
        let source = "trait Producer { type Item fun item() -> Item } fun read<T: Producer>(T value) -> T::Item { return value.item() }";
        let program = parse_source(source).expect("associated type syntax should parse");
        assert!(matches!(
            &program.statements[0],
            Stmt::Trait { associated_types, .. } if associated_types.len() == 1
        ));
        assert!(matches!(
            &program.statements[1],
            Stmt::Function { return_type: Some(ret), .. }
                if matches!(&ret.ty, TypeAnnotation::Named(token) if token.lexeme() == "T::Item")
        ));
    }

    #[test]
    fn parse_view_var_declarations() {
        let prog = parse_source("view var r = x\nview mut var m = y").unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                name,
                initializer,
                view,
                is_static,
                ..
            } => {
                assert_eq!(ident_name(name), "r");
                assert!(initializer.is_some());
                assert!(!is_static);
                assert_eq!(*view, Some(ViewMutability::ReadOnly));
            }
            other => panic!("expected Var, got {:?}", other),
        }
        match &prog.statements[1] {
            Stmt::Var { view, .. } => {
                assert_eq!(*view, Some(ViewMutability::Mutable));
            }
            other => panic!("expected Var, got {:?}", other),
        }
    }

    #[test]
    fn parse_view_var_with_annotation() {
        let prog = parse_source("view var array[int] r = xs").unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                type_annotation,
                view,
                ..
            } => {
                assert!(matches!(
                    type_annotation,
                    Some(TypeAnnotation::Array(Some(_)))
                ));
                assert_eq!(*view, Some(ViewMutability::ReadOnly));
            }
            other => panic!("expected Var, got {:?}", other),
        }
    }

    #[test]
    fn parse_view_expression_statement_still_works() {
        let prog = parse_source("view x\n").unwrap();
        assert!(matches!(prog.statements[0], Stmt::Expression { .. }));
    }

    #[test]
    fn parse_pointer_type_annotations() {
        let prog = parse_source(
            "var own Packet packet = alloc(Packet())\nvar &Packet read = &packet\nvar &mut Packet write = &mut packet\nvar *mut int raw = 0\nvar *const int ro = 0",
        )
        .unwrap();
        assert!(matches!(
            &prog.statements[0],
            Stmt::Var {
                type_annotation: Some(TypeAnnotation::Own(_)),
                ..
            }
        ));
        assert!(matches!(
            &prog.statements[1],
            Stmt::Var {
                type_annotation: Some(TypeAnnotation::Ref(_, false)),
                ..
            }
        ));
        assert!(matches!(
            &prog.statements[2],
            Stmt::Var {
                type_annotation: Some(TypeAnnotation::Ref(_, true)),
                ..
            }
        ));
        assert!(matches!(
            &prog.statements[3],
            Stmt::Var {
                type_annotation: Some(TypeAnnotation::RawPointer(_, true)),
                ..
            }
        ));
        assert!(matches!(
            &prog.statements[4],
            Stmt::Var {
                type_annotation: Some(TypeAnnotation::RawPointer(_, false)),
                ..
            }
        ));
    }

    #[test]
    fn parse_both_extern_declaration_forms() {
        let program = parse_source(
            r#"extern "C" fun abs(int value) -> int
extern "C" { int labs(int value); }"#,
        )
        .expect("extern declarations should parse");
        assert!(matches!(
            &program.statements[0],
            Stmt::ExternFunction { c_style: false, .. }
        ));
        assert!(matches!(
            &program.statements[1],
            Stmt::ExternBlock { functions, .. }
                if matches!(&functions[0], Stmt::ExternFunction { c_style: true, .. })
        ));
    }

    #[test]
    fn parse_borrow_and_raw_deref_expressions() {
        let prog = parse_source("&a\n&mut b\n*c\n*d = 1\n").unwrap();
        assert!(matches!(prog.statements[0], Stmt::Expression { .. }));
        assert!(matches!(prog.statements[1], Stmt::Expression { .. }));
        assert!(matches!(prog.statements[2], Stmt::Expression { .. }));
        assert!(matches!(prog.statements[3], Stmt::Expression { .. }));
    }

    #[test]
    fn parse_named_type_annotations_in_neutron_declaration_style() {
        let prog = parse_source(
            "var Person owner\nfun describe(Person person) -> string { return \"ok\" }",
        )
        .unwrap();

        let Stmt::Var {
            type_annotation: Some(TypeAnnotation::Named(token)),
            ..
        } = &prog.statements[0]
        else {
            panic!("expected a named variable type annotation");
        };
        assert_eq!(token.lexeme(), "Person");

        let Stmt::Function { params, .. } = &prog.statements[1] else {
            panic!("expected a function declaration");
        };
        assert!(matches!(
            params[0].type_annotation,
            Some(TypeAnnotation::Named(_))
        ));
    }

    #[test]
    fn parse_explicit_array_and_option_types() {
        let prog =
            parse_source("var array[int] values = [1, 2]\nvar option[Person] owner = nil").unwrap();
        let Stmt::Var {
            type_annotation: Some(TypeAnnotation::Array(Some(element))),
            ..
        } = &prog.statements[0]
        else {
            panic!("expected an array element type annotation");
        };
        assert_eq!(**element, TypeAnnotation::Int);
        assert!(matches!(
            prog.statements[1],
            Stmt::Var {
                type_annotation: Some(TypeAnnotation::Option(_)),
                ..
            }
        ));
    }

    #[test]
    fn parse_trait_with_supertraits_defaults_and_dyn() {
        let program = parse_source(
            "trait Named: Printable + Resettable {\n\
             \x20 type Item\n\
             \x20 fun name() -> string\n\
             \x20 fun rename(string to) { say(to) }\n\
             }\n\
             var dyn Named label\n\
             fun make() -> impl Named { }",
        )
        .expect("trait syntax should parse");
        let Stmt::Trait {
            name,
            parents,
            associated_types,
            methods,
        } = &program.statements[0]
        else {
            panic!("expected a trait declaration");
        };
        assert_eq!(ident_name(name), "Named");
        assert_eq!(
            parents.iter().map(ident_name).collect::<Vec<_>>(),
            vec!["Printable", "Resettable"]
        );
        assert_eq!(associated_types.len(), 1);
        assert_eq!(methods.len(), 2);
        assert!(matches!(&methods[0], Stmt::Function { body, .. } if body.is_empty()));
        assert!(matches!(&methods[1], Stmt::Function { body, .. } if !body.is_empty()));

        assert!(matches!(
            &program.statements[1],
            Stmt::Var {
                type_annotation: Some(TypeAnnotation::Dyn(_)),
                ..
            }
        ));
        assert!(matches!(
            &program.statements[2],
            Stmt::Function {
                return_type: Some(ReturnType {
                    ty: TypeAnnotation::ImplTrait(_),
                    ..
                }),
                ..
            }
        ));

        // `dyn` followed by a non-type stays an ordinary identifier.
        let program = parse_source("var dyn = [1]").unwrap();
        assert!(matches!(
            &program.statements[0],
            Stmt::Var {
                type_annotation: None,
                ..
            }
        ));
    }

    #[test]
    fn parse_function() {
        let source = "fun add(int a, int b) -> int {\n    return a + b\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Function {
                name,
                params,
                return_type,
                body,
                ..
            } => {
                assert_eq!(ident_name(name), "add");
                assert_eq!(params.len(), 2);
                assert!(return_type.is_some());
                assert!(!body.is_empty());
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_else() {
        let source = "if (x > 0) {\n    say(\"positive\")\n} else {\n    say(\"non-positive\")\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::If { else_branch, .. } => assert!(else_branch.is_some()),
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_while_loop() {
        let source = "while (x < 10) {\n    x = x + 1\n}";
        let prog = parse_source(source).unwrap();
        assert!(matches!(&prog.statements[0], Stmt::While { .. }));
    }

    #[test]
    fn parse_class() {
        let source = "class Animal {\n    var name\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Class { name, body, .. } => {
                assert_eq!(ident_name(name), "Animal");
                assert!(!body.is_empty());
            }
            other => panic!("expected Class, got {:?}", other),
        }
    }

    #[test]
    fn parse_class_extends() {
        let source = "class Dog extends Animal {\n    var breed\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Class { name, parent, .. } => {
                assert_eq!(ident_name(name), "Dog");
                assert!(parent.is_some());
            }
            other => panic!("expected Class, got {:?}", other),
        }
    }

    #[test]
    fn parse_generic_class_and_application() {
        let program = parse_source(
            "class Pair<T, U> { var T first var U second } fun main() { var Pair<int, string> pair = Pair<int, string>() }",
        )
        .expect("generic class syntax should parse");
        assert!(matches!(
            &program.statements[0],
            Stmt::Class { generic_params, .. } if generic_params.len() == 2
        ));
        assert!(matches!(&program.statements[1], Stmt::Function { .. }));
    }

    #[test]
    fn parse_match() {
        let source = "match (x) {\n    case 1 => say(\"one\")\n    default => say(\"other\")\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Match {
                cases,
                default_case,
                ..
            } => {
                assert_eq!(cases.len(), 1);
                assert!(default_case.is_some());
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn parse_try_catch() {
        let source = "try {\n    throw \"error\"\n} catch (e) {\n    say(e)\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Try {
                catch_var,
                catch_block,
                ..
            } => {
                assert!(catch_var.is_some());
                assert!(catch_block.is_some());
            }
            other => panic!("expected Try, got {:?}", other),
        }
    }

    #[test]
    fn parse_for_loop() {
        let source = "for (var i = 0; i < 10; i = i + 1) {\n    say(i)\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::For {
                init,
                condition,
                update,
                ..
            } => {
                assert!(init.is_some());
                assert!(condition.is_some());
                assert!(update.is_some());
            }
            other => panic!("expected For, got {:?}", other),
        }
    }

    #[test]
    fn parse_for_in() {
        let source = "for (var key in obj) {\n    say(key)\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::ForIn { variable, .. } => assert_eq!(ident_name(variable), "key"),
            other => panic!("expected ForIn, got {:?}", other),
        }
    }

    #[test]
    fn parse_operator_precedence() {
        let source = "say(2 + 3 * 4)";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Say { expression, .. } => assert!(matches!(expression, Expr::Binary { .. })),
            other => panic!("expected Say, got {:?}", other),
        }
    }

    #[test]
    fn parse_no_semicolons() {
        let source = "var x = 1\nvar y = 2\nsay(x + y)";
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 3);
    }

    #[test]
    fn parse_string_interpolation() {
        let source = r#"say("Hello, ${name}!")"#;
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 1);
    }

    #[test]
    fn parse_string_interpolation_leading_expression() {
        let source = r#"say("${n}")"#;
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 1);

        let source = r#"say("${n}!")"#;
        parse_source(source).unwrap();
    }

    #[test]
    fn parse_lambda() {
        let source = "var add = fun(a, b) {\n    return a + b\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer: Some(Expr::Lambda { params, .. }),
                ..
            } => {
                assert_eq!(params.len(), 2);
            }
            other => panic!("expected Var with Lambda, got {:?}", other),
        }
    }

    #[test]
    fn parse_array_literal() {
        let source = "var arr = [1, 2, 3]";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer: Some(Expr::ArrayLiteral { elements, .. }),
                ..
            } => {
                assert_eq!(elements.len(), 3);
            }
            other => panic!("expected Var with Array, got {:?}", other),
        }
    }

    #[test]
    fn parse_object_literal() {
        let source = r#"var obj = {"name": "Alice", "age": 30}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer: Some(Expr::ObjectLiteral { properties, .. }),
                ..
            } => {
                assert_eq!(properties.len(), 2);
            }
            other => panic!("expected Var with Object, got {:?}", other),
        }
    }

    #[test]
    fn parse_member_access() {
        let source = "obj.property";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Member { property, .. },
            } => {
                assert_eq!(ident_name(property), "property");
            }
            other => panic!("expected Member, got {:?}", other),
        }
    }

    #[test]
    fn parse_async_function() {
        let source = "async fun fetch(int n) -> string {\n    return n + \"\"\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::AsyncFunction {
                name,
                params,
                return_type,
                body,
            } => {
                assert_eq!(ident_name(name), "fetch");
                assert_eq!(params.len(), 1);
                assert!(return_type.is_some());
                assert!(!body.is_empty());
            }
            other => panic!("expected AsyncFunction, got {:?}", other),
        }
    }

    #[test]
    fn parse_await_call() {
        let source = "var r = await fetch(42)";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer:
                    Some(Expr::Await {
                        callee, arguments, ..
                    }),
                ..
            } => {
                assert!(matches!(callee.as_ref(), Expr::Variable { .. }));
                assert_eq!(arguments.len(), 1);
            }
            other => panic!("expected Var with Await initializer, got {:?}", other),
        }
    }

    #[test]
    fn parse_await_sleep_statement() {
        let source = "await async.sleep(100)";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Await { callee, .. },
            } => {
                assert!(matches!(callee.as_ref(), Expr::Member { .. }));
            }
            other => panic!("expected Await expression statement, got {:?}", other),
        }
    }

    #[test]
    fn parse_await_requires_call() {
        assert!(parse_source("await x").is_err());
    }

    #[test]
    fn parse_function_call() {
        let source = "add(1, 2)";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Call { arguments, .. },
            } => {
                assert_eq!(arguments.len(), 2);
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn parse_method_call() {
        let source = "person.greet()";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Call { callee, .. },
            } => {
                assert!(matches!(callee.as_ref(), Expr::Member { .. }));
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn parse_return_statement() {
        let source = "return 42";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Return {
                value:
                    Some(Expr::Literal {
                        value: LiteralValue::Number(n),
                        ..
                    }),
            } => {
                assert_eq!(n, "42");
            }
            other => panic!("expected Return, got {:?}", other),
        }
    }

    #[test]
    fn parse_ternary() {
        let source = "var x = a > 0 ? a : -a";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer: Some(Expr::Ternary { .. }),
                ..
            } => {}
            other => panic!("expected Ternary, got {:?}", other),
        }
    }

    #[test]
    fn parse_static_var() {
        let source = "static var PI = 3.14";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                is_static, name, ..
            } => {
                assert!(*is_static);
                assert_eq!(ident_name(name), "PI");
            }
            other => panic!("expected static Var, got {:?}", other),
        }
    }

    #[test]
    fn parse_enum() {
        let source = "enum Color {\n    RED\n    GREEN\n    BLUE\n}";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Enum { name, members, .. } => {
                assert_eq!(ident_name(name), "Color");
                assert_eq!(members.len(), 3);
            }
            other => panic!("expected Enum, got {:?}", other),
        }
    }

    #[test]
    fn parse_index_access() {
        let source = "arr[0]";
        let prog = parse_source(source).unwrap();
        assert!(matches!(
            &prog.statements[0],
            Stmt::Expression {
                expression: Expr::IndexGet { .. }
            }
        ));
    }

    #[test]
    fn parse_nested_member() {
        let source = "a.b.c";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Member { object, property },
            } => {
                assert_eq!(ident_name(property), "c");
                assert!(matches!(object.as_ref(), Expr::Member { .. }));
            }
            other => panic!("expected nested Member, got {:?}", other),
        }
    }

    #[test]
    fn parse_optional_chaining() {
        let source = "obj?.prop";
        let prog = parse_source(source).unwrap();
        assert!(matches!(
            &prog.statements[0],
            Stmt::Expression {
                expression: Expr::OptionalMember { .. }
            }
        ));
    }
}

#[cfg(test)]
mod debug_tests {
    #[test]
    fn debug_tokens() {
        let tokens = ntsc_lexer::tokenize(r#"say("Hello, ${name}!")"#);
        for t in &tokens {
            println!("{:?} at {:?}", t.kind, t.span);
        }
    }
}

#[cfg(test)]
mod comprehensive_tests {
    use super::*;

    fn parse_source(source: &str) -> Result<ntsc_ast::stmt::Program, Vec<ParseError>> {
        let tokens = ntsc_lexer::tokenize(source);
        parse(&tokens)
    }

    #[test]
    fn fibonacci_function() {
        let source = r#"
fun fib(n) {
    if (n <= 1) return n
    return fib(n - 1) + fib(n - 2)
}"#;
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 1);
        match &prog.statements[0] {
            Stmt::Function {
                name, params, body, ..
            } => {
                assert_eq!(name.lexeme(), "fib");
                assert_eq!(params.len(), 1);
                assert!(!body.is_empty());
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn person_class_with_methods() {
        let source = r#"
class Person {
    var name
    var age

    fun init(name, age) {
        this.name = name
        this.age = age
    }

    fun greet() {
        return "Hi, I am " + this.name
    }
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Class {
                name, body, parent, ..
            } => {
                assert_eq!(name.lexeme(), "Person");
                assert!(parent.is_none());

                assert_eq!(body.len(), 4);
            }
            other => panic!("expected Class, got {:?}", other),
        }
    }

    #[test]
    fn dog_extends_animal() {
        let source = r#"
class Dog extends Animal {
    fun speak() {
        return "Woof!"
    }
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Class { name, parent, .. } => {
                assert_eq!(name.lexeme(), "Dog");
                assert!(parent.is_some());
                assert_eq!(parent.as_ref().unwrap().lexeme(), "Animal");
            }
            other => panic!("expected Class, got {:?}", other),
        }
    }

    #[test]
    fn log_analyzer_structure() {
        let source = r#"
var content = "line1\nline2\nline3"
var lines = content.split("\n")
var errors = 0
var warnings = 0
for (var i = 0; i < lines.length(); i = i + 1) {
    var line = lines[i]
    if (line.contains("ERROR")) {
        errors = errors + 1
    }
}
say("Errors: " + errors)"#;
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 6);
    }

    #[test]
    fn complex_control_flow() {
        let source = r#"
var x = 10
if (x > 0) {
    say("positive")
} elif (x == 0) {
    say("zero")
} else {
    say("negative")
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[1] {
            Stmt::If {
                elif_branches,
                else_branch,
                ..
            } => {
                assert_eq!(elif_branches.len(), 1);
                assert!(else_branch.is_some());
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn do_while_loop() {
        let source = r#"
var i = 0
do {
    say(i)
    i = i + 1
} while (i < 10)"#;
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 2);
        assert!(matches!(&prog.statements[1], Stmt::DoWhile { .. }));
    }

    #[test]
    fn try_catch_finally() {
        let source = r#"
try {
    throw "error"
} catch (e) {
    say(e)
} finally {
    say("cleanup")
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Try {
                catch_var,
                catch_block,
                finally_block,
                ..
            } => {
                assert!(catch_var.is_some());
                assert!(catch_block.is_some());
                assert!(finally_block.is_some());
            }
            other => panic!("expected Try, got {:?}", other),
        }
    }

    #[test]
    fn match_with_guards() {
        let source = r#"
match (x) {
    case 1 => say("one")
    case 2 if x > 0 => say("positive two")
    default => say("other")
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Match { cases, .. } => {
                assert_eq!(cases.len(), 2);
                assert!(cases[1].guard.is_some());
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn enum_declaration() {
        let source = r#"
enum Direction {
    NORTH
    SOUTH
    EAST
    WEST
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Enum { name, members, .. } => {
                assert_eq!(name.lexeme(), "Direction");
                assert_eq!(members.len(), 4);

                for m in members {
                    assert!(m.value.is_none());
                }
            }
            other => panic!("expected Enum, got {:?}", other),
        }
    }

    #[test]
    fn enum_with_explicit_values() {
        let source = r#"
enum HttpStatus {
    OK = 200
    NOT_FOUND = 404
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Enum { members, .. } => {
                assert_eq!(members.len(), 2);
                assert!(members[0].value.is_some());
                assert!(members[1].value.is_some());
            }
            other => panic!("expected Enum, got {:?}", other),
        }
    }

    #[test]
    fn static_variable() {
        let source = "static var PI = 3.14";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                is_static,
                name,
                initializer,
                ..
            } => {
                assert!(*is_static);
                assert_eq!(name.lexeme(), "PI");
                assert!(initializer.is_some());
            }
            other => panic!("expected static Var, got {:?}", other),
        }
    }

    #[test]
    fn string_interpolation_in_say() {
        let source = r#"say("Hello, ${name}!")"#;
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 1);
    }

    #[test]
    fn lambda_passed_as_argument() {
        let source = r#"applyTwice(double, 5)"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Call { arguments, .. },
            } => {
                assert_eq!(arguments.len(), 2);
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn immediately_invoked_lambda() {
        let source = r#"fun(x) { return x + 1 }(5)"#;
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 1);
    }

    #[test]
    fn chained_member_access() {
        let source = "a.b.c.d";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Member { object, property },
            } => {
                assert_eq!(property.lexeme(), "d");
                match object.as_ref() {
                    Expr::Member { object, property } => {
                        assert_eq!(property.lexeme(), "c");
                        match object.as_ref() {
                            Expr::Member { property, .. } => {
                                assert_eq!(property.lexeme(), "b");
                            }
                            other => panic!("expected Member, got {:?}", other),
                        }
                    }
                    other => panic!("expected Member, got {:?}", other),
                }
            }
            other => panic!("expected Member, got {:?}", other),
        }
    }

    #[test]
    fn index_chained_with_member() {
        let source = "arr[0].name";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Member { object, property },
            } => {
                assert_eq!(property.lexeme(), "name");
                assert!(matches!(object.as_ref(), Expr::IndexGet { .. }));
            }
            other => panic!("expected Member(IndexGet), got {:?}", other),
        }
    }

    #[test]
    fn nested_if_elif_else() {
        let source = r#"
if (a) {
    say("a")
} elif (b) {
    say("b")
} elif (c) {
    say("c")
} else {
    say("d")
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::If {
                elif_branches,
                else_branch,
                ..
            } => {
                assert_eq!(elif_branches.len(), 2);
                assert!(else_branch.is_some());
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn for_update_expression() {
        let source = r#"
for (var i = 0; i < 10; i = i + 1) {
    say(i)
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::For { update, .. } => {
                assert!(update.is_some());
                assert!(matches!(update.as_ref().unwrap(), Expr::Assign { .. }));
            }
            other => panic!("expected For, got {:?}", other),
        }
    }

    #[test]
    fn bitwise_operators() {
        let source = "say(a & b | c ^ d)";
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 1);
    }

    #[test]
    fn shift_operators() {
        let source = "say(a << 2 >> 1)";
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 1);
    }

    #[test]
    fn channel_send_and_receive_parse() {
        // `<|` receives, `|>` sends; they do not collide with the `<<`/`>>`
        // shift operators.
        let source = "jobs <| value\nvalue |> jobs";
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 2);
        assert!(matches!(
            &prog.statements[0],
            Stmt::Expression {
                expression: Expr::ChanRecv { .. }
            }
        ));
        assert!(matches!(
            &prog.statements[1],
            Stmt::Expression {
                expression: Expr::ChanSend { .. }
            }
        ));
    }

    #[test]
    fn channel_receive_rejects_non_variable_target() {
        let source = "(a + b) <| jobs";
        let err = parse_source(source).unwrap_err();
        assert!(
            err.iter()
                .any(|e| e.message.contains("receive target must be a variable"))
        );
    }

    #[test]
    fn shift_operators_still_binary_after_channel_tokens() {
        // `8 >> -2` is a shift, not a channel receive.
        let source = "var x = 8 >> -2";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer: Some(expr),
                ..
            } => assert!(matches!(expr, Expr::Binary { .. })),
            other => panic!("expected Var with binary shift, got {other:?}"),
        }
    }

    #[test]
    fn ternary_precedence_correct() {
        let source = "var x = a > 0 ? a : -a";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer: Some(Expr::Ternary { condition, .. }),
                ..
            } => {
                assert!(matches!(condition.as_ref(), Expr::Binary { .. }));
            }
            other => panic!("expected Ternary, got {:?}", other),
        }
    }

    #[test]
    fn mixed_precedence_operators() {
        // `result` is now a type keyword, so the accumulator uses another name.
        let source = "var total = 2 + 3 * 4 - 1";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer: Some(expr),
                ..
            } => {
                assert!(matches!(expr, Expr::Binary { .. }));
            }
            other => panic!("expected Binary, got {:?}", other),
        }
    }

    #[test]
    fn use_statement() {
        let source = "use http";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Use {
                library,
                is_file_path,
                ..
            } => {
                assert_eq!(library.lexeme(), "http");
                assert!(!is_file_path);
            }
            other => panic!("expected Use, got {:?}", other),
        }
    }

    #[test]
    fn file_import_statement() {
        let source = "use \"math.nt\"";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Use {
                library,
                is_file_path,
                ..
            } => {
                assert_eq!(library.lexeme(), "math.nt");
                assert!(*is_file_path);
            }
            other => panic!("expected Use, got {:?}", other),
        }
    }

    #[test]
    fn use_with_alias() {
        let source = "use http as web";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Use { alias, .. } => {
                assert!(alias.is_some());
                assert_eq!(alias.as_ref().unwrap().lexeme(), "web");
            }
            other => panic!("expected Use, got {:?}", other),
        }
    }

    #[test]
    fn file_import_with_alias() {
        let source = "use \"math.nt\" as m";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Use {
                is_file_path,
                alias,
                ..
            } => {
                assert!(*is_file_path);
                assert_eq!(alias.as_ref().unwrap().lexeme(), "m");
            }
            other => panic!("expected Use, got {:?}", other),
        }
    }

    #[test]
    fn selective_import() {
        let source = "use (now) = from time";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Use {
                imported_symbols, ..
            } => {
                assert_eq!(imported_symbols.len(), 1);
                assert_eq!(imported_symbols[0].lexeme(), "now");
            }
            other => panic!("expected Use, got {:?}", other),
        }
    }

    #[test]
    fn selective_file_import() {
        let source = "use (v) = from \"util.nt\"";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Use {
                library,
                is_file_path,
                imported_symbols,
                ..
            } => {
                assert_eq!(library.lexeme(), "util.nt");
                assert!(*is_file_path);
                assert_eq!(imported_symbols.len(), 1);
                assert_eq!(imported_symbols[0].lexeme(), "v");
            }
            other => panic!("expected Use, got {:?}", other),
        }
    }

    #[test]
    fn safe_block() {
        let source = r#"
unsafe {
    var int x = 10
    say(x)
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Unsafe { body } => {
                assert!(matches!(body.as_ref(), Stmt::Block { .. }));
            }
            other => panic!("expected Unsafe, got {:?}", other),
        }
    }

    #[test]
    fn quiet_statement() {
        let source = r#"
quiet [unused_var] var x = 1
say(x)
quiet {
    var int unused = 2
    var other = 3
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Quiet { suppressed, body } => {
                assert_eq!(suppressed, &["unused_var"]);
                assert!(matches!(body.as_ref(), Stmt::Var { .. }));
            }
            other => panic!("expected Quiet, got {:?}", other),
        }
        match &prog.statements[2] {
            Stmt::Quiet { suppressed, body } => {
                assert!(suppressed.is_empty());
                assert!(matches!(body.as_ref(), Stmt::Block { .. }));
            }
            other => panic!("expected Quiet, got {:?}", other),
        }
    }

    #[test]
    fn retry_statement() {
        let source = r#"
retry 3 {
    riskyOperation()
} catch (e) {
    say(e)
}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Retry {
                catch_var,
                catch_block,
                ..
            } => {
                assert!(catch_var.is_some());
                assert!(catch_block.is_some());
            }
            other => panic!("expected Retry, got {:?}", other),
        }
    }

    #[test]
    fn spread_in_call() {
        let source = "add3(...nums)";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::Call { arguments, .. },
            } => {
                assert_eq!(arguments.len(), 1);
                assert!(matches!(arguments[0], Expr::Spread { .. }));
            }
            other => panic!("expected Call with Spread, got {:?}", other),
        }
    }

    #[test]
    fn optional_chain_on_nil() {
        let source = "missing?.name";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Expression {
                expression: Expr::OptionalMember { object, property },
            } => {
                assert!(matches!(object.as_ref(), Expr::Variable { .. }));
                assert_eq!(property.lexeme(), "name");
            }
            other => panic!("expected OptionalMember, got {:?}", other),
        }
    }

    #[test]
    fn destructuring_array() {
        let source = "var [a, b, c] = [1, 2, 3]";
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Destructure {
                is_array, names, ..
            } => {
                assert!(*is_array);
                assert_eq!(names.len(), 3);
            }
            other => panic!("expected Destructure, got {:?}", other),
        }
    }

    #[test]
    fn destructuring_object() {
        let source = r#"var {name, age} = {"name": "Bob", "age": 25}"#;
        let prog = parse_source(source).unwrap();
        match &prog.statements[0] {
            Stmt::Destructure {
                is_array, names, ..
            } => {
                assert!(!is_array);
                assert_eq!(names.len(), 2);
            }
            other => panic!("expected Destructure, got {:?}", other),
        }
    }

    #[test]
    fn break_and_continue() {
        let source = r#"
while (true) {
    break
}
while (true) {
    continue
}"#;
        let prog = parse_source(source).unwrap();
        assert_eq!(prog.statements.len(), 2);
    }

    #[test]
    fn parse_result_annotation() {
        let prog = parse_source("fun f() -> result[int, string] { return Ok(1) }").unwrap();
        match &prog.statements[0] {
            Stmt::Function { return_type, .. } => {
                let ret = return_type.as_ref().expect("return type");
                assert_eq!(type_source(&ret.ty), "result[int,string]");
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn parse_question_propagate() {
        let prog =
            parse_source("fun f() -> result[int, string] { var v = g()?  return Ok(v) }").unwrap();
        let Stmt::Function { body, .. } = &prog.statements[0] else {
            panic!("expected Function")
        };
        // First statement: `let v = g()?` — the initializer is a Propagate.
        let Stmt::Var {
            initializer: Some(init),
            ..
        } = &body[0]
        else {
            panic!("expected Var")
        };
        assert!(matches!(init, Expr::Propagate { .. }));
    }

    #[test]
    fn question_ternary_still_parses() {
        let prog = parse_source("var x = a ? b : c").unwrap();
        match &prog.statements[0] {
            Stmt::Var {
                initializer: Some(init),
                ..
            } => assert!(matches!(init, Expr::Ternary { .. })),
            other => panic!("expected Var, got {:?}", other),
        }
    }
}
