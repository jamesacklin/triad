//! `triad serve`: a chat session with no terminal, on a WebSocket.
//!
//! Each connection is served by hyper's HTTP/1 server (head capped at
//! 8 KiB and 5 s, no keep-alive) running an axum router with one route,
//! `/v1`. Its handler refuses a bad method, a foreign `Origin` or a wrong
//! bearer token with an HTTP error before anything is upgraded, then
//! upgrades with axum's WebSocket. See API.md for the protocol,
//! WEBSOCKET-PLAN.md for the design and AXUM-PLAN.md for the listener.

mod hub;
pub mod proto;
mod requests;

use std::cell::RefCell;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, timeout_at};
use triad_core::identity::Identity;

pub use hub::{Hub, HubMsg};
pub use proto::{ClientId, Request, Target};

use crate::{BotStart, Input, Opts, Sink, Start};

/// The token file in the data directory.
pub const TOKEN_FILE: &str = "api-token";
/// The one path the socket answers on.
pub const PATH: &str = "/v1";
/// Time from connecting to presenting the token.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the server pings; a client silent for three of these is
/// dropped.
const PING_EVERY: Duration = Duration::from_secs(30);
/// The largest frame (message) a client may send.
pub const MAX_FRAME: usize = 1024 * 1024;
/// The largest HTTP request head the listener reads (hyper's read buffer);
/// a longer one is refused with 431.
pub const MAX_HEAD: usize = 8 * 1024;

/// Close codes the server sends (API.md, "Close codes").
pub const CLOSE_UNAUTHORIZED: u16 = 4001;
pub const CLOSE_AUTH_TIMEOUT: u16 = 4002;
pub const CLOSE_LAGGED: u16 = 4008;

/// `triad serve`'s socket options.
pub struct Listen {
    pub addr: String,
    pub allow_remote: bool,
    pub allow_origins: Vec<String>,
}

/// The request being handled, if any, and the last error emitted under it;
/// part of the printer.
#[derive(Default)]
pub struct Current {
    target: RefCell<Option<Target>>,
    error: RefCell<Option<String>>,
}

impl Current {
    pub fn begin(&self, target: Target) {
        *self.target.borrow_mut() = Some(target);
        *self.error.borrow_mut() = None;
    }

    /// The last error emitted under the request so far.
    pub fn take_error(&self) -> Option<String> {
        self.error.borrow_mut().take()
    }

    pub fn end(&self) {
        *self.target.borrow_mut() = None;
        *self.error.borrow_mut() = None;
    }

    /// Sends one event to the hub, tagged with the current request.
    pub fn emit(&self, hub: &Hub, value: Value) {
        let target = self.target.borrow().clone();
        if target.is_some() && value["event"] == "error" {
            *self.error.borrow_mut() = Some(value["text"].as_str().unwrap_or("error").to_owned());
        }
        let _ = hub.send(HubMsg::Event { value, target });
    }
}

/// The API token: read from `<data-dir>/api-token`, or created there
/// (mode 0600) on the first `serve`.
pub fn load_or_create_token(data_dir: &Path) -> Result<(String, PathBuf)> {
    let path = data_dir.join(TOKEN_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path)?.permissions().mode();
                if mode & 0o077 != 0 {
                    bail!(
                        "{} can be read by other users (mode {:o}); run chmod 600 on it",
                        path.display(),
                        mode & 0o777
                    );
                }
            }
            let token = text.trim().to_owned();
            if token.len() < 32 {
                bail!(
                    "{} holds no usable token; delete it to make a new one",
                    path.display()
                );
            }
            Ok((token, path))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::io::Write;
            let token = hex::encode(triad_core::long::random_key()?);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(&path)
                .with_context(|| format!("creating {}", path.display()))?
                .write_all(format!("{token}\n").as_bytes())?;
            Ok((token, path))
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Compares two tokens in time that depends only on their lengths.
fn same_token(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Runs `triad serve`: binds the socket, prints where it is, and runs a
/// chat session whose only input and output is the socket.
pub fn serve(
    rt: &tokio::runtime::Runtime,
    data_dir: &Path,
    listen: Listen,
    channel: Option<String>,
    opts: Opts,
    bots: BotStart,
) -> Result<()> {
    let addrs: Vec<SocketAddr> = listen
        .addr
        .to_socket_addrs()
        .with_context(|| format!("--listen {}: not an address", listen.addr))?
        .collect();
    if addrs.is_empty() {
        bail!("--listen {}: resolves to no address", listen.addr);
    }
    let remote = addrs.iter().any(|a| !a.ip().is_loopback());
    if remote && !listen.allow_remote {
        bail!(
            "--listen {} is not a loopback address; the socket is not encrypted and the token \
             gives full access, so pass --allow-remote to listen there anyway",
            listen.addr
        );
    }
    let (token, token_path) = load_or_create_token(data_dir)?;
    let std_listener = std::net::TcpListener::bind(&addrs[..])
        .with_context(|| format!("listening on {}", listen.addr))?;
    std_listener.set_nonblocking(true)?;
    let local = std_listener.local_addr()?;
    {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        writeln!(out, "listening  ws://{local}{PATH}")?;
        writeln!(out, "token      {}", token_path.display())?;
        if remote {
            writeln!(
                out,
                "warning    not encrypted: anyone on the network path can read the token and \
                 every message; use an SSH tunnel or a TLS proxy"
            )?;
        }
        out.flush()?;
    }
    tracing::info!(%local, "websocket API listening");
    let node = Identity::load_or_create(data_dir)?.node_hex();
    rt.block_on(async move {
        let hub = hub::spawn(node, crate::VERSION);
        let (input, inputs) = mpsc::channel(64);
        let listener = TcpListener::from_std(std_listener)?;
        let shared = Arc::new(Shared {
            hub: hub.clone(),
            input,
            token,
            origins: listen.allow_origins,
        });
        tokio::spawn(accept_loop(listener, shared));
        crate::chat(
            data_dir,
            Start::Resume { channel },
            opts,
            bots,
            Sink::Ws(hub),
            Some(inputs),
        )
        .await
    })
}

struct Shared {
    hub: Hub,
    input: mpsc::Sender<Input>,
    token: String,
    origins: Vec<String>,
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    let mut next: ClientId = 0;
    loop {
        match listener.accept().await {
            Ok((tcp, peer)) => {
                next += 1;
                let client = next;
                tracing::debug!(client, %peer, "websocket connection");
                let _ = tcp.set_nodelay(true);
                tokio::spawn(serve_connection(tcp, client, shared.clone()));
            }
            Err(e) => {
                tracing::warn!("accepting a websocket connection: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// What one connection's handlers share.
#[derive(Clone)]
struct Conn {
    shared: Arc<Shared>,
    client: ClientId,
    /// When the token must have arrived by: 5 s from the TCP accept,
    /// covering the request head and the first frame together.
    deadline: Instant,
}

/// Serves one TCP connection: hyper reads the request head (at most
/// [`MAX_HEAD`] bytes, within [`AUTH_TIMEOUT`]) and the router answers it.
/// HTTP/1.1 only, one request per connection.
async fn serve_connection(tcp: TcpStream, client: ClientId, shared: Arc<Shared>) {
    let conn = Conn {
        shared,
        client,
        deadline: Instant::now() + AUTH_TIMEOUT,
    };
    let app = Router::new()
        .route(PATH, any(upgrade))
        .fallback(elsewhere)
        .layer(axum::middleware::map_response(close_unless_upgraded))
        .with_state(conn);
    let served = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(AUTH_TIMEOUT)
        .max_buf_size(MAX_HEAD)
        .serve_connection(TokioIo::new(tcp), TowerToHyperService::new(app))
        .with_upgrades()
        .await;
    if let Err(e) = served {
        tracing::debug!(client, "websocket request: {e}");
    }
}

/// One request per connection: every answer but the upgrade itself asks
/// for the connection to be closed, and hyper then closes it. Turning
/// keep-alive off on the builder would do the same but also adds `close`
/// to the `101`'s `Connection` header, which strict clients (Node's
/// built-in WebSocket) refuse.
async fn close_unless_upgraded(mut response: Response) -> Response {
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        response.headers_mut().insert(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("close"),
        );
    }
    response
}

fn refuse(status: StatusCode, why: &'static str) -> Response {
    (status, why).into_response()
}

/// Any path but [`PATH`].
async fn elsewhere(method: Method) -> Response {
    if method != Method::GET {
        return refuse(StatusCode::METHOD_NOT_ALLOWED, "use GET\n");
    }
    refuse(StatusCode::NOT_FOUND, "the API is at /v1\n")
}

/// `/v1`: checks the method, `Origin` and bearer token, in that order,
/// before the upgrade itself is checked and answered.
async fn upgrade(
    State(conn): State<Conn>,
    method: Method,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let client = conn.client;
    if method != Method::GET {
        return refuse(StatusCode::METHOD_NOT_ALLOWED, "use GET\n");
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin.to_str().unwrap_or_default();
        if !origin_allowed(&conn.shared.origins, origin) {
            tracing::info!(client, origin, "refused a websocket from a web page");
            return refuse(
                StatusCode::FORBIDDEN,
                "this origin is not allowed; start serve with --allow-origin\n",
            );
        }
    }
    let mut authed = false;
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        match auth.to_str().ok().and_then(|a| a.strip_prefix("Bearer ")) {
            Some(t) if same_token(t.trim().as_bytes(), conn.shared.token.as_bytes()) => {
                authed = true
            }
            _ => {
                tracing::info!(client, "refused a websocket with a wrong token");
                return refuse(StatusCode::UNAUTHORIZED, "wrong token\n");
            }
        }
    }
    match ws {
        Ok(ws) => ws
            .max_message_size(MAX_FRAME)
            .max_frame_size(MAX_FRAME)
            .on_upgrade(move |ws| connection(ws, conn, authed)),
        Err(rejection) => rejection.into_response(),
    }
}

async fn send(ws: &mut WebSocket, text: String) -> Result<(), axum::Error> {
    ws.send(Message::Text(text.into())).await
}

async fn close(ws: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = ws.send(Message::Close(Some(frame))).await;
}

/// Whether a read failed because the message was over [`MAX_FRAME`].
/// axum hands back tungstenite's capacity error only as text.
fn too_long(e: &axum::Error) -> bool {
    e.to_string().contains("Message too long")
}

/// Whether a client's `Origin` was allowed with `--allow-origin`.
fn origin_allowed(origins: &[String], origin: &str) -> bool {
    origins
        .iter()
        .any(|o| o.trim_end_matches('/').eq_ignore_ascii_case(origin))
}

/// One client, from the upgrade to the close.
async fn connection(mut ws: WebSocket, conn: Conn, authed: bool) {
    let Conn {
        shared,
        client,
        deadline,
    } = conn;
    if !authed {
        loop {
            match timeout_at(deadline, ws.recv()).await {
                Err(_) => {
                    return close(&mut ws, CLOSE_AUTH_TIMEOUT, "no token within 5 seconds").await;
                }
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                Ok(Some(Ok(m))) => {
                    let ok = match &m {
                        Message::Text(t) => proto::auth_token(t.as_str())
                            .is_some_and(|t| same_token(t.as_bytes(), shared.token.as_bytes())),
                        _ => false,
                    };
                    if ok {
                        break;
                    }
                    tracing::info!(client, "refused a websocket with a wrong token");
                    return close(&mut ws, CLOSE_UNAUTHORIZED, "unauthorized").await;
                }
                Ok(_) => return,
            }
        }
    }
    let (frames_tx, mut frames) = mpsc::channel(hub::QUEUE);
    let (lagged_tx, mut lagged) = oneshot::channel();
    if shared
        .hub
        .send(HubMsg::Join {
            client,
            frames: frames_tx,
            lagged: lagged_tx,
        })
        .is_err()
        || shared.input.send(Input::Connected(client)).await.is_err()
    {
        return;
    }
    tracing::info!(client, "websocket client connected");
    run(&mut ws, client, &shared, &mut frames, &mut lagged).await;
    let _ = shared.hub.send(HubMsg::Leave(client));
    tracing::info!(client, "websocket client gone");
}

async fn run(
    ws: &mut WebSocket,
    client: ClientId,
    shared: &Shared,
    frames: &mut mpsc::Receiver<String>,
    lagged: &mut oneshot::Receiver<u64>,
) {
    let mut ping = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
    let mut heard = Instant::now();
    let mut lag_gone = false;
    loop {
        tokio::select! {
            incoming = ws.recv() => {
                heard = Instant::now();
                match incoming {
                    Some(Ok(Message::Text(text))) => match proto::parse(text.as_str()) {
                        Ok((id, op)) => {
                            let request = Request { client, id, op };
                            if shared.input.send(Input::Request(request)).await.is_err() {
                                return close(ws, 1001, "the session ended").await;
                            }
                        }
                        Err(bad) => {
                            let target = Target { client, re: bad.id };
                            let _ = shared.hub.send(HubMsg::Event {
                                value: proto::done(Some(&bad.error)),
                                target: Some(target),
                            });
                        }
                    },
                    Some(Ok(Message::Binary(_))) => {
                        return close(ws, 1003, "send text frames").await;
                    }
                    Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::debug!(client, "websocket read failed: {e}");
                        if too_long(&e) {
                            close(ws, 1009, "a frame is limited to 1 MiB").await;
                        }
                        return;
                    }
                    None => return,
                }
            }
            frame = frames.recv() => match frame {
                Some(text) => {
                    if send(ws, text).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
            seq = &mut *lagged, if !lag_gone => {
                lag_gone = true;
                if let Ok(seq) = seq {
                    // Frames already queued are delivered first.
                    while let Ok(text) = frames.try_recv() {
                        if send(ws, text).await.is_err() {
                            return;
                        }
                    }
                    let note = json!({"event": "lagged", "seq": seq}).to_string();
                    let _ = send(ws, note).await;
                    return close(ws, CLOSE_LAGGED, "too far behind; resync").await;
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() > PING_EVERY * 3 {
                    return close(ws, 1001, "no answer to pings").await;
                }
                if ws.send(Message::Ping(Default::default())).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_tokens() {
        assert!(same_token(b"abc", b"abc"));
        assert!(!same_token(b"abc", b"abd"));
        assert!(!same_token(b"abc", b"ab"));
    }

    #[test]
    fn origins() {
        let allowed = vec!["http://localhost:5173/".to_string()];
        assert!(origin_allowed(&allowed, "http://localhost:5173"));
        assert!(!origin_allowed(&allowed, "http://evil.test"));
        assert!(!origin_allowed(&[], "null"));
    }
}
