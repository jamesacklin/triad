//! `triad bot …`: see and change what the personal bot keeps, from outside
//! chat mode. Everything is in the data directory: memories and schedules
//! are rows in `state.sqlite`, skills are `skills/*.md`, the persona is
//! `persona.md` (or the file `agent.json` names). These commands work while a
//! chat session runs; it reads memories, schedules, skills and the persona
//! again at each turn.
//!
//! `--edit` opens a memories or schedules file in `$VISUAL` / `$EDITOR`
//! (else `vi`), one row per line, and applies what changed when the editor
//! exits: a deleted line removes its row, a changed line updates it, a line
//! starting with `+` adds one. Rows the bot added meanwhile are left alone.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use triad_agent::assistant::{self, MAX_MEMORY_BYTES, MAX_PROMPT_BYTES};
use triad_agent::schedule::{self, MIN_EVERY_MS};
use triad_agent::skills;
use triad_core::state::{MemoryRow, ScheduleRow, State, now_ms};

#[derive(Subcommand)]
pub enum BotCmd {
    /// List the personal bot's memories (matching QUERY), or edit them.
    Memory {
        query: Option<String>,
        /// Open every memory in your editor and apply the changes.
        #[arg(long, conflicts_with = "query")]
        edit: bool,
    },
    /// List the personal bot's schedules, or edit them.
    Schedules {
        #[arg(long)]
        edit: bool,
    },
    /// List the skills, print one, or write one in your editor.
    Skills {
        name: Option<String>,
        /// Open skills/NAME.md in your editor, creating it if needed.
        #[arg(long, requires = "name")]
        edit: bool,
    },
    /// Print the persona (standing instructions for every turn), or edit it.
    Persona {
        #[arg(long)]
        edit: bool,
    },
}

/// `triad bot` without a subcommand: where everything is, and how much.
pub fn overview(data_dir: &Path) -> Result<()> {
    let state = State::open(data_dir)?;
    let persona = persona_path(data_dir)?;
    let skill_list = skills::list(data_dir);
    let pinned = state.memory_pinned()?.len();
    println!("data directory: {}", data_dir.display());
    println!(
        "memories:  {} ({pinned} pinned)  triad bot memory [--edit]",
        state.memory_count()?
    );
    println!(
        "schedules: {}  triad bot schedules [--edit]",
        state.schedules()?.len()
    );
    println!(
        "skills:    {}  {}  triad bot skills [NAME [--edit]]",
        skill_list.len(),
        data_dir.join(skills::SKILLS_DIR).display()
    );
    println!(
        "persona:   {}{}  triad bot persona [--edit]",
        persona.display(),
        if persona.exists() { "" } else { " (none yet)" }
    );
    println!(
        "model and settings: {}",
        data_dir.join(triad_agent::CONFIG_FILE).display()
    );
    println!(
        "MCP servers: {}",
        data_dir.join(triad_agent::mcp::CONFIG_FILE).display()
    );
    println!("In chat, /prompt shows the system prompt a turn in the focused channel gets.");
    Ok(())
}

pub fn run(data_dir: &Path, cmd: BotCmd) -> Result<()> {
    match cmd {
        BotCmd::Memory { query, edit: false } => {
            let state = State::open(data_dir)?;
            let rows = state.memory_search(query.as_deref().unwrap_or(""), 10_000)?;
            if rows.is_empty() {
                println!("no memories");
            }
            for m in rows.into_iter().rev() {
                println!("{}", assistant::memory_line(&memory(m)));
            }
        }
        BotCmd::Memory { edit: true, .. } => edit_memories(data_dir)?,
        BotCmd::Schedules { edit: false } => {
            let state = State::open(data_dir)?;
            let offset = utc_offset(data_dir)?;
            let rows = state.schedules()?;
            if rows.is_empty() {
                println!("no schedules");
            }
            for s in rows {
                println!("{}", assistant::schedule_line(&schedule_of(s), offset));
            }
        }
        BotCmd::Schedules { edit: true } => edit_schedules(data_dir)?,
        BotCmd::Skills { name: None, .. } => {
            let list = skills::list(data_dir);
            if list.is_empty() {
                println!(
                    "no skills; write one with: triad bot skills <name> --edit \
                     (it becomes {}/<name>.md)",
                    data_dir.join(skills::SKILLS_DIR).display()
                );
            }
            for s in list {
                println!("{}: {}", s.name, s.summary);
            }
        }
        BotCmd::Skills {
            name: Some(name),
            edit,
        } => {
            let path = skills::path(data_dir, &name).map_err(anyhow::Error::msg)?;
            if edit {
                if !path.exists() {
                    std::fs::create_dir_all(path.parent().expect("in the skills folder"))?;
                    std::fs::write(&path, SKILL_TEMPLATE)?;
                }
                open_editor(&path)?;
                println!("saved {}", path.display());
            } else {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("no skill {name} ({})", path.display()))?;
                print!("{text}");
            }
        }
        BotCmd::Persona { edit } => {
            let path = persona_path(data_dir)?;
            if edit {
                open_editor(&path)?;
                println!("saved {}", path.display());
            } else {
                match std::fs::read_to_string(&path) {
                    Ok(text) => print!("{text}"),
                    Err(_) => println!(
                        "no persona; write one with: triad bot persona --edit (it becomes {})",
                        path.display()
                    ),
                }
            }
        }
    }
    Ok(())
}

const SKILL_TEMPLATE: &str = "---\ndescription: One line saying when to use this skill.\n---\n\
                              Steps for the bot to follow.\n";

fn persona_path(data_dir: &Path) -> Result<PathBuf> {
    triad_agent::persona_path(data_dir).map_err(|e| anyhow::anyhow!("{e}"))
}

fn utc_offset(data_dir: &Path) -> Result<i32> {
    triad_agent::configured_utc_offset(data_dir).map_err(|e| anyhow::anyhow!("{e}"))
}

fn memory(m: MemoryRow) -> assistant::Memory {
    assistant::Memory {
        id: m.id,
        text: m.text,
        pinned: m.pinned,
        created_at_ms: m.created_at_ms,
    }
}

fn schedule_of(s: ScheduleRow) -> assistant::Schedule {
    assistant::Schedule {
        id: s.id,
        name: s.name,
        prompt: s.prompt,
        next_at_ms: s.next_at_ms,
        every_ms: s.every_ms,
    }
}

/// Opens `path` in the user's editor and waits for it.
fn open_editor(path: &Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|e| !e.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|e| !e.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".into());
    // Through the shell, so EDITOR may carry arguments (`code --wait`).
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .with_context(|| format!("starting {editor}"))?;
    if !status.success() {
        bail!("{editor} exited with {status}; nothing changed");
    }
    Ok(())
}

/// Writes `text` to a private file in the data directory, lets the user
/// edit it, and returns the file and what they saved. The caller removes
/// the file once the edit is applied, and keeps it if the edit is refused.
fn edit_text(data_dir: &Path, name: &str, text: &str) -> Result<(PathBuf, String)> {
    let path = data_dir.join(name);
    let _ = std::fs::remove_file(&path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut f = options
            .open(&path)
            .with_context(|| format!("writing {}", path.display()))?;
        f.write_all(text.as_bytes())?;
    }
    open_editor(&path)?;
    let edited = std::fs::read_to_string(&path)?;
    Ok((path, edited))
}

/// A line break inside a row is written `\n`, and a backslash `\\`.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Lines that are not blank and not comments, with their line numbers.
fn content_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim_end()))
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
}

/// The ID at the start of a line, or `None` for `+` (a new row).
fn line_id(word: &str, n: usize) -> Result<Option<i64>> {
    if word == "+" {
        return Ok(None);
    }
    word.parse()
        .map(Some)
        .with_context(|| format!("line {n}: starts with {word:?}, not an ID or +"))
}

// --- memories ---------------------------------------------------------

const MEMORY_HEADER: &str = "\
# Your bot's memories. Save and close to apply; close without saving to
# change nothing.
#   <id> <text>      a memory
#   <id> * <text>    a pinned memory: shown to the bot in every DM turn
# Delete a line to forget that memory. Change its text or * to edit it.
# Add a line starting with + to save a new one (\"+ text\" or \"+ * text\").
# A line break inside a memory is written \\n.
";

/// A memory as a line of the edit file.
fn memory_edit_line(m: &MemoryRow) -> String {
    let star = if m.pinned { "* " } else { "" };
    format!("{} {star}{}", m.id, escape(&m.text))
}

/// One line of the edit file: ID (or `None` for new), pinned, text.
#[derive(Debug, PartialEq, Eq)]
struct MemoryEdit {
    id: Option<i64>,
    pinned: bool,
    text: String,
}

fn parse_memories(text: &str) -> Result<Vec<MemoryEdit>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (n, line) in content_lines(text) {
        let line = line.trim_start();
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let id = line_id(word, n)?;
        if let Some(id) = id
            && !seen.insert(id)
        {
            bail!("line {n}: memory {id} appears twice");
        }
        let rest = rest.trim_start();
        let (pinned, rest) = match rest.strip_prefix('*') {
            Some(r) if r.is_empty() || r.starts_with(' ') => (true, r.trim_start()),
            _ => (false, rest),
        };
        let text = unescape(rest.trim());
        if text.is_empty() {
            bail!("line {n}: a memory needs text (delete the line to forget it)");
        }
        if text.len() > MAX_MEMORY_BYTES {
            bail!(
                "line {n}: at most {MAX_MEMORY_BYTES} bytes per memory, got {}",
                text.len()
            );
        }
        out.push(MemoryEdit { id, pinned, text });
    }
    Ok(out)
}

/// What an edit changes, against the rows it started from.
#[derive(Debug, PartialEq, Eq)]
struct Changes<T> {
    add: Vec<T>,
    update: Vec<(i64, T)>,
    remove: Vec<i64>,
}

impl<T> Default for Changes<T> {
    fn default() -> Self {
        Changes {
            add: Vec::new(),
            update: Vec::new(),
            remove: Vec::new(),
        }
    }
}

fn memory_changes(before: &[MemoryRow], after: Vec<MemoryEdit>) -> Result<Changes<MemoryEdit>> {
    let old: BTreeMap<i64, &MemoryRow> = before.iter().map(|m| (m.id, m)).collect();
    let mut changes = Changes::default();
    let mut kept = BTreeSet::new();
    for e in after {
        match e.id {
            None => changes.add.push(e),
            Some(id) => {
                let Some(m) = old.get(&id) else {
                    bail!("there is no memory {id} to edit; start a new one with +");
                };
                kept.insert(id);
                if m.text != e.text || m.pinned != e.pinned {
                    changes.update.push((id, e));
                }
            }
        }
    }
    changes.remove = old
        .keys()
        .filter(|id| !kept.contains(id))
        .copied()
        .collect();
    Ok(changes)
}

fn edit_memories(data_dir: &Path) -> Result<()> {
    let state = State::open(data_dir)?;
    let mut before = state.memory_search("", usize::MAX >> 1)?;
    before.reverse();
    let mut text = MEMORY_HEADER.to_string();
    for m in &before {
        text.push_str(&memory_edit_line(m));
        text.push('\n');
    }
    let (path, edited) = edit_text(data_dir, ".memory-edit.txt", &text)?;
    let changes = match parse_memories(&edited).and_then(|a| memory_changes(&before, a)) {
        Ok(c) => c,
        Err(e) => bail!(
            "{e:#}. Nothing changed; your edit is in {} (copy from it and run --edit again)",
            path.display()
        ),
    };
    let _ = std::fs::remove_file(&path);
    for id in &changes.remove {
        state.memory_forget(*id)?;
    }
    for (id, e) in &changes.update {
        state.memory_update(*id, &e.text, e.pinned)?;
    }
    for e in &changes.add {
        let id = state.memory_save(&e.text, e.pinned)?;
        println!("saved memory #{id}");
    }
    println!(
        "{} added, {} changed, {} forgotten",
        changes.add.len(),
        changes.update.len(),
        changes.remove.len()
    );
    Ok(())
}

// --- schedules --------------------------------------------------------

fn schedule_header(offset: i32) -> String {
    format!(
        "\
# Your bot's schedules. Save and close to apply; close without saving to
# change nothing. Times are {zone} (utc_offset_minutes in agent.json).
#   <id> | <next> | <every> | <name> | <prompt>
# <next> is when it fires next: 2026-11-13 09:00, 09:00, or in 20m.
# <every> is - for once, or an interval of at least 5m: 30m, 2h, 1d, 1w.
# Delete a line to remove that schedule. Change a field to edit it.
# Add one with a line starting with +: + | in 20m | - | tea | Make tea.
# A line break inside a prompt is written \\n.
",
        zone = schedule::zone(offset)
    )
}

fn schedule_edit_line(s: &ScheduleRow, offset: i32) -> String {
    format!(
        "{} | {} | {} | {} | {}",
        s.id,
        schedule::local_stamp(s.next_at_ms, offset),
        s.every_ms.map_or("-".into(), schedule::duration_text),
        s.name,
        escape(&s.prompt)
    )
}

#[derive(Debug, PartialEq, Eq)]
struct ScheduleEdit {
    id: Option<i64>,
    /// As written, so an unchanged time keeps its seconds.
    next: String,
    every: String,
    name: String,
    prompt: String,
}

fn parse_schedules(text: &str) -> Result<Vec<ScheduleEdit>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (n, line) in content_lines(text) {
        let fields: Vec<&str> = line.splitn(5, '|').map(str::trim).collect();
        let [id, next, every, name, prompt] = fields.as_slice() else {
            bail!("line {n}: expected <id> | <next> | <every> | <name> | <prompt>");
        };
        let id = line_id(id, n)?;
        if let Some(id) = id
            && !seen.insert(id)
        {
            bail!("line {n}: schedule {id} appears twice");
        }
        let prompt = unescape(prompt);
        if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            bail!("line {n}: a prompt is 1 to {MAX_PROMPT_BYTES} bytes");
        }
        out.push(ScheduleEdit {
            id,
            next: next.to_string(),
            every: every.to_string(),
            name: assistant::schedule_name(Some(name), &prompt),
            prompt,
        });
    }
    Ok(out)
}

/// A schedule's fields after an edit.
#[derive(Debug, PartialEq, Eq)]
struct ScheduleFields {
    name: String,
    prompt: String,
    next_at_ms: u64,
    every_ms: Option<u64>,
}

fn every_of(text: &str) -> Result<Option<u64>> {
    if text == "-" || text.is_empty() {
        return Ok(None);
    }
    let every = schedule::parse_duration(text).map_err(anyhow::Error::msg)?;
    if every < MIN_EVERY_MS {
        bail!("the shortest interval is 5m, got {text}");
    }
    Ok(Some(every))
}

fn schedule_changes(
    before: &[ScheduleRow],
    after: Vec<ScheduleEdit>,
    offset: i32,
    now: u64,
) -> Result<Changes<ScheduleFields>> {
    let old: BTreeMap<i64, &ScheduleRow> = before.iter().map(|s| (s.id, s)).collect();
    let mut changes = Changes::default();
    let mut kept = BTreeSet::new();
    for e in after {
        let what =
            e.id.map_or("a new schedule".into(), |id| format!("schedule {id}"));
        let every = every_of(&e.every).with_context(|| what.clone())?;
        let new_time = |text: &str| -> Result<u64> {
            let at = schedule::parse_at(text, now, offset)
                .map_err(anyhow::Error::msg)
                .with_context(|| what.clone())?;
            if at <= now {
                bail!("{what}: {text} has passed");
            }
            Ok(at)
        };
        match e.id {
            None => changes.add.push(ScheduleFields {
                next_at_ms: new_time(&e.next)?,
                name: e.name,
                prompt: e.prompt,
                every_ms: every,
            }),
            Some(id) => {
                let Some(s) = old.get(&id) else {
                    bail!("there is no schedule {id} to edit; start a new one with +");
                };
                kept.insert(id);
                let next_at_ms = if e.next == schedule::local_stamp(s.next_at_ms, offset) {
                    s.next_at_ms
                } else {
                    new_time(&e.next)?
                };
                let fields = ScheduleFields {
                    name: e.name,
                    prompt: e.prompt,
                    next_at_ms,
                    every_ms: every,
                };
                if fields.name != s.name
                    || fields.prompt != s.prompt
                    || fields.next_at_ms != s.next_at_ms
                    || fields.every_ms != s.every_ms
                {
                    changes.update.push((id, fields));
                }
            }
        }
    }
    changes.remove = old
        .keys()
        .filter(|id| !kept.contains(id))
        .copied()
        .collect();
    let total = before.len() + changes.add.len() - changes.remove.len();
    if total as u64 > schedule::MAX_SCHEDULES {
        bail!("at most {} schedules", schedule::MAX_SCHEDULES);
    }
    Ok(changes)
}

fn edit_schedules(data_dir: &Path) -> Result<()> {
    let state = State::open(data_dir)?;
    let offset = utc_offset(data_dir)?;
    let before = state.schedules()?;
    let mut text = schedule_header(offset);
    for s in &before {
        text.push_str(&schedule_edit_line(s, offset));
        text.push('\n');
    }
    let (path, edited) = edit_text(data_dir, ".schedules-edit.txt", &text)?;
    let changes = match parse_schedules(&edited)
        .and_then(|a| schedule_changes(&before, a, offset, now_ms()))
    {
        Ok(c) => c,
        Err(e) => bail!(
            "{e:#}. Nothing changed; your edit is in {} (copy from it and run --edit again)",
            path.display()
        ),
    };
    let _ = std::fs::remove_file(&path);
    for id in &changes.remove {
        state.schedule_remove(*id)?;
    }
    for (id, f) in &changes.update {
        state.schedule_update(*id, &f.name, &f.prompt, f.next_at_ms, f.every_ms)?;
    }
    for f in &changes.add {
        let id = state.schedule_add(&f.name, &f.prompt, f.next_at_ms, f.every_ms)?;
        println!(
            "added schedule #{id}, next at {}",
            schedule::local(f.next_at_ms, offset)
        );
    }
    println!(
        "{} added, {} changed, {} removed",
        changes.add.len(),
        changes.update.len(),
        changes.remove.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: i64, text: &str, pinned: bool) -> MemoryRow {
        MemoryRow {
            id,
            text: text.into(),
            pinned,
            created_at_ms: 0,
        }
    }

    #[test]
    fn memory_lines_round_trip() {
        let rows = [mem(3, "two\nlines \\ slash", true), mem(5, "plain", false)];
        let text: String = rows
            .iter()
            .map(|m| memory_edit_line(m) + "\n")
            .collect::<String>();
        let parsed = parse_memories(&format!("{MEMORY_HEADER}{text}")).unwrap();
        let back: Vec<(Option<i64>, bool, &str)> = parsed
            .iter()
            .map(|e| (e.id, e.pinned, e.text.as_str()))
            .collect();
        assert_eq!(
            back,
            [
                (Some(3), true, "two\nlines \\ slash"),
                (Some(5), false, "plain")
            ]
        );
        let none = memory_changes(&rows, parsed).unwrap();
        assert_eq!(none, Changes::default());
    }

    #[test]
    fn memory_edits_become_changes() {
        let rows = [mem(1, "a", false), mem(2, "b", false), mem(3, "c", true)];
        let edited = "# comment\n1 * a\n\n3 c changed\n+ new one\n+ * *star\n";
        let c = memory_changes(&rows, parse_memories(edited).unwrap()).unwrap();
        assert_eq!(c.remove, [2]);
        let updates: Vec<(i64, bool, &str)> = c
            .update
            .iter()
            .map(|(id, e)| (*id, e.pinned, e.text.as_str()))
            .collect();
        assert_eq!(updates, [(1, true, "a"), (3, false, "c changed")]);
        let adds: Vec<(bool, &str)> = c.add.iter().map(|e| (e.pinned, e.text.as_str())).collect();
        assert_eq!(adds, [(false, "new one"), (true, "*star")]);
        for bad in ["x hello", "1 a\n1 b", "1", "+ *", "9 not there"] {
            let r = parse_memories(bad).and_then(|a| memory_changes(&rows, a));
            assert!(r.is_err(), "{bad}");
        }
    }

    #[test]
    fn schedule_edits_become_changes() {
        // 2026-11-12 15:04:05 UTC.
        let now = 1_794_495_845_000;
        let row = |id, next, every| ScheduleRow {
            id,
            name: format!("s{id}"),
            prompt: "Do it.".into(),
            next_at_ms: next,
            every_ms: every,
            last_fired_ms: None,
            created_at_ms: 0,
        };
        let day = schedule::DAY;
        // Seconds in its time, which the file does not show.
        let rows = [
            row(1, now + 3_723_000, None),
            row(2, now + day, Some(day)),
            row(3, now + 60_000, None),
        ];
        let text: String = rows
            .iter()
            .map(|s| schedule_edit_line(s, 60) + "\n")
            .collect();
        let unchanged = parse_schedules(&format!("{}{text}", schedule_header(60))).unwrap();
        assert_eq!(
            schedule_changes(&rows, unchanged, 60, now).unwrap(),
            Changes::default(),
            "{text}"
        );
        let edited = text
            .lines()
            .filter(|l| !l.starts_with("3 "))
            .map(|l| {
                if l.starts_with("2 ") {
                    "2 | 2026-11-14 10:30 | 2h | renamed | New prompt.".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n+ | in 20m | - | tea | Make tea.\n";
        let c = schedule_changes(&rows, parse_schedules(&edited).unwrap(), 60, now).unwrap();
        assert_eq!(c.remove, [3]);
        assert_eq!(c.update.len(), 1);
        let (id, f) = &c.update[0];
        assert_eq!(
            (*id, f.name.as_str(), f.prompt.as_str()),
            (2, "renamed", "New prompt.")
        );
        assert_eq!(
            schedule::local(f.next_at_ms, 60),
            "2026-11-14 10:30 UTC+01:00"
        );
        assert_eq!(f.every_ms, Some(2 * 3_600_000));
        assert_eq!(c.add[0].next_at_ms, now + 20 * 60_000);
        assert_eq!(c.add[0].every_ms, None);
        for bad in [
            "1 | soon | - | a | b",
            "1 | in 1h | 1m | a | b",
            "1 | in 1h | - | a",
            "+ | 2020-01-01 00:00 | - | a | b",
            "7 | in 1h | - | a | b",
        ] {
            let r = parse_schedules(bad).and_then(|a| schedule_changes(&rows, a, 60, now));
            assert!(r.is_err(), "{bad}");
        }
    }
}
