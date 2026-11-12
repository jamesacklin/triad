mod rooms;

use std::{
    collections::HashMap, convert::Infallible, net::SocketAddr, path::PathBuf, sync::Arc,
    time::Duration,
};

use anyhow::{Result, bail};
use clap::Parser;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::HOST,
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use rooms::{Account, Outgoing, Start};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use tokio::{net::TcpListener, sync::Mutex};
use triad_core::{
    envelope::{ChatBody, Invite, normalize_nickname},
    state::now_ms,
};

type HttpRequest = Request<Incoming>;
type HttpResponse = Response<Full<Bytes>>;
type ApiResult = std::result::Result<HttpResponse, (StatusCode, String)>;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8765")]
    listen: SocketAddr,
    #[arg(long, default_value = "demo-data")]
    data: PathBuf,
}

struct App {
    root: PathBuf,
    accounts: Mutex<HashMap<String, Arc<Account>>>,
    authority: String,
    localhost: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if !args.listen.ip().is_loopback() {
        bail!("this demo listens on loopback");
    }
    std::fs::create_dir_all(&args.data)?;
    let listener = TcpListener::bind(args.listen).await?;
    let addr = listener.local_addr()?;
    let app = Arc::new(App {
        root: args.data.canonicalize()?,
        accounts: Mutex::new(HashMap::new()),
        authority: addr.to_string(),
        localhost: format!("localhost:{}", addr.port()),
    });
    eprintln!("Triad demo at http://{addr}");
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            result = listener.accept() => {
                let (stream, _) = result?;
                let app = app.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let app = app.clone();
                        async move { Ok::<_, Infallible>(app.respond(request).await) }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal handler");
    tokio::select! {
        _ = terminate.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
}

impl App {
    async fn respond(&self, request: HttpRequest) -> HttpResponse {
        match self.route(request).await {
            Ok(response) => response,
            Err((status, error)) => response(
                status,
                "application/json",
                json!({"Error": error}).to_string(),
            ),
        }
    }

    async fn route(&self, request: HttpRequest) -> ApiResult {
        let host = request
            .headers()
            .get(HOST)
            .and_then(|host| host.to_str().ok());
        if host != Some(&self.authority) && host != Some(&self.localhost) {
            return Err(bad_request("use the local address printed at startup"));
        }
        match (request.method(), request.uri().path()) {
            (&Method::GET, "/") => Ok(response(
                StatusCode::OK,
                "text/html; charset=utf-8",
                include_str!("index.html"),
            )),
            (&Method::POST, "/api/users") => self.create_user().await,
            (&Method::GET, "/api/state") => {
                let account = self.account(&request).await?;
                account.resume_on_load().await;
                let session = account.session.lock().await;
                ok(&session.view)
            }
            (&Method::POST, "/api/room") => {
                let account = self.account(&request).await?;
                let input: RoomInput = body(request).await?;
                let start = match input.action.as_str() {
                    "create" => Start::Create,
                    "join" => Start::Join(Invite::parse(&input.invite).map_err(bad_request)?),
                    "resume" => Start::Resume,
                    _ => return Err(bad_request("unknown room action")),
                };
                account
                    .start(start)
                    .await
                    .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
                ok(&json!({"Accepted": true}))
            }
            (&Method::POST, "/api/send") => {
                let account = self.account(&request).await?;
                let input: SendInput = body(request).await?;
                let text = input.text.trim();
                if text.is_empty() || text.contains(['\n', '\r']) {
                    return Err(bad_request("enter one line of text"));
                }
                let message = ChatBody::new(text, now_ms()).map_err(bad_request)?;
                publish(&account, Outgoing::Chat(message)).await
            }
            (&Method::POST, "/api/profile") => {
                let account = self.account(&request).await?;
                let input: ProfileInput = body(request).await?;
                let nickname = normalize_nickname(&input.nickname)
                    .map_err(bad_request)?
                    .unwrap_or_default()
                    .to_owned();
                publish(&account, Outgoing::Nickname(nickname)).await
            }
            _ => Err((StatusCode::NOT_FOUND, "not found".into())),
        }
    }

    async fn create_user(&self) -> ApiResult {
        let token = hex::encode(rand::random::<[u8; 24]>());
        let dir = self.root.join(&token);
        let account = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
            Account::load(dir)
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        let node = account.identity.node_hex();
        self.accounts
            .lock()
            .await
            .insert(token.clone(), Arc::new(account));
        ok(&json!({"Token": token, "Node": node}))
    }

    async fn account(
        &self,
        request: &HttpRequest,
    ) -> std::result::Result<Arc<Account>, (StatusCode, String)> {
        let token = request
            .headers()
            .get("authorization")
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.strip_prefix("Bearer "))
            .filter(|token| {
                token.len() == 48
                    && token
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or_else(|| (StatusCode::UNAUTHORIZED, "choose a browser identity".into()))?;
        if let Some(account) = self.accounts.lock().await.get(token).cloned() {
            return Ok(account);
        }
        let dir = self.root.join(token);
        if !dir.join("credentials.cbor").is_file() {
            return Err((
                StatusCode::NOT_FOUND,
                "this identity is missing from the server".into(),
            ));
        }
        let account = tokio::task::spawn_blocking(move || Account::load(dir))
            .await
            .map_err(internal)?
            .map_err(internal)?;
        let mut accounts = self.accounts.lock().await;
        Ok(accounts
            .entry(token.to_owned())
            .or_insert_with(|| Arc::new(account))
            .clone())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RoomInput {
    action: String,
    #[serde(default)]
    invite: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SendInput {
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ProfileInput {
    nickname: String,
}

async fn publish(account: &Account, event: Outgoing) -> ApiResult {
    tokio::time::timeout(Duration::from_secs(10), account.send(event))
        .await
        .map_err(|_| (StatusCode::GATEWAY_TIMEOUT, "send timed out".into()))?
        .map_err(|err| (StatusCode::CONFLICT, err.to_string()))?;
    ok(&json!({"Accepted": true}))
}

async fn body<T: DeserializeOwned>(
    request: HttpRequest,
) -> std::result::Result<T, (StatusCode, String)> {
    let bytes = Limited::new(request.into_body(), 8192)
        .collect()
        .await
        .map_err(|_| bad_request("request is too large"))?
        .to_bytes();
    serde_json::from_slice(&bytes).map_err(bad_request)
}

fn bad_request(error: impl ToString) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, error.to_string())
}

fn internal(error: impl ToString) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

fn ok(value: &impl Serialize) -> ApiResult {
    Ok(response(
        StatusCode::OK,
        "application/json",
        serde_json::to_string(value).map_err(internal)?,
    ))
}

fn response(status: StatusCode, kind: &str, body: impl Into<Bytes>) -> HttpResponse {
    Response::builder()
        .status(status)
        .header("content-type", kind)
        .header("cache-control", "no-store")
        .body(Full::new(body.into()))
        .expect("valid HTTP response")
}
