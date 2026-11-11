//! H4 and H6: tools and permissions, on the mock brain. Host A, admin B
//! (promoted) and member C. The mock script makes each ask attempt
//! `calendar.create_event` (or `linear.create_issue`).
//!
//! A's and B's calls reach the tool; C's does not, C's turn was offered no
//! tools, and C is told to ask A. After `/grant C calendar.create_event`,
//! C's call succeeds and `linear.create_issue` still does not. After
//! `/revoke`, it fails again. `/audit` lists each call with its asker.
//!
//! The same test runs against fake tools (H4) and against the MCP fixture
//! server over stdio and over HTTP (H6).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};

mod support;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_triad");

const SCRIPT: &str = "\
cal => call calendar.create_event {\"title\": \"hedge trimming\"} => Added ({result})
lin => call linear.create_issue {\"title\": \"fence\"} => Filed ({result})
";

fn bot_reply(v: &Value) -> Option<String> {
    (is_event(v, "message") && v["bot"] == true).then(|| v["text"].as_str().unwrap().to_string())
}

/// Lines in the tool log: one per call that reached a tool.
fn calls(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The mock log's turns, newest last.
fn turns(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Sends an ask and returns the bot's reply as seen by the asker.
fn ask(p: &mut Proc, text: &str) -> String {
    p.send(text);
    let op = p.wait_for("own ask", |v| own_text(v).as_deref() == Some(text))["op"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = op;
    let v = p.wait_for(&format!("reply to {text:?}"), |v| bot_reply(v).is_some());
    bot_reply(&v).unwrap()
}

struct Asked {
    reply: String,
    /// The ask's operation timestamp, in ms.
    ts_ms: u64,
}

/// Like [`ask`], also returning when the ask was written.
fn ask_at(p: &mut Proc, text: &str) -> Asked {
    p.send(text);
    let ts_ms = p.wait_for("own ask", |v| own_text(v).as_deref() == Some(text))["ts"]
        .as_u64()
        .unwrap();
    let v = p.wait_for(&format!("reply to {text:?}"), |v| bot_reply(v).is_some());
    Asked {
        reply: bot_reply(&v).unwrap(),
        ts_ms,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// How the host gets its tools.
enum Backend {
    Fake,
    Stdio,
    Http,
}

struct Tools {
    env: Vec<(String, String)>,
    /// Where each call that reaches a tool is logged.
    log: PathBuf,
    /// HTTP fixture servers to stop at the end.
    servers: Vec<Child>,
}

/// The MCP fixture server: `examples/mcp_fixture.rs`, built on first use.
fn fixture() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let path = Path::new(BIN)
                .parent()
                .unwrap()
                .join("examples")
                .join("mcp_fixture");
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let status = Command::new(cargo)
                .args(["build", "-q", "-p", "triad", "--example", "mcp_fixture"])
                .status()
                .expect("run cargo");
            assert!(status.success(), "building the MCP fixture");
            assert!(path.exists(), "{} missing", path.display());
            path
        })
        .clone()
}

fn http_fixture(server: &str, tool: &str, log: &Path) -> (Child, String) {
    let mut child = Command::new(fixture())
        .args(["--server", server, "--tool", tool, "--log"])
        .arg(log)
        .args(["--http", "127.0.0.1:0", "--token", "fixture-token"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("start fixture");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line
        .trim()
        .strip_prefix("listening ")
        .expect("fixture url")
        .to_string();
    (child, url)
}

fn setup(backend: Backend, dir: &Path, host_dir: &Path) -> Tools {
    let log = dir.join("tool-calls.log");
    let mut env = vec![
        ("TRIAD_AGENT_BRAIN".into(), "mock".into()),
        (
            "TRIAD_AGENT_MOCK_SCRIPT".into(),
            dir.join("script").display().to_string(),
        ),
        (
            "TRIAD_AGENT_MOCK_LOG".into(),
            dir.join("mock.log").display().to_string(),
        ),
    ];
    std::fs::write(dir.join("script"), SCRIPT).unwrap();
    let mut servers = Vec::new();
    match backend {
        Backend::Fake => {
            let fake = dir.join("fake.json");
            std::fs::write(
                &fake,
                json!({"log": log, "servers": {"calendar": ["create_event"], "linear": ["create_issue"]}})
                    .to_string(),
            )
            .unwrap();
            env.push(("TRIAD_AGENT_TOOLS".into(), "fake".into()));
            env.push(("TRIAD_AGENT_FAKE_TOOLS".into(), fake.display().to_string()));
        }
        Backend::Stdio => {
            let fixture = fixture();
            let cfg = json!({"servers": [
                {"name": "calendar", "command": [fixture, "--server", "calendar", "--tool", "create_event", "--log", log, "--stdio"]},
                {"name": "linear", "command": [fixture, "--server", "linear", "--tool", "create_issue", "--log", log, "--stdio"]},
                // Down at start: reported, and the bot runs without it.
                {"name": "broken", "command": [dir.join("no-such-server")]},
            ]});
            std::fs::write(host_dir.join("mcp.json"), cfg.to_string()).unwrap();
        }
        Backend::Http => {
            let (cal, cal_url) = http_fixture("calendar", "create_event", &log);
            let (lin, lin_url) = http_fixture("linear", "create_issue", &log);
            servers.push(cal);
            servers.push(lin);
            let cfg = json!({"servers": [
                {"name": "calendar", "url": cal_url, "token_env": "FIXTURE_TOKEN"},
                {"name": "linear", "url": lin_url, "token_env": "FIXTURE_TOKEN"},
                // Down at start: nothing listens on port 9.
                {"name": "broken", "url": "http://127.0.0.1:9/mcp"},
            ]});
            std::fs::write(host_dir.join("mcp.json"), cfg.to_string()).unwrap();
            env.push(("FIXTURE_TOKEN".into(), "fixture-token".into()));
        }
    }
    Tools { env, log, servers }
}

fn run(backend: Backend) {
    let work = tempfile::tempdir().unwrap();
    let (da, db, dc) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let check_broken = !matches!(backend, Backend::Fake);
    let mut tools = setup(backend, work.path(), da.path());
    let env: Vec<(&str, &str)> = tools
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mock_log = work.path().join("mock.log");

    let mut a = Proc::spawn_env(
        "A",
        da.path(),
        &[
            "create",
            "--name",
            "garden",
            "--agent",
            "--auto-accept",
            "--json",
        ],
        &env,
    );
    let ready = a.wait_for("ready", |v| is_event(v, "ready"));
    if check_broken {
        let warning = a
            .seen
            .iter()
            .find(|v| is_event(v, "info") && v["text"].as_str().unwrap().contains("broken"))
            .cloned();
        assert!(warning.is_some(), "the down server is reported");
    }
    let invite = ready["invite"].as_str().unwrap().to_string();
    let node_a = ready["node"].as_str().unwrap().to_string();
    let on = a.wait_for("bot_on", |v| is_event(v, "bot_on"));
    assert_eq!(on["servers"], json!(["calendar", "linear"]), "{on}");

    let mut b = Proc::spawn("B", db.path(), &["join", &invite, "--json"]);
    let node_b = b.wait_for("B ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    b.wait_for("B joined", |v| {
        is_event(v, "joined") && v["member"] == node_b.as_str()
    });
    let mut c = Proc::spawn("C", dc.path(), &["join", &invite, "--json"]);
    let node_c = c.wait_for("C ready", |v| is_event(v, "ready"))["node"]
        .as_str()
        .unwrap()
        .to_string();
    c.wait_for("C joined", |v| {
        is_event(v, "joined") && v["member"] == node_c.as_str()
    });
    a.wait_for("C on A", |v| {
        is_event(v, "joined") && v["member"] == node_c.as_str()
    });
    a.send(&format!("/promote {node_b}"));
    for p in [&mut a, &mut b] {
        p.wait_for("B promoted", |v| {
            is_event(v, "promoted") && v["member"] == node_b.as_str() && v["access"] == "manage"
        });
    }

    // The host and the admin reach the tool.
    assert_eq!(ask(&mut a, "@bot cal from a"), "Added (ok)");
    assert_eq!(calls(&tools.log).len(), 1);
    let asked = ask_at(&mut b, "@bot cal from b");
    assert_eq!(asked.reply, "Added (ok)");
    assert_eq!(calls(&tools.log).len(), 2);
    // From B's @bot to the reply on C's screen, split into the parts A's
    // job reports. All three processes share one clock.
    let seen_on_c = c.wait_for("reply on C", |v| {
        bot_reply(v).as_deref() == Some("Added (ok)")
    });
    let seen_on_c_ms = now_ms();
    let done = a.wait_for("job done", |v| {
        is_event(v, "job") && v["state"] == "done" && v["asker"] == node_b.as_str()
    });
    let total = seen_on_c_ms.saturating_sub(asked.ts_ms);
    let since = done["since_ask_ms"].as_u64().unwrap();
    let turn = done["turn_ms"].as_u64().unwrap();
    eprintln!(
        "LATENCY total={total}ms sync_in~={}ms turn={turn}ms (model={}ms tool={}ms) sync_out~={}ms reply_op_ts={}",
        since.saturating_sub(turn),
        done["model_ms"],
        done["tool_ms"],
        total.saturating_sub(since),
        seen_on_c["ts"]
    );
    assert_eq!(calls(&tools.log)[1]["tool"], "calendar.create_event");
    assert_eq!(calls(&tools.log)[1]["args"]["title"], "hedge trimming");

    // A member does not, is offered nothing, and is told to ask the host.
    let reply = ask(&mut c, "@bot cal from c");
    assert_eq!(
        reply,
        format!(
            "I can't use calendar for you. Ask {} for access.",
            &node_a[..8]
        )
    );
    assert_eq!(calls(&tools.log).len(), 2, "C's call reached the tool");
    let t = turns(&mock_log);
    let c_turn = t
        .iter()
        .rev()
        .find(|t| t["ask"] == "@bot cal from c")
        .unwrap();
    assert_eq!(c_turn["tools"], json!([]), "C was offered tools");
    assert_eq!(c_turn["asker_tier"], "member");
    let a_turn = t.iter().find(|t| t["ask"] == "@bot cal from a").unwrap();
    assert_eq!(a_turn["tools"].as_array().unwrap().len(), 2);
    let b_turn = t.iter().find(|t| t["ask"] == "@bot cal from b").unwrap();
    assert_eq!(b_turn["asker_tier"], "admin");

    // A grant lets C use exactly that tool.
    a.send(&format!("/grant {} calendar.create_event", &node_c[..8]));
    a.wait_for("granted", |v| is_event(v, "granted"));
    assert_eq!(ask(&mut c, "@bot cal again from c"), "Added (ok)");
    assert_eq!(calls(&tools.log).len(), 3);
    let t = turns(&mock_log);
    let granted_turn = t
        .iter()
        .rev()
        .find(|t| t["ask"] == "@bot cal again from c")
        .unwrap();
    let offered: Vec<&str> = granted_turn["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(offered, ["calendar.create_event"]);
    let reply = ask(&mut c, "@bot lin from c");
    assert!(reply.starts_with("I can't use linear for you."), "{reply}");
    assert_eq!(calls(&tools.log).len(), 3);

    // Revoked: fails again.
    a.send(&format!("/revoke {}", &node_c[..8]));
    a.wait_for("revoked", |v| is_event(v, "revoked"));
    let reply = ask(&mut c, "@bot cal once more from c");
    assert!(
        reply.starts_with("I can't use calendar for you."),
        "{reply}"
    );
    assert_eq!(calls(&tools.log).len(), 3);

    // The audit log lists each call with its asker.
    a.send("/audit");
    let audit = a.wait_for("audit", |v| is_event(v, "audit"));
    let rows: Vec<(String, String, String)> = audit["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["asker"].as_str().unwrap()[..8].to_string(),
                c["tool"].as_str().unwrap().to_string(),
                c["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let (sa, sb, sc) = (&node_a[..8], &node_b[..8], &node_c[..8]);
    let want: Vec<(String, String, String)> = [
        (sa, "calendar.create_event", "ok"),
        (sb, "calendar.create_event", "ok"),
        (sc, "calendar.create_event", "denied"),
        (sc, "calendar.create_event", "ok"),
        (sc, "linear.create_issue", "denied"),
        (sc, "calendar.create_event", "denied"),
    ]
    .iter()
    .map(|(a, t, s)| (a.to_string(), t.to_string(), s.to_string()))
    .collect();
    assert_eq!(rows, want);
    // Members cannot read the audit log or edit grants.
    c.send("/audit");
    c.wait_for("audit refused", |v| {
        is_event(v, "error") && v["text"].as_str().unwrap().contains("only the host")
    });
    b.send(&format!("/grant {} linear.*", &node_c[..8]));
    b.wait_for("grant refused", |v| {
        is_event(v, "error") && v["text"].as_str().unwrap().contains("only the host")
    });

    c.quit();
    b.quit();
    a.quit();
    for s in &mut tools.servers {
        let _ = s.kill();
        let _ = s.wait();
    }
}

#[test]
fn tools_and_permissions_with_fake_tools() {
    run(Backend::Fake);
}

#[test]
#[ignore = "needs the MCP fixture server (H6)"]
fn tools_and_permissions_over_mcp_stdio() {
    run(Backend::Stdio);
}

#[test]
#[ignore = "needs the MCP fixture server (H6)"]
fn tools_and_permissions_over_mcp_http() {
    run(Backend::Http);
}
