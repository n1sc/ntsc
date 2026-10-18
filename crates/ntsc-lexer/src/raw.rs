use logos::Logos;

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r]+")]
#[logos(skip r"//[^\n]*")]
// `#{...}` — block comments (non-nesting; may span newlines).
#[logos(skip r"#\{[^\}]*\}")]
/// Raw logos token kind. The public [`Lexer`](crate::Lexer) turns these into
/// [`ntsc_ast::token::Token`]s, doing quote stripping and interpolation.
pub(crate) enum RawToken {
    // ── Single-character tokens ──────────────────────────────────
    #[token("(")]
    LeftParen,
    #[token(")")]
    RightParen,
    #[token("{")]
    LeftBrace,
    #[token("}")]
    RightBrace,
    #[token("[")]
    LeftBracket,
    #[token("]")]
    RightBracket,
    #[token(",")]
    Comma,
    #[token(".")]
    Dot,
    #[token("-")]
    Minus,
    #[token("+")]
    Plus,
    #[token(";")]
    Semicolon,
    #[token(":")]
    Colon,
    #[token("*")]
    Star,
    #[token("%")]
    Percent,
    #[token("&")]
    Ampersand,
    #[token("|")]
    Pipe,
    #[token("^")]
    Caret,
    #[token("~")]
    Tilde,
    #[token("?")]
    Question,

    // ── One or two character tokens ──────────────────────────────
    #[token("!=")]
    BangEqual,
    #[token("!")]
    Bang,
    #[token("==")]
    EqualEqual,
    #[token("=>")]
    Arrow,
    #[token("=")]
    Equal,
    #[token(">=")]
    GreaterEqual,
    #[token(">>")]
    GreaterGreater,
    #[token("|>")]
    PipeGreater,
    #[token(">")]
    Greater,
    #[token("<=")]
    LessEqual,
    #[token("<<")]
    LessLess,
    #[token("<|")]
    LessPipe,
    #[token("<")]
    Less,
    #[token("++")]
    PlusPlus,
    #[token("--")]
    MinusMinus,
    #[token("&&")]
    AndSym,
    #[token("||")]
    OrSym,
    #[token("->")]
    ReturnArrow,
    #[token("...")]
    DotDotDot,
    #[token("..")]
    DotDot,
    #[token("?.")]
    QuestionDot,
    #[token("@")]
    At,

    #[token("/")]
    Slash,

    // ── Newline (significant for ASI) ────────────────────────────
    #[regex(r"\n")]
    Newline,

    // ── Number literals ──────────────────────────────────────────
    #[regex(r"[0-9]+(\.[0-9]+)?")]
    NumberLiteral,

    // ── String literals (both " and ' delimiters) ────────────────
    // Regexes match the full literal including quotes; stripping and
    // interpolation are handled by the public Lexer.
    #[regex(r#""([^"\\]|\\.)*""#)]
    StringLiteral,
    #[regex(r#"'([^'\\]|\\.)*'"#)]
    SingleQuoteStringLiteral,

    // ── Raw string literals r"..." and r'...' ────────────────────
    #[regex(r#"r"([^"\\]|\\.)*""#)]
    RawStringLiteral,
    #[regex(r#"r'([^'\\]|\\.)*'"#)]
    RawSingleQuoteStringLiteral,

    // ── Identifiers and keywords ─────────────────────────────────
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*", priority = 0)]
    Identifier,

    // ── Keywords (higher priority than plain identifiers) ────────
    #[token("and")]
    And,
    #[token("class")]
    Class,
    #[token("elif")]
    Elif,
    #[token("else")]
    Else,
    #[token("false")]
    False,
    #[token("for")]
    For,
    #[token("fun")]
    Fun,
    #[token("if")]
    If,
    #[token("nil")]
    Nil,
    #[token("or")]
    Or,
    #[token("say")]
    Say,
    #[token("return")]
    Return,
    #[token("static")]
    Static,
    #[token("super")]
    Super,
    #[token("this")]
    This,
    #[token("true")]
    True,
    #[token("var")]
    Var,
    #[token("while")]
    While,
    #[token("do")]
    Do,
    #[token("break")]
    Break,
    #[token("continue")]
    Continue,
    #[token("match")]
    Match,
    #[token("case")]
    Case,
    #[token("default")]
    Default,
    #[token("try")]
    Try,
    #[token("catch")]
    Catch,
    #[token("finally")]
    Finally,
    #[token("throw")]
    Throw,
    #[token("retry")]
    Retry,
    #[token("unsafe")]
    Unsafe,
    #[token("quiet")]
    Quiet,
    #[token("enum")]
    Enum,
    #[token("type")]
    Type,
    #[token("trait")]
    Trait,
    #[token("impl")]
    Impl,
    #[token("in")]
    In,
    #[token("use")]
    Use,
    #[token("from")]
    From,
    #[token("as")]
    As,
    #[token("test")]
    Test,
    #[token("async")]
    Async,
    #[token("await")]
    Await,
    #[token("extern")]
    Extern,

    // `view`/`mut`/`shared`/`copy` — ownership keywords: borrow, exclusive
    // borrow, refcounted, deep copy.
    #[token("view")]
    View,

    #[token("mut")]
    Mut,

    #[token("shared")]
    Shared,

    #[token("copy")]
    Copy,
    #[token("own")]
    Own,

    // `go`/`chan`/`close` — virtual-task concurrency vocabulary.
    #[token("go")]
    Go,

    #[token("chan")]
    Chan,

    #[token("close")]
    Close,

    // ── Type annotation keywords ─────────────────────────────────
    #[token("int")]
    TypeInt,
    #[token("float")]
    TypeFloat,
    #[token("string")]
    TypeString,
    #[token("bool")]
    TypeBool,
    #[token("array")]
    TypeArray,
    #[token("object")]
    TypeObject,
    #[token("option")]
    TypeOption,
    #[token("result")]
    TypeResult,
    #[token("any")]
    TypeAny,
    #[token("pointer")]
    TypePointer,
    #[token("slice")]
    TypeSlice,
}

#[cfg(test)]
mod tests {
    use super::*;
    use logos::Logos;

    fn token_kinds(source: &str) -> Vec<RawToken> {
        RawToken::lexer(source)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn single_char_tokens() {
        assert_eq!(
            token_kinds("( ) { } [ ] , . ; :"),
            vec![
                RawToken::LeftParen,
                RawToken::RightParen,
                RawToken::LeftBrace,
                RawToken::RightBrace,
                RawToken::LeftBracket,
                RawToken::RightBracket,
                RawToken::Comma,
                RawToken::Dot,
                RawToken::Semicolon,
                RawToken::Colon,
            ]
        );
    }

    #[test]
    fn operators() {
        assert_eq!(
            token_kinds("+ - * / % ++ -- && || ! != == = => -> ... ?. > >= >> < <= <<"),
            vec![
                RawToken::Plus,
                RawToken::Minus,
                RawToken::Star,
                RawToken::Slash,
                RawToken::Percent,
                RawToken::PlusPlus,
                RawToken::MinusMinus,
                RawToken::AndSym,
                RawToken::OrSym,
                RawToken::Bang,
                RawToken::BangEqual,
                RawToken::EqualEqual,
                RawToken::Equal,
                RawToken::Arrow,
                RawToken::ReturnArrow,
                RawToken::DotDotDot,
                RawToken::QuestionDot,
                RawToken::Greater,
                RawToken::GreaterEqual,
                RawToken::GreaterGreater,
                RawToken::Less,
                RawToken::LessEqual,
                RawToken::LessLess,
            ]
        );
    }

    #[test]
    fn keywords() {
        assert_eq!(
            token_kinds(
                "and class elif else false for fun if nil or say return static super this \
                 true var while do break continue match case default try catch finally throw \
                 retry unsafe enum in use from as view mut copy"
            ),
            vec![
                RawToken::And,
                RawToken::Class,
                RawToken::Elif,
                RawToken::Else,
                RawToken::False,
                RawToken::For,
                RawToken::Fun,
                RawToken::If,
                RawToken::Nil,
                RawToken::Or,
                RawToken::Say,
                RawToken::Return,
                RawToken::Static,
                RawToken::Super,
                RawToken::This,
                RawToken::True,
                RawToken::Var,
                RawToken::While,
                RawToken::Do,
                RawToken::Break,
                RawToken::Continue,
                RawToken::Match,
                RawToken::Case,
                RawToken::Default,
                RawToken::Try,
                RawToken::Catch,
                RawToken::Finally,
                RawToken::Throw,
                RawToken::Retry,
                RawToken::Unsafe,
                RawToken::Enum,
                RawToken::In,
                RawToken::Use,
                RawToken::From,
                RawToken::As,
                RawToken::View,
                RawToken::Mut,
                RawToken::Copy,
            ]
        );
    }

    #[test]
    fn type_keywords() {
        assert_eq!(
            token_kinds("int float string bool array object any"),
            vec![
                RawToken::TypeInt,
                RawToken::TypeFloat,
                RawToken::TypeString,
                RawToken::TypeBool,
                RawToken::TypeArray,
                RawToken::TypeObject,
                RawToken::TypeAny,
            ]
        );
    }

    #[test]
    fn string_literals() {
        assert_eq!(
            token_kinds(r#""hello world" 'single'"#),
            vec![RawToken::StringLiteral, RawToken::SingleQuoteStringLiteral,]
        );
    }

    #[test]
    fn number_literals() {
        assert_eq!(
            token_kinds("42 3.14 0"),
            vec![
                RawToken::NumberLiteral,
                RawToken::NumberLiteral,
                RawToken::NumberLiteral,
            ]
        );
    }

    #[test]
    fn identifiers() {
        assert_eq!(
            token_kinds("foo _bar baz123"),
            vec![
                RawToken::Identifier,
                RawToken::Identifier,
                RawToken::Identifier,
            ]
        );
    }

    #[test]
    fn comments_skipped() {
        assert_eq!(
            token_kinds("// this is a comment\n42"),
            vec![RawToken::Newline, RawToken::NumberLiteral,]
        );
    }

    #[test]
    fn hello_world() {
        assert_eq!(
            token_kinds(r#"say("Hello, World!");"#),
            vec![
                RawToken::Say,
                RawToken::LeftParen,
                RawToken::StringLiteral,
                RawToken::RightParen,
                RawToken::Semicolon,
            ]
        );
    }
}
