//! The interface's model, built from the session's emitted events. Pure:
//! no terminal, and no I/O apart from the backlog reads through [`Store`].

use std::cell::Cell;
use std::collections::BTreeSet;

use serde_json::Value;

use super::input::Input;

/// One line of a channel's pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Msg {
        who: String,
        text: String,
        mine: bool,
        bot: bool,
    },
    /// A dim line: membership changes, bot notes, command output.
    Notice(String),
    /// An error, shown in red.
    Error(String),
}

/// What is stored for a channel when the interface first shows it.
#[derive(Debug, Default)]
pub struct Backfill {
    /// The channel is the personal bot's DM.
    pub dm: bool,
    /// Stored messages, oldest first, with their operation hashes (hex).
    pub messages: Vec<(String, Line)>,
}

/// Reads a channel's backlog. The real one is a second, read-only use of
/// `state.sqlite`; tests use a fixed one.
pub trait Store {
    fn backfill(&mut self, channel: &str, me: &str) -> Result<Backfill, String>;
}

#[derive(Debug, Default)]
pub struct Chan {
    pub name: String,
    pub role: String,
    /// Still waiting to be let in.
    pub pending: bool,
    pub dm: bool,
    pub lines: Vec<Line>,
    /// Operation hashes of the messages in `lines`.
    ops: BTreeSet<String>,
    /// Messages from others since this channel was last focused.
    pub unread: usize,
    /// Member count, once a membership event has said.
    pub members: Option<u64>,
    /// First pane row shown when scrolled up; `None` follows the bottom.
    pub scroll_top: Option<usize>,
}

pub struct App {
    /// This node's ID (hex), from `ready`. Empty until the session's event
    /// loop is running.
    pub me: String,
    /// Sidebar order: the DM first, then by name.
    pub channels: Vec<Chan>,
    /// The channel the session sends plain lines to.
    pub focus: Option<String>,
    /// The sidebar selection; becomes the focus when `switched` arrives.
    pub selected: Option<String>,
    pub input: Input,
    /// Lines that came before `ready` said where they belong.
    early: Vec<Line>,
    store: Box<dyn Store>,
    /// Rows in the focused pane and its height, as last drawn.
    pub pane: Cell<(usize, usize)>,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn bool_of(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// A text form without the `[#name] ` prefix that plain mode puts on lines
/// about another channel: in that channel's pane it says nothing.
fn without_prefix(text: &str, channel: Option<&str>) -> String {
    match channel {
        Some(c) => text
            .strip_prefix(&format!("[#{c}] "))
            .unwrap_or(text)
            .to_string(),
        None => text.to_string(),
    }
}

impl App {
    pub fn new(store: Box<dyn Store>) -> Self {
        App {
            me: String::new(),
            channels: Vec::new(),
            focus: None,
            selected: None,
            input: Input::default(),
            early: Vec::new(),
            store,
            pane: Cell::new((0, 0)),
        }
    }

    pub fn chan(&self, name: &str) -> Option<&Chan> {
        self.channels.iter().find(|c| c.name == name)
    }

    fn chan_mut(&mut self, name: &str) -> Option<&mut Chan> {
        self.channels.iter_mut().find(|c| c.name == name)
    }

    pub fn focused(&self) -> Option<&Chan> {
        self.focus.as_deref().and_then(|f| self.chan(f))
    }

    fn focused_mut(&mut self) -> Option<&mut Chan> {
        let f = self.focus.clone()?;
        self.chan_mut(&f)
    }

    /// Adds a line to the named channel, or to the focused one when the
    /// channel is unknown or not given.
    fn push(&mut self, channel: Option<&str>, line: Line) {
        let target = channel
            .filter(|c| self.chan(c).is_some())
            .or(self.focus.as_deref())
            .map(str::to_string);
        match target.and_then(|t| self.chan_mut(&t)) {
            Some(c) => c.lines.push(line),
            None => self.early.push(line),
        }
    }

    /// Adds a channel from its `ready` or `channel` entry, with its backlog.
    fn add_channel(&mut self, entry: &Value) {
        let Some(name) = str_of(entry, "channel") else {
            return;
        };
        let role = str_of(entry, "role").unwrap_or_default().to_string();
        let pending = str_of(entry, "status") == Some("pending");
        if let Some(c) = self.chan_mut(name) {
            c.role = role;
            c.pending = pending;
            return;
        }
        let mut chan = Chan {
            name: name.to_string(),
            role,
            pending,
            ..Default::default()
        };
        match self.store.backfill(name, &self.me) {
            Ok(b) => {
                chan.dm = b.dm;
                for (op, line) in b.messages {
                    chan.ops.insert(op);
                    chan.lines.push(line);
                }
            }
            Err(e) => chan
                .lines
                .push(Line::Error(format!("reading stored messages: {e}"))),
        }
        self.channels.push(chan);
        self.channels
            .sort_by(|a, b| (!a.dm, &a.name).cmp(&(!b.dm, &b.name)));
    }

    /// Applies one emitted event. `text` is its plain-text form.
    pub fn apply(&mut self, v: &Value, text: &str) {
        let channel = str_of(v, "channel");
        match str_of(v, "event").unwrap_or("") {
            "ready" => {
                self.me = str_of(v, "node").unwrap_or_default().to_string();
                if let Some(list) = v.get("channels").and_then(Value::as_array) {
                    for c in list {
                        self.add_channel(c);
                    }
                }
                self.focus = channel.map(str::to_string);
                self.selected = self.focus.clone();
                let early = std::mem::take(&mut self.early);
                for line in early {
                    self.push(None, line);
                }
            }
            "channel" => {
                self.add_channel(v);
                if bool_of(v, "focused") {
                    self.set_focus(channel);
                }
                self.push(channel, Line::Notice(text.to_string()));
            }
            "switched" => self.set_focus(channel),
            "message" => self.on_message(v),
            "joined" | "removed" | "promoted" => {
                let me = self.me.clone();
                if let Some(c) = channel.and_then(|c| self.chan_mut(c)) {
                    if let Some(n) = v.get("members").and_then(Value::as_u64) {
                        c.members = Some(n);
                    }
                    if str_of(v, "event") == Some("joined") && str_of(v, "member") == Some(&me) {
                        c.pending = false;
                    }
                }
                self.push(channel, Line::Notice(without_prefix(text, channel)));
            }
            "error" => {
                // A /switch that failed leaves the selection where it was.
                self.selected = self.focus.clone();
                self.push(channel, Line::Error(text.to_string()));
            }
            _ => self.push(channel, Line::Notice(without_prefix(text, channel))),
        }
    }

    fn set_focus(&mut self, channel: Option<&str>) {
        let Some(name) = channel.filter(|c| self.chan(c).is_some()) else {
            return;
        };
        self.focus = Some(name.to_string());
        self.selected = self.focus.clone();
        if let Some(c) = self.focused_mut() {
            c.unread = 0;
        }
    }

    fn on_message(&mut self, v: &Value) {
        let Some(name) = str_of(v, "channel") else {
            return;
        };
        let focused = self.focus.as_deref() == Some(name);
        let Some(chan) = self.chan_mut(name) else {
            let line = message_line(v, false);
            self.push(None, line);
            return;
        };
        // The backlog read and the live stream can overlap.
        if let Some(op) = str_of(v, "op")
            && !chan.ops.insert(op.to_string())
        {
            return;
        }
        let line = message_line(v, chan.dm);
        // My own lines are not unread; my bot's replies are.
        if !focused && (!bool_of(v, "mine") || bool_of(v, "bot")) {
            chan.unread += 1;
        }
        chan.lines.push(line);
    }

    /// Moves the sidebar selection by `step`. Returns the `/switch` line to
    /// submit; the focus follows when the session confirms it.
    pub fn select(&mut self, step: isize) -> Option<String> {
        if self.channels.is_empty() {
            return None;
        }
        let n = self.channels.len() as isize;
        let at = self
            .selected
            .as_deref()
            .and_then(|s| self.channels.iter().position(|c| c.name == s))
            .unwrap_or(0) as isize;
        let next = &self.channels[(at + step).rem_euclid(n) as usize].name;
        self.selected = Some(next.clone());
        Some(format!("/switch {next}"))
    }

    /// Scrolls the focused pane by `pages` (negative is up).
    pub fn scroll(&mut self, pages: isize) {
        let (rows, height) = self.pane.get();
        let Some(c) = self.focused_mut() else { return };
        let bottom = rows.saturating_sub(height);
        let step = height.saturating_sub(1).max(1) as isize * pages;
        let top = c.scroll_top.unwrap_or(bottom) as isize + step;
        c.scroll_top = if top >= bottom as isize {
            None
        } else {
            Some(top.max(0) as usize)
        };
    }
}

/// A live message as a pane line, labelled as the plain prompt labels it.
pub fn message_line(v: &Value, dm: bool) -> Line {
    let mine = bool_of(v, "mine");
    let accepted = bool_of(v, "bot");
    let claimed = accepted || v.get("bot_claim").is_some_and(|c| !c.is_null());
    let author = str_of(v, "author")
        .and_then(|a| crate::parse_node(a).ok())
        .unwrap_or_default();
    let nickname = str_of(v, "nickname").map(str::to_string);
    let who = crate::author_label(
        crate::sender_name(mine, &author, nickname),
        dm,
        claimed,
        accepted,
    );
    Line::Msg {
        who,
        text: str_of(v, "text").unwrap_or_default().to_string(),
        mine,
        bot: accepted,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    pub const ME: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
    pub const SAM: &str = "bb11223300000000000000000000000000000000000000000000000000000000";

    /// A store with nothing in it.
    pub struct NoStore;

    impl Store for NoStore {
        fn backfill(&mut self, _: &str, _: &str) -> Result<Backfill, String> {
            Ok(Backfill::default())
        }
    }

    /// A store with a fixed backlog for `garden` and `agent` (the DM).
    pub struct Fixed;

    impl Store for Fixed {
        fn backfill(&mut self, channel: &str, _: &str) -> Result<Backfill, String> {
            let msg = |who: &str, text: &str| Line::Msg {
                who: who.into(),
                text: text.into(),
                mine: who == "me",
                bot: false,
            };
            Ok(match channel {
                "garden" => Backfill {
                    dm: false,
                    messages: vec![
                        ("01".into(), msg("sam", "old one")),
                        ("02".into(), msg("me", "old two")),
                    ],
                },
                "agent" => Backfill {
                    dm: true,
                    ..Default::default()
                },
                _ => Backfill::default(),
            })
        }
    }

    pub fn ready() -> Value {
        json!({"event": "ready", "node": ME, "channel": "garden", "role": "creator",
               "status": "joined", "invite": null,
               "channels": [
                   {"channel": "agent", "role": "creator", "status": "joined", "focused": false},
                   {"channel": "garden", "role": "creator", "status": "joined", "focused": true},
                   {"channel": "ops", "role": "member", "status": "pending", "focused": false}]})
    }

    pub fn message(channel: &str, op: &str, author: &str, text: &str) -> Value {
        json!({"event": "message", "channel": channel, "author": author, "text": text,
               "nickname": null, "op": op, "mine": author == ME, "bot": false,
               "ask": false, "long": false, "ts": 1})
    }

    pub fn started() -> App {
        let mut app = App::new(Box::new(Fixed));
        app.apply(&ready(), "");
        app
    }

    #[test]
    fn early_lines_and_errors_go_to_the_focus() {
        let mut app = App::new(Box::new(NoStore));
        app.apply(&json!({"event": "info", "text": "early"}), "early");
        app.apply(&ready(), "");
        assert_eq!(app.focus.as_deref(), Some("garden"));
        assert_eq!(
            app.chan("garden").unwrap().lines,
            vec![Line::Notice("early".into())]
        );
        app.apply(&json!({"event": "error", "text": "nope"}), "error: nope");
        assert_eq!(
            app.focused().unwrap().lines.last(),
            Some(&Line::Error("error: nope".into()))
        );
    }

    #[test]
    fn sidebar_order_pending_and_backlog() {
        let app = started();
        let names: Vec<&str> = app.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["agent", "garden", "ops"]);
        assert!(app.chan("agent").unwrap().dm);
        assert!(app.chan("ops").unwrap().pending);
        assert_eq!(app.chan("garden").unwrap().lines.len(), 2);
    }

    #[test]
    fn live_messages_dedup_against_the_backlog() {
        let mut app = started();
        app.apply(&message("garden", "02", ME, "old two"), "");
        app.apply(&message("garden", "03", SAM, "new"), "");
        app.apply(&message("garden", "03", SAM, "new"), "");
        let lines = &app.chan("garden").unwrap().lines;
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[2],
            Line::Msg {
                who: "bb112233".into(),
                text: "new".into(),
                mine: false,
                bot: false
            }
        );
    }

    #[test]
    fn unread_counts_and_focus() {
        let mut app = started();
        app.apply(&message("agent", "a1", SAM, "x"), "");
        app.apply(&message("agent", "a2", ME, "mine"), "");
        app.apply(&message("garden", "g1", SAM, "focused"), "");
        assert_eq!(app.chan("agent").unwrap().unread, 1);
        assert_eq!(app.chan("garden").unwrap().unread, 0);
        // Selecting does not move the focus; `switched` does.
        assert_eq!(app.select(-1).as_deref(), Some("/switch agent"));
        assert_eq!(app.focus.as_deref(), Some("garden"));
        app.apply(
            &json!({"event": "switched", "channel": "agent"}),
            "now talking in #agent",
        );
        assert_eq!(app.focus.as_deref(), Some("agent"));
        assert_eq!(app.chan("agent").unwrap().unread, 0);
        assert_eq!(app.select(-1).as_deref(), Some("/switch ops"));
        // A failed switch puts the selection back.
        app.apply(&json!({"event": "error", "text": "x"}), "error: x");
        assert_eq!(app.selected.as_deref(), Some("agent"));
    }

    #[test]
    fn new_channels_and_membership() {
        let mut app = started();
        app.apply(
            &json!({"event": "channel", "channel": "beta", "role": "creator",
                    "status": "joined", "focused": true, "invite": "triad:x"}),
            "now in #beta\ninvite  triad:x",
        );
        assert_eq!(app.focus.as_deref(), Some("beta"));
        assert_eq!(app.channels[1].name, "beta");
        // The DM reported by /bot on in a --no-bot session keeps the focus.
        app.apply(
            &json!({"event": "channel", "channel": "zz", "role": "creator",
                    "status": "joined", "focused": false}),
            "your bot's DM",
        );
        assert_eq!(app.focus.as_deref(), Some("beta"));
        app.apply(
            &json!({"event": "joined", "channel": "ops", "member": ME, "members": 3,
                    "access": "write"}),
            "[#ops] you joined (3 members)",
        );
        let ops = app.chan("ops").unwrap();
        assert!(!ops.pending);
        assert_eq!(ops.members, Some(3));
        assert_eq!(
            ops.lines.last(),
            Some(&Line::Notice("you joined (3 members)".into()))
        );
    }

    #[test]
    fn bot_labels_match_the_plain_prompt() {
        let mut app = started();
        let mut v = message("agent", "b1", ME, "hi from the bot");
        v["bot"] = json!(true);
        app.apply(&v, "");
        let mut v = message("garden", "b2", SAM, "fake");
        v["bot_claim"] = json!("not accepted");
        app.apply(&v, "");
        let mut v = message("garden", "b3", SAM, "real");
        v["bot"] = json!(true);
        app.apply(&v, "");
        let who = |c: &str, i: usize| match &app.chan(c).unwrap().lines[i] {
            Line::Msg { who, .. } => who.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(who("agent", 0), "my bot");
        assert_eq!(who("garden", 2), "bb112233 (bot claim not accepted)");
        assert_eq!(who("garden", 3), "bot");
    }

    #[test]
    fn unknown_events_are_kept() {
        let mut app = started();
        app.apply(
            &json!({"event": "something_new", "channel": "ops"}),
            "[#ops] something new",
        );
        assert_eq!(
            app.chan("ops").unwrap().lines.last(),
            Some(&Line::Notice("something new".into()))
        );
    }

    #[test]
    fn scrolling_holds_its_place() {
        let mut app = started();
        app.pane.set((100, 10));
        app.scroll(-1);
        assert_eq!(app.focused().unwrap().scroll_top, Some(81));
        app.apply(&message("garden", "n", SAM, "new"), "");
        assert_eq!(app.focused().unwrap().scroll_top, Some(81));
        app.scroll(1);
        assert_eq!(app.focused().unwrap().scroll_top, None);
        app.scroll(-20);
        assert_eq!(app.focused().unwrap().scroll_top, Some(0));
    }
}
