//! H9 (stretch): encryption at rest for `state.sqlite` with SQLCipher, the
//! key from `TRIAD_STATE_KEY` at start. Only built with the `sqlcipher`
//! feature:
//!
//!   cargo test -p triad --features sqlcipher --test bots_at_rest
//!
//! The DM's lines and the bot's replies, decrypted into `state.sqlite`, must
//! not appear in it as plaintext, scanned as `inspect` scans
//! `p2panda.sqlite`. A wrong key fails at start instead of reading garbage.
#![cfg(feature = "sqlcipher")]

use std::path::Path;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn found(dir: &Path, needle: &str) -> usize {
    ["", "-wal", "-shm", "-journal"]
        .iter()
        .filter_map(|s| std::fs::read(dir.join(format!("state.sqlite{s}"))).ok())
        .map(|b| {
            b.windows(needle.len())
                .filter(|w| *w == needle.as_bytes())
                .count()
        })
        .sum()
}

#[test]
fn state_sqlite_holds_no_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let key = ("TRIAD_STATE_KEY", "correct horse battery staple");
    let env = [("TRIAD_AGENT_BRAIN", "mock"), key];
    let mut a = Proc::spawn_env("A", dir.path(), &["chat", "--json"], &env);
    a.wait_for("ready", |v| is_event(v, "ready"));
    a.send("at-rest-secret-line");
    a.wait_for("reply", |v| {
        is_event(v, "message") && v["text"] == "echo: at-rest-secret-line"
    });
    a.quit();

    assert_eq!(
        found(dir.path(), "at-rest-secret-line"),
        0,
        "plaintext in state.sqlite"
    );
    assert_eq!(
        found(dir.path(), "SQLite format 3"),
        0,
        "state.sqlite has a plain header"
    );

    // The history is readable with the key...
    let out = std::process::Command::new(BIN)
        .env(key.0, key.1)
        .arg("--data-dir")
        .arg(dir.path())
        .arg("history")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(text.contains("my bot: echo: at-rest-secret-line"), "{text}");

    // ...and not without it, or with a wrong one.
    for wrong in [None, Some("wrong key")] {
        let mut cmd = std::process::Command::new(BIN);
        cmd.env_remove(key.0);
        if let Some(w) = wrong {
            cmd.env(key.0, w);
        }
        let out = cmd
            .arg("--data-dir")
            .arg(dir.path())
            .arg("history")
            .output()
            .unwrap();
        assert!(!out.status.success(), "history opened with key {wrong:?}");
    }
}
