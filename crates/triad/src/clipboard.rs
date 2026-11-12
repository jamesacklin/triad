//! Copying text for the user: to the system clipboard where there is one,
//! else through the terminal (OSC 52), which also works over SSH in most
//! terminals.

use std::io::{IsTerminal, Write};

use base64::Engine;

/// Where a copy went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Copied {
    /// The system clipboard.
    System,
    /// Asked the terminal to copy; it may not support it.
    Terminal,
}

impl Copied {
    pub fn as_str(self) -> &'static str {
        match self {
            Copied::System => "system",
            Copied::Terminal => "terminal",
        }
    }
}

/// The session's clipboard. On X11 the copied text lives only as long as
/// its owner, so one is kept for the whole session, opened on first use.
#[derive(Default)]
pub struct Clipboard {
    #[cfg(feature = "clipboard")]
    system: Option<arboard::Clipboard>,
}

impl Clipboard {
    /// Copies `text`. `None` if there was no way to.
    pub fn copy(&mut self, text: &str) -> Option<Copied> {
        if self.copy_system(text) {
            return Some(Copied::System);
        }
        osc52(text).then_some(Copied::Terminal)
    }

    #[cfg(feature = "clipboard")]
    fn copy_system(&mut self, text: &str) -> bool {
        if self.system.is_none() {
            match arboard::Clipboard::new() {
                Ok(c) => self.system = Some(c),
                Err(e) => {
                    tracing::info!("no system clipboard: {e}");
                    return false;
                }
            }
        }
        let c = self.system.as_mut().expect("just opened");
        match c.set_text(text) {
            Ok(()) => true,
            Err(e) => {
                tracing::info!("system clipboard: {e}");
                false
            }
        }
    }

    #[cfg(not(feature = "clipboard"))]
    fn copy_system(&mut self, _: &str) -> bool {
        false
    }
}

/// The OSC 52 sequence that asks a terminal to copy `text`, wrapped for
/// tmux when inside it.
fn osc52_sequence(text: &str, tmux: bool) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let seq = format!("\x1b]52;c;{b64}\x07");
    if tmux {
        format!("\x1bPtmux;\x1b{seq}\x1b\\")
    } else {
        seq
    }
}

fn osc52(text: &str) -> bool {
    let mut out = std::io::stdout();
    if !out.is_terminal() {
        return false;
    }
    let seq = osc52_sequence(text, std::env::var_os("TMUX").is_some());
    out.write_all(seq.as_bytes())
        .and_then(|_| out.flush())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_encodes_the_text() {
        assert_eq!(osc52_sequence("hi", false), "\x1b]52;c;aGk=\x07");
        assert_eq!(
            osc52_sequence("hi", true),
            "\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
        );
    }
}
