//! `/me`: an IRC-style action travels as a flagged message, is stored as
//! one, and prints as `* name text`.

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

#[test]
fn me_sends_an_action() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = Proc::spawn("A", dir.path(), &["create", "--name", "garden", "--json"]);
    a.wait_for("ready", |v| is_event(v, "ready"));
    a.send("/me waves at everyone");
    let m = a.wait_for("action", |v| is_event(v, "message"));
    assert_eq!(m["text"], "waves at everyone");
    assert_eq!(m["action"], true, "{m}");
    a.send("plain words");
    let m = a.wait_for("plain", |v| is_event(v, "message"));
    assert_eq!(m["action"], false, "{m}");
    a.send("/me");
    let e = a.wait_for("usage", |v| is_event(v, "error"));
    assert!(e["text"].as_str().unwrap().contains("/me <action>"), "{e}");
    a.quit();

    let (code, out) = run_cmd(dir.path(), &["history", "--channel", "garden"]);
    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].starts_with("* ") && lines[0].ends_with(" waves at everyone"),
        "{out}"
    );
    assert!(lines[1].ends_with(": plain words"), "{out}");
}
