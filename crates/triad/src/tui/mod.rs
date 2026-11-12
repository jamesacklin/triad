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
mod input;
mod view;

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Result, anyhow};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::Value;
use tokio::sync::mpsc;

use app::App;

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
pub fn spawn() -> Result<(Events, mpsc::Receiver<String>, Ui)> {
    let terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            ratatui::restore();
            return Err(anyhow!("starting the terminal interface: {e}"));
        }
    };
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICKED.store(true, Ordering::SeqCst);
        previous(info);
    }));
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (lines_tx, lines_rx) = mpsc::channel(64);
    let handle = std::thread::Builder::new()
        .name("triad-tui".into())
        .spawn(move || {
            let result = run(terminal, events_rx, lines_tx);
            restore();
            result
        })?;
    Ok((events_tx, lines_rx, Ui { handle }))
}

/// Leaves raw mode and the alternate screen, and shows the cursor.
fn restore() {
    ratatui::restore();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::cursor::Show);
}

fn run(
    mut terminal: DefaultTerminal,
    mut events: mpsc::UnboundedReceiver<(Value, String)>,
    lines: mpsc::Sender<String>,
) -> Result<()> {
    let mut app = App::default();
    let mut dirty = true;
    loop {
        if PANICKED.load(Ordering::SeqCst) {
            return Ok(());
        }
        if dirty {
            terminal.draw(|frame| view::draw(frame, &app))?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if !on_key(&mut app, key, &lines) {
                        return Ok(());
                    }
                    dirty = true;
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
fn submit(lines: &mpsc::Sender<String>, line: String) -> bool {
    match lines.try_send(line) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => true,
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

/// Handles one key. Returns false when the interface should close.
fn on_key(app: &mut App, key: KeyEvent, lines: &mpsc::Sender<String>) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let quit = |app: &App| {
        if app.me.is_empty() {
            // The session has not started its event loop, so it would not
            // read `/quit`; leave now, as Ctrl-C does in plain mode.
            restore();
            std::process::exit(130);
        }
        submit(lines, "/quit".into())
    };
    match key.code {
        KeyCode::Char('c') if ctrl => return quit(app),
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return quit(app),
        KeyCode::Char('u') if ctrl => app.input.kill_to_start(),
        KeyCode::Char('w') if ctrl => app.input.kill_word(),
        KeyCode::Char('a') if ctrl => app.input.home(),
        KeyCode::Char('e') if ctrl => app.input.end(),
        KeyCode::Char(c) if !ctrl => app.input.insert(c),
        KeyCode::Enter => {
            let line = app.input.take();
            return submit(lines, line);
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
