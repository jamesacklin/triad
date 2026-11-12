//! The personal bot's own tools: memory (`memory.*`). They are offered only
//! in the DM ([`Scope::DmOnly`]), so nothing a channel says is saved and no
//! memory reaches a channel turn.
//!
//! The rows live in the host's `state.sqlite`, which this crate never opens:
//! the binary implements [`Store`] over it, as it does [`crate::tools::Audit`].

use std::sync::Arc;

use serde_json::{Value, json};

use crate::tools::{BoxFuture, CallContext, Scope, ToolSpec, Tools};

/// Most bytes of one memory.
pub const MAX_MEMORY_BYTES: usize = 2_000;
/// Most memories kept; a save past this is refused.
pub const MAX_MEMORIES: u64 = 2_000;
/// Most bytes of pinned memories put in a DM turn's system prompt.
pub const MAX_PINNED_BYTES: usize = 2_000;
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

/// Where the personal bot's rows are kept. Errors are text for the model.
pub trait Store: Send + Sync {
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
}

impl Assistant {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Assistant { store }
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
    pub struct MemStore(pub Mutex<Vec<Memory>>);

    impl Store for MemStore {
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
        let tools = Assistant::new(store.clone());
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
