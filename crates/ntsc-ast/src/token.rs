use crate::span::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Comma,
    Dot,
    Minus,
    Plus,
    Semicolon,
    Slash,
    Star,
    Colon,
    Percent,
    Ampersand,
    Pipe,
    Caret,
    Tilde,
    Question,

    Bang,
    BangEqual,
    Equal,
    EqualEqual,
    Greater,
    GreaterEqual,
    GreaterGreater,
    Less,
    LessEqual,
    LessLess,
    PlusPlus,
    MinusMinus,

    /// `|>` — channel receive (`value |> chan`).
    PipeGreater,
    /// `<|` — channel send (`chan <| value`).
    LessPipe,

    AndSym,

    OrSym,

    Arrow,

    ReturnArrow,

    DotDotDot,

    DotDot,

    QuestionDot,

    Identifier(String),
    StringLiteral(String),

    /// A text segment of an interpolated string (before/after `${}`).
    StringSegment(String),

    /// Number literal kept as text so formatting round-trips losslessly.
    NumberLiteral(String),

    /// Newline — significant for automatic semicolon insertion (ASI).
    Newline,

    And,
    Class,
    Else,
    Elif,
    False,
    Fun,
    For,
    If,
    Nil,
    Or,
    Say,
    Return,
    Static,
    Super,
    This,
    True,
    Var,
    While,
    Do,
    Break,
    Continue,
    Match,
    Case,
    Default,
    Try,
    Catch,
    Finally,
    Throw,
    Retry,

    Unsafe,

    Quiet,
    Enum,
    Type,
    Trait,
    Impl,
    In,
    Use,
    From,
    As,
    Test,
    Async,
    Await,
    Extern,

    View,

    Mut,

    Shared,

    Copy,
    Own,

    Go,
    Chan,
    Close,

    TypeInt,
    TypeFloat,
    TypeString,
    TypeBool,
    TypeArray,
    TypeObject,

    TypeOption,
    TypeResult,
    TypeAny,

    TypePointer,

    TypeSlice,

    At,

    Eof,
}

impl TokenKind {
    pub fn is_type_keyword(&self) -> bool {
        matches!(
            self,
            TokenKind::TypeInt
                | TokenKind::TypeFloat
                | TokenKind::TypeString
                | TokenKind::TypeBool
                | TokenKind::TypeArray
                | TokenKind::TypeObject
                | TokenKind::TypeOption
                | TokenKind::TypeResult
                | TokenKind::TypeAny
                | TokenKind::TypePointer
                | TokenKind::TypeSlice
        )
    }

    /// Source text of a keyword token, if any. The parser uses this to accept
    /// keywords as property names (`random.int`) where the lexer cannot know
    /// whether the word is used as an identifier.
    pub fn keyword_lexeme(&self) -> Option<&'static str> {
        match self {
            TokenKind::And => Some("and"),
            TokenKind::Class => Some("class"),
            TokenKind::Else => Some("else"),
            TokenKind::Elif => Some("elif"),
            TokenKind::False => Some("false"),
            TokenKind::Fun => Some("fun"),
            TokenKind::For => Some("for"),
            TokenKind::If => Some("if"),
            TokenKind::Nil => Some("nil"),
            TokenKind::Or => Some("or"),
            TokenKind::Say => Some("say"),
            TokenKind::Return => Some("return"),
            TokenKind::Static => Some("static"),
            TokenKind::Super => Some("super"),
            TokenKind::This => Some("this"),
            TokenKind::True => Some("true"),
            TokenKind::Var => Some("var"),
            TokenKind::While => Some("while"),
            TokenKind::Do => Some("do"),
            TokenKind::Break => Some("break"),
            TokenKind::Continue => Some("continue"),
            TokenKind::Match => Some("match"),
            TokenKind::Case => Some("case"),
            TokenKind::Default => Some("default"),
            TokenKind::Try => Some("try"),
            TokenKind::Catch => Some("catch"),
            TokenKind::Finally => Some("finally"),
            TokenKind::Throw => Some("throw"),
            TokenKind::Retry => Some("retry"),
            TokenKind::Unsafe => Some("unsafe"),
            TokenKind::Quiet => Some("quiet"),
            TokenKind::Enum => Some("enum"),
            TokenKind::Type => Some("type"),
            TokenKind::Trait => Some("trait"),
            TokenKind::Impl => Some("impl"),
            TokenKind::In => Some("in"),
            TokenKind::Use => Some("use"),
            TokenKind::From => Some("from"),
            TokenKind::As => Some("as"),
            TokenKind::Test => Some("test"),
            TokenKind::Async => Some("async"),
            TokenKind::Await => Some("await"),
            TokenKind::Extern => Some("extern"),
            TokenKind::View => Some("view"),
            TokenKind::Mut => Some("mut"),
            TokenKind::Shared => Some("shared"),
            TokenKind::Copy => Some("copy"),
            TokenKind::Own => Some("own"),
            TokenKind::Go => Some("go"),
            TokenKind::Chan => Some("chan"),
            TokenKind::Close => Some("close"),
            TokenKind::TypeInt => Some("int"),
            TokenKind::TypeFloat => Some("float"),
            TokenKind::TypeString => Some("string"),
            TokenKind::TypeBool => Some("bool"),
            TokenKind::TypeArray => Some("array"),
            TokenKind::TypeObject => Some("object"),
            TokenKind::TypeOption => Some("option"),
            TokenKind::TypeResult => Some("result"),
            TokenKind::TypeAny => Some("any"),
            TokenKind::TypePointer => Some("pointer"),
            TokenKind::TypeSlice => Some("slice"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Source text of the token; `""` for keyword/operator/punctuation tokens.
    pub fn lexeme(&self) -> &str {
        match &self.kind {
            TokenKind::Identifier(s)
            | TokenKind::StringLiteral(s)
            | TokenKind::StringSegment(s)
            | TokenKind::NumberLiteral(s) => s,
            _ => "",
        }
    }
}
