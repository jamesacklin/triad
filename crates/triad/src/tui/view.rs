//! Drawing: the sidebar, the chat pane, the status line and the input box.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, Paragraph};

use super::app::{App, Line};
use super::input::width;

/// Below this many columns the sidebar is hidden.
pub const SIDEBAR_MIN_COLS: u16 = 60;
const SIDEBAR_COLS: u16 = 20;

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

/// Text from other people can hold anything; terminal control characters
/// must never reach the screen.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' => '\n',
            '\t' => ' ',
            c if c.is_control() => '\u{fffd}',
            c => c,
        })
        .collect()
}

/// Wraps text to `cols` columns: at spaces where possible, inside a word
/// when it is longer than a row. Newlines start a new row.
pub fn wrap(s: &str, cols: usize) -> Vec<String> {
    let mut rows = Vec::new();
    if cols == 0 {
        return rows;
    }
    for para in s.split('\n') {
        let mut row = String::new();
        let mut used = 0;
        for word in para.split_inclusive(' ') {
            let word_cols = width(word.trim_end_matches(' '));
            if used > 0 && used + word_cols > cols {
                rows.push(row.trim_end().to_string());
                row.clear();
                used = 0;
            }
            for c in word.chars() {
                let w = width(c.encode_utf8(&mut [0; 4]));
                if used + w > cols {
                    if c == ' ' {
                        continue;
                    }
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
                if used == 0 && c == ' ' && !row.is_empty() {
                    continue;
                }
                row.push(c);
                used += w;
            }
        }
        rows.push(row.trim_end().to_string());
    }
    rows
}

/// The rows of one pane line, wrapped to `cols`.
fn rows_of(line: &Line, cols: usize) -> Vec<TextLine<'static>> {
    match line {
        Line::Msg {
            who,
            text,
            mine,
            bot,
        } => {
            let who = clean(who);
            let full = format!("{who}: {}", clean(text));
            let who_style = if *bot {
                Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
            } else if *mine {
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
            };
            let label = format!("{who}:");
            wrap(&full, cols)
                .into_iter()
                .enumerate()
                .map(|(i, row)| {
                    if i == 0 && row.starts_with(&label) {
                        let rest = row[label.len()..].to_string();
                        TextLine::from(vec![
                            Span::styled(label.clone(), who_style),
                            Span::raw(rest),
                        ])
                    } else {
                        TextLine::from(row)
                    }
                })
                .collect()
        }
        Line::Notice(text) => wrap(&clean(text), cols)
            .into_iter()
            .map(|r| TextLine::styled(r, dim()))
            .collect(),
        Line::Error(text) => wrap(&clean(text), cols)
            .into_iter()
            .map(|r| TextLine::styled(r, Style::new().fg(Color::Red)))
            .collect(),
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 4 || area.height < 5 {
        return;
    }
    let [main, status, input] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .areas(area);
    let pane = if area.width >= SIDEBAR_MIN_COLS {
        let [side, pane] =
            Layout::horizontal([Constraint::Length(SIDEBAR_COLS), Constraint::Min(1)]).areas(main);
        draw_sidebar(frame, app, side);
        pane
    } else {
        main
    };
    draw_pane(frame, app, pane);
    draw_status(frame, app, status);
    draw_input(frame, app, input);
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<TextLine> = app
        .channels
        .iter()
        .map(|c| {
            let focused = app.focus.as_deref() == Some(c.name.as_str());
            let style = if focused {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            TextLine::styled(
                format!("{} {}", if focused { "*" } else { " " }, clean(&c.name)),
                style,
            )
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" channels ")),
        area,
    );
}

fn draw_pane(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.focused() {
        Some(c) => format!(" #{} ", clean(&c.name)),
        None => " starting… ".to_string(),
    };
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(chan) = app.focused() else { return };
    let cols = inner.width as usize;
    let height = inner.height as usize;
    let rows: Vec<TextLine> = chan.lines.iter().flat_map(|l| rows_of(l, cols)).collect();
    let start = rows.len().saturating_sub(height);
    frame.render_widget(Paragraph::new(rows[start..].to_vec()), inner);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let text = match app.focused() {
        Some(c) => format!(" #{}", clean(&c.name)),
        None => " connecting…".to_string(),
    };
    frame.render_widget(Paragraph::new(text).style(dim()), area);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 3 {
        return;
    }
    let prompt = "> ";
    let cols = inner.width as usize - prompt.len();
    if app.input.is_empty() {
        let hint = match app.focused() {
            Some(c) => format!("message #{} or /command", clean(&c.name)),
            None => "message or /command".to_string(),
        };
        frame.render_widget(
            Paragraph::new(TextLine::from(vec![
                Span::raw(prompt),
                Span::styled(hint, dim()),
            ])),
            inner,
        );
        frame.set_cursor_position(Position::new(inner.x + prompt.len() as u16, inner.y));
        return;
    }
    let (shown, cursor) = app.input.visible(cols);
    frame.render_widget(Paragraph::new(format!("{prompt}{}", clean(&shown))), inner);
    frame.set_cursor_position(Position::new(
        inner.x + (prompt.len() + cursor) as u16,
        inner.y,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces_and_inside_long_words() {
        assert_eq!(wrap("hello there world", 11), vec!["hello there", "world"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\nb", 10), vec!["a", "b"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert!(wrap("x", 0).is_empty());
        assert_eq!(wrap("ab cdefghij", 4), vec!["ab", "cdef", "ghij"]);
    }

    #[test]
    fn control_characters_are_replaced() {
        assert_eq!(clean("a\x1b[2Jb\tc\nd"), "a\u{fffd}[2Jb c\nd");
    }
}
