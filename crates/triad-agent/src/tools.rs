//! Tools: the trait every tool source implements, the fake tools tests use,
//! and the gate every call passes through.
//!
//! The gate is applied twice. Tools the asker may not use are left out of
//! what the model is offered ([`offered`]), and every call is checked again
//! before it is sent ([`Gate::call`]). The model is never trusted to enforce
//! permissions.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use triad_core::envelope::truncate_text;

use crate::context::{Kind, TurnInput};
use crate::policy::ToolSet;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where a tool may be used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// In the DM and in channels, for askers whose tool set allows it.
    #[default]
    Any,
    /// Only in the personal bot's DM: never offered to a channel turn, and
    /// refused by the gate there.
    DmOnly,
}

/// One tool, named `server.tool`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments.
    pub schema: Value,
    /// Not part of what the model is given.
    #[serde(skip)]
    pub scope: Scope,
}

impl ToolSpec {
    pub fn server(&self) -> &str {
        self.name.split_once('.').map_or(&self.name, |(s, _)| s)
    }
}

/// Who a call is made for, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    pub kind: Kind,
    /// The channel's name (`agent` for the DM).
    pub channel: String,
    /// Short node ID of the asker.
    pub asker: String,
    pub asker_tier: String,
}

impl CallContext {
    pub fn of(input: &TurnInput) -> Self {
        CallContext {
            kind: input.kind,
            channel: input.channel.clone(),
            asker: input.asker.clone(),
            asker_tier: input.asker_tier.clone(),
        }
    }

    /// The host in its own DM, for tests.
    pub fn dm() -> Self {
        CallContext {
            kind: Kind::Dm,
            channel: "agent".into(),
            asker: "me".into(),
            asker_tier: "host".into(),
        }
    }
}

/// A source of tools: fake, MCP, built in, or none.
pub trait Tools: Send + Sync {
    fn list(&self) -> Vec<ToolSpec>;
    /// Calls `name` (`server.tool`) for the asker `cx` names. The result is
    /// text for the model.
    fn call<'a>(
        &'a self,
        cx: &'a CallContext,
        name: &'a str,
        args: Value,
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// The names of the servers a tool list covers, sorted, without repeats.
pub fn servers(tools: &[ToolSpec]) -> Vec<String> {
    let mut s: Vec<String> = tools.iter().map(|t| t.server().to_string()).collect();
    s.sort();
    s.dedup();
    s
}

/// Whether a tool may be used in a turn of `kind`.
pub fn in_scope(tool: &ToolSpec, kind: Kind) -> bool {
    tool.scope == Scope::Any || kind == Kind::Dm
}

/// The tools an asker is offered in a turn of `kind`: those its tool set
/// allows and whose scope covers the turn.
pub fn offered(all: &[ToolSpec], allowed: &ToolSet, kind: Kind) -> Vec<ToolSpec> {
    all.iter()
        .filter(|t| allowed.allows(&t.name) && in_scope(t, kind))
        .cloned()
        .collect()
}

/// No tools at all.
pub struct NoTools;

impl Tools for NoTools {
    fn list(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn call<'a>(
        &'a self,
        _: &'a CallContext,
        name: &'a str,
        _args: Value,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { Err(format!("no tool named {name}")) })
    }
}

/// Two tool sources as one. A tool of `second` whose server name `first`
/// also uses is left out, so no name is offered twice.
pub struct Both {
    first: Arc<dyn Tools>,
    second: Arc<dyn Tools>,
    specs: Vec<ToolSpec>,
    /// How many of `specs` are `first`'s.
    split: usize,
    /// Server names of `second` that `first` hides.
    pub hidden: Vec<String>,
}

impl Both {
    pub fn new(first: Arc<dyn Tools>, second: Arc<dyn Tools>) -> Self {
        let mut specs = first.list();
        let split = specs.len();
        let taken = servers(&specs);
        let theirs = second.list();
        let hidden: Vec<String> = servers(&theirs)
            .into_iter()
            .filter(|s| taken.contains(s))
            .collect();
        specs.extend(
            theirs
                .into_iter()
                .filter(|t| !hidden.iter().any(|h| h == t.server())),
        );
        Both {
            first,
            second,
            specs,
            split,
            hidden,
        }
    }
}

impl Tools for Both {
    fn list(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn call<'a>(
        &'a self,
        cx: &'a CallContext,
        name: &'a str,
        args: Value,
    ) -> BoxFuture<'a, Result<String, String>> {
        match self.specs.iter().position(|t| t.name == name) {
            Some(i) if i < self.split => self.first.call(cx, name, args),
            Some(_) => self.second.call(cx, name, args),
            None => Box::pin(async move { Err(format!("no tool named {name}")) }),
        }
    }
}

/// The file `TRIAD_AGENT_FAKE_TOOLS` names.
#[derive(Deserialize)]
struct FakeFile {
    /// Every call appends one JSON line here.
    log: PathBuf,
    /// Returned by every call.
    #[serde(default = "default_result")]
    result: String,
    /// Server name to tool names.
    servers: BTreeMap<String, Vec<String>>,
}

fn default_result() -> String {
    "ok".into()
}

/// Fake tools for tests: each call appends a line to a log file and returns
/// a fixed result.
pub struct FakeTools {
    log: PathBuf,
    result: String,
    specs: Vec<ToolSpec>,
    lock: Mutex<()>,
}

impl FakeTools {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file: FakeFile = serde_json::from_str(&text)
            .with_context(|| format!("parsing fake tools {}", path.display()))?;
        let mut specs = Vec::new();
        for (server, tools) in &file.servers {
            for tool in tools {
                let name = format!("{server}.{tool}");
                if !crate::policy::pattern_valid(&name) || tool == "*" {
                    bail!("invalid fake tool name {name}");
                }
                specs.push(ToolSpec {
                    name,
                    description: format!("Fake {tool} on {server}."),
                    schema: serde_json::json!({"type": "object", "additionalProperties": true}),
                    scope: Scope::Any,
                });
            }
        }
        Ok(FakeTools {
            log: file.log,
            result: file.result,
            specs,
            lock: Mutex::new(()),
        })
    }
}

impl Tools for FakeTools {
    fn list(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn call<'a>(
        &'a self,
        _: &'a CallContext,
        name: &'a str,
        args: Value,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            if !self.specs.iter().any(|s| s.name == name) {
                return Err(format!("no tool named {name}"));
            }
            let line = serde_json::json!({"tool": name, "args": args});
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log)
                .map_err(|e| format!("fake tool log: {e}"))?;
            writeln!(f, "{line}").map_err(|e| format!("fake tool log: {e}"))?;
            Ok(self.result.clone())
        })
    }
}

/// Where the gate records calls: the host's audit log.
pub trait Audit: Send + Sync {
    /// A call is about to be sent. Returns its audit ID. Called before the
    /// call goes out, so a crash leaves it recorded as sent.
    fn sent(&self, tool: &str, args: &Value) -> i64;
    /// The call returned.
    fn finished(&self, id: i64, ok: bool, result_bytes: usize);
    /// The call was refused by the gate and never sent.
    fn denied(&self, tool: &str, args: &Value, why: &str);
}

/// An audit log that keeps nothing, for tests.
pub struct NoAudit;

impl Audit for NoAudit {
    fn sent(&self, _: &str, _: &Value) -> i64 {
        0
    }
    fn finished(&self, _: i64, _: bool, _: usize) {}
    fn denied(&self, _: &str, _: &Value, _: &str) {}
}

/// What the brain calls tools through.
pub trait ToolCaller: Send + Sync {
    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, Result<String, String>>;
}

/// The most bytes of a tool's result the model is given. A web page or a
/// long listing is cut to this.
pub const MAX_RESULT_BYTES: usize = 24_000;

/// Why the gate refuses a DM-only tool in a channel.
pub const DM_ONLY: &str = "only available in the personal DM";

/// The permission gate for one turn.
pub struct Gate<'t> {
    tools: &'t dyn Tools,
    cx: CallContext,
    allowed: ToolSet,
    max_steps: usize,
    audit: &'t dyn Audit,
    steps: AtomicUsize,
    denied: Mutex<Vec<String>>,
    tool_time: Mutex<Duration>,
}

impl<'t> Gate<'t> {
    pub fn new(
        tools: &'t dyn Tools,
        cx: CallContext,
        allowed: ToolSet,
        max_steps: usize,
        audit: &'t dyn Audit,
    ) -> Self {
        Gate {
            tools,
            cx,
            allowed,
            max_steps,
            audit,
            steps: AtomicUsize::new(0),
            denied: Mutex::new(Vec::new()),
            tool_time: Mutex::new(Duration::ZERO),
        }
    }

    /// Tools the gate refused this turn, in order.
    pub fn denied(&self) -> Vec<String> {
        self.denied
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Time spent inside allowed tool calls.
    pub fn tool_time(&self) -> Duration {
        *self.tool_time.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn deny(&self, name: &str, args: &Value, why: &str) -> String {
        self.audit.denied(name, args, why);
        self.denied
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(name.to_string());
        format!("{name}: {why}")
    }
}

impl ToolCaller for Gate<'_> {
    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let step = self.steps.fetch_add(1, Ordering::SeqCst) + 1;
            if step > self.max_steps {
                return Err(self.deny(name, &args, "step limit reached"));
            }
            if !self.allowed.allows(name) {
                return Err(self.deny(name, &args, "not permitted for this asker"));
            }
            let Some(spec) = self.tools.list().into_iter().find(|t| t.name == name) else {
                return Err(format!("{name}: no such tool"));
            };
            if !in_scope(&spec, self.cx.kind) {
                return Err(self.deny(name, &args, DM_ONLY));
            }
            let id = self.audit.sent(name, &args);
            let started = Instant::now();
            let result = match self.tools.call(&self.cx, name, args).await {
                Ok(text) => Ok(truncate_text(&text, MAX_RESULT_BYTES)),
                Err(text) => Err(truncate_text(&text, MAX_RESULT_BYTES)),
            };
            *self.tool_time.lock().unwrap_or_else(|e| e.into_inner()) += started.elapsed();
            match &result {
                Ok(text) => self.audit.finished(id, true, text.len()),
                Err(text) => self.audit.finished(id, false, text.len()),
            }
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(dir: &Path) -> FakeTools {
        let path = dir.join("fake.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "log": dir.join("calls.log"),
                "servers": {"calendar": ["create_event"], "linear": ["create_issue"]}
            })
            .to_string(),
        )
        .unwrap();
        FakeTools::load(&path).unwrap()
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Audit for Recorder {
        fn sent(&self, tool: &str, _: &Value) -> i64 {
            self.0.lock().unwrap().push(format!("sent {tool}"));
            1
        }
        fn finished(&self, _: i64, ok: bool, _: usize) {
            self.0.lock().unwrap().push(format!("finished {ok}"));
        }
        fn denied(&self, tool: &str, _: &Value, _: &str) {
            self.0.lock().unwrap().push(format!("denied {tool}"));
        }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn offered_tools_follow_the_tool_set() {
        let dir = tempfile::tempdir().unwrap();
        let tools = fake(dir.path());
        let all = tools.list();
        assert_eq!(servers(&all), ["calendar", "linear"]);
        assert_eq!(offered(&all, &ToolSet::All, Kind::Dm).len(), 2);
        assert!(offered(&all, &ToolSet::none(), Kind::Dm).is_empty());
        let one = ToolSet::parse("calendar.create_event");
        let names: Vec<_> = offered(&all, &one, Kind::Dm)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, ["calendar.create_event"]);
    }

    #[test]
    fn the_gate_checks_every_call() {
        let dir = tempfile::tempdir().unwrap();
        let tools = fake(dir.path());
        let audit = Recorder::default();
        let gate = Gate::new(
            &tools,
            CallContext::dm(),
            ToolSet::parse("calendar.create_event"),
            8,
            &audit,
        );
        block_on(async {
            let ok = gate.call("calendar.create_event", serde_json::json!({"x": 1}));
            assert_eq!(ok.await.unwrap(), "ok");
            // Not offered, called anyway: refused before it is sent.
            let no = gate.call("linear.create_issue", serde_json::json!({}));
            assert!(no.await.unwrap_err().contains("not permitted"));
        });
        assert_eq!(calls(dir.path()).len(), 1);
        assert!(calls(dir.path())[0].contains("calendar.create_event"));
        assert_eq!(gate.denied(), ["linear.create_issue"]);
        assert_eq!(
            *audit.0.lock().unwrap(),
            [
                "sent calendar.create_event",
                "finished true",
                "denied linear.create_issue"
            ]
        );
    }

    /// One tool that returns a long result, and the context it was called
    /// with.
    struct Long(&'static str, Scope, Mutex<Vec<CallContext>>);

    impl Long {
        fn new(name: &'static str, scope: Scope) -> Self {
            Long(name, scope, Mutex::new(Vec::new()))
        }
    }

    impl Tools for Long {
        fn list(&self) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: self.0.into(),
                description: String::new(),
                schema: Value::Null,
                scope: self.1,
            }]
        }
        fn call<'a>(
            &'a self,
            cx: &'a CallContext,
            name: &'a str,
            _: Value,
        ) -> BoxFuture<'a, Result<String, String>> {
            self.2.lock().unwrap().push(cx.clone());
            Box::pin(async move { Ok(format!("{name} {}", "x".repeat(MAX_RESULT_BYTES))) })
        }
    }

    #[test]
    fn a_dm_only_tool_stays_in_the_dm() {
        let dir = tempfile::tempdir().unwrap();
        let notes: Arc<dyn Tools> = Arc::new(Long::new("notes.read", Scope::DmOnly));
        let both = Both::new(notes, Arc::new(fake(dir.path())));
        let all = both.list();
        let names = |kind| -> Vec<String> {
            offered(&all, &ToolSet::All, kind)
                .into_iter()
                .map(|t| t.name)
                .collect()
        };
        // Offered in a DM turn; absent from a channel turn, even for the
        // host, whose tool set allows everything.
        assert_eq!(
            names(Kind::Dm),
            ["notes.read", "calendar.create_event", "linear.create_issue"]
        );
        assert_eq!(
            names(Kind::Channel),
            ["calendar.create_event", "linear.create_issue"]
        );

        let channel = CallContext {
            kind: Kind::Channel,
            channel: "garden".into(),
            asker: "7c1d09ee".into(),
            asker_tier: "host".into(),
        };
        let audit = Recorder::default();
        let gate = Gate::new(&both, channel.clone(), ToolSet::All, 8, &audit);
        block_on(async {
            // Called anyway from a channel turn: refused, never sent.
            let no = gate.call("notes.read", Value::Null).await.unwrap_err();
            assert!(no.contains(DM_ONLY), "{no}");
            // Tools of any scope still work there, and see the context.
            let ok = gate.call("calendar.create_event", Value::Null).await;
            assert_eq!(ok.unwrap(), "ok");
        });
        assert_eq!(gate.denied(), ["notes.read"]);
        assert_eq!(
            *audit.0.lock().unwrap(),
            [
                "denied notes.read",
                "sent calendar.create_event",
                "finished true"
            ]
        );

        let notes = Long::new("notes.read", Scope::DmOnly);
        let gate = Gate::new(&notes, CallContext::dm(), ToolSet::All, 8, &NoAudit);
        block_on(async { gate.call("notes.read", Value::Null).await.unwrap() });
        assert_eq!(*notes.2.lock().unwrap(), [CallContext::dm()]);
    }

    #[test]
    fn two_sources_act_as_one_and_results_are_cut() {
        let dir = tempfile::tempdir().unwrap();
        let both = Both::new(
            Arc::new(Long::new("calendar.long", Scope::Any)),
            Arc::new(fake(dir.path())),
        );
        // The first source's `calendar` hides the second's.
        assert_eq!(both.hidden, ["calendar"]);
        let names: Vec<_> = both.list().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["calendar.long", "linear.create_issue"]);
        let gate = Gate::new(&both, CallContext::dm(), ToolSet::All, 8, &NoAudit);
        block_on(async {
            let long = gate.call("calendar.long", Value::Null).await.unwrap();
            assert!(long.starts_with("calendar.long x"));
            assert!(long.len() <= MAX_RESULT_BYTES && long.ends_with('…'));
            assert_eq!(
                gate.call("linear.create_issue", Value::Null).await.unwrap(),
                "ok"
            );
            let hidden = gate.call("calendar.create_event", Value::Null).await;
            assert!(hidden.unwrap_err().contains("no such tool"));
        });
        assert_eq!(calls(dir.path()).len(), 1);
    }

    #[test]
    fn the_gate_enforces_the_step_limit() {
        let dir = tempfile::tempdir().unwrap();
        let tools = fake(dir.path());
        let gate = Gate::new(&tools, CallContext::dm(), ToolSet::All, 2, &NoAudit);
        block_on(async {
            for _ in 0..2 {
                gate.call("calendar.create_event", Value::Null)
                    .await
                    .unwrap();
            }
            let third = gate.call("calendar.create_event", Value::Null).await;
            assert!(third.unwrap_err().contains("step limit"));
        });
        assert_eq!(calls(dir.path()).len(), 2);
    }
}
