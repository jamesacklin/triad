//! Bots for triad: a personal bot in a one-member DM and a channel bot run
//! by each channel's host. See HARNESS-PLAN.md.
//!
//! `rig-core` is named only in `brain.rs` and `rmcp` only in `mcp.rs`.
//! Nothing here touches p2panda or the terminal: the binary records
//! messages, asks [`policy`] what to do, builds a [`context`], and runs a
//! turn with [`run_turn`] off its event loop.

pub mod brain;
pub mod context;
pub mod http;
pub mod mcp;
pub mod policy;
pub mod tools;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use triad_core::envelope::{MAX_TEXT_BYTES, truncate_text};

use crate::brain::{Brain, MockBrain, ModelBrain, TurnOutput};
use crate::context::TurnInput;
use crate::policy::{Limits, ToolSet};
use crate::tools::{Audit, FakeTools, Gate, NoTools, ToolSpec, Tools};

/// The model when none is configured.
pub const DEFAULT_MODEL: &str = "anthropic/claude-opus-5-5";
/// Claude Opus's name on OpenRouter, after `openrouter/`.
pub const DEFAULT_OPENROUTER_MODEL: &str = "anthropic/claude-opus-5.5";
/// The bots' settings file in the data directory.
pub const CONFIG_FILE: &str = "agent.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrainKind {
    /// Any model rig-core's catalog reaches, as `vendor/model`.
    Model {
        reference: String,
        base_url: Option<String>,
    },
    Mock {
        script: Option<PathBuf>,
        log: Option<PathBuf>,
        delay: Duration,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolsKind {
    /// `mcp.json` in the data directory; none if the file is missing.
    Mcp,
    /// `TRIAD_AGENT_FAKE_TOOLS`.
    Fake(PathBuf),
}

/// How the bots run, from `agent.json` and the environment.
#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub brain: BrainKind,
    pub tools: ToolsKind,
    pub persona: Option<PathBuf>,
    pub limits: Limits,
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
/// here; each vendor's key is read from its own variable.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    /// `vendor/model`.
    model: Option<String>,
    /// Replaces the vendor's endpoint.
    base_url: Option<String>,
    /// A file whose text is added to the system prompt.
    persona: Option<PathBuf>,
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

impl AgentConfig {
    /// Reads `agent.json` in the data directory, then the `TRIAD_AGENT_*`
    /// variables, which win. Checks that the model's vendor is one rig-core
    /// reaches and that its key variable is set; keys are never copied.
    pub fn load(data_dir: &Path) -> Result<Self, UsageError> {
        let file = FileConfig::read(data_dir)?;
        let brain = match env("TRIAD_AGENT_BRAIN").as_deref() {
            Some("mock") => BrainKind::Mock {
                script: env("TRIAD_AGENT_MOCK_SCRIPT").map(PathBuf::from),
                log: env("TRIAD_AGENT_MOCK_LOG").map(PathBuf::from),
                delay: Duration::from_millis(env_num("TRIAD_AGENT_MOCK_DELAY_MS", 0)?),
            },
            // `anthropic` and `openrouter` were the only brains before any
            // rig-core vendor could be named; they still pick the vendor for
            // a model given without one.
            brain @ (None | Some("model" | "anthropic" | "openrouter")) => {
                let openrouter = brain == Some("openrouter");
                let reference = match env("TRIAD_AGENT_MODEL").or(file.model) {
                    Some(m) if openrouter && !m.starts_with("openrouter/") => {
                        format!("openrouter/{m}")
                    }
                    Some(m) if m.contains('/') => m,
                    Some(m) => format!("anthropic/{m}"),
                    None if openrouter => format!("openrouter/{DEFAULT_OPENROUTER_MODEL}"),
                    None => DEFAULT_MODEL.into(),
                };
                brain::check_model(&reference).map_err(UsageError)?;
                BrainKind::Model {
                    reference,
                    base_url: env("TRIAD_AGENT_BASE_URL").or(file.base_url),
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
        };
        let persona = env("TRIAD_AGENT_PERSONA")
            .map(PathBuf::from)
            .or(file.persona.map(|p| data_dir.join(p)));
        Ok(AgentConfig {
            brain,
            tools,
            persona,
            limits,
        })
    }
}

/// The running bots' shared parts: brain, tools, persona, limits.
pub struct Agent {
    pub brain: Arc<dyn Brain>,
    pub tools: Arc<dyn Tools>,
    pub persona: Option<String>,
    pub limits: Limits,
    /// Every tool the bots have.
    pub all_tools: Vec<ToolSpec>,
    /// Problems found at start (an MCP server that did not connect), for
    /// the user. The bots run without the failed parts.
    pub warnings: Vec<String>,
}

impl Agent {
    pub async fn start(config: &AgentConfig, data_dir: &Path) -> Result<Agent> {
        http::install_crypto_provider();
        let brain: Arc<dyn Brain> = match &config.brain {
            BrainKind::Mock { script, log, delay } => {
                Arc::new(MockBrain::new(script.as_deref(), log.clone(), *delay)?)
            }
            BrainKind::Model {
                reference,
                base_url,
            } => {
                let (client, _) = http::client().map_err(anyhow::Error::msg)?;
                Arc::new(ModelBrain::connect(
                    reference,
                    base_url.as_deref(),
                    None,
                    client,
                )?)
            }
        };
        let mut warnings = Vec::new();
        let tools: Arc<dyn Tools> = match &config.tools {
            ToolsKind::Fake(path) => Arc::new(FakeTools::load(path)?),
            ToolsKind::Mcp => {
                let path = data_dir.join(mcp::CONFIG_FILE);
                if path.exists() {
                    let (tools, failed) = mcp::McpTools::connect(&path).await?;
                    warnings.extend(failed);
                    Arc::new(tools)
                } else {
                    Arc::new(NoTools)
                }
            }
        };
        let persona = match &config.persona {
            Some(p) => Some(
                std::fs::read_to_string(p)
                    .with_context(|| format!("reading persona {}", p.display()))?,
            ),
            None => None,
        };
        let all_tools = tools.list();
        if config.limits.max_steps == 0 {
            bail!("TRIAD_AGENT_MAX_STEPS must be at least 1");
        }
        Ok(Agent {
            brain,
            tools,
            persona,
            limits: config.limits,
            all_tools,
            warnings,
        })
    }

    pub fn provider(&self) -> String {
        self.brain.provider()
    }

    pub fn servers(&self) -> Vec<String> {
        tools::servers(&self.all_tools)
    }
}

/// What a turn sends back to the event loop.
#[derive(Clone, Debug, Default)]
pub struct TurnResult {
    /// The reply, at most `MAX_TEXT_BYTES`.
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
    let gate = Gate::new(agent.tools.as_ref(), allowed, agent.limits.max_steps, audit);
    let output = agent.brain.turn(input, &gate).await?;
    let denied = gate.denied();
    // A tool that was offered can only have been refused by the step
    // limit; the rest were not permitted for this asker.
    let refused: Vec<String> = denied
        .iter()
        .filter(|t| !offered(input, t))
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
        text: truncate_text(&text, MAX_TEXT_BYTES),
        denied,
        tool_time: gate.tool_time(),
        output,
    })
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
            all_tools: Vec::new(),
            warnings: Vec::new(),
        };
        let input = TurnInput {
            kind: Kind::Dm,
            channel: "agent".into(),
            system: String::new(),
            transcript: Vec::new(),
            asker: "a".into(),
            asker_tier: "host".into(),
            ask: "é".repeat(1100),
            tools: Vec::new(),
            unavailable: Vec::new(),
            host: "a".into(),
        };
        let r = run_turn(&agent, ToolSet::All, &crate::tools::NoAudit, &input)
            .await
            .unwrap();
        assert!(r.text.len() <= MAX_TEXT_BYTES, "{}", r.text.len());
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
