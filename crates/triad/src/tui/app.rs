//! The interface's model, built from the session's emitted events. Pure:
//! no terminal and no I/O.

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

#[derive(Debug, Default)]
pub struct Chan {
    pub name: String,
    pub lines: Vec<Line>,
}

impl Chan {
    fn new(name: &str) -> Self {
        Chan {
            name: name.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Default)]
pub struct App {
    /// This node's ID (hex), from `ready`.
    pub me: String,
    pub channels: Vec<Chan>,
    /// The channel the session sends plain lines to.
    pub focus: Option<String>,
    pub input: Input,
    /// Lines that came before `ready` said where they belong.
    early: Vec<Line>,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

impl App {
    pub fn chan(&self, name: &str) -> Option<&Chan> {
        self.channels.iter().find(|c| c.name == name)
    }

    fn chan_mut(&mut self, name: &str) -> Option<&mut Chan> {
        self.channels.iter_mut().find(|c| c.name == name)
    }

    pub fn focused(&self) -> Option<&Chan> {
        self.focus.as_deref().and_then(|f| self.chan(f))
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

    /// Applies one emitted event. `text` is its plain-text form.
    pub fn apply(&mut self, v: &Value, text: &str) {
        let channel = str_of(v, "channel");
        match str_of(v, "event").unwrap_or("") {
            "ready" => {
                self.me = str_of(v, "node").unwrap_or_default().to_string();
                if let Some(list) = v.get("channels").and_then(Value::as_array) {
                    for c in list {
                        if let Some(name) = str_of(c, "channel")
                            && self.chan(name).is_none()
                        {
                            self.channels.push(Chan::new(name));
                        }
                    }
                }
                self.focus = channel.map(str::to_string);
                let early = std::mem::take(&mut self.early);
                for line in early {
                    self.push(None, line);
                }
            }
            "message" => {
                let line = message_line(v);
                self.push(channel, line);
            }
            "error" => self.push(channel, Line::Error(text.to_string())),
            _ => self.push(channel, Line::Notice(text.to_string())),
        }
    }
}

/// The label for a live message's author, as the plain prompt shows it.
pub fn message_line(v: &Value) -> Line {
    let mine = v.get("mine").and_then(Value::as_bool).unwrap_or(false);
    let bot = v.get("bot").and_then(Value::as_bool).unwrap_or(false);
    let who = str_of(v, "author").unwrap_or_default();
    let who = if mine {
        "me".to_string()
    } else {
        str_of(v, "nickname")
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| who.chars().take(8).collect())
    };
    Line::Msg {
        who,
        text: str_of(v, "text").unwrap_or_default().to_string(),
        mine,
        bot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ME: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
    const SAM: &str = "bb11223300000000000000000000000000000000000000000000000000000000";

    fn ready() -> Value {
        json!({"event": "ready", "node": ME, "channel": "garden", "role": "creator",
               "status": "joined", "invite": null,
               "channels": [
                   {"channel": "agent", "role": "creator", "status": "joined", "focused": false},
                   {"channel": "garden", "role": "creator", "status": "joined", "focused": true}]})
    }

    #[test]
    fn messages_go_to_their_channel() {
        let mut app = App::default();
        app.apply(&json!({"event": "info", "text": "early"}), "early");
        app.apply(&ready(), "");
        assert_eq!(app.focus.as_deref(), Some("garden"));
        assert_eq!(
            app.chan("garden").unwrap().lines,
            vec![Line::Notice("early".into())]
        );
        app.apply(
            &json!({"event": "message", "channel": "agent", "author": SAM, "text": "hi",
                    "nickname": null, "mine": false, "bot": false, "op": "01"}),
            "",
        );
        assert_eq!(
            app.chan("agent").unwrap().lines,
            vec![Line::Msg {
                who: "bb112233".into(),
                text: "hi".into(),
                mine: false,
                bot: false
            }]
        );
        app.apply(&json!({"event": "error", "text": "nope"}), "error: nope");
        assert_eq!(
            app.focused().unwrap().lines.last(),
            Some(&Line::Error("error: nope".into()))
        );
    }
}
