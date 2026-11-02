use std::io;
use std::time::Duration;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::crossterm::event::{
    self, Event as CrosstermEvent, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use super::{ACCENT, ACCENT2, BG, BORDER, HIGHLIGHT, MUTED, TEXT, key, panel};
use crate::docs::DocPage;

const CODE_BG: Color = Color::Rgb(24, 27, 42);
const CODE_STRING: Color = Color::Rgb(134, 239, 172);
const CODE_NUMBER: Color = Color::Rgb(252, 211, 77);
const CODE_KEYWORD: Color = ACCENT2;
const CODE_TYPE: Color = ACCENT;
const CODE_COMMENT: Color = MUTED;

/// Open the documentation browser at the page matching a name or path.
pub(crate) fn find_doc(docs: &[DocPage], query: &str) -> Option<usize> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let needle = query.trim_end_matches(".md").to_lowercase();
    docs.iter()
        .position(|doc| {
            doc.path
                .trim_end_matches(".md")
                .eq_ignore_ascii_case(needle.as_str())
                || doc.title.eq_ignore_ascii_case(needle.as_str())
        })
        .or_else(|| {
            docs.iter().position(|doc| {
                doc.path.to_lowercase().contains(&needle)
                    || doc.title.to_lowercase().contains(&needle)
            })
        })
}

/// Run the interactive documentation browser.
pub(crate) fn run_docs(docs: Vec<DocPage>, initial: Option<usize>) -> io::Result<()> {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            return Err(error);
        }
    };
    let result = docs_loop(&mut terminal, DocsState::new(docs, initial));
    ratatui::restore();
    result
}

struct DocsState {
    docs: Vec<DocPage>,
    selected: usize,
    filtered: Vec<usize>,
    filter: String,
    searching: bool,
    scroll: u16,
    max_scroll: u16,
}

impl DocsState {
    fn new(docs: Vec<DocPage>, initial: Option<usize>) -> Self {
        let filtered: Vec<usize> = (0..docs.len()).collect();
        let selected = initial.unwrap_or(0).min(docs.len().saturating_sub(1));
        Self {
            docs,
            selected,
            filtered,
            filter: String::new(),
            searching: false,
            scroll: 0,
            max_scroll: 0,
        }
    }

    fn current(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }

    fn refresh(&mut self) {
        let query = self.filter.to_lowercase();
        self.filtered = self
            .docs
            .iter()
            .enumerate()
            .filter(|(_, doc)| {
                query.is_empty()
                    || doc.title.to_lowercase().contains(&query)
                    || doc.path.to_lowercase().contains(&query)
                    || doc.body.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        self.scroll = 0;
    }
}

fn docs_loop(terminal: &mut DefaultTerminal, mut state: DocsState) -> io::Result<()> {
    loop {
        terminal.draw(|frame| draw_docs(frame, &mut state))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let CrosstermEvent::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if state.searching {
            match key.code {
                KeyCode::Esc => {
                    state.searching = false;
                    state.filter.clear();
                    state.refresh();
                }
                KeyCode::Enter => state.searching = false,
                KeyCode::Backspace => {
                    state.filter.pop();
                    state.refresh();
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char(character) => {
                    state.filter.push(character);
                    state.refresh();
                }
                KeyCode::Up => move_selection(&mut state, -1),
                KeyCode::Down => move_selection(&mut state, 1),
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('/') => state.searching = true,
            KeyCode::Char('r') => {
                state.docs = crate::docs::load_docs();
                state.refresh();
            }
            KeyCode::Up | KeyCode::Char('k') => move_selection(&mut state, -1),
            KeyCode::Down | KeyCode::Char('j') => move_selection(&mut state, 1),
            KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
            KeyCode::PageDown => {
                state.scroll = state.scroll.saturating_add(10).min(state.max_scroll);
            }
            KeyCode::Home | KeyCode::Char('g') => state.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => state.scroll = state.max_scroll,
            _ => {}
        }
    }
}

fn move_selection(state: &mut DocsState, delta: isize) {
    if state.filtered.is_empty() {
        return;
    }
    let last = state.filtered.len() - 1;
    let next = (state.selected as isize + delta).clamp(0, last as isize) as usize;
    if next != state.selected {
        state.selected = next;
        state.scroll = 0;
    }
}

fn draw_docs(frame: &mut Frame, state: &mut DocsState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, rows[0], state);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(30)])
        .split(rows[1]);

    draw_sidebar(frame, columns[0], state);
    draw_page(frame, columns[1], state);
    draw_footer(frame, rows[2], state);
}

fn draw_header(frame: &mut Frame, area: Rect, state: &DocsState) {
    let title = Line::from(vec![
        Span::styled(" ✦ ", Style::default().fg(ACCENT2)),
        Span::styled(
            "NTSC docs",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}/{}", state.filtered.len(), state.docs.len()),
            Style::default().fg(MUTED),
        ),
    ]);
    let hint = if state.searching {
        "typing…"
    } else {
        "/ search"
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(12)])
        .split(area);
    frame.render_widget(Paragraph::new(title), columns[0]);
    frame.render_widget(
        Paragraph::new(hint)
            .alignment(Alignment::Right)
            .style(Style::default().fg(MUTED)),
        columns[1],
    );
}

fn draw_sidebar(frame: &mut Frame, area: Rect, state: &DocsState) {
    let items: Vec<ListItem> = state
        .filtered
        .iter()
        .map(|index| {
            let doc = &state.docs[*index];
            ListItem::new(Line::from(Span::styled(
                format!(" {} ", doc.title),
                Style::default().fg(TEXT),
            )))
        })
        .collect();

    let title = if state.filter.is_empty() {
        Line::from(" Pages ")
    } else {
        Line::from(vec![
            Span::styled(" Pages ", Style::default().fg(TEXT)),
            Span::styled(format!("· {}", state.filter), Style::default().fg(ACCENT)),
        ])
    };

    let mut list_state = ListState::default().with_selected(Some(state.selected));
    let list = List::new(items)
        .block(panel(title))
        .highlight_style(
            Style::default()
                .bg(HIGHLIGHT)
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_page(frame: &mut Frame, area: Rect, state: &mut DocsState) {
    let Some(index) = state.current() else {
        frame.render_widget(
            Paragraph::new("No pages match the search.")
                .block(panel("Docs"))
                .style(Style::default().fg(MUTED)),
            area,
        );
        return;
    };
    let doc: &DocPage = &state.docs[index];
    let title = Line::from(vec![
        Span::styled(" ✦ ", Style::default().fg(ACCENT2)),
        Span::styled(
            doc.title.clone(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", doc.path), Style::default().fg(MUTED)),
    ]);

    let block = panel(title);
    let inner = block.inner(area);
    let lines = render_markdown(&doc.body, inner.width as usize);
    state.max_scroll =
        (lines.len().saturating_sub(inner.height as usize)).min(u16::MAX as usize) as u16;
    state.scroll = state.scroll.min(state.max_scroll);

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((state.scroll, 0)),
        area,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect, state: &DocsState) {
    let line = if state.searching {
        Line::from(vec![
            key(" / "),
            Span::styled(" ", Style::default()),
            Span::styled(state.filter.clone(), Style::default().fg(TEXT)),
            Span::styled("█", Style::default().fg(ACCENT)),
            Span::styled("  Enter keep · Esc clear", Style::default().fg(MUTED)),
        ])
    } else {
        Line::from(vec![
            key(" ↑↓ "),
            Span::styled(" pages  ", Style::default().fg(MUTED)),
            key(" PgUp/PgDn "),
            Span::styled(" scroll  ", Style::default().fg(MUTED)),
            key(" / "),
            Span::styled(" search  ", Style::default().fg(MUTED)),
            key(" r "),
            Span::styled(" reload  ", Style::default().fg(MUTED)),
            key(" q "),
            Span::styled(" quit", Style::default().fg(MUTED)),
        ])
    };
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

fn render_markdown(body: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut styles = vec![Style::default().fg(TEXT)];
    let mut in_code = false;
    let mut code_language = Lang::Text;
    let mut in_table_head = false;
    let mut list_depth = 0usize;
    let mut in_item = 0usize;

    for event in Parser::new_ext(body, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut lines, &mut current);
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let style = match level {
                    HeadingLevel::H1 => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    HeadingLevel::H2 => Style::default().fg(ACCENT2).add_modifier(Modifier::BOLD),
                    _ => Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                };
                styles.push(style);
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut lines, &mut current);
                lines.push(Line::default());
                styles.pop();
            }
            Event::Start(Tag::Paragraph) => flush(&mut lines, &mut current),
            Event::End(TagEnd::Paragraph) => {
                flush(&mut lines, &mut current);
                if in_item == 0 {
                    lines.push(Line::default());
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                lines.push(Line::default());
                styles.push(Style::default().fg(MUTED).add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                styles.pop();
                lines.push(Line::default());
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut lines, &mut current);
                in_code = true;
                match kind {
                    CodeBlockKind::Fenced(info) => {
                        code_language = Lang::from_info(info.as_ref());
                        if !info.is_empty() {
                            lines.push(code_line(&format!(" {info} "), width, code_language, true));
                        }
                    }
                    CodeBlockKind::Indented => code_language = Lang::Text,
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                code_language = Lang::Text;
                lines.push(Line::default());
            }
            Event::Text(text) => {
                if in_code {
                    for line in text.lines() {
                        lines.push(code_line(line, width, code_language, false));
                    }
                } else {
                    let mut style = styles.last().copied().unwrap_or_default();
                    if in_table_head {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    current.push(Span::styled(text.to_string(), style));
                }
            }
            Event::Code(text) => {
                current.push(Span::styled(
                    text.to_string(),
                    Style::default().fg(ACCENT).bg(CODE_BG),
                ));
            }
            Event::SoftBreak | Event::HardBreak => flush(&mut lines, &mut current),
            Event::Start(Tag::Strong) => {
                styles.push(
                    styles
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::BOLD),
                );
            }
            Event::End(TagEnd::Strong) => {
                styles.pop();
            }
            Event::Start(Tag::Emphasis) => {
                styles.push(
                    styles
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::ITALIC),
                );
            }
            Event::End(TagEnd::Emphasis) => {
                styles.pop();
            }
            Event::Start(Tag::Strikethrough) => {
                styles.push(
                    styles
                        .last()
                        .copied()
                        .unwrap_or_default()
                        .add_modifier(Modifier::CROSSED_OUT),
                );
            }
            Event::End(TagEnd::Strikethrough) => {
                styles.pop();
            }
            Event::Start(Tag::Link { .. }) => {
                styles.push(Style::default().fg(ACCENT));
            }
            Event::End(TagEnd::Link) => {
                styles.pop();
            }
            Event::Start(Tag::List(_)) => {
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                if list_depth == 0 {
                    lines.push(Line::default());
                }
            }
            Event::Start(Tag::Item) => {
                flush(&mut lines, &mut current);
                in_item += 1;
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                current.push(Span::styled(
                    format!("{indent}• "),
                    Style::default().fg(ACCENT2),
                ));
            }
            Event::End(TagEnd::Item) => {
                flush(&mut lines, &mut current);
                in_item = in_item.saturating_sub(1);
            }
            Event::Rule => {
                flush(&mut lines, &mut current);
                lines.push(Line::from(Span::styled(
                    "─".repeat(width.max(1)),
                    Style::default().fg(BORDER),
                )));
                lines.push(Line::default());
            }
            Event::Start(Tag::Table(_)) => {
                flush(&mut lines, &mut current);
                lines.push(Line::default());
            }
            Event::End(TagEnd::Table) => {
                flush(&mut lines, &mut current);
                lines.push(Line::default());
            }
            Event::Start(Tag::TableHead) => {
                in_table_head = true;
                flush(&mut lines, &mut current);
            }
            Event::End(TagEnd::TableHead) => {
                in_table_head = false;
                flush(&mut lines, &mut current);
                if let Some(line) = lines.last() {
                    let table_width = line_width(line);
                    if table_width > 0 {
                        lines.push(Line::from(Span::styled(
                            "─".repeat(table_width.min(64)),
                            Style::default().fg(BORDER),
                        )));
                    }
                }
            }
            Event::Start(Tag::TableRow) => flush(&mut lines, &mut current),
            Event::End(TagEnd::TableRow) => flush(&mut lines, &mut current),
            Event::Start(Tag::TableCell) => {
                if !current.is_empty() {
                    current.push(Span::styled(" │ ", Style::default().fg(BORDER)));
                }
            }
            Event::End(TagEnd::TableCell) => {}
            _ => {}
        }
    }

    flush(&mut lines, &mut current);
    lines
}

fn flush(lines: &mut Vec<Line<'static>>, current: &mut Vec<Span<'static>>) {
    if !current.is_empty() {
        lines.push(Line::from(std::mem::take(current)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lang {
    Text,
    Neutron,
    Toml,
    Shell,
    Console,
    Rust,
    PowerShell,
}

impl Lang {
    fn from_info(info: &str) -> Self {
        let name = info.split_whitespace().next().unwrap_or_default();
        match name.to_ascii_lowercase().as_str() {
            "ntsc" | "neutron" | "nt" => Self::Neutron,
            "toml" => Self::Toml,
            "sh" | "bash" | "shell" | "zsh" => Self::Shell,
            "console" | "shell-session" => Self::Console,
            "rust" | "rs" => Self::Rust,
            "powershell" | "ps1" | "pwsh" => Self::PowerShell,
            _ => Self::Text,
        }
    }

    fn badge(self) -> Color {
        match self {
            Self::Neutron => CODE_KEYWORD,
            Self::Toml => CODE_NUMBER,
            Self::Rust => Color::Rgb(251, 146, 60),
            Self::PowerShell => Color::Rgb(125, 211, 252),
            Self::Text | Self::Shell | Self::Console => ACCENT,
        }
    }
}

const NEUTRON_KEYWORDS: &[&str] = &[
    "and", "as", "async", "await", "break", "case", "catch", "class", "continue", "copy",
    "default", "do", "elif", "else", "enum", "false", "finally", "for", "from", "fun", "if", "in",
    "match", "mut", "nil", "or", "quiet", "retry", "return", "say", "shared", "static", "super",
    "test", "this", "throw", "true", "try", "unsafe", "use", "var", "view", "while",
];

const NEUTRON_TYPES: &[&str] = &[
    "any", "array", "bool", "float", "int", "object", "option", "string",
];

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

const RUST_TYPES: &[&str] = &[
    "bool", "f32", "f64", "i8", "i16", "i32", "i64", "isize", "str", "u8", "u16", "u32", "u64",
    "usize", "String", "Vec", "Option", "Result", "Box",
];

const SHELL_KEYWORDS: &[&str] = &[
    "case", "cd", "do", "done", "elif", "else", "esac", "eval", "exec", "export", "fi", "for",
    "function", "if", "local", "return", "set", "source", "then", "unset", "while",
];

const POWERSHELL_KEYWORDS: &[&str] = &[
    "begin", "catch", "else", "elseif", "end", "finally", "for", "foreach", "function", "if",
    "param", "process", "return", "switch", "throw", "try", "while",
];

const SHELL_COMMANDS: &[&str] = &[
    "arq", "cargo", "cat", "cd", "cp", "curl", "docker", "echo", "export", "git", "ls", "make",
    "mkdir", "mv", "npm", "ntsc", "python3", "rm", "rustup", "sudo",
];

fn code_line(text: &str, width: usize, lang: Lang, label: bool) -> Line<'static> {
    let text = text.replace('\t', "    ");
    let mut spans = Vec::new();
    let used;
    if label {
        used = text.chars().count();
        spans.push(Span::styled(
            text,
            Style::default()
                .fg(BG)
                .bg(lang.badge())
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled("│ ", code_style(ACCENT2)));
        let body = Line::from(highlight(&text, lang));
        used = 2 + line_width(&body);
        spans.extend(body.spans);
    }
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(used)),
        Style::default().bg(CODE_BG),
    ));
    Line::from(spans)
}

fn code_style(colour: Color) -> Style {
    Style::default().fg(colour).bg(CODE_BG)
}

fn highlight(text: &str, lang: Lang) -> Vec<Span<'static>> {
    match lang {
        Lang::Text => vec![Span::styled(text.to_string(), code_style(TEXT))],
        Lang::Console => highlight_console(text),
        _ => highlight_source(text, lang),
    }
}

fn highlight_console(text: &str) -> Vec<Span<'static>> {
    let trimmed = text.trim_start_matches(' ');
    let indent = text.len() - trimmed.len();
    let mut spans = Vec::new();
    if indent > 0 {
        spans.push(Span::styled(" ".repeat(indent), code_style(TEXT)));
    }

    let prompt = ["$ ", "> ", "❯ "]
        .iter()
        .find(|marker| trimmed.starts_with(**marker));
    if let Some(marker) = prompt {
        spans.push(Span::styled(
            (*marker).to_string(),
            code_style(CODE_TYPE).add_modifier(Modifier::BOLD),
        ));
        spans.extend(highlight_source(&trimmed[marker.len()..], Lang::Shell));
        return spans;
    }

    let first = trimmed.split_whitespace().next().unwrap_or_default();
    if SHELL_COMMANDS.contains(&first) {
        spans.extend(highlight_source(trimmed, Lang::Shell));
    } else {
        spans.push(Span::styled(trimmed.to_string(), code_style(MUTED)));
    }
    spans
}

fn highlight_source(text: &str, lang: Lang) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut index = 0;
    let mut leading = true;

    while index < chars.len() {
        let ch = chars[index];

        if let Some(marker) = comment_marker(lang)
            && starts_with(&chars, index, marker)
        {
            spans.push(Span::styled(
                slice(&chars, index, chars.len()),
                code_style(CODE_COMMENT).add_modifier(Modifier::ITALIC),
            ));
            break;
        }

        if ch == ' ' || ch == '\t' {
            let start = index;
            while index < chars.len() && (chars[index] == ' ' || chars[index] == '\t') {
                index += 1;
            }
            spans.push(Span::styled(slice(&chars, start, index), code_style(TEXT)));
            continue;
        }

        if lang == Lang::Toml && ch == '[' && leading {
            let start = index;
            while index < chars.len() && chars[index] != ']' {
                index += 1;
            }
            if index < chars.len() {
                index += 1;
            }
            spans.push(Span::styled(
                slice(&chars, start, index),
                code_style(CODE_KEYWORD).add_modifier(Modifier::BOLD),
            ));
            leading = false;
            continue;
        }

        if ch == '$' && matches!(lang, Lang::Shell | Lang::PowerShell) {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_alphanumeric()
                    || chars[index] == '_'
                    || chars[index] == ':'
                    || chars[index] == '{'
                    || chars[index] == '}')
            {
                index += 1;
            }
            spans.push(Span::styled(
                slice(&chars, start, index),
                code_style(CODE_TYPE),
            ));
            leading = false;
            continue;
        }

        if lang == Lang::Rust
            && ch == '\''
            && chars.get(index + 1).is_some_and(|c| c.is_alphabetic())
        {
            let start = index;
            index += 1;
            while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            spans.push(Span::styled(
                slice(&chars, start, index),
                code_style(CODE_TYPE),
            ));
            leading = false;
            continue;
        }

        if ch == '"' || (ch == '\'' && lang != Lang::Rust) {
            let start = index;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' && index + 1 < chars.len() {
                    index += 2;
                    continue;
                }
                if chars[index] == ch {
                    index += 1;
                    break;
                }
                index += 1;
            }
            spans.push(Span::styled(
                slice(&chars, start, index),
                code_style(CODE_STRING),
            ));
            leading = false;
            continue;
        }

        if ch.is_ascii_digit() {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric()
                    || chars[index] == '.'
                    || chars[index] == '_')
            {
                index += 1;
            }
            spans.push(Span::styled(
                slice(&chars, start, index),
                code_style(CODE_NUMBER),
            ));
            leading = false;
            continue;
        }

        if ch.is_alphabetic() || ch == '_' {
            let start = index;
            while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            let word = slice(&chars, start, index);
            let style = classify(lang, &word, leading, &chars, index);
            spans.push(Span::styled(word, style));
            leading = false;
            continue;
        }

        if ch == '-'
            && matches!(lang, Lang::Shell | Lang::Console | Lang::PowerShell)
            && chars
                .get(index + 1)
                .is_some_and(|c| c.is_alphanumeric() || *c == '-')
        {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_alphanumeric() || chars[index] == '-' || chars[index] == '_')
            {
                index += 1;
            }
            spans.push(Span::styled(
                slice(&chars, start, index),
                code_style(CODE_TYPE),
            ));
            leading = false;
            continue;
        }

        spans.push(Span::styled(ch.to_string(), code_style(TEXT)));
        index += 1;
        leading = false;
    }

    spans
}

fn classify(lang: Lang, word: &str, leading: bool, chars: &[char], index: usize) -> Style {
    if keywords(lang).contains(&word) {
        return code_style(CODE_KEYWORD).add_modifier(Modifier::BOLD);
    }
    if types(lang).contains(&word) {
        return code_style(CODE_TYPE);
    }
    if lang == Lang::Toml && next_is_assignment(chars, index) {
        return code_style(CODE_TYPE);
    }
    if lang == Lang::Shell && leading && SHELL_COMMANDS.contains(&word) {
        return code_style(ACCENT).add_modifier(Modifier::BOLD);
    }
    code_style(TEXT)
}

fn next_is_assignment(chars: &[char], mut index: usize) -> bool {
    while index < chars.len() && chars[index] == ' ' {
        index += 1;
    }
    chars.get(index) == Some(&'=')
}

fn keywords(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Neutron => NEUTRON_KEYWORDS,
        Lang::Rust => RUST_KEYWORDS,
        Lang::Shell => SHELL_KEYWORDS,
        Lang::PowerShell => POWERSHELL_KEYWORDS,
        Lang::Text | Lang::Toml | Lang::Console => &[],
    }
}

fn types(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Neutron => NEUTRON_TYPES,
        Lang::Rust => RUST_TYPES,
        _ => &[],
    }
}

fn comment_marker(lang: Lang) -> Option<&'static [char]> {
    match lang {
        Lang::Neutron | Lang::Rust => Some(&['/', '/']),
        Lang::Toml | Lang::Shell | Lang::Console | Lang::PowerShell => Some(&['#']),
        Lang::Text => None,
    }
}

fn starts_with(chars: &[char], index: usize, needle: &[char]) -> bool {
    chars.len() >= index + needle.len() && chars[index..index + needle.len()] == *needle
}

fn slice(chars: &[char], start: usize, end: usize) -> String {
    chars[start..end].iter().collect()
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_blocks_fill_the_content_width() {
        let fence = "\u{60}\u{60}\u{60}";
        let body = format!("{fence}ntsc\nlet x = 1;\n{fence}\n");
        let lines = render_markdown(&body, 40);
        let code = lines
            .iter()
            .find(|line| {
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                text.contains("let x")
            })
            .unwrap();
        assert_eq!(line_width(code), 40);
        assert!(code.spans.iter().any(|span| span.content.contains('│')));
    }

    #[test]
    fn inline_code_keeps_original_spacing() {
        let inline = "\u{60}inline\u{60}";
        let body = format!("Some bold and {inline} text.\n");
        let lines = render_markdown(&body, 40);
        let text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "Some bold and inline text.");
    }

    #[test]
    fn code_blocks_colour_keywords_and_strings() {
        let fence = "\u{60}\u{60}\u{60}";
        let body = format!("{fence}ntsc\nfun main() {{ say(\"hi\") }}\n{fence}\n");
        let lines = render_markdown(&body, 60);
        let code = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.contains("fun")))
            .unwrap();
        let keyword = code
            .spans
            .iter()
            .find(|span| span.content == "fun")
            .unwrap();
        assert_eq!(keyword.style.fg, Some(CODE_KEYWORD));
        let string = code
            .spans
            .iter()
            .find(|span| span.content == "\"hi\"")
            .unwrap();
        assert_eq!(string.style.fg, Some(CODE_STRING));
    }

    #[test]
    fn toml_sections_and_keys_are_highlighted() {
        let fence = "\u{60}\u{60}\u{60}";
        let body = format!("{fence}toml\n[package]\nentry = \"src/main.nt\"\n{fence}\n");
        let lines = render_markdown(&body, 60);
        let section = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == "[package]")
            .unwrap();
        assert_eq!(section.style.fg, Some(CODE_KEYWORD));
        let key = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == "entry")
            .unwrap();
        assert_eq!(key.style.fg, Some(CODE_TYPE));
    }
}
