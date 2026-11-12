//! Message sizes. p2panda-core 0.8.0 decodes operation headers with a
//! 512-byte limit on byte strings, and spaces carry the encrypted message in
//! the header: with 460 bytes of text a message still arrived, with 470 it
//! was published but never synced, and the author's next publish failed
//! ("could not decode corrupted 'header' value"). Found while running the
//! bots on the real model. Text over `SEND_TEXT_BYTES` now goes as a long
//! message (`triad_core::long`): ciphertext in an operation body on a plain
//! stream, its key in a short space message.
//!
//! Every size up to `MAX_TEXT_BYTES` reaches the other member, the ones over
//! `SEND_TEXT_BYTES` as long messages; a longer line is refused locally, and
//! the author can still send after it.

use std::time::Duration;

use triad_core::envelope::{MAX_TEXT_BYTES, SEND_TEXT_BYTES};

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn sized(n: usize) -> String {
    format!("{n:04}-{}", "x".repeat(n - 5))
}

#[test]
fn messages_up_to_the_limit_arrive() {
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut a = Proc::spawn("A", da.path(), &["create", "--auto-accept", "--json"]);
    let invite = a.wait_for("ready", |v| is_event(v, "ready"))["invite"]
        .as_str()
        .unwrap()
        .to_string();
    let mut b = Proc::spawn("B", db.path(), &["join", &invite, "--json"]);
    let node_b = b.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    b.wait_for("joined", |v| {
        is_event(v, "joined") && v["member"] == node_b.as_str()
    });
    a.wait_for("B on A", |v| {
        is_event(v, "joined") && v["member"] == node_b.as_str()
    });

    let sizes = [
        100,
        300,
        SEND_TEXT_BYTES,
        SEND_TEXT_BYTES + 1,
        1000,
        MAX_TEXT_BYTES,
    ];
    for n in sizes {
        a.send(&sized(n));
    }
    // Over the limit: refused before it is published.
    a.send(&sized(MAX_TEXT_BYTES + 1));
    a.wait_for("refused", |v| {
        is_event(v, "error") && v["text"].as_str().unwrap().contains("limit is")
    });
    a.send("still-here");
    let got = receive(&mut b, sizes.len() + 1);
    let want: Vec<String> = sizes
        .iter()
        .map(|n| sized(*n))
        .chain(["still-here".to_string()])
        .collect();
    assert_eq!(got, want);
    for v in b.seen.iter().filter(|v| foreign_text(v).is_some()) {
        let len = v["text"].as_str().unwrap().len();
        assert_eq!(v["long"], len > SEND_TEXT_BYTES, "{len} bytes");
    }
    let errors = a
        .drain(Duration::from_secs(2))
        .iter()
        .chain(b.seen.iter())
        .filter(|v| {
            v["error"]
                .as_str()
                .is_some_and(|e| e.contains("reasonable size"))
        })
        .count();
    assert_eq!(errors, 0, "header decoding failed");
    a.quit();
    b.quit();
    let _ = BIN;
}
