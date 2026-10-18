//! Minimal document formatter for Neutron source.
//!
//! The formatter is whitespace-only: it never touches tokens. It expands
//! tabs, strips trailing whitespace, re-indents open blocks by brace depth,
//! and guarantees a single trailing newline.

/// Format `text`, returning the formatted document and the list of edits as
/// a single whole-document replacement when anything changed.
pub fn format_document(text: &str) -> Option<String> {
    let formatted = format_text(text);
    if formatted == text {
        None
    } else {
        Some(formatted)
    }
}

fn format_text(text: &str) -> String {
    let expanded = text.replace('\t', "    ");
    let mut output = String::with_capacity(expanded.len());
    let mut depth: usize = 0;
    // A pending closing brace at line start dedents before printing.
    for line in expanded.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            output.push('\n');
            continue;
        }
        let closes = leading_close_count(trimmed);
        depth = depth.saturating_sub(closes);
        output.push_str(&"    ".repeat(depth));
        output.push_str(trimmed);
        output.push('\n');
        let opens = brace_delta(trimmed);
        depth = depth.saturating_add(opens);
    }
    output
}

/// Number of leading `}` / `)` / `]` characters that dedent the line.
fn leading_close_count(trimmed: &str) -> usize {
    trimmed
        .chars()
        .take_while(|ch| matches!(ch, '}' | ')' | ']'))
        .count()
}

/// Net brace-depth change caused by the line, ignoring braces inside
/// strings and line comments.
fn brace_delta(line: &str) -> usize {
    let mut opens = 0usize;
    let mut closes = 0usize;
    let mut chars = line.chars().peekable();
    let mut in_string: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(quote) = in_string {
            if ch == '\\' {
                let _ = chars.next();
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => in_string = Some(ch),
            '/' if chars.peek() == Some(&'/') => break,
            '{' | '(' | '[' => opens += 1,
            '}' | ')' | ']' => closes += 1,
            _ => {}
        }
    }
    // The leading closers already dedented; don't double-count them, and
    // never let a line with unbalanced closers increase depth.
    let leading = leading_close_count(line);
    let closes = closes.saturating_sub(leading.min(closes));
    opens.saturating_sub(closes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indents_blocks_and_strips_trailing_space() {
        let formatted = format_text("fun main() {\nsay(\"hi\")   \n}\n");
        assert_eq!(formatted, "fun main() {\n    say(\"hi\")\n}\n");
    }

    #[test]
    fn leaves_clean_documents_untouched() {
        let text = "fun main() {\n    say(\"hi\")\n}\n";
        assert_eq!(format_document(text), None);
    }

    #[test]
    fn ignores_braces_in_strings_and_comments() {
        let text = "fun main() {\n    say(\"{ not a block }\") // }\n}\n";
        assert_eq!(format_document(text), None);
    }
}
