//! The full-screen terminal interface for `create`, `join` and `chat`.
//!
//! A view on the session, not a second client: it reads the events the
//! session emits (the same `(json, text)` pairs `--json` and plain mode
//! print) and submits lines exactly as typed at the plain prompt. It runs on
//! one OS thread of its own; the session runs on tokio, unchanged.
//!
//! The terminal is restored when the session ends (the event sender is
//! dropped), on Ctrl-C, and on a panic anywhere.

mod app;
mod commands;
mod input;
mod view;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
use serde_json::Value;
use tokio::sync::mpsc;

use app::{App, Backfill, Bot, Line, Store};
use triad_core::state::State;

/// Stored messages, numbered like the session's own `history`.
const BACKLOG: usize = 500;

/// The backlog from a second connection to `state.sqlite`, opened when
/// first needed (the bots keep a second connection the same way).
struct StateStore {
    data_dir: PathBuf,
    state: Option<State>,
}

impl StateStore {
    fn read(&mut self, channel: &str, me: &str) -> Result<Backfill> {
        if self.state.is_none() {
            self.state = Some(State::open(&self.data_dir)?);
        }
        let state = self.state.as_ref().expect("just opened");
        let me = crate::parse_node(me)?;
        let meta = state
            .group_named(channel)?
            .with_context(|| format!("no channel named #{channel}"))?;
        let dm = state.dm_topic()? == Some(meta.topic);
        let bot = state.bot(&meta.topic)?;
        let host = bot.as_ref().map(|b| b.host);
        let profiles = state.profiles(&meta.topic)?;
        let messages = state
            .history(&meta.topic, Some(BACKLOG))?
            .into_iter()
            .map(|m| {
                let base = crate::message_sender(&m, &me, profiles.get(&m.author).cloned());
                let accepted =
                    m.bot && crate::bot_author_accepted(dm, &me, host.as_ref(), &m.author);
                let line = Line::Msg {
                    who: crate::author_label(base, dm, m.bot, accepted),
                    text: m.text,
                    mine: m.author == me,
                    bot: accepted,
                    action: m.action,
                };
                (hex::encode(m.op_hash), line)
            })
            .collect();
        let queued = state
            .queued_jobs()?
            .into_iter()
            .filter(|j| j.topic == meta.topic)
            .map(|j| hex::encode(j.op_hash))
            .collect();
        let bot = bot.map(|b| Bot {
            host: hex::encode(b.host),
            provider: b.provider,
            servers: b.servers,
            enabled: b.enabled,
        });
        Ok(Backfill {
            dm,
            messages,
            bot,
            queued,
        })
    }
}

impl Store for StateStore {
    fn backfill(&mut self, channel: &str, me: &str) -> Result<Backfill, String> {
        self.read(channel, me).map_err(|e| format!("{e:#}"))
    }
}

/// Events from the session: the JSON form and the text form.
pub type Events = mpsc::UnboundedSender<(Value, String)>;

/// Set by the panic hook, so the interface thread stops drawing over the
/// restored screen while the panic is reported.
static PANICKED: AtomicBool = AtomicBool::new(false);

/// The running interface. Join it after the session ends.
pub struct Ui {
    handle: JoinHandle<Result<()>>,
}

impl Ui {
    /// Waits for the interface thread, which has restored the terminal by
    /// the time it returns.
    pub fn join(self) -> Result<()> {
        match self.handle.join() {
            Ok(result) => result,
            Err(_) => Err(anyhow!("the terminal interface panicked")),
        }
    }
}

/// Takes over the terminal and starts the interface thread. Returns the
/// sender the session's printer emits into, and the receiver of submitted
/// lines.
pub fn spawn(data_dir: &Path) -> Result<(Events, mpsc::Receiver<String>, Ui)> {
    let terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            ratatui::restore();
            return Err(anyhow!("starting the terminal interface: {e}"));
        }
    };
    // The wheel scrolls the sidebar and the pane. Most terminals still
    // select text with Shift held.
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A bot turn's panic is caught and shown as its job's failure; it
        // must not end the interface (or print over it).
        if crate::bots::in_turn() {
            return;
        }
        PANICKED.store(true, Ordering::SeqCst);
        previous(info);
    }));
    let data_dir = data_dir.to_path_buf();
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (lines_tx, lines_rx) = mpsc::channel(64);
    let handle = std::thread::Builder::new()
        .name("triad-tui".into())
        .spawn(move || {
            let store = StateStore {
                data_dir,
                state: None,
            };
            let result = run(terminal, events_rx, lines_tx, store);
            restore();
            result
        })?;
    Ok((events_tx, lines_rx, Ui { handle }))
}

/// Leaves raw mode and the alternate screen, and shows the cursor.
fn restore() {
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::cursor::Show);
}

fn run(
    mut terminal: DefaultTerminal,
    mut events: mpsc::UnboundedReceiver<(Value, String)>,
    lines: mpsc::Sender<String>,
    store: StateStore,
) -> Result<()> {
    let mut app = App::new(Box::new(store));
    let mut dirty = true;
    let mut drawn = Instant::now();
    loop {
        if PANICKED.load(Ordering::SeqCst) {
            return Ok(());
        }
        // A running turn's clock ticks while nothing else happens.
        if app.ticking() && drawn.elapsed() >= Duration::from_millis(500) {
            dirty = true;
        }
        if dirty {
            terminal.draw(|frame| view::draw(frame, &app))?;
            dirty = false;
            drawn = Instant::now();
        }
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if !on_key(&mut app, key, &lines) {
                        return Ok(());
                    }
                    dirty = true;
                }
                Event::Mouse(m) => {
                    let step = match m.kind {
                        MouseEventKind::ScrollUp => -1,
                        MouseEventKind::ScrollDown => 1,
                        _ => 0,
                    };
                    if step != 0 {
                        app.wheel(m.column, m.row, step);
                        dirty = true;
                    }
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
        loop {
            match events.try_recv() {
                Ok((value, text)) => {
                    app.apply(&value, &text);
                    dirty = true;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                // The session ended.
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }
}

/// Hands a line to the session. The UI does no checking of its own.
/// Returns false once the session has stopped reading.
fn submit(app: &mut App, lines: &mpsc::Sender<String>, line: String) -> bool {
    match lines.try_send(line.clone()) {
        Ok(()) => {
            app.submitted(&line);
            true
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            app.error(&format!(
                "the session is busy; not sent: {}",
                commands::redact(&line)
            ));
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

/// Moves the sidebar selection and asks the session to switch.
fn switch(app: &mut App, step: isize, lines: &mpsc::Sender<String>) -> bool {
    match app.select(step) {
        Some(line) => submit(app, lines, line),
        None => true,
    }
}

/// Handles one key. Returns false when the interface should close.
fn on_key(app: &mut App, key: KeyEvent, lines: &mpsc::Sender<String>) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let quit = |app: &mut App| {
        if app.me.is_empty() {
            // The session has not started its event loop, so it would not
            // read `/quit`; leave now, as Ctrl-C does in plain mode.
            restore();
            std::process::exit(130);
        }
        submit(app, lines, "/quit".into())
    };
    // Suggestions keep their selection only while the text is unchanged.
    if !matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Tab) || alt {
        app.completion = 0;
    }
    let suggesting = !app.suggestions().is_empty();
    match key.code {
        KeyCode::Char('c') if ctrl => return quit(app),
        KeyCode::Tab if suggesting => app.complete(),
        KeyCode::Up if suggesting && !alt => app.move_completion(-1),
        KeyCode::Down if suggesting && !alt => app.move_completion(1),
        KeyCode::Char('n') if ctrl => return switch(app, 1, lines),
        KeyCode::Char('p') if ctrl => return switch(app, -1, lines),
        KeyCode::Down if alt => return switch(app, 1, lines),
        KeyCode::Up if alt => return switch(app, -1, lines),
        KeyCode::PageUp => app.scroll(-1),
        KeyCode::PageDown => app.scroll(1),
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return quit(app),
        KeyCode::Char('u') if ctrl => app.input.kill_to_start(),
        KeyCode::Char('w') if ctrl => app.input.kill_word(),
        KeyCode::Char('a') if ctrl => app.input.home(),
        KeyCode::Char('e') if ctrl => app.input.end(),
        KeyCode::Char(c) if !ctrl && !alt => app.input.insert(c),
        KeyCode::Enter => {
            let line = app.input.take();
            return submit(app, lines, line);
        }
        KeyCode::Backspace => app.input.backspace(),
        KeyCode::Delete => app.input.delete(),
        KeyCode::Left => app.input.left(),
        KeyCode::Right => app.input.right(),
        KeyCode::Home => app.input.home(),
        KeyCode::End => app.input.end(),
        _ => {}
    }
    true
}
