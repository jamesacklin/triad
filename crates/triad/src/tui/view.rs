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
    let cols = area.width.saturating_sub(2) as usize;
    let lines: Vec<TextLine> = app
        .channels
        .iter()
        .map(|c| {
            let focused = app.focus.as_deref() == Some(c.name.as_str());
            let selected = app.selected.as_deref() == Some(c.name.as_str());
            let mark = match (focused, selected) {
                (true, _) => "*",
                (false, true) => ">",
                _ => " ",
            };
            let mut name = clean(&c.name);
            if c.dm {
                name.push_str(" (DM)");
            }
            if c.pending {
                name.push_str(" (…)");
            }
            let unread = if c.unread > 0 {
                format!(" {}", c.unread)
            } else {
                String::new()
            };
            // Truncate the name so the unread count always shows.
            let room = cols.saturating_sub(2 + width(&unread));
            let mut shown = String::new();
            for ch in name.chars() {
                if width(&shown) + width(ch.encode_utf8(&mut [0; 4])) > room {
                    break;
                }
                shown.push(ch);
            }
            let pad = room.saturating_sub(width(&shown));
            let style = match (focused, c.pending) {
                (true, _) => Style::new().add_modifier(Modifier::BOLD),
                (false, true) => dim(),
                _ => Style::new(),
            };
            TextLine::from(vec![
                Span::styled(format!("{mark} {shown}{}", " ".repeat(pad)), style),
                Span::styled(unread, Style::new().fg(Color::Yellow)),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" channels ")),
        area,
    );
}

fn pane_title(app: &App) -> String {
    let Some(c) = app.focused() else {
        return " starting… ".to_string();
    };
    let mut parts = vec![format!("#{}", clean(&c.name))];
    if c.dm {
        parts.push("your bot".into());
    }
    if c.pending {
        parts.push("waiting to be let in".into());
    }
    if let Some(n) = c.members {
        parts.push(format!("{n} member{}", if n == 1 { "" } else { "s" }));
    }
    if !c.role.is_empty() {
        parts.push(c.role.clone());
    }
    if c.scroll_top.is_some() {
        parts.push("scrolled; PgDn".into());
    }
    format!(" {} ", parts.join(" · "))
}

fn draw_pane(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(pane_title(app));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(chan) = app.focused() else { return };
    let cols = inner.width as usize;
    let height = inner.height as usize;
    let rows: Vec<TextLine> = chan.lines.iter().flat_map(|l| rows_of(l, cols)).collect();
    app.pane.set((rows.len(), height));
    let bottom = rows.len().saturating_sub(height);
    let start = chan.scroll_top.map_or(bottom, |t| t.min(bottom));
    let end = (start + height).min(rows.len());
    frame.render_widget(Paragraph::new(rows[start..end].to_vec()), inner);
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
    use crate::tui::app::tests::{SAM, message, started};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The screen as text, one string per row.
    fn screen(app: &App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    fn has(rows: &[String], text: &str) -> bool {
        rows.iter().any(|r| r.contains(text))
    }

    #[test]
    fn populated_at_100_by_30() {
        let mut app = started();
        app.apply(&message("garden", "n1", SAM, "hello garden"), "");
        app.apply(&message("ops", "o1", SAM, "unseen"), "");
        let rows = screen(&app, 100, 30);
        assert!(rows[0].starts_with("┌ channels"), "{}", rows[0]);
        assert!(has(&rows, "  agent (DM)"));
        assert!(has(&rows, "* garden"));
        // Unread counts sit at the sidebar's right edge.
        assert!(has(&rows, "│  ops (…)        1│"), "{rows:#?}");
        assert!(has(&rows, "#garden · creator"));
        assert!(has(&rows, "sam: old one"));
        assert!(has(&rows, "bb112233: hello garden"));
        assert!(has(&rows, "> message #garden or /command"));
    }

    #[test]
    fn small_terminal_hides_the_sidebar() {
        let mut app = started();
        app.apply(&message("garden", "n1", SAM, "hi"), "");
        let rows = screen(&app, 50, 12);
        assert!(!has(&rows, "channels"));
        assert!(rows[0].starts_with("┌ #garden"), "{}", rows[0]);
        assert!(has(&rows, "bb112233: hi"));
        // Tiny sizes draw what fits and never panic.
        for (w, h) in [(1, 1), (3, 4), (10, 5), (59, 6), (60, 7)] {
            screen(&app, w, h);
        }
    }

    #[test]
    fn long_messages_wrap() {
        let mut app = started();
        let long = "word ".repeat(30);
        app.apply(&message("garden", "n1", SAM, long.trim()), "");
        let rows = screen(&app, 50, 12);
        let wrapped = rows.iter().filter(|r| r.contains("word word")).count();
        assert!(wrapped >= 3, "{rows:#?}");
        assert!(rows.iter().all(|r| r.chars().count() == 50));
    }

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
