//! Bots for triad: a personal bot in a one-member DM and a channel bot run
//! by each channel's host. See HARNESS-PLAN.md.
//!
//! `rig-core` is named only in `brain.rs` and `rmcp` only in `mcp.rs`.
//! Nothing here touches p2panda or the terminal: the binary records
//! messages, asks [`policy`] what to do, builds a [`context`], and runs a
//! turn with [`run_turn`] off its event loop.

pub mod assistant;
pub mod brain;
pub mod builtin;
pub mod context;
pub mod http;
pub mod mcp;
pub mod policy;
pub mod schedule;
pub mod skills;
pub mod tools;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use triad_core::envelope::{MAX_DM_TEXT_BYTES, MAX_TEXT_BYTES, truncate_text};

use crate::brain::{Brain, MockBrain, ModelBrain, TurnOutput};
use crate::context::TurnInput;
use crate::policy::{Limits, ToolSet};
use crate::tools::{Audit, Both, CallContext, FakeTools, Gate, NoTools, Scope, ToolSpec, Tools};

/// Model references shown when asking for one. There is no default model:
/// the user picks the vendor, and with it who sees the bots' messages.
pub const EXAMPLE_MODELS: &str =
    "openrouter/anthropic/claude-opus-5.5, openai/gpt-5.5 or ollama/qwen3:4b";
/// The bots' settings file in the data directory.
pub const CONFIG_FILE: &str = "agent.json";
/// Keys saved with `/bot provider-key`, by vendor, readable only by the
/// user. A vendor's environment variable wins over it.
pub const KEYS_FILE: &str = "keys.json";

/// A model provider's key. Its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(pub String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(..)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrainKind {
    /// Any model rig-core's catalog reaches, as `vendor/model`.
    Model {
        reference: String,
        base_url: Option<String>,
        /// From `keys.json`, when the vendor's variable is not set.
        api_key: Option<ApiKey>,
    },
    Mock {
        script: Option<PathBuf>,
        log: Option<PathBuf>,
        delay: Duration,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolsKind {
    /// The built-in tools (unless turned off) and the servers in `mcp.json`
    /// in the data directory, if the file is there.
    Mcp,
    /// `TRIAD_AGENT_FAKE_TOOLS`.
    Fake(PathBuf),
}

/// How the bots run, from `agent.json` and the environment.
#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub brain: BrainKind,
    pub tools: ToolsKind,
    /// Whether the bots have the built-in tools (`web.*`, `time.*`).
    pub builtin: bool,
    /// Replaces the search service's endpoint.
    pub search_url: Option<String>,
    pub persona: Option<PathBuf>,
    pub limits: Limits,
    /// Minutes east of UTC that schedule times are written in.
    pub utc_offset_minutes: i32,
}

/// A configuration the user must fix before the bots can run.
#[derive(Debug)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_num(name: &str, default: u64) -> Result<u64, UsageError> {
    match env(name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| UsageError(format!("{name} must be a whole number, got {v:?}"))),
    }
}

/// `agent.json`: settings worth keeping between sessions. Keys never go
/// here; they live in `keys.json` or the vendor's own variable.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    /// `vendor/model`.
    model: Option<String>,
    /// Replaces the vendor's endpoint.
    base_url: Option<String>,
    /// A file whose text is added to the system prompt.
    persona: Option<PathBuf>,
    /// `false` leaves out the built-in tools.
    builtin: Option<bool>,
    /// Minutes east of UTC for schedule times (`-300` for UTC-5). There is
    /// no time zone database, so a change of daylight saving time means
    /// changing this.
    utc_offset_minutes: Option<i32>,
}

impl FileConfig {
    fn read(data_dir: &Path) -> Result<Self, UsageError> {
        let path = data_dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| UsageError(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(UsageError(format!("{}: {e}", path.display()))),
        }
    }
}

pub(crate) fn read_keys(
    data_dir: &Path,
) -> Result<std::collections::BTreeMap<String, String>, UsageError> {
    let path = data_dir.join(KEYS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| UsageError(format!("{}: {e}", path.display())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(UsageError(format!("{}: {e}", path.display()))),
    }
}

/// Writes `text` to `path` through a temporary file, readable only by the
/// user when `private`.
fn write_file(path: &Path, text: &str, private: bool) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut f = options
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// What to do when no model is chosen.
pub fn no_model_help() -> String {
    format!(
        "no model chosen for your bots; type /bot model <vendor/model> (such as {EXAMPLE_MODELS}), \
         then /bot provider-key <key> if the vendor needs one"
    )
}

/// The model the bots would use: `TRIAD_AGENT_MODEL`, else `agent.json`;
/// `None` if neither names one. Not checked.
pub fn configured_model(data_dir: &Path) -> Result<Option<String>, UsageError> {
    let file = FileConfig::read(data_dir)?;
    Ok(reference(env("TRIAD_AGENT_BRAIN").as_deref(), file.model))
}

fn reference(brain: Option<&str>, file_model: Option<String>) -> Option<String> {
    // `anthropic` and `openrouter` were the only brains before any rig-core
    // vendor could be named; set explicitly, they still pick the vendor for
    // a model given without one. Otherwise a model names its vendor.
    let m = env("TRIAD_AGENT_MODEL").or(file_model)?;
    Some(match brain {
        Some("openrouter") if !m.starts_with("openrouter/") => format!("openrouter/{m}"),
        Some("anthropic") if !m.contains('/') => format!("anthropic/{m}"),
        _ => m,
    })
}

/// Saves `vendor/model` as the bots' model in `agent.json`, keeping its
/// other fields. Checks the vendor, not the key.
pub fn save_model(data_dir: &Path, model: &str) -> Result<()> {
    brain::vendor_of(model).map_err(anyhow::Error::msg)?;
    let path = data_dir.join(CONFIG_FILE);
    let mut value = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .with_context(|| format!("{}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
    };
    let Some(obj) = value.as_object_mut() else {
        bail!("{} is not a JSON object", path.display());
    };
    obj.insert("model".into(), model.into());
    write_file(&path, &format!("{value:#}\n"), false)
}

/// Saves a key for `vendor` in `keys.json`, which only the user can read.
/// `vendor` is a model vendor or the search service.
pub fn save_key(data_dir: &Path, vendor: &str, key: &str) -> Result<()> {
    if vendor != builtin::SEARCH_VENDOR && brain::key_vars(vendor).is_empty() {
        brain::vendor_of(&format!("{vendor}/x")).map_err(anyhow::Error::msg)?;
        bail!("{vendor} needs no key");
    }
    if key.is_empty() || key.chars().any(char::is_whitespace) {
        bail!("usage: /bot provider-key [vendor] <key>");
    }
    let mut keys = read_keys(data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    keys.insert(vendor.to_string(), key.to_string());
    let text = serde_json::to_string_pretty(&keys)?;
    let path = data_dir.join(KEYS_FILE);
    write_file(&path, &format!("{text}\n"), true)?;
    #[cfg(unix)]
    {
        // An older file made by hand keeps its mode through the rename of
        // a new one, but not if it was a different inode; be sure.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

impl AgentConfig {
    /// Reads `agent.json` in the data directory, then the `TRIAD_AGENT_*`
    /// variables, which win. Checks that the model's vendor is one rig-core
    /// reaches and that it has a key: the vendor's variable, else one saved
    /// in `keys.json`.
    pub fn load(data_dir: &Path) -> Result<Self, UsageError> {
        let file = FileConfig::read(data_dir)?;
        let brain = match env("TRIAD_AGENT_BRAIN").as_deref() {
            Some("mock") => BrainKind::Mock {
                script: env("TRIAD_AGENT_MOCK_SCRIPT").map(PathBuf::from),
                log: env("TRIAD_AGENT_MOCK_LOG").map(PathBuf::from),
                delay: Duration::from_millis(env_num("TRIAD_AGENT_MOCK_DELAY_MS", 0)?),
            },
            brain @ (None | Some("model" | "anthropic" | "openrouter")) => {
                let reference =
                    reference(brain, file.model).ok_or_else(|| UsageError(no_model_help()))?;
                let vendor = brain::vendor_of(&reference).map_err(UsageError)?;
                let api_key = match brain::key_var_set(vendor) {
                    Some(_) => None,
                    None => read_keys(data_dir)?.remove(vendor).map(ApiKey),
                };
                brain::check_model_keyed(&reference, api_key.is_some()).map_err(UsageError)?;
                BrainKind::Model {
                    reference,
                    base_url: env("TRIAD_AGENT_BASE_URL").or(file.base_url),
                    api_key,
                }
            }
            Some(other) => {
                return Err(UsageError(format!(
                    "TRIAD_AGENT_BRAIN must be mock or unset, got {other:?}; \
                     choose the model with TRIAD_AGENT_MODEL=vendor/model"
                )));
            }
        };
        let tools = match env("TRIAD_AGENT_TOOLS").as_deref().unwrap_or("mcp") {
            "mcp" => ToolsKind::Mcp,
            "fake" => ToolsKind::Fake(
                env("TRIAD_AGENT_FAKE_TOOLS")
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        UsageError("TRIAD_AGENT_TOOLS=fake needs TRIAD_AGENT_FAKE_TOOLS".into())
                    })?,
            ),
            other => {
                return Err(UsageError(format!(
                    "TRIAD_AGENT_TOOLS must be mcp or fake, got {other:?}"
                )));
            }
        };
        let d = Limits::default();
        let limits = Limits {
            max_age_ms: env_num("TRIAD_AGENT_MAX_AGE_SECS", d.max_age_ms / 1000)? * 1000,
            asker_per_hour: env_num("TRIAD_AGENT_ASKER_PER_HOUR", d.asker_per_hour)?,
            channel_per_day: env_num("TRIAD_AGENT_CHANNEL_PER_DAY", d.channel_per_day)?,
            channel_per_minute: env_num("TRIAD_AGENT_BURST_PER_MINUTE", d.channel_per_minute)?,
            max_steps: env_num("TRIAD_AGENT_MAX_STEPS", d.max_steps as u64)? as usize,
            dm_max_steps: env_num("TRIAD_AGENT_DM_MAX_STEPS", d.dm_max_steps as u64)? as usize,
        };
        let persona = env("TRIAD_AGENT_PERSONA")
            .map(PathBuf::from)
            .or(file.persona.map(|p| data_dir.join(p)));
        let builtin = match env("TRIAD_AGENT_BUILTIN").as_deref() {
            None => file.builtin.unwrap_or(true),
            Some("on") => true,
            Some("off") => false,
            Some(other) => {
                return Err(UsageError(format!(
                    "TRIAD_AGENT_BUILTIN must be on or off, got {other:?}"
                )));
            }
        };
        let utc_offset_minutes = file.utc_offset_minutes.unwrap_or(0);
        if !(-14 * 60..=14 * 60).contains(&utc_offset_minutes) {
            return Err(UsageError(format!(
                "utc_offset_minutes in {CONFIG_FILE} must be between -840 and 840, got \
                 {utc_offset_minutes}"
            )));
        }
        Ok(AgentConfig {
            brain,
            tools,
            builtin,
            search_url: env("TRIAD_AGENT_SEARCH_URL"),
            persona,
            limits,
            utc_offset_minutes,
        })
    }
}

/// The running bots' shared parts: brain, tools, persona, limits.
pub struct Agent {
    pub brain: Arc<dyn Brain>,
    pub tools: Arc<dyn Tools>,
    pub persona: Option<String>,
    pub limits: Limits,
    /// Minutes east of UTC that schedule times are written in.
    pub utc_offset_minutes: i32,
    /// The host's skills, listed in the system prompt.
    pub skills: Vec<skills::Skill>,
    /// Every tool the bots have.
    pub all_tools: Vec<ToolSpec>,
    /// Problems found at start (an MCP server that did not connect), for
    /// the user. The bots run without the failed parts.
    pub warnings: Vec<String>,
}

/// The brain a configuration names.
fn brain(config: &AgentConfig) -> Result<Arc<dyn Brain>> {
    http::install_crypto_provider();
    Ok(match &config.brain {
        BrainKind::Mock { script, log, delay } => {
            Arc::new(MockBrain::new(script.as_deref(), log.clone(), *delay)?)
        }
        BrainKind::Model {
            reference,
            base_url,
            api_key,
        } => {
            let (client, _) = http::client().map_err(anyhow::Error::msg)?;
            Arc::new(ModelBrain::connect(
                reference,
                base_url.as_deref(),
                api_key.as_ref().map(|k| k.0.as_str()),
                client,
            )?)
        }
    })
}

impl Agent {
    /// Starts the bots. With a `store`, the personal bot also has its own
    /// DM-only tools (memory) over it.
    pub async fn start(
        config: &AgentConfig,
        data_dir: &Path,
        store: Option<Arc<dyn assistant::Store>>,
    ) -> Result<Agent> {
        let brain = brain(config)?;
        let mut warnings = Vec::new();
        let mut tools: Arc<dyn Tools> = match &config.tools {
            ToolsKind::Fake(path) => Arc::new(FakeTools::load(path)?),
            ToolsKind::Mcp => {
                let path = data_dir.join(mcp::CONFIG_FILE);
                let mcp: Option<Arc<dyn Tools>> = if path.exists() {
                    let (tools, failed) = mcp::McpTools::connect(&path).await?;
                    warnings.extend(failed);
                    Some(Arc::new(tools))
                } else {
                    None
                };
                let builtin: Option<Arc<dyn Tools>> = config.builtin.then(|| {
                    Arc::new(builtin::Builtin::new(
                        data_dir,
                        config.search_url.as_deref(),
                    )) as _
                });
                match (builtin, mcp) {
                    (Some(b), Some(m)) => {
                        let both = Both::new(b, m);
                        for name in &both.hidden {
                            warnings.push(format!(
                                "MCP server {name} is left out: the built-in tools use that \
                                 name. Rename it in {}",
                                mcp::CONFIG_FILE
                            ));
                        }
                        Arc::new(both)
                    }
                    (Some(one), None) | (None, Some(one)) => one,
                    (None, None) => Arc::new(NoTools),
                }
            }
        };
        let mut skill_list = Vec::new();
        if let Some(found) = skills::Skills::load(data_dir) {
            skill_list = found.list.clone();
            let both = Both::new(Arc::new(found), tools);
            for name in &both.hidden {
                warnings.push(format!(
                    "tool server {name} is left out: the skills use that name"
                ));
            }
            tools = Arc::new(both);
        }
        if let Some(store) = store {
            let own = assistant::Assistant::new(store, config.utc_offset_minutes);
            let both = Both::new(Arc::new(own), tools);
            for name in &both.hidden {
                warnings.push(format!(
                    "tool server {name} is left out: the personal bot's own tools use that name"
                ));
            }
            tools = Arc::new(both);
        }
        let persona = match &config.persona {
            Some(p) => Some(
                std::fs::read_to_string(p)
                    .with_context(|| format!("reading persona {}", p.display()))?,
            ),
            None => None,
        };
        let all_tools = tools.list();
        if config.limits.max_steps == 0 || config.limits.dm_max_steps == 0 {
            bail!("TRIAD_AGENT_MAX_STEPS and TRIAD_AGENT_DM_MAX_STEPS must be at least 1");
        }
        Ok(Agent {
            brain,
            tools,
            persona,
            limits: config.limits,
            utc_offset_minutes: config.utc_offset_minutes,
            skills: skill_list,
            all_tools,
            warnings,
        })
    }

    /// These bots with the brain `config` names, keeping their tools and
    /// persona. Turns already running finish with the old brain.
    pub fn with_brain(&self, config: &AgentConfig) -> Result<Agent> {
        Ok(Agent {
            brain: brain(config)?,
            tools: self.tools.clone(),
            persona: self.persona.clone(),
            limits: self.limits,
            utc_offset_minutes: self.utc_offset_minutes,
            skills: self.skills.clone(),
            all_tools: self.all_tools.clone(),
            warnings: Vec::new(),
        })
    }

    pub fn provider(&self) -> String {
        self.brain.provider()
    }

    /// Every server the bots have, the DM's own included.
    pub fn servers(&self) -> Vec<String> {
        tools::servers(&self.all_tools)
    }

    /// The servers a channel's bot can use: those with a tool of any scope.
    /// This is what a channel is told about.
    pub fn channel_servers(&self) -> Vec<String> {
        let any: Vec<ToolSpec> = self
            .all_tools
            .iter()
            .filter(|t| t.scope == Scope::Any)
            .cloned()
            .collect();
        tools::servers(&any)
    }
}

/// What a turn sends back to the event loop.
#[derive(Clone, Debug, Default)]
pub struct TurnResult {
    /// The reply: at most `MAX_TEXT_BYTES` in a channel, `MAX_DM_TEXT_BYTES`
    /// in the DM.
    pub text: String,
    /// Tools the gate refused.
    pub denied: Vec<String>,
    pub output: TurnOutput,
    pub tool_time: Duration,
}

/// The reply when the asker lacked a tool the turn needed. Names the host,
/// not who else has access.
pub fn denial_text(denied: &[String], host: &str) -> String {
    let mut servers: Vec<&str> = denied
        .iter()
        .map(|t| t.split_once('.').map_or(t.as_str(), |(s, _)| s))
        .collect();
    servers.sort();
    servers.dedup();
    format!(
        "I can't use {} for you. Ask {host} for access.",
        servers.join(", ")
    )
}

/// Runs one turn: the brain, with every tool call through the gate.
pub async fn run_turn(
    agent: &Agent,
    allowed: ToolSet,
    audit: &dyn Audit,
    input: &TurnInput,
) -> Result<TurnResult> {
    let gate = Gate::new(
        agent.tools.as_ref(),
        CallContext::of(input),
        allowed,
        agent.limits.steps(input.kind),
        audit,
    );
    let output = agent.brain.turn(input, &gate).await?;
    let denied = gate.denied();
    // A tool that was offered can only have been refused by the step
    // limit, and a DM-only one in a channel by its scope (the model was
    // told why); the rest were not permitted for this asker.
    let dm_only = |t: &str| {
        agent
            .all_tools
            .iter()
            .any(|s| s.name == t && s.scope == Scope::DmOnly)
    };
    let refused: Vec<String> = denied
        .iter()
        .filter(|t| !offered(input, t) && !dm_only(t))
        .cloned()
        .collect();
    let text = if refused.is_empty() {
        output.text.clone()
    } else {
        denial_text(&refused, &input.host)
    };
    let text = if text.trim().is_empty() {
        "(no reply)".to_string()
    } else {
        text
    };
    Ok(TurnResult {
        text: truncate_text(&text, max_reply_bytes(input.kind)),
        denied,
        tool_time: gate.tool_time(),
        output,
    })
}

/// The longest reply a turn of `kind` sends; the rest is cut.
pub fn max_reply_bytes(kind: context::Kind) -> usize {
    match kind {
        context::Kind::Dm => MAX_DM_TEXT_BYTES,
        context::Kind::Channel => MAX_TEXT_BYTES,
    }
}

/// Whether a tool was offered to the asker.
fn offered(input: &TurnInput, tool: &str) -> bool {
    input.tools.iter().any(|t| t.name == tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replies_are_cut_to_what_spaces_can_carry() {
        use crate::context::{Kind, TurnInput};
        let agent = Agent {
            brain: Arc::new(MockBrain::new(None, None, Duration::ZERO).unwrap()),
            tools: Arc::new(NoTools),
            persona: None,
            limits: Limits::default(),
            utc_offset_minutes: 0,
            skills: Vec::new(),
            all_tools: Vec::new(),
            warnings: Vec::new(),
        };
        let mut input = TurnInput {
            kind: Kind::Channel,
            channel: "agent".into(),
            system: String::new(),
            transcript: Vec::new(),
            asker: "a".into(),
            asker_tier: "host".into(),
            ask: "é".repeat(8100),
            tools: Vec::new(),
            unavailable: Vec::new(),
            host: "a".into(),
        };
        let r = run_turn(&agent, ToolSet::All, &crate::tools::NoAudit, &input)
            .await
            .unwrap();
        assert!(r.text.len() <= MAX_TEXT_BYTES, "{}", r.text.len());
        assert!(r.text.ends_with('…'));
        // The DM's replies go as long messages, with a higher limit.
        input.kind = Kind::Dm;
        input.ask = "é".repeat(2000);
        let r = run_turn(&agent, ToolSet::All, &crate::tools::NoAudit, &input)
            .await
            .unwrap();
        assert_eq!(r.text, format!("echo: {}", input.ask));
        input.ask = "é".repeat(8100);
        let r = run_turn(&agent, ToolSet::All, &crate::tools::NoAudit, &input)
            .await
            .unwrap();
        assert!(r.text.len() <= MAX_DM_TEXT_BYTES && r.text.len() > MAX_TEXT_BYTES);
        assert!(r.text.ends_with('…'));
    }

    #[test]
    fn denial_names_servers_and_the_host() {
        let d = denial_text(
            &[
                "calendar.create_event".into(),
                "calendar.list".into(),
                "linear.x".into(),
            ],
            "7c1d09ee",
        );
        assert_eq!(
            d,
            "I can't use calendar, linear for you. Ask 7c1d09ee for access."
        );
    }
}
