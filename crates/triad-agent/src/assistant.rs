//! The personal bot's own tools: memory (`memory.*`) and schedules
//! (`schedule.*`). They are offered only
//! in the DM ([`Scope::DmOnly`]), so nothing a channel says is saved and no
//! memory reaches a channel turn.
//!
//! The rows live in the host's `state.sqlite`, which this crate never opens:
//! the binary implements [`Store`] over it, as it does [`crate::tools::Audit`].

use std::sync::Arc;

use serde_json::{Value, json};

use crate::schedule::{self, MAX_SCHEDULES, MIN_EVERY_MS};
use crate::tools::{BoxFuture, CallContext, Scope, ToolSpec, Tools};

/// Most bytes of one memory.
pub const MAX_MEMORY_BYTES: usize = 2_000;
/// Most memories kept; a save past this is refused.
pub const MAX_MEMORIES: u64 = 2_000;
/// Most bytes of pinned memories put in a DM turn's system prompt.
pub const MAX_PINNED_BYTES: usize = 2_000;
/// Most bytes of a schedule's prompt.
pub const MAX_PROMPT_BYTES: usize = 2_000;
/// Most characters of a schedule's name.
const MAX_NAME_CHARS: usize = 60;
const DEFAULT_SEARCH: usize = 5;
const MAX_SEARCH: usize = 20;

/// One saved memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    pub id: i64,
    pub text: String,
    pub pinned: bool,
    pub created_at_ms: u64,
}

/// One schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub id: i64,
    pub name: String,
    pub prompt: String,
    pub next_at_ms: u64,
    /// `None` for a one-shot.
    pub every_ms: Option<u64>,
}

/// A schedule as a line for the model or the host.
pub fn schedule_line(s: &Schedule, offset_minutes: i32) -> String {
    let every = match s.every_ms {
        Some(e) => format!(", every {}", schedule::duration_text(e)),
        None => String::new(),
    };
    format!(
        "#{} {}: next {}{every}. Prompt: {}",
        s.id,
        s.name,
        schedule::local(s.next_at_ms, offset_minutes),
        s.prompt
    )
}

/// Where the personal bot's rows are kept. Errors are text for the model.
pub trait Store: Send + Sync {
    fn schedule_count(&self) -> Result<u64, String>;
    /// Adds a schedule; returns its ID.
    fn schedule_add(
        &self,
        name: &str,
        prompt: &str,
        next_at_ms: u64,
        every_ms: Option<u64>,
    ) -> Result<i64, String>;
    /// Every schedule, soonest first.
    fn schedules(&self) -> Result<Vec<Schedule>, String>;
    /// Whether a schedule with this ID existed.
    fn schedule_remove(&self, id: i64) -> Result<bool, String>;
    fn memory_count(&self) -> Result<u64, String>;
    /// Saves a memory; returns its ID.
    fn memory_save(&self, text: &str, pinned: bool) -> Result<i64, String>;
    /// Up to `count` memories matching `query`, best first.
    fn memory_search(&self, query: &str, count: usize) -> Result<Vec<Memory>, String>;
    /// Whether a memory with this ID existed.
    fn memory_forget(&self, id: i64) -> Result<bool, String>;
    /// Pinned memories, oldest first.
    fn memory_pinned(&self) -> Result<Vec<Memory>, String>;
}

/// The pinned memories that fit in [`MAX_PINNED_BYTES`], oldest first.
pub fn pinned_notes(memories: &[Memory]) -> Vec<String> {
    let mut used = 0;
    let mut out = Vec::new();
    for m in memories.iter().filter(|m| m.pinned) {
        used += m.text.len();
        if used > MAX_PINNED_BYTES {
            break;
        }
        out.push(m.text.clone());
    }
    out
}

/// A memory as a line for the model or the host: `#12 (2026-11-12, pinned) text`.
pub fn memory_line(m: &Memory) -> String {
    let day = crate::builtin::utc(m.created_at_ms / 1000);
    let day = day.split(' ').next().unwrap_or_default();
    let pinned = if m.pinned { ", pinned" } else { "" };
    format!("#{} ({day}{pinned}) {}", m.id, m.text)
}

/// The DM-only tools over a [`Store`].
pub struct Assistant {
    store: Arc<dyn Store>,
    /// Minutes east of UTC that schedule times are written in.
    offset_minutes: i32,
}

fn id_arg(args: &Value, tool: &str) -> Result<i64, String> {
    args.get("id")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str()?.trim_start_matches('#').parse().ok())
        })
        .ok_or_else(|| format!("{tool} needs an `id`"))
}

/// A schedule's name: one short line, safe inside `[scheduled: …]`.
pub fn schedule_name(name: Option<&str>, prompt: &str) -> String {
    let raw = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(prompt);
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || c == '[' || c == ']' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let words = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut name: String = words.chars().take(MAX_NAME_CHARS).collect();
    if words.chars().count() > MAX_NAME_CHARS {
        name.push('…');
    }
    name
}

impl Assistant {
    pub fn new(store: Arc<dyn Store>, offset_minutes: i32) -> Self {
        Assistant {
            store,
            offset_minutes,
        }
    }

    fn zone(&self) -> String {
        let local = schedule::local(0, self.offset_minutes);
        local[17..].to_string()
    }

    fn schedule_add(&self, args: &Value, now_ms: u64) -> Result<String, String> {
        let str_arg = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
        };
        let prompt = str_arg("prompt").ok_or("schedule.add needs a `prompt`")?;
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(format!(
                "schedule.add: the prompt is at most {MAX_PROMPT_BYTES} bytes"
            ));
        }
        let (next, every) = match (str_arg("at"), str_arg("every"), str_arg("time")) {
            (Some(at), None, None) => (
                schedule::parse_at(at, now_ms, self.offset_minutes)
                    .map_err(|e| format!("schedule.add: {e}"))?,
                None,
            ),
            (None, Some(every), time) => {
                let every =
                    schedule::parse_duration(every).map_err(|e| format!("schedule.add: {e}"))?;
                if every < MIN_EVERY_MS {
                    return Err("schedule.add: the shortest interval is 5m".into());
                }
                let first = schedule::first_every(every, time, now_ms, self.offset_minutes)
                    .map_err(|e| format!("schedule.add: {e}"))?;
                (first, Some(every))
            }
            (None, None, _) => {
                return Err("schedule.add needs `at` (once) or `every` (repeating)".into());
            }
            _ => {
                return Err(
                    "schedule.add takes `at` alone, or `every` with an optional `time`".into(),
                );
            }
        };
        if next <= now_ms {
            return Err("schedule.add: that time has passed".into());
        }
        if self.store.schedule_count()? >= MAX_SCHEDULES {
            return Err(format!(
                "schedule.add: there are already {MAX_SCHEDULES} schedules; remove one first"
            ));
        }
        let name = schedule_name(str_arg("name"), prompt);
        let id = self.store.schedule_add(&name, prompt, next, every)?;
        let every = match every {
            Some(e) => format!(", then every {}", schedule::duration_text(e)),
            None => String::new(),
        };
        Ok(format!(
            "Scheduled \"{name}\" (#{id}), next at {}{every}.",
            schedule::local(next, self.offset_minutes)
        ))
    }

    fn schedule_list(&self) -> Result<String, String> {
        let all = self.store.schedules()?;
        if all.is_empty() {
            return Ok("No schedules.".into());
        }
        let lines: Vec<String> = all
            .iter()
            .map(|s| schedule_line(s, self.offset_minutes))
            .collect();
        Ok(lines.join("\n"))
    }

    fn schedule_remove(&self, args: &Value) -> Result<String, String> {
        let id = id_arg(args, "schedule.remove")?;
        if self.store.schedule_remove(id)? {
            Ok(format!("Removed schedule #{id}."))
        } else {
            Err(format!("schedule.remove: no schedule #{id}"))
        }
    }

    fn save(&self, args: &Value) -> Result<String, String> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or("memory.save needs a `text`")?;
        if text.len() > MAX_MEMORY_BYTES {
            return Err(format!(
                "memory.save: at most {MAX_MEMORY_BYTES} bytes, got {}; save something shorter",
                text.len()
            ));
        }
        let pinned = args.get("pinned").and_then(Value::as_bool).unwrap_or(false);
        if self.store.memory_count()? >= MAX_MEMORIES {
            return Err(format!(
                "memory.save: memory is full ({MAX_MEMORIES} memories). Tell the user, who can \
                 remove some with /forget <id>, or forget old ones with memory.forget."
            ));
        }
        let id = self.store.memory_save(text, pinned)?;
        Ok(format!(
            "Saved as memory #{id}{}.",
            if pinned { ", pinned" } else { "" }
        ))
    }

    fn search(&self, args: &Value) -> Result<String, String> {
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        let count = args
            .get("count")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_SEARCH, |n| n as usize)
            .clamp(1, MAX_SEARCH);
        let found = self.store.memory_search(query, count)?;
        if found.is_empty() {
            return Ok(format!("No saved memory matches {query:?}."));
        }
        let lines: Vec<String> = found.iter().map(memory_line).collect();
        Ok(format!(
            "Saved memories (the user's notes):\n{}",
            lines.join("\n")
        ))
    }

    fn forget(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("id")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str()?.trim_start_matches('#').parse().ok())
            })
            .ok_or("memory.forget needs an `id`")?;
        if self.store.memory_forget(id)? {
            Ok(format!("Forgot memory #{id}."))
        } else {
            Err(format!("memory.forget: no memory #{id}"))
        }
    }
}

fn spec(name: &str, description: &str, schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        schema,
        scope: Scope::DmOnly,
    }
}

impl Tools for Assistant {
    fn list(&self) -> Vec<ToolSpec> {
        vec![
            spec(
                "memory.forget",
                "Deletes a saved memory by its ID.",
                json!({
                    "type": "object",
                    "properties": {"id": {"type": "integer"}},
                    "required": ["id"]
                }),
            ),
            spec(
                "memory.save",
                "Saves a lasting fact, preference or note for the user, kept between \
                 conversations. Pinned memories are shown to you in every conversation; \
                 pin only what is short and almost always relevant.",
                json!({
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The memory, self-contained, at most 2000 bytes."
                        },
                        "pinned": {"type": "boolean", "description": "Default false."}
                    },
                    "required": ["text"]
                }),
            ),
            spec(
                "memory.search",
                "Searches saved memories by words; returns their IDs and text, best match \
                 first. An empty query lists the newest.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "count": {
                            "type": "integer",
                            "description": "Most results, 1 to 20. Default 5."
                        }
                    },
                    "required": ["query"]
                }),
            ),
            spec(
                "schedule.add",
                &format!(
                    "Schedules a prompt for later. At that time it is posted in this DM as \
                     \"[scheduled: <name>] <prompt>\" and you answer it then, with your tools. \
                     Give `at` for once (\"in 20m\", \"09:00\", \"2026-11-13 09:00\"), or \
                     `every` (\"30m\", \"1d\", at least 5m) with an optional `time` of day \
                     (\"09:00\"). Times are {}. Write the prompt as an instruction to \
                     yourself.",
                    self.zone()
                ),
                json!({
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string"},
                        "name": {"type": "string", "description": "A short label."},
                        "at": {"type": "string"},
                        "every": {"type": "string"},
                        "time": {"type": "string"}
                    },
                    "required": ["prompt"]
                }),
            ),
            spec(
                "schedule.list",
                "Lists the schedules with their IDs and next times.",
                json!({"type": "object", "properties": {}}),
            ),
            spec(
                "schedule.remove",
                "Removes a schedule by its ID.",
                json!({
                    "type": "object",
                    "properties": {"id": {"type": "integer"}},
                    "required": ["id"]
                }),
            ),
        ]
    }

    fn call<'a>(
        &'a self,
        _: &'a CallContext,
        name: &'a str,
        args: Value,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            match name {
                "memory.save" => self.save(&args),
                "memory.search" => self.search(&args),
                "memory.forget" => self.forget(&args),
                "schedule.add" => self.schedule_add(&args, triad_core::state::now_ms()),
                "schedule.list" => self.schedule_list(),
                "schedule.remove" => self.schedule_remove(&args),
                _ => Err(format!("no tool named {name}")),
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A store in memory, for tests.
    #[derive(Default)]
    pub struct MemStore(pub Mutex<Vec<Memory>>, pub Mutex<Vec<Schedule>>);

    impl Store for MemStore {
        fn schedule_count(&self) -> Result<u64, String> {
            Ok(self.1.lock().unwrap().len() as u64)
        }
        fn schedule_add(
            &self,
            name: &str,
            prompt: &str,
            next_at_ms: u64,
            every_ms: Option<u64>,
        ) -> Result<i64, String> {
            let mut rows = self.1.lock().unwrap();
            let id = rows.last().map_or(1, |s| s.id + 1);
            rows.push(Schedule {
                id,
                name: name.into(),
                prompt: prompt.into(),
                next_at_ms,
                every_ms,
            });
            Ok(id)
        }
        fn schedules(&self) -> Result<Vec<Schedule>, String> {
            Ok(self.1.lock().unwrap().clone())
        }
        fn schedule_remove(&self, id: i64) -> Result<bool, String> {
            let mut rows = self.1.lock().unwrap();
            let before = rows.len();
            rows.retain(|s| s.id != id);
            Ok(rows.len() < before)
        }
        fn memory_count(&self) -> Result<u64, String> {
            Ok(self.0.lock().unwrap().len() as u64)
        }
        fn memory_save(&self, text: &str, pinned: bool) -> Result<i64, String> {
            let mut rows = self.0.lock().unwrap();
            let id = rows.last().map_or(1, |m| m.id + 1);
            rows.push(Memory {
                id,
                text: text.into(),
                pinned,
                created_at_ms: 0,
            });
            Ok(id)
        }
        fn memory_search(&self, query: &str, count: usize) -> Result<Vec<Memory>, String> {
            let q = query.to_lowercase();
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .rev()
                .filter(|m| m.text.to_lowercase().contains(&q))
                .take(count)
                .cloned()
                .collect())
        }
        fn memory_forget(&self, id: i64) -> Result<bool, String> {
            let mut rows = self.0.lock().unwrap();
            let before = rows.len();
            rows.retain(|m| m.id != id);
            Ok(rows.len() < before)
        }
        fn memory_pinned(&self) -> Result<Vec<Memory>, String> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.pinned)
                .cloned()
                .collect())
        }
    }

    #[tokio::test]
    async fn memory_tools() {
        let store = Arc::new(MemStore::default());
        let tools = Assistant::new(store.clone(), 0);
        assert!(tools.list().iter().all(|t| t.scope == Scope::DmOnly));
        let cx = CallContext::dm();
        let call = |name: &'static str, args: Value| {
            let tools = &tools;
            let cx = &cx;
            async move { tools.call(cx, name, args).await }
        };
        let saved = call(
            "memory.save",
            json!({"text": "Plumber: Dana", "pinned": true}),
        );
        assert_eq!(saved.await.unwrap(), "Saved as memory #1, pinned.");
        let found = call("memory.search", json!({"query": "plumber"})).await;
        assert!(
            found
                .unwrap()
                .contains("#1 (1970-01-01, pinned) Plumber: Dana")
        );
        let long = call("memory.save", json!({"text": "x".repeat(2001)})).await;
        assert!(long.unwrap_err().contains("at most 2000 bytes"));
        assert!(call("memory.save", json!({})).await.is_err());
        let gone = call("memory.forget", json!({"id": "#1"})).await;
        assert_eq!(gone.unwrap(), "Forgot memory #1.");
        assert!(call("memory.forget", json!({"id": 1})).await.is_err());
        let none = call("memory.search", json!({"query": "plumber"})).await;
        assert!(none.unwrap().starts_with("No saved memory"));
    }

    #[test]
    fn schedule_tools() {
        /// 2026-11-12 15:04:05 UTC.
        const NOW: u64 = 1_794_495_845_000;
        let store = Arc::new(MemStore::default());
        let tools = Assistant::new(store.clone(), 60);
        let add = |args: Value| tools.schedule_add(&args, NOW);
        let out = add(json!({"prompt": "Check the build.", "every": "1d", "time": "09:00"}));
        assert_eq!(
            out.unwrap(),
            "Scheduled \"Check the build.\" (#1), next at 2026-11-13 09:00 UTC+01:00, \
             then every 1d."
        );
        let out = add(json!({"prompt": "Tea", "name": "[a]\nb", "at": "in 2s"}));
        assert!(out.unwrap().starts_with("Scheduled \"a b\" (#2)"));
        for (args, why) in [
            (json!({"at": "in 1m"}), "needs a `prompt`"),
            (json!({"prompt": "x"}), "needs `at`"),
            (json!({"prompt": "x", "every": "1m"}), "shortest interval"),
            (
                json!({"prompt": "x", "at": "in 1m", "every": "1h"}),
                "takes `at` alone",
            ),
            (
                json!({"prompt": "x", "at": "2020-01-01 00:00"}),
                "has passed",
            ),
            (json!({"prompt": "x", "at": "whenever"}), "not a time"),
        ] {
            let e = add(args).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
        let list = tools.schedule_list().unwrap();
        assert!(list.contains("#1 Check the build.: next 2026-11-13 09:00 UTC+01:00, every 1d."));
        assert!(tools.schedule_remove(&json!({"id": 2})).is_ok());
        assert!(tools.schedule_remove(&json!({"id": 2})).is_err());
        for _ in 0..49 {
            add(json!({"prompt": "x", "at": "in 1h"})).unwrap();
        }
        assert!(
            add(json!({"prompt": "x", "at": "in 1h"}))
                .unwrap_err()
                .contains("already 50")
        );
        assert!(
            tools
                .list()
                .iter()
                .any(|t| t.description.contains("Times are UTC+01:00"))
        );
    }

    #[test]
    fn pinned_notes_are_capped() {
        let m = |id, n, pinned| Memory {
            id,
            text: "x".repeat(n),
            pinned,
            created_at_ms: 0,
        };
        let notes = pinned_notes(&[
            m(1, 900, true),
            m(2, 5000, false),
            m(3, 900, true),
            m(4, 900, true),
        ]);
        assert_eq!(notes.len(), 2);
    }
}
