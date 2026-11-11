//! MCP connections through `rmcp`: the host's servers, listed in `mcp.json`
//! in the data directory. This is the only file that names `rmcp`.
//!
//! ```json
//! { "servers": [
//!   { "name": "linear",   "url": "https://…/mcp", "token_env": "LINEAR_MCP_TOKEN" },
//!   { "name": "calendar", "command": ["calendar-mcp", "--stdio"] }
//! ] }
//! ```
//!
//! Tokens are named by environment variable and never stored in the file.
//! A server that fails to connect at start is reported and left out.

use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde::Deserialize;
use serde_json::Value;

use crate::tools::{BoxFuture, ToolSpec, Tools};

pub const CONFIG_FILE: &str = "mcp.json";

/// How long a server gets to connect and list its tools.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one tool call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ConfigFile {
    servers: Vec<ServerConfig>,
}

#[derive(Deserialize)]
struct ServerConfig {
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    token_env: Option<String>,
    #[serde(default)]
    command: Option<Vec<String>>,
}

struct Server {
    name: String,
    peer: Peer<RoleClient>,
}

/// The host's MCP servers.
pub struct McpTools {
    servers: Vec<Server>,
    specs: Vec<ToolSpec>,
    /// Keeps the connections (and child processes) alive.
    _running: Mutex<Vec<RunningService<RoleClient, ()>>>,
}

fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

async fn connect_one(cfg: &ServerConfig) -> Result<RunningService<RoleClient, ()>> {
    match (&cfg.url, &cfg.command) {
        (Some(url), None) => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.as_str());
            if let Some(var) = &cfg.token_env {
                let token = std::env::var(var)
                    .ok()
                    .filter(|t| !t.is_empty())
                    .with_context(|| format!("{var} is not set"))?;
                config = config.auth_header(token);
            }
            let (client, _) = crate::http::client().map_err(anyhow::Error::msg)?;
            let transport = StreamableHttpClientTransport::with_client(client, config);
            Ok(().serve(transport).await?)
        }
        (None, Some(command)) => {
            let (program, args) = command.split_first().context("empty command")?;
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args);
            let (transport, _) = TokioChildProcess::builder(cmd)
                .stderr(Stdio::null())
                .spawn()
                .with_context(|| format!("starting {program}"))?;
            Ok(().serve(transport).await?)
        }
        _ => bail!("needs exactly one of `url` or `command`"),
    }
}

impl McpTools {
    /// Connects to every server in `path`. Returns the tools of those that
    /// connected and a message for each that did not.
    pub async fn connect(path: &Path) -> Result<(McpTools, Vec<String>)> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file: ConfigFile =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let mut servers = Vec::new();
        let mut running = Vec::new();
        let mut specs = Vec::new();
        let mut failed = Vec::new();
        for cfg in &file.servers {
            if !ident(&cfg.name) || servers.iter().any(|s: &Server| s.name == cfg.name) {
                failed.push(format!(
                    "MCP server {:?}: invalid or repeated name",
                    cfg.name
                ));
                continue;
            }
            let connected = tokio::time::timeout(CONNECT_TIMEOUT, async {
                let service = connect_one(cfg).await?;
                let tools = service.list_all_tools().await?;
                anyhow::Ok((service, tools))
            })
            .await;
            let (service, tools) = match connected {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    failed.push(format!("MCP server {} did not connect: {e:#}", cfg.name));
                    continue;
                }
                Err(_) => {
                    failed.push(format!("MCP server {} did not connect in time", cfg.name));
                    continue;
                }
            };
            for t in tools {
                if !ident(&t.name) {
                    failed.push(format!(
                        "MCP server {}: skipped tool {:?}",
                        cfg.name, t.name
                    ));
                    continue;
                }
                specs.push(ToolSpec {
                    name: format!("{}.{}", cfg.name, t.name),
                    description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                    schema: Value::Object((*t.input_schema).clone()),
                });
            }
            servers.push(Server {
                name: cfg.name.clone(),
                peer: service.peer().clone(),
            });
            running.push(service);
        }
        Ok((
            McpTools {
                servers,
                specs,
                _running: Mutex::new(running),
            },
            failed,
        ))
    }
}

impl Tools for McpTools {
    fn list(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let (server, tool) = name
                .split_once('.')
                .ok_or_else(|| format!("{name}: not a server.tool name"))?;
            let peer = &self
                .servers
                .iter()
                .find(|s| s.name == server)
                .ok_or_else(|| format!("{name}: no such server"))?
                .peer;
            let mut params = CallToolRequestParams::new(tool.to_string());
            match args {
                Value::Object(map) => params = params.with_arguments(map),
                Value::Null => {}
                other => return Err(format!("{name}: arguments must be an object, got {other}")),
            }
            let result = tokio::time::timeout(CALL_TIMEOUT, peer.call_tool(params))
                .await
                .map_err(|_| format!("{name}: timed out"))?
                .map_err(|e| format!("{name}: {e}"))?;
            let mut text: Vec<String> = result
                .content
                .iter()
                .filter_map(|c| match c {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            if text.is_empty()
                && let Some(v) = &result.structured_content
            {
                text.push(v.to_string());
            }
            let text = text.join("\n");
            if result.is_error == Some(true) {
                Err(text)
            } else {
                Ok(text)
            }
        })
    }
}
