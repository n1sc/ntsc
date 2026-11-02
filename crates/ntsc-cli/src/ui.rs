use std::env;
use std::io::{self, IsTerminal};
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, Padding, Paragraph, Widget, Wrap,
};
use ratatui::{DefaultTerminal, Frame};

use crate::{COMMANDS, CommandDoc, NTSC_VERSION};

pub(crate) mod docs;

const BG: Color = Color::Rgb(11, 13, 21);
const PANEL: Color = Color::Rgb(17, 20, 32);
const HIGHLIGHT: Color = Color::Rgb(31, 36, 54);
const BORDER: Color = Color::Rgb(58, 66, 94);
const TEXT: Color = Color::Rgb(226, 232, 240);
const MUTED: Color = Color::Rgb(122, 134, 160);
const ACCENT: Color = Color::Rgb(94, 234, 212);
const ACCENT2: Color = Color::Rgb(192, 132, 252);
const ERROR: Color = Color::Rgb(248, 113, 113);
const ERROR_TEXT: Color = Color::Rgb(254, 202, 202);

const LOGO: [&str; 6] = [
    "███╗   ██╗████████╗███████╗ ██████╗",
    "████╗  ██║╚══██╔══╝██╔════╝██╔════╝",
    "██╔██╗ ██║   ██║   ███████╗██║     ",
    "██║╚██╗██║   ██║   ╚════██║██║     ",
    "██║ ╚████║   ██║   ███████║╚██████╗",
    "╚═╝  ╚═══╝   ╚═╝   ╚══════╝ ╚═════╝",
];

const LOGO_COLOURS: [Color; 6] = [
    Color::Rgb(94, 234, 212),
    Color::Rgb(125, 211, 252),
    Color::Rgb(147, 197, 253),
    Color::Rgb(167, 139, 250),
    Color::Rgb(192, 132, 252),
    Color::Rgb(244, 114, 182),
];

/// Whether the terminal can host the interactive screen and the error panel.
pub(crate) fn interactive_available() -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && io::stderr().is_terminal()
        && !no_colour()
}

fn no_colour() -> bool {
    env::var_os("NO_COLOR").is_some()
        || env::var("CLICOLOR")
            .map(|value| value == "0")
            .unwrap_or(false)
        || env::var("TERM")
            .map(|value| value == "dumb")
            .unwrap_or(false)
}

/// Run the interactive landing screen: ASCII art, command list, and a
/// detail pane for the selected command. Returns when the user quits.
pub(crate) fn run_landing() -> io::Result<()> {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            return Err(error);
        }
    };
    let result = landing_loop(&mut terminal);
    ratatui::restore();
    result
}

fn landing_loop(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut state = ListState::default().with_selected(Some(0));
    loop {
        terminal.draw(|frame| draw_landing(frame, &mut state))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => move_selection(&mut state, 1),
            KeyCode::Up | KeyCode::Char('k') => move_selection(&mut state, -1),
            KeyCode::Home | KeyCode::Char('g') => state.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') => {
                state.select(Some(COMMANDS.len().saturating_sub(1)))
            }
            _ => {}
        }
    }
}

fn move_selection(state: &mut ListState, delta: isize) {
    if COMMANDS.is_empty() {
        return;
    }
    let length = COMMANDS.len() as isize;
    let current = state.selected().unwrap_or(0) as isize;
    state.select(Some((current + delta).rem_euclid(length) as usize));
}

fn draw_landing(frame: &mut Frame, state: &mut ListState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);

    if area.width < 44 || area.height < 14 {
        draw_compact(frame, area, state);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Min(7),
            Constraint::Length(1),
        ])
        .split(area);

    draw_logo(frame, rows[0]);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(26), Constraint::Min(24)])
        .split(rows[1]);

    draw_commands(frame, columns[0], state);
    draw_details(frame, columns[1], state);
    draw_footer(frame, rows[2]);
}

fn draw_logo(frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = LOGO
        .iter()
        .enumerate()
        .map(|(index, row)| {
            Line::from(Span::styled(
                *row,
                Style::default()
                    .fg(LOGO_COLOURS[index % LOGO_COLOURS.len()])
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();

    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("✦ ", Style::default().fg(ACCENT2)),
        Span::styled(
            "Neutron Type-Safe Compiler",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  v{NTSC_VERSION}"), Style::default().fg(MUTED)),
    ]));

    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

fn draw_commands(frame: &mut Frame, area: Rect, state: &mut ListState) {
    let items: Vec<ListItem> = COMMANDS
        .iter()
        .map(|doc| {
            ListItem::new(Line::from(Span::styled(
                format!(" {} ", doc.name),
                Style::default().fg(TEXT),
            )))
        })
        .collect();

    let list = List::new(items)
        .block(panel("Commands"))
        .highlight_style(
            Style::default()
                .bg(HIGHLIGHT)
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    frame.render_stateful_widget(list, area, state);
}

fn draw_details(frame: &mut Frame, area: Rect, state: &ListState) {
    let Some(index) = state.selected() else {
        return;
    };
    let doc: &CommandDoc = &COMMANDS[index];

    let mut lines = vec![
        Line::from(Span::styled(
            doc.summary,
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("usage  ", Style::default().fg(MUTED)),
            Span::styled(doc.usage, Style::default().fg(ACCENT)),
        ]),
    ];

    if !doc.details.is_empty() {
        lines.push(Line::default());
        for detail in doc.details.iter().copied() {
            lines.push(Line::from(vec![
                Span::styled("• ", Style::default().fg(ACCENT2)),
                Span::styled(detail, Style::default().fg(TEXT)),
            ]));
        }
    }

    if !doc.options.is_empty() {
        lines.push(Line::default());
        for (flag, description) in doc.options.iter().copied() {
            lines.push(Line::from(vec![
                Span::styled(format!("{flag:<18}"), Style::default().fg(ACCENT)),
                Span::styled(description, Style::default().fg(MUTED)),
            ]));
        }
    }

    let title = Line::from(vec![
        Span::styled(" ✦ ", Style::default().fg(ACCENT2)),
        Span::styled(
            doc.name,
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default()),
    ]);

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel(title))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        key(" ↑↓ "),
        Span::styled(" move  ", Style::default().fg(MUTED)),
        key(" q "),
        Span::styled(" quit  ", Style::default().fg(MUTED)),
        Span::styled("run ", Style::default().fg(MUTED)),
        Span::styled("ntsc <command>", Style::default().fg(ACCENT)),
    ]);
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

fn key(label: &'static str) -> Span<'static> {
    Span::styled(
        label,
        Style::default()
            .fg(BG)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
    )
}

fn draw_compact(frame: &mut Frame, area: Rect, state: &mut ListState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " NTSC ",
                Style::default()
                    .fg(BG)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " Neutron Type-Safe Compiler",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
        ])),
        rows[0],
    );

    draw_commands(frame, rows[1], state);

    frame.render_widget(
        Paragraph::new("↑↓ move · q quit")
            .alignment(Alignment::Center)
            .style(Style::default().fg(MUTED)),
        rows[2],
    );
}

fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL))
        .title(title)
        .padding(Padding::horizontal(1))
}

/// Render a plain CLI error as a styled panel. Falls back to plain text when
/// the terminal cannot host it.
pub(crate) fn report_plain_error(message: &str) {
    if interactive_available() && render_panel(message).is_ok() {
        return;
    }
    eprintln!("error: {message}");
}

fn render_panel(message: &str) -> io::Result<()> {
    let (terminal_width, _) = ratatui::crossterm::terminal::size()?;
    let width = terminal_width.saturating_sub(4).clamp(30, 76);
    let inner_width = width.saturating_sub(4).max(1) as usize;
    let wrapped: usize = message
        .lines()
        .map(|line| line.chars().count().max(1).div_ceil(inner_width))
        .sum();
    let height = (wrapped + 2).clamp(3, 20) as u16;

    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ERROR))
        .style(Style::default().bg(PANEL))
        .title(Line::from(vec![
            Span::styled(
                " ✖ ",
                Style::default().fg(ERROR).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "error",
                Style::default().fg(ERROR_TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default()),
        ]))
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);
    block.render(area, &mut buffer);

    let lines: Vec<Line> = message
        .lines()
        .map(|line| Line::from(Span::styled(line.to_string(), Style::default().fg(TEXT))))
        .collect();
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, &mut buffer);

    let rendered = buffer_to_ansi(&buffer);
    for line in rendered.lines() {
        eprintln!("  {line}");
    }
    Ok(())
}

fn buffer_to_ansi(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();

    for y in 0..area.height {
        let mut current: Option<Style> = None;
        for x in 0..area.width {
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };
            let style = cell.style();
            if current != Some(style) {
                out.push_str(&style_to_ansi(style));
                current = Some(style);
            }
            out.push_str(cell.symbol());
        }
        out.push_str("\x1b[0m");
        if y + 1 < area.height {
            out.push('\n');
        }
    }

    out
}

fn style_to_ansi(style: Style) -> String {
    let mut codes: Vec<String> = Vec::new();

    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("1".to_string());
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push("2".to_string());
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        codes.push("3".to_string());
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        codes.push("4".to_string());
    }
    if let Some(code) = style.fg.and_then(|colour| colour_code(colour, false)) {
        codes.push(code);
    }
    if let Some(code) = style.bg.and_then(|colour| colour_code(colour, true)) {
        codes.push(code);
    }

    if codes.is_empty() {
        "\x1b[0m".to_string()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

fn colour_code(colour: Color, background: bool) -> Option<String> {
    let base = if background { 48 } else { 38 };
    match colour {
        Color::Reset => None,
        Color::Rgb(red, green, blue) => Some(format!("{base};2;{red};{green};{blue}")),
        Color::Indexed(index) => Some(format!("{base};5;{index}")),
        named => {
            let code = named_code(named)?;
            if background {
                Some((code + 10).to_string())
            } else {
                Some(code.to_string())
            }
        }
    }
}

fn named_code(colour: Color) -> Option<u8> {
    match colour {
        Color::Black => Some(30),
        Color::Red => Some(31),
        Color::Green => Some(32),
        Color::Yellow => Some(33),
        Color::Blue => Some(34),
        Color::Magenta => Some(35),
        Color::Cyan => Some(36),
        Color::Gray => Some(37),
        Color::DarkGray => Some(90),
        Color::LightRed => Some(91),
        Color::LightGreen => Some(92),
        Color::LightYellow => Some(93),
        Color::LightBlue => Some(94),
        Color::LightMagenta => Some(95),
        Color::LightCyan => Some(96),
        Color::White => Some(97),
        Color::Reset | Color::Rgb(_, _, _) | Color::Indexed(_) => None,
    }
}
