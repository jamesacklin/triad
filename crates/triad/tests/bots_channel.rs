//! H3: the channel bot, talking only, on the mock brain. Host A creates a
//! channel with `--agent`; B and C are members.
//!
//! B and C see `bot_on` from A on joining. B's `@bot` line gets one reply,
//! shown as the bot's; lines without `@bot` get none. A bot reply that
//! arrives while a join request waits at A's prompt is not taken as its
//! answer. After `/block B`, B gets none and C still does. With the
//! per-asker limit at 2, C's third ask is refused. `/bot on` typed by B
//! fails locally. A `BotOn` injected by C is not accepted by A or B. After
//! `/bot off`, nobody is answered. B asks while A is offline: A returns
//! inside the age limit and answers once; returning after it, A does not.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

fn bot_reply(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["bot"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

/// Waits for the bot's reply to `ask` (the mock echoes it).
fn wait_reply(p: &mut Proc, ask: &str) -> Value {
    let want = format!("echo: {ask}");
    if let Some(v) = p
        .seen
        .iter()
        .find(|v| bot_reply(v).as_deref() == Some(&want))
    {
        return v.clone();
    }
    p.wait_for(&format!("reply to {ask:?}"), |v| {
        bot_reply(v).as_deref() == Some(&want)
    })
}

fn replies_to(p: &Proc, ask: &str) -> usize {
    let want = format!("echo: {ask}");
    p.seen
        .iter()
        .filter(|v| bot_reply(v).as_deref() == Some(&want))
        .count()
}

fn wait_job(p: &mut Proc, state: &str) -> Value {
    p.wait_for(&format!("job {state}"), |v| {
        is_event(v, "job") && v["state"] == state
    })
}

fn host_env(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![
        ("TRIAD_AGENT_BRAIN", "mock"),
        ("TRIAD_AGENT_ASKER_PER_HOUR", "2"),
    ];
    env.extend_from_slice(extra);
    env
}

fn join(name: &'static str, dir: &Path, invite: &str, env: &[(&str, &str)]) -> (Proc, String) {
    let mut p = Proc::spawn_env(name, dir, &["join", invite, "--json"], env);
    let id = p.wait_for("ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    (p, id)
}

fn wait_member(p: &mut Proc, id: &str) {
    p.wait_for("member joined", |v| {
        is_event(v, "joined") && v["member"] == id
    });
}

#[test]
fn channel_bot_talking_only() {
    let (da, db, dc) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );

    // A hosts, without --auto-accept, so a join request can be held open.
    let mut a = Proc::spawn_env(
        "A",
        da.path(),
        &["create", "--name", "garden", "--agent", "--json"],
        &host_env(&[]),
    );
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    let invite = ready["invite"].as_str().unwrap().to_string();
    let node_a = ready["node"].as_str().unwrap().to_string();
    a.wait_for("own bot_on", |v| {
        is_event(v, "bot_on") && v["host"] == node_a.as_str()
    });

    let (mut b, node_b) = join("B", db.path(), &invite, &[]);
    a.wait_for("B's join request", |v| is_event(v, "join_request"));
    a.send("y");
    wait_member(&mut b, &node_b);
    let on = b.wait_for("bot_on on B", |v| is_event(v, "bot_on"));
    assert_eq!(on["host"], node_a.as_str());
    assert_eq!(on["provider"], "mock");

    // B asks while C's join request waits at A's prompt. The reply is
    // published by A's bot and never read as the answer to the prompt.
    let (mut c, node_c) = join("C", dc.path(), &invite, &[("TRIAD_AGENT_TEST_HOOKS", "1")]);
    a.wait_for("C's join request", |v| {
        is_event(v, "join_request") && v["member"] == node_c.as_str()
    });
    b.send("@bot hello from b");
    let r = wait_reply(&mut b, "@bot hello from b");
    assert_eq!(r["author"], node_a.as_str(), "the reply is the host's");
    assert_eq!(r["mine"], false);
    wait_reply(&mut a, "@bot hello from b");
    a.send("y");
    wait_member(&mut c, &node_c);
    c.wait_for("bot_on on C", |v| {
        is_event(v, "bot_on") && v["host"] == node_a.as_str()
    });
    wait_member(&mut a, &node_c);

    // Lines without @bot get no reply. (B keeps its second answer for the
    // offline ask below: the per-asker limit is 2.)
    b.send("no address here");
    b.wait_for("own line", |v| {
        own_text(v).as_deref() == Some("no address here")
    });
    c.wait_for("B's line on C", |v| {
        foreign_text(v).as_deref() == Some("no address here")
    });
    let a_events = a.drain(Duration::from_secs(4));
    assert!(
        !a_events.iter().any(|v| is_event(v, "job")),
        "an unaddressed line made a job: {a_events:?}"
    );
    b.drain(Duration::from_secs(1));
    assert_eq!(replies_to(&b, "no address here"), 0);
    assert_eq!(replies_to(&b, "@bot hello from b"), 1, "answered once");

    // /bot on by a member who did not create the channel fails locally.
    b.send("/bot on");
    b.wait_for("bot on refused", |v| {
        is_event(v, "error") && v["text"].as_str().unwrap().contains("creator")
    });

    // A BotOn injected by C (no Manage access) is not accepted, and a bot
    // flag from C is shown as an ordinary message.
    c.send("/debug-bot-on");
    for p in [&mut a, &mut b] {
        let rej = p.wait_for("bot_rejected", |v| is_event(v, "bot_rejected"));
        assert_eq!(rej["author"], node_c.as_str());
    }
    c.send("/debug-bot-says forged reply");
    let forged = b.wait_for("forged message", |v| {
        is_event(v, "message") && v["text"] == "forged reply"
    });
    assert_eq!(forged["bot"], false);
    assert_eq!(forged["bot_claim"], "not accepted");
    b.send("/bot");
    let info = b.wait_for("bot info", |v| is_event(v, "bot"));
    assert_eq!(info["host"], node_a.as_str(), "C did not become the host");
    assert_eq!(info["enabled"], true);

    // /block B: B gets no reply, C still does.
    a.send(&format!("/block {}", &node_b[..8]));
    a.wait_for("blocked", |v| is_event(v, "blocked"));
    b.send("@bot blocked ask");
    let refused = wait_job(&mut a, "refused");
    assert_eq!(refused["reason"], "blocked");
    assert_eq!(refused["asker"], node_b.as_str());
    c.send("@bot c ask 1");
    wait_reply(&mut c, "@bot c ask 1");

    // Per-asker limit 2: C's third ask is refused.
    c.send("@bot c ask 2");
    wait_reply(&mut c, "@bot c ask 2");
    c.send("@bot c ask 3");
    let refused = wait_job(&mut a, "refused");
    assert_eq!(refused["reason"], "asker limit");
    assert_eq!(refused["asker"], node_c.as_str());
    c.drain(Duration::from_secs(3));
    b.drain(Duration::from_secs(1));
    assert_eq!(replies_to(&b, "@bot blocked ask"), 0);
    assert_eq!(replies_to(&c, "@bot c ask 3"), 0);
    a.send(&format!("/unblock {}", &node_b[..8]));
    a.wait_for("unblocked", |v| is_event(v, "unblocked"));

    // /bot off: nobody is answered; a client refuses @bot with no bot.
    a.send("/bot off");
    for p in [&mut a, &mut b, &mut c] {
        p.wait_for("bot_off", |v| {
            is_event(v, "bot_off") && v["host"] == node_a.as_str()
        });
    }
    b.send("@bot anyone there");
    b.wait_for("no bot", |v| {
        is_event(v, "error") && v["text"].as_str().unwrap().contains("has no bot")
    });
    a.send("@bot host asks too");
    a.wait_for("no bot on host", |v| {
        is_event(v, "error") && v["text"].as_str().unwrap().contains("has no bot")
    });

    // Back on, then A goes offline. B's ask waits for A, and A answers it
    // once on return, inside the age limit.
    a.send("/bot on");
    b.wait_for("bot_on again", |v| {
        is_event(v, "bot_on") && v["host"] == node_a.as_str()
    });
    a.wait_for("own bot_on again", |v| is_event(v, "bot_on"));
    a.quit();
    b.send("@bot while you were out");
    b.wait_for("own ask stored", |v| {
        own_text(v).as_deref() == Some("@bot while you were out")
    });
    let mut a = Proc::spawn_env(
        "A",
        da.path(),
        &["chat", "--agent", "--json"],
        &host_env(&[]),
    );
    a.wait_for("ready", |v| is_event(v, "ready"));
    wait_reply(&mut b, "@bot while you were out");
    let done = wait_job(&mut a, "done");
    eprintln!(
        "offline ask answered {} ms after it was written",
        done["since_ask_ms"]
    );
    b.drain(Duration::from_secs(5));
    assert_eq!(replies_to(&b, "@bot while you were out"), 1);
    a.quit();

    // Returning after the age limit: no answer.
    b.send("@bot too late");
    b.wait_for("own ask stored", |v| {
        own_text(v).as_deref() == Some("@bot too late")
    });
    std::thread::sleep(Duration::from_secs(7));
    let mut a = Proc::spawn_env(
        "A",
        da.path(),
        &["chat", "--agent", "--json"],
        &host_env(&[("TRIAD_AGENT_MAX_AGE_SECS", "5")]),
    );
    a.wait_for("ready", |v| is_event(v, "ready"));
    let refused = wait_job(&mut a, "refused");
    assert_eq!(refused["reason"], "too old");
    b.drain(Duration::from_secs(8));
    assert_eq!(replies_to(&b, "@bot too late"), 0);

    c.quit();
    b.quit();
    a.quit();
    let _ = BIN;
}
