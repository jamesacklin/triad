//! A3: the personal bot's schedules, on the mock brain.
//!
//! A schedule two seconds out posts its prefixed line once in `#agent`,
//! which gets one reply. Killed with SIGKILL after a fire is recorded and
//! restarted, it does not fire again. A one-shot made overdue while the
//! session was down fires once on start; one more than a day overdue is
//! dropped; a recurring one fires once and moves forward by whole intervals.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use triad_core::state::{State, now_ms};

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

const HOUR: u64 = 3_600_000;

fn bot_text(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["bot"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

/// A line this node posted (not the bot) in the DM.
fn own_line(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["bot"] == false && v["mine"] == true)
        .then(|| v["text"].as_str().unwrap().to_string())
}

fn start(dir: &Path, env: &[(&str, &str)], restart: bool) -> Proc {
    let mut p = Proc::spawn_env("A", dir, &["chat", "--json"], env);
    let ready = p.wait_for("ready", |v| is_event(v, "ready"));
    assert_eq!(ready["channel"], "agent");
    let me = ready["node"].as_str().unwrap().to_string();
    if restart
        && !p
            .seen
            .iter()
            .any(|v| is_event(v, "joined") && v["member"] == me.as_str())
    {
        p.wait_for("space open", |v| {
            is_event(v, "joined") && v["member"] == me.as_str()
        });
    }
    p
}

fn count(events: &[Value], f: impl Fn(&Value) -> bool) -> usize {
    events.iter().filter(|v| f(v)).count()
}

#[test]
fn schedules_fire_once_across_restarts() {
    let work = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = work.path().join("script");
    std::fs::write(
        &script,
        "remind me once => call schedule.add {\"name\": \"standup\", \"prompt\": \"Check the build.\", \"at\": \"in 2s\"} => {result}\n\
         remind again => call schedule.add {\"name\": \"again\", \"prompt\": \"Again.\", \"at\": \"in 2s\"} => {result}\n\
         [scheduled: standup] => Time to check the build.\n",
    )
    .unwrap();
    let env = [
        ("TRIAD_AGENT_BRAIN", "mock"),
        ("TRIAD_AGENT_MOCK_SCRIPT", script.to_str().unwrap()),
        ("TRIAD_AGENT_BUILTIN", "off"),
    ];
    let line = "[scheduled: standup] Check the build.";

    // A schedule two seconds out: one line, one reply.
    let mut a = start(dir.path(), &env, false);
    a.send("remind me once");
    let added = a.wait_for("added", |v| bot_text(v).is_some());
    let added = bot_text(&added).unwrap();
    assert!(
        added.starts_with("Scheduled \"standup\" (#1), next at "),
        "{added}"
    );
    let fired = a.wait_for("fired", |v| is_event(v, "schedule_fired"));
    assert_eq!(fired["name"], "standup");
    let late = fired["late_ms"].as_u64().unwrap();
    assert!(late < 2_000, "fired {late} ms late");
    a.wait_for("the line", |v| own_line(v).as_deref() == Some(line));
    a.wait_for("its reply", |v| {
        bot_text(v).as_deref() == Some("Time to check the build.")
    });
    a.drain(Duration::from_secs(4));
    assert_eq!(count(&a.seen, |v| own_line(v).as_deref() == Some(line)), 1);
    assert_eq!(
        count(&a.seen, |v| bot_text(v).as_deref()
            == Some("Time to check the build.")),
        1
    );
    a.send("/schedules");
    let list = a.wait_for("schedules", |v| is_event(v, "schedules"));
    assert_eq!(
        list["schedules"],
        serde_json::json!([]),
        "a one-shot is gone"
    );

    // Killed right after a fire is recorded: never a second fire.
    a.send("remind again");
    a.wait_for("fired again", |v| {
        is_event(v, "schedule_fired") && v["name"] == "again"
    });
    a.kill9();
    let mut a = start(dir.path(), &env, true);
    a.drain(Duration::from_secs(5));
    assert_eq!(
        count(&a.seen, |v| is_event(v, "schedule_fired")),
        0,
        "fired again after the restart"
    );
    assert!(
        count(&a.seen, |v| own_line(v).as_deref()
            == Some("[scheduled: again] Again."))
            <= 1
    );
    a.send("/schedules");
    let list = a.wait_for("schedules", |v| is_event(v, "schedules"));
    assert_eq!(list["schedules"], serde_json::json!([]));
    a.quit();

    // Overdue while the session was down.
    let now = now_ms();
    let hourly_due = now - 7 * HOUR / 2;
    let (late, stale, hourly) = {
        let state = State::open(dir.path()).unwrap();
        (
            state
                .schedule_add("late", "Late.", now - HOUR, None)
                .unwrap(),
            state
                .schedule_add("stale", "Stale.", now - 25 * HOUR, None)
                .unwrap(),
            state
                .schedule_add("hourly", "Hourly.", hourly_due, Some(HOUR))
                .unwrap(),
        )
    };
    let mut a = start(dir.path(), &env, true);
    // Fired in the order they were due; wait for both lines.
    a.wait_for("late line", |v| {
        own_line(v).as_deref() == Some("[scheduled: late] Late.")
    });
    a.drain(Duration::from_secs(4));
    for (text, n) in [
        ("[scheduled: late] Late.", 1),
        ("[scheduled: hourly] Hourly.", 1),
        ("[scheduled: stale] Stale.", 0),
    ] {
        assert_eq!(
            count(&a.seen, |v| own_line(v).as_deref() == Some(text)),
            n,
            "{text}"
        );
    }
    let fired = |name: &str| {
        count(&a.seen, |v| {
            is_event(v, "schedule_fired") && v["name"] == name
        })
    };
    assert_eq!((fired("late"), fired("hourly"), fired("stale")), (1, 1, 0));
    assert_eq!(
        count(&a.seen, |v| is_event(v, "schedule_missed")
            && v["name"] == "stale"),
        1
    );
    a.quit();

    let state = State::open(dir.path()).unwrap();
    assert!(state.schedule(late).unwrap().is_none());
    assert!(state.schedule(stale).unwrap().is_none());
    let row = state.schedule(hourly).unwrap().unwrap();
    // Missed three and a half times: moved forward by four whole intervals.
    assert_eq!(row.next_at_ms, hourly_due + 4 * HOUR);
    assert!(row.next_at_ms > now_ms());
    assert!(row.last_fired_ms.is_some());
    let _ = BIN;
}
