//! Slash-command suggestions for the input box, and hiding a key typed
//! into it. The session still checks every command; this list is only for
//! the eye and the Tab key.

/// One command form: what to type, the arguments it takes, and what it does.
pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub help: &'static str,
}

const fn c(name: &'static str, args: &'static str, help: &'static str) -> Command {
    Command { name, args, help }
}

/// Every command, in the order suggestions list them.
pub const COMMANDS: &[Command] = &[
    c("/switch", "<name>", "focus another channel"),
    c("/s", "<name>", "focus another channel"),
    c(
        "/msg",
        "<name> <text>",
        "send to a channel without switching",
    ),
    c("/me", "<action>", "say what you are doing: * you waves"),
    c("/channels", "", "list channels"),
    c("/list", "", "list channels"),
    c("/create", "<name> [--no-bot]", "create a channel"),
    c("/join", "<invite> [name]", "join a channel"),
    c("/invite", "[name]", "print an invite to a channel"),
    c("/members", "", "the focused channel's members"),
    c("/status", "", "the focused channel's counts"),
    c("/add", "<node>", "add a member"),
    c("/remove", "<node>", "remove a member"),
    c("/promote", "<node>", "give a member Manage access"),
    c("/nick", "<name>", "change your nickname"),
    c("/bot", "", "show the focused channel's bot"),
    c("/bot on", "", "add your bot to the focused channel"),
    c("/bot off", "", "remove your bot from the focused channel"),
    c(
        "/bot model",
        "[vendor/model]",
        "show or set your bots' model",
    ),
    c(
        "/bot provider-key",
        "[vendor] <key>",
        "save the model provider's key",
    ),
    c(
        "/grant",
        "<node> <server.tool|server.*>",
        "let a member use bot tools",
    ),
    c("/revoke", "<node> [pattern]", "take bot tools back"),
    c("/grants", "", "the focused channel's grants"),
    c("/block", "<node>", "your bots never answer this node"),
    c("/unblock", "<node>", "answer this node again"),
    c("/audit", "[N]", "the focused channel's last tool calls"),
    c("/prompt", "", "your bot's system prompt here"),
    c("/memory", "[query]", "what your bot remembers"),
    c("/forget", "<id>", "delete one of your bot's memories"),
    c("/schedules", "", "your bot's schedules"),
    c("/unschedule", "<id>", "remove one of your bot's schedules"),
    c("/quit", "", "leave"),
];

/// Commands whose first argument is a channel name.
const TAKES_CHANNEL: &[&str] = &["/switch", "/s", "/msg", "/invite"];

/// A suggestion: the input it completes to, and how it is shown.
#[derive(Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub complete: String,
    pub label: String,
    pub help: &'static str,
}

/// Suggestions for the input `text`, given the channel names. Empty unless
/// `text` is a slash command still being typed.
pub fn suggest(text: &str, channels: &[&str]) -> Vec<Suggestion> {
    if !text.starts_with('/') || text.contains('\n') {
        return Vec::new();
    }
    // Typing a channel name after a command that takes one.
    if let Some((cmd, arg)) = text.split_once(' ')
        && TAKES_CHANNEL.contains(&cmd)
        && !arg.contains(' ')
    {
        return channels
            .iter()
            .filter(|n| n.starts_with(arg) && **n != arg)
            .map(|n| Suggestion {
                complete: format!("{cmd} {n} "),
                label: format!("{cmd} {n}"),
                help: "",
            })
            .collect();
    }
    // The longest command already typed in full, with arguments after it:
    // show its usage only.
    let typed = COMMANDS
        .iter()
        .filter(|c| !c.args.is_empty() && text.starts_with(&format!("{} ", c.name)))
        .max_by_key(|c| c.name.len());
    let prefix = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(text) && c.name != text);
    let out: Vec<&Command> = match typed {
        // Just past the name: longer forms ("/bot model" after "/bot ")
        // if there are any, else the usage.
        Some(t) if t.name.len() + 1 == text.len() => {
            let longer: Vec<&Command> = prefix.collect();
            if longer.is_empty() { vec![t] } else { longer }
        }
        Some(t) => vec![t],
        None => {
            let exact = COMMANDS
                .iter()
                .filter(|c| c.name == text && !c.args.is_empty());
            exact.chain(prefix).collect()
        }
    };
    out.into_iter()
        .map(|c| Suggestion {
            complete: if c.args.is_empty() {
                c.name.to_string()
            } else {
                format!("{} ", c.name)
            },
            label: if c.args.is_empty() {
                c.name.to_string()
            } else {
                format!("{} {}", c.name, c.args)
            },
            help: c.help,
        })
        .collect()
}

const KEY_COMMAND: &str = "/bot provider-key ";

/// `text` with a typed provider key hidden, for the screen and errors.
pub fn redact(text: &str) -> String {
    let Some(arg) = text.strip_prefix(KEY_COMMAND) else {
        return text.to_string();
    };
    let words: Vec<&str> = arg.split(' ').collect();
    let (vendor, key) = match words.as_slice() {
        [v, k] if !v.is_empty() => (Some(*v), *k),
        _ => (None, arg),
    };
    let hidden: String = key.chars().map(|_| '•').collect();
    match vendor {
        Some(v) => format!("{KEY_COMMAND}{v} {hidden}"),
        None => format!("{KEY_COMMAND}{hidden}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(text: &str) -> Vec<String> {
        suggest(text, &["agent", "garden", "general"])
            .into_iter()
            .map(|s| s.label)
            .collect()
    }

    #[test]
    fn prefixes_list_commands() {
        assert!(labels("hello").is_empty());
        let all = labels("/");
        assert_eq!(all.len(), COMMANDS.len());
        assert_eq!(
            labels("/gr"),
            ["/grant <node> <server.tool|server.*>", "/grants"]
        );
        assert_eq!(
            labels("/bot"),
            [
                "/bot on",
                "/bot off",
                "/bot model [vendor/model]",
                "/bot provider-key [vendor] <key>"
            ]
        );
        assert_eq!(labels("/bot m"), ["/bot model [vendor/model]"]);
        assert_eq!(labels("/bot model "), ["/bot model [vendor/model]"]);
        assert_eq!(labels("/bot "), labels("/bot"));
        assert!(labels("/quit").is_empty());
        assert!(labels("/nope").is_empty());
    }

    #[test]
    fn usage_while_typing_arguments() {
        assert_eq!(labels("/bot model open"), ["/bot model [vendor/model]"]);
        assert_eq!(labels("/nick"), ["/nick <name>"]);
        assert_eq!(labels("/nick sam"), ["/nick <name>"]);
    }

    #[test]
    fn channel_names() {
        assert_eq!(labels("/s g"), ["/s garden", "/s general"]);
        assert_eq!(
            labels("/switch "),
            ["/switch agent", "/switch garden", "/switch general"]
        );
        assert!(labels("/switch garden").is_empty());
        let s = suggest("/msg ga", &["garden"]);
        assert_eq!(s[0].complete, "/msg garden ");
    }

    #[test]
    fn completions() {
        let s = suggest("/bot pro", &[]);
        assert_eq!(s[0].complete, "/bot provider-key ");
        let s = suggest("/qu", &[]);
        assert_eq!(s[0].complete, "/quit");
    }

    #[test]
    fn keys_are_hidden() {
        assert_eq!(redact("/bot provider-key sk-1"), "/bot provider-key ••••");
        assert_eq!(
            redact("/bot provider-key openai sk-1"),
            "/bot provider-key openai ••••"
        );
        assert_eq!(redact("/bot provider-key "), "/bot provider-key ");
        assert_eq!(redact("/bot model x"), "/bot model x");
    }
}
