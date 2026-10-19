mod raw;

use ntsc_ast::span::Span;
use ntsc_ast::token::{Token, TokenKind};

use raw::RawToken;

/// High-level lexer over [`Token`]s: strips quotes, splits interpolated
/// strings into segments + expression tokens, and emits `Newline` tokens for
/// automatic semicolon insertion.
pub struct Lexer<'src> {
    inner: logos::Lexer<'src, RawToken>,

    /// Queue of extra tokens from multi-token expansions (interpolated strings).
    buffer: Vec<Token>,

    offset: usize,

    line: u32,

    column: u32,
}

impl<'src> Lexer<'src> {
    pub fn new(source: &'src str) -> Self {
        Self {
            inner: logos::Lexer::new(source),
            buffer: Vec::new(),
            offset: 0,
            line: 1,
            column: 1,
        }
    }

    /// Lex the whole source; the returned vector always ends with a trailing `Eof`.
    pub fn tokenize(source: &'src str) -> Vec<Token> {
        let mut lexer = Self::new(source);
        let mut tokens: Vec<Token> = lexer.by_ref().collect();

        // Guarantee the stream always ends with `Eof` — the parser relies on it.
        if tokens.last().is_none_or(|t| t.kind != TokenKind::Eof) {
            tokens.push(Token::new(
                TokenKind::Eof,
                Span::new(lexer.offset, lexer.offset, lexer.line, lexer.column),
            ));
        }

        tokens
    }

    fn next_token(&mut self) -> Token {
        // Drain tokens buffered by multi-token expansions before the next raw token.
        if !self.buffer.is_empty() {
            return self.buffer.remove(0);
        }

        let raw = self.inner.next();
        let Some(result) = raw else {
            return Token::new(
                TokenKind::Eof,
                Span::new(self.offset, self.offset, self.line, self.column),
            );
        };

        let slice = self.inner.slice();

        let range = self.inner.span();
        // Logos skips whitespace and comments internally, so a skipped gap may
        // precede the token; advance line/column through it (block comments can
        // contain newlines) so spans stay accurate.
        if self.offset < range.start {
            self.advance_through(&self.inner.source()[self.offset..range.start]);
        }
        let start_line = self.line;
        let start_column = self.column;

        self.advance_through(slice);
        self.offset = range.end;

        let span = Span::new(range.start, range.end, start_line, start_column);

        match result {
            Ok(raw_tok) => self.convert_raw(raw_tok, span, slice),
            Err(_error) => {
                // Unknown characters become one-char identifiers so parsing can recover.
                let ch = slice.chars().next().unwrap_or('?');
                Token::new(TokenKind::Identifier(ch.to_string()), span)
            }
        }
    }

    fn convert_raw(&mut self, raw: RawToken, span: Span, slice: &str) -> Token {
        // Span bookkeeping was already done in `next_token`; this only maps token kinds.
        match raw {
            // ── Newlines ──────────────────────────────────────────
            RawToken::Newline => Token::new(TokenKind::Newline, span),

            // ── Single-char tokens ────────────────────────────────
            RawToken::LeftParen => Token::new(TokenKind::LeftParen, span),
            RawToken::RightParen => Token::new(TokenKind::RightParen, span),
            RawToken::LeftBrace => Token::new(TokenKind::LeftBrace, span),
            RawToken::RightBrace => Token::new(TokenKind::RightBrace, span),
            RawToken::LeftBracket => Token::new(TokenKind::LeftBracket, span),
            RawToken::RightBracket => Token::new(TokenKind::RightBracket, span),
            RawToken::Comma => Token::new(TokenKind::Comma, span),
            RawToken::Dot => Token::new(TokenKind::Dot, span),
            RawToken::Minus => Token::new(TokenKind::Minus, span),
            RawToken::Plus => Token::new(TokenKind::Plus, span),
            RawToken::Semicolon => Token::new(TokenKind::Semicolon, span),
            RawToken::Colon => Token::new(TokenKind::Colon, span),
            RawToken::Star => Token::new(TokenKind::Star, span),
            RawToken::Percent => Token::new(TokenKind::Percent, span),
            RawToken::Ampersand => Token::new(TokenKind::Ampersand, span),
            RawToken::Pipe => Token::new(TokenKind::Pipe, span),
            RawToken::Caret => Token::new(TokenKind::Caret, span),
            RawToken::Tilde => Token::new(TokenKind::Tilde, span),
            RawToken::Question => Token::new(TokenKind::Question, span),

            // ── Two-char / three-char tokens ──────────────────────
            RawToken::BangEqual => Token::new(TokenKind::BangEqual, span),
            RawToken::Bang => Token::new(TokenKind::Bang, span),
            RawToken::EqualEqual => Token::new(TokenKind::EqualEqual, span),
            RawToken::Arrow => Token::new(TokenKind::Arrow, span),
            RawToken::Equal => Token::new(TokenKind::Equal, span),
            RawToken::GreaterEqual => Token::new(TokenKind::GreaterEqual, span),
            RawToken::GreaterGreater => Token::new(TokenKind::GreaterGreater, span),
            RawToken::PipeGreater => Token::new(TokenKind::PipeGreater, span),
            RawToken::Greater => Token::new(TokenKind::Greater, span),
            RawToken::LessEqual => Token::new(TokenKind::LessEqual, span),
            RawToken::LessLess => Token::new(TokenKind::LessLess, span),
            RawToken::LessPipe => Token::new(TokenKind::LessPipe, span),
            RawToken::Less => Token::new(TokenKind::Less, span),
            RawToken::PlusPlus => Token::new(TokenKind::PlusPlus, span),
            RawToken::MinusMinus => Token::new(TokenKind::MinusMinus, span),
            RawToken::AndSym => Token::new(TokenKind::AndSym, span),
            RawToken::OrSym => Token::new(TokenKind::OrSym, span),
            RawToken::ReturnArrow => Token::new(TokenKind::ReturnArrow, span),
            RawToken::DotDotDot => Token::new(TokenKind::DotDotDot, span),
            RawToken::DotDot => Token::new(TokenKind::DotDot, span),
            RawToken::QuestionDot => Token::new(TokenKind::QuestionDot, span),
            RawToken::At => Token::new(TokenKind::At, span),
            RawToken::Slash => Token::new(TokenKind::Slash, span),

            // ── Number literals ───────────────────────────────────
            RawToken::NumberLiteral => {
                Token::new(TokenKind::NumberLiteral(slice.to_string()), span)
            }

            // ── String literals (strip surrounding quotes) ────────
            RawToken::StringLiteral => {
                let content = &slice[1..slice.len() - 1];
                self.emit_string_tokens(content, span)
            }
            RawToken::SingleQuoteStringLiteral => {
                let content = &slice[1..slice.len() - 1];
                self.emit_string_tokens(content, span)
            }
            RawToken::RawStringLiteral => {
                // `r"..."`: strip the leading `r` and both quotes.
                let content = &slice[2..slice.len() - 1];
                Token::new(TokenKind::StringLiteral(content.to_string()), span)
            }
            RawToken::RawSingleQuoteStringLiteral => {
                let content = &slice[2..slice.len() - 1];
                Token::new(TokenKind::StringLiteral(content.to_string()), span)
            }

            // ── Identifiers / keywords ────────────────────────────
            RawToken::Identifier => Token::new(TokenKind::Identifier(slice.to_string()), span),
            RawToken::And => Token::new(TokenKind::And, span),
            RawToken::Class => Token::new(TokenKind::Class, span),
            RawToken::Elif => Token::new(TokenKind::Elif, span),
            RawToken::Else => Token::new(TokenKind::Else, span),
            RawToken::False => Token::new(TokenKind::False, span),
            RawToken::For => Token::new(TokenKind::For, span),
            RawToken::Fun => Token::new(TokenKind::Fun, span),
            RawToken::If => Token::new(TokenKind::If, span),
            RawToken::Nil => Token::new(TokenKind::Nil, span),
            RawToken::Or => Token::new(TokenKind::Or, span),
            RawToken::Say => Token::new(TokenKind::Say, span),
            RawToken::Return => Token::new(TokenKind::Return, span),
            RawToken::Static => Token::new(TokenKind::Static, span),
            RawToken::Super => Token::new(TokenKind::Super, span),
            RawToken::This => Token::new(TokenKind::This, span),
            RawToken::True => Token::new(TokenKind::True, span),
            RawToken::Var => Token::new(TokenKind::Var, span),
            RawToken::While => Token::new(TokenKind::While, span),
            RawToken::Do => Token::new(TokenKind::Do, span),
            RawToken::Break => Token::new(TokenKind::Break, span),
            RawToken::Continue => Token::new(TokenKind::Continue, span),
            RawToken::Match => Token::new(TokenKind::Match, span),
            RawToken::Case => Token::new(TokenKind::Case, span),
            RawToken::Default => Token::new(TokenKind::Default, span),
            RawToken::Try => Token::new(TokenKind::Try, span),
            RawToken::Catch => Token::new(TokenKind::Catch, span),
            RawToken::Finally => Token::new(TokenKind::Finally, span),
            RawToken::Throw => Token::new(TokenKind::Throw, span),
            RawToken::Retry => Token::new(TokenKind::Retry, span),
            RawToken::Unsafe => Token::new(TokenKind::Unsafe, span),
            RawToken::Quiet => Token::new(TokenKind::Quiet, span),
            RawToken::Enum => Token::new(TokenKind::Enum, span),
            RawToken::Type => Token::new(TokenKind::Type, span),
            RawToken::Trait => Token::new(TokenKind::Trait, span),
            RawToken::Impl => Token::new(TokenKind::Impl, span),
            RawToken::In => Token::new(TokenKind::In, span),
            RawToken::Use => Token::new(TokenKind::Use, span),
            RawToken::From => Token::new(TokenKind::From, span),
            RawToken::As => Token::new(TokenKind::As, span),
            RawToken::Test => Token::new(TokenKind::Test, span),
            RawToken::Async => Token::new(TokenKind::Async, span),
            RawToken::Await => Token::new(TokenKind::Await, span),
            RawToken::Extern => Token::new(TokenKind::Extern, span),
            RawToken::View => Token::new(TokenKind::View, span),
            RawToken::Mut => Token::new(TokenKind::Mut, span),
            RawToken::Shared => Token::new(TokenKind::Shared, span),
            RawToken::Copy => Token::new(TokenKind::Copy, span),
            RawToken::Own => Token::new(TokenKind::Own, span),
            RawToken::Go => Token::new(TokenKind::Go, span),
            RawToken::Chan => Token::new(TokenKind::Chan, span),
            RawToken::Close => Token::new(TokenKind::Close, span),
            RawToken::TypeInt => Token::new(TokenKind::TypeInt, span),
            RawToken::TypeFloat => Token::new(TokenKind::TypeFloat, span),
            RawToken::TypeString => Token::new(TokenKind::TypeString, span),
            RawToken::TypeBool => Token::new(TokenKind::TypeBool, span),
            RawToken::TypeArray => Token::new(TokenKind::TypeArray, span),
            RawToken::TypeObject => Token::new(TokenKind::TypeObject, span),
            RawToken::TypeOption => Token::new(TokenKind::TypeOption, span),
            RawToken::TypeResult => Token::new(TokenKind::TypeResult, span),
            RawToken::TypeAny => Token::new(TokenKind::TypeAny, span),
            RawToken::TypePointer => Token::new(TokenKind::TypePointer, span),
            RawToken::TypeSlice => Token::new(TokenKind::TypeSlice, span),
        }
    }

    fn emit_string_tokens(&mut self, content: &str, span: Span) -> Token {
        let parts = split_string_body(content);

        if parts.is_empty() {
            return Token::new(TokenKind::StringLiteral(String::new()), span);
        }

        if let [InterpPart::Text(only)] = &parts[..] {
            return Token::new(TokenKind::StringLiteral(only.clone()), span);
        }

        let mut result_tokens: Vec<Token> = Vec::new();
        for part in &parts {
            match part {
                InterpPart::Text(text) => {
                    result_tokens.push(Token::new(TokenKind::StringSegment(text.clone()), span));
                }
                InterpPart::Expression(expr_source) => {
                    // Lex each interpolation's source text as a separate mini-program.
                    let expr_tokens = Lexer::tokenize(expr_source);
                    for tok in expr_tokens {
                        if tok.kind != TokenKind::Eof {
                            result_tokens.push(tok);
                        }
                    }
                }
            }
        }

        if result_tokens.is_empty() {
            return Token::new(TokenKind::StringLiteral(String::new()), span);
        }

        // A string that opens with an interpolation (`"${n}"`) must still start
        // with a StringSegment so the parser recognizes it as an interpolated
        // string rather than a bare expression.
        if !matches!(
            result_tokens.first().map(|token| &token.kind),
            Some(TokenKind::StringSegment(_))
        ) {
            result_tokens.insert(0, Token::new(TokenKind::StringSegment(String::new()), span));
        }

        let first = result_tokens.remove(0);
        self.buffer.clear();
        for tok in result_tokens {
            self.buffer.push(tok);
        }

        first
    }

    fn advance_through(&mut self, slice: &str) {
        for ch in slice.chars() {
            if ch == '\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }
        self.offset += slice.len();
    }
}

impl<'src> Iterator for Lexer<'src> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        let tok = self.next_token();
        if tok.kind == TokenKind::Eof {
            None
        } else {
            Some(tok)
        }
    }
}

// ── String interpolation helpers ────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum InterpPart {
    Text(String),

    /// Source text of an interpolated expression, without the `${}` wrappers.
    Expression(String),
}

/// Split a string-literal body into literal text and interpolation parts,
/// processing escape sequences in the literal parts. `${...}` boundaries are
/// only honored when the `$` was not escaped, so `\${` yields a literal
/// `${` instead of starting an interpolation. Recognized escapes are
/// `\n`, `\t`, `\r`, `\0`, `\\`, `\"`, `\'`, and `\$`; any other `\x` stays
/// as the two characters it is written as.
fn split_string_body(content: &str) -> Vec<InterpPart> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = content.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        match ch {
            '\\' => match chars.peek().copied() {
                Some((_, escape)) => {
                    chars.next();
                    match escape {
                        'n' => text.push('\n'),
                        't' => text.push('\t'),
                        'r' => text.push('\r'),
                        '0' => text.push('\0'),
                        '\\' => text.push('\\'),
                        '"' => text.push('"'),
                        '\'' => text.push('\''),
                        '$' => text.push('$'),
                        other => {
                            text.push('\\');
                            text.push(other);
                        }
                    }
                }
                None => text.push('\\'),
            },
            '$' if matches!(chars.peek(), Some((_, '{'))) => {
                if !text.is_empty() {
                    parts.push(InterpPart::Text(std::mem::take(&mut text)));
                }
                let after_open = i + 2;
                let mut depth = 1u32;
                let mut close_pos = None;
                for (j, c) in content[after_open..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                close_pos = Some(after_open + j);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                match close_pos {
                    Some(pos) => {
                        parts.push(InterpPart::Expression(content[after_open..pos].to_string()));
                        for (j, _) in chars.by_ref() {
                            if j >= pos {
                                break;
                            }
                        }
                    }
                    None => {
                        parts.push(InterpPart::Expression(content[after_open..].to_string()));
                        chars.by_ref().for_each(|_| {});
                    }
                }
            }
            other => text.push(other),
        }
    }

    if !text.is_empty() {
        parts.push(InterpPart::Text(text));
    }

    parts
}

// ── Public convenience function ─────────────────────────────────────────

/// Tokenize `source` into a vector of tokens, always ending with `Eof`.
pub fn tokenize(source: &str) -> Vec<Token> {
    Lexer::tokenize(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source)
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| *k != TokenKind::Eof)
            .collect()
    }

    fn strip_newlines(kinds: Vec<TokenKind>) -> Vec<TokenKind> {
        kinds
            .into_iter()
            .filter(|k| *k != TokenKind::Newline)
            .collect()
    }

    #[test]
    fn hello_world() {
        let kinds = strip_newlines(token_kinds(r#"say("Hello, World!");"#));
        assert_eq!(
            kinds,
            vec![
                TokenKind::Say,
                TokenKind::LeftParen,
                TokenKind::StringLiteral("Hello, World!".to_string()),
                TokenKind::RightParen,
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn no_semicolons_needed() {
        let kinds = strip_newlines(token_kinds("say(\"hi\")\nsay(\"bye\")"));
        assert_eq!(
            kinds,
            vec![
                TokenKind::Say,
                TokenKind::LeftParen,
                TokenKind::StringLiteral("hi".to_string()),
                TokenKind::RightParen,
                TokenKind::Say,
                TokenKind::LeftParen,
                TokenKind::StringLiteral("bye".to_string()),
                TokenKind::RightParen,
            ]
        );
    }

    #[test]
    fn variable_declaration() {
        let kinds = strip_newlines(token_kinds("var x = 42"));
        assert_eq!(
            kinds,
            vec![
                TokenKind::Var,
                TokenKind::Identifier("x".to_string()),
                TokenKind::Equal,
                TokenKind::NumberLiteral("42".to_string()),
            ]
        );
    }

    #[test]
    fn radix_number_literal_lexeme() {
        let kinds = strip_newlines(token_kinds("var x = 0xff"));
        assert_eq!(
            kinds,
            vec![
                TokenKind::Var,
                TokenKind::Identifier("x".to_string()),
                TokenKind::Equal,
                TokenKind::NumberLiteral("0xff".to_string()),
            ]
        );
    }

    #[test]
    fn typed_variable() {
        let kinds = strip_newlines(token_kinds("var int x = 42"));
        assert_eq!(
            kinds,
            vec![
                TokenKind::Var,
                TokenKind::TypeInt,
                TokenKind::Identifier("x".to_string()),
                TokenKind::Equal,
                TokenKind::NumberLiteral("42".to_string()),
            ]
        );
    }

    #[test]
    fn function_def() {
        let source = "fun add(int a, int b) -> int {\n    return a + b\n}";
        let kinds = strip_newlines(token_kinds(source));
        assert_eq!(
            kinds,
            vec![
                TokenKind::Fun,
                TokenKind::Identifier("add".to_string()),
                TokenKind::LeftParen,
                TokenKind::TypeInt,
                TokenKind::Identifier("a".to_string()),
                TokenKind::Comma,
                TokenKind::TypeInt,
                TokenKind::Identifier("b".to_string()),
                TokenKind::RightParen,
                TokenKind::ReturnArrow,
                TokenKind::TypeInt,
                TokenKind::LeftBrace,
                TokenKind::Return,
                TokenKind::Identifier("a".to_string()),
                TokenKind::Plus,
                TokenKind::Identifier("b".to_string()),
                TokenKind::RightBrace,
            ]
        );
    }

    #[test]
    fn string_interpolation_simple() {
        let kinds = strip_newlines(token_kinds(r#""Hello, ${name}!""#));
        assert!(kinds.contains(&TokenKind::Identifier("name".to_string())));
        let seg_count = kinds
            .iter()
            .filter(|k| matches!(k, TokenKind::StringSegment(_)))
            .count();
        assert!(
            seg_count >= 2,
            "Expected at least 2 StringSegment tokens, got {seg_count}"
        );
    }

    #[test]
    fn string_interpolation_leading_expression_opens_with_segment() {
        let kinds = strip_newlines(token_kinds(r#""${n}""#));
        assert!(
            matches!(kinds.first(), Some(TokenKind::StringSegment(_))),
            "interpolated string must open with a StringSegment, got {kinds:?}"
        );

        let kinds = strip_newlines(token_kinds(r#""${n}!""#));
        assert!(
            matches!(kinds.first(), Some(TokenKind::StringSegment(_))),
            "interpolated string must open with a StringSegment, got {kinds:?}"
        );
    }

    #[test]
    fn class_declaration() {
        let source = "class Animal {\n    var name\n}";
        let kinds = strip_newlines(token_kinds(source));
        assert_eq!(
            kinds,
            vec![
                TokenKind::Class,
                TokenKind::Identifier("Animal".to_string()),
                TokenKind::LeftBrace,
                TokenKind::Var,
                TokenKind::Identifier("name".to_string()),
                TokenKind::RightBrace,
            ]
        );
    }

    #[test]
    fn match_statement() {
        let source = "match (x) {\n    case 1 => say(\"one\")\n    default => say(\"other\")\n}";
        let kinds = strip_newlines(token_kinds(source));
        assert!(kinds.contains(&TokenKind::Match));
        assert!(kinds.contains(&TokenKind::Case));
        assert!(kinds.contains(&TokenKind::Arrow));
        assert!(kinds.contains(&TokenKind::Default));
    }

    #[test]
    fn operators_and_precedence_tokens() {
        let kinds = strip_newlines(token_kinds("2 + 3 * 4"));
        assert_eq!(
            kinds,
            vec![
                TokenKind::NumberLiteral("2".to_string()),
                TokenKind::Plus,
                TokenKind::NumberLiteral("3".to_string()),
                TokenKind::Star,
                TokenKind::NumberLiteral("4".to_string()),
            ]
        );
    }

    #[test]
    fn string_interpolation_nested_dot() {
        let kinds = strip_newlines(token_kinds(r#""val: ${obj.key}""#));
        assert!(kinds.contains(&TokenKind::Identifier("obj".to_string())));
        assert!(kinds.contains(&TokenKind::Dot));
        assert!(kinds.contains(&TokenKind::Identifier("key".to_string())));
    }

    #[test]
    fn escape_sequences() {
        // `\n` becomes a real newline in the produced token.
        let kinds = strip_newlines(token_kinds(r#""line1\nline2""#));
        assert_eq!(
            kinds,
            vec![TokenKind::StringLiteral("line1\nline2".to_string())]
        );
    }

    #[test]
    fn escape_sequences_converted() {
        assert_eq!(
            token_kinds(r#""tab\there\r\n\0""#),
            vec![TokenKind::StringLiteral("tab\there\r\n\0".to_string())]
        );
    }

    #[test]
    fn escaped_quote_keeps_literal_open() {
        // `\"` does not terminate the literal and becomes a plain quote.
        assert_eq!(
            token_kinds(r#""say \"hi\" now""#),
            vec![TokenKind::StringLiteral("say \"hi\" now".to_string())]
        );
    }

    #[test]
    fn escaped_backslash_and_unknown_escape_verbatim() {
        assert_eq!(
            token_kinds(r#""path\\end\q""#),
            vec![TokenKind::StringLiteral("path\\end\\q".to_string())]
        );
    }

    #[test]
    fn escaped_dollar_suppresses_interpolation() {
        // `\${` is literal text, not an interpolation opening.
        assert_eq!(
            token_kinds(r#""price is \${5} today""#),
            vec![TokenKind::StringLiteral("price is ${5} today".to_string())]
        );
    }

    #[test]
    fn escape_in_interpolated_segment() {
        let kinds = token_kinds(r#""a${n}\tb""#);
        assert!(kinds.contains(&TokenKind::StringSegment("a".to_string())));
        assert!(kinds.contains(&TokenKind::StringSegment("\tb".to_string())));
    }
}
