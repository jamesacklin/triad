//! A small MCP server for the bot tests (H6). It exposes one tool and logs
//! every call it receives, as the fake tools do, so a test can tell which
//! calls reached a tool.
//!
//!   mcp_fixture --server calendar --tool create_event --log calls.log --stdio
//!   mcp_fixture --server linear --tool create_issue --log calls.log \
//!       --http 127.0.0.1:0 [--token T]
//!
//! Over HTTP it prints `listening http://<addr>/mcp` on its first line and,
//! with `--token`, answers 401 to any request without `Bearer <T>`.

use std::convert::Infallible;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;

#[derive(Clone)]
struct Fixture {
    server: String,
    tool: String,
    log: PathBuf,
}

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = json!({"type": "object", "properties": {"title": {"type": "string"}}});
        let serde_json::Value::Object(schema) = schema else {
            unreachable!()
        };
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            self.tool.clone(),
            format!("Fixture tool {} on {}.", self.tool, self.server),
            Arc::new(schema),
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name != self.tool {
            return Ok(CallToolResult::error(vec![ContentBlock::text("no such tool")]).into());
        }
        let line = json!({"tool": format!("{}.{}", self.server, self.tool),
                          "args": request.arguments});
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        writeln!(f, "{line}").map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let fixture = Fixture {
        server: arg(&args, "--server").unwrap_or_else(|| "fixture".into()),
        tool: arg(&args, "--tool").unwrap_or_else(|| "create_event".into()),
        log: PathBuf::from(arg(&args, "--log").expect("--log PATH")),
    };
    if args.iter().any(|a| a == "--stdio") {
        fixture
            .serve(rmcp::transport::stdio())
            .await?
            .waiting()
            .await?;
        return Ok(());
    }
    let addr = arg(&args, "--http").expect("--stdio or --http ADDR");
    let token = arg(&args, "--token");
    let service: StreamableHttpService<Fixture, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(fixture.clone()),
        Default::default(),
        StreamableHttpServerConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("listening http://{}/mcp", listener.local_addr()?);
    std::io::stdout().flush()?;
    loop {
        let (stream, _) = listener.accept().await?;
        let service = service.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let handler = hyper::service::service_fn(move |req: Request<Incoming>| {
                let service = service.clone();
                let token = token.clone();
                async move {
                    let authorized = match &token {
                        None => true,
                        Some(t) => {
                            req.headers()
                                .get(hyper::header::AUTHORIZATION)
                                .and_then(|v| v.to_str().ok())
                                == Some(format!("Bearer {t}").as_str())
                        }
                    };
                    if !authorized || req.uri().path() != "/mcp" {
                        let status = if authorized {
                            StatusCode::NOT_FOUND
                        } else {
                            StatusCode::UNAUTHORIZED
                        };
                        let mut resp = Response::new(Full::new(Bytes::new()).boxed());
                        *resp.status_mut() = status;
                        return Ok::<_, Infallible>(resp);
                    }
                    Ok(service.handle(req).await)
                }
            });
            let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), handler)
                .await;
        });
    }
}
