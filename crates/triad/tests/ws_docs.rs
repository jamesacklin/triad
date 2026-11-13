//! W6: every event name the code emits is documented in API.md and has a
//! schema in openapi.json, and every request op has one too. Reads the
//! emit sites, so a new event without documentation fails here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Rust files of the binary, with any `#[cfg(test)] mod tests` cut off.
fn sources() -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![
        src.join("main.rs"),
        src.join("bots.rs"),
        src.join("topics.rs"),
    ];
    for entry in std::fs::read_dir(src.join("ws")).unwrap() {
        files.push(entry.unwrap().path());
    }
    files
        .iter()
        .map(|f| {
            let text = std::fs::read_to_string(f).unwrap();
            match text.find("#[cfg(test)]\nmod tests") {
                Some(at) => text[..at].to_string(),
                None => text,
            }
        })
        .collect()
}

/// The literals in `text` up to the first `;` or `)`.
fn literals(text: &str) -> Vec<String> {
    let end = text.find([';', ')']).unwrap_or(text.len());
    text[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// Event names at the emit sites: `"event": "x"`, `"event": if … { "x" }
/// else { "y" }`, `["event"] = json!("x")`, `insert("event".into(),
/// json!("x"))`, and names chosen ahead (`let event = if … { "x" } …`).
fn emitted() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for text in sources() {
        for line in text.lines() {
            let found = if let Some(at) = line.find("\"event\": ") {
                let rest = &line[at + "\"event\": ".len()..];
                if let Some(lit) = rest.strip_prefix('"') {
                    vec![lit.split('"').next().unwrap().to_string()]
                } else if rest.starts_with("if ") {
                    // Up to the `}` that closes the else branch.
                    let end = rest
                        .match_indices('}')
                        .nth(1)
                        .map_or(rest.len(), |(i, _)| i);
                    literals(&rest[..end])
                } else {
                    vec![]
                }
            } else if line.contains("[\"event\"] = json!(\"")
                || line.contains("insert(\"event\".into(), json!(\"")
            {
                let at = line.find("json!(\"").unwrap();
                literals(&line[at..])
            } else if line.trim_start().starts_with("let event = if ") {
                literals(line)
            } else {
                vec![]
            };
            names.extend(found);
        }
    }
    names
}

#[test]
fn ws_every_event_is_documented() {
    let names = emitted();
    for must in [
        "hello",
        "ready",
        "done",
        "history",
        "lagged",
        "message",
        "join_request",
    ] {
        assert!(names.contains(must), "the scan missed {must}: {names:?}");
    }
    assert!(names.len() >= 41, "too few events found: {names:?}");
    eprintln!("{} events: {names:?}", names.len());

    let api = std::fs::read_to_string(root().join("API.md")).unwrap();
    let missing: Vec<&String> = names
        .iter()
        .filter(|n| !api.contains(&format!("`{n}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "events missing from API.md: {missing:?}"
    );

    let spec: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("openapi.json")).unwrap())
            .unwrap();
    assert_eq!(spec["openapi"], "3.1.0");
    let schemas = &spec["components"]["schemas"];
    let mapping = schemas["Event"]["discriminator"]["mapping"]
        .as_object()
        .unwrap();
    let missing: Vec<&String> = names.iter().filter(|n| !mapping.contains_key(*n)).collect();
    assert!(
        missing.is_empty(),
        "events missing from openapi.json: {missing:?}"
    );
    for (name, target) in mapping {
        let schema = target.as_str().unwrap().rsplit('/').next().unwrap();
        assert!(
            schemas.get(schema).is_some(),
            "{name} maps to a missing schema"
        );
    }

    let ops = schemas["Request"]["discriminator"]["mapping"]
        .as_object()
        .unwrap();
    for op in ["auth", "send", "answer", "history", "snapshot", "command"] {
        assert!(ops.contains_key(op), "op {op} missing from openapi.json");
        assert!(
            api.contains(&format!("`{op}`")),
            "op {op} missing from API.md"
        );
    }
}
