//! The HTTP API and the embedded web UI.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use cell_hub_api::{
    ApiError, ChallengeRequest, ConsoleInput, CreateInvite, CreateServer, FileContent, HubInfo,
    LoginRequest, PairPeer, PairRequest, PairResponse, Presence, SendInvite, UpdateMember,
    UpdateServer,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tower_http::cors::CorsLayer;

use crate::members::{Caller, Identity, Role};
use crate::servers::Manager;
use crate::store::{safe_name, Store};

pub struct App {
    pub store: Store,
    pub servers: Manager,
    /// Legacy admin token, for development. Sessions are the normal way in.
    pub token: Option<String>,
    pub name: String,
    pub identity: Identity,
}

pub type Shared = Arc<App>;

pub struct Fail(StatusCode, String);

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        (self.0, Json(ApiError { error: self.1 })).into_response()
    }
}

impl From<anyhow::Error> for Fail {
    fn from(e: anyhow::Error) -> Self {
        let msg = format!("{e:#}");
        let code = if msg.starts_with("no ") || msg.contains("No such file") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        Fail(code, msg)
    }
}

type R<T> = Result<Json<T>, Fail>;

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

/// Who is calling: a session token, or the legacy admin token.
fn caller_from_token(app: &App, token: Option<&str>) -> Option<Caller> {
    let token = token?;
    if app.token.as_deref() == Some(token) {
        return Some(Caller {
            member: cell_hub_api::Member {
                pubkey: String::new(),
                name: "legacy-token".into(),
                avatar: None,
                role: "admin".into(),
                servers: Vec::new(),
                added_at: 0,
            },
            role: Role::Admin,
        });
    }
    app.identity.caller_for_token(token)
}

fn caller(app: &App, headers: &HeaderMap) -> Result<Caller, Fail> {
    caller_from_token(app, bearer(headers).as_deref()).ok_or_else(|| {
        Fail(
            StatusCode::UNAUTHORIZED,
            "sign in with a paired profile".into(),
        )
    })
}

fn admin(app: &App, headers: &HeaderMap) -> Result<Caller, Fail> {
    let c = caller(app, headers)?;
    if c.role == Role::Admin {
        Ok(c)
    } else {
        Err(Fail(StatusCode::FORBIDDEN, "admin role required".into()))
    }
}

/// Admin, or an operator of this server.
fn operator(app: &App, headers: &HeaderMap, server_id: &str) -> Result<Caller, Fail> {
    let c = caller(app, headers)?;
    if c.can_control(server_id) {
        Ok(c)
    } else {
        Err(Fail(
            StatusCode::FORBIDDEN,
            "operator role for this server required".into(),
        ))
    }
}

// ---- info and catalog ------------------------------------------------------------

async fn info(State(app): State<Shared>) -> R<HubInfo> {
    let servers = app.servers.list().await;
    Ok(Json(HubInfo {
        name: app.name.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        games: app.store.list().len(),
        running: servers.iter().filter(|s| s.state == "running").count(),
        servers: servers.len(),
        auth_required: true,
        pubkey: app.identity.hub_pubkey.clone(),
        fingerprint: cell_identity::fingerprint(&app.identity.hub_pubkey),
    }))
}

async fn games(State(app): State<Shared>) -> R<Vec<cell_hub_api::Game>> {
    Ok(Json(app.store.list()))
}

async fn game(State(app): State<Shared>, Path(id): Path<String>) -> R<cell_hub_api::Game> {
    Ok(Json(app.store.load(&id)?))
}

async fn game_delete(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<serde_json::Value> {
    admin(&app, &headers)?;
    app.store.remove(&id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn art(
    State(app): State<Shared>,
    Path((id, kind)): Path<(String, String)>,
) -> Result<Response, Fail> {
    safe_name(&kind)?;
    let p = app
        .store
        .game_dir(&id)
        .join("art")
        .join(format!("{kind}.png"));
    let bytes = tokio::fs::read(&p)
        .await
        .map_err(|_| Fail(StatusCode::NOT_FOUND, format!("no {kind} art")))?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        bytes,
    )
        .into_response())
}

async fn send_file(path: std::path::PathBuf, name: &str) -> Result<Response, Fail> {
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| Fail(StatusCode::NOT_FOUND, format!("no file {name}")))?;
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let stream = tokio_util_stream(file);
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, len)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .body(Body::from_stream(stream))
        .unwrap())
}

fn tokio_util_stream(
    file: tokio::fs::File,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    futures_util::stream::unfold(file, |mut f| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 1 << 20];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    })
}

async fn download(State(app): State<Shared>, Path(id): Path<String>) -> Result<Response, Fail> {
    let g = app.store.load(&id)?;
    send_file(app.store.game_dir(&id).join(&g.file), &g.file).await
}

async fn sidecar(
    State(app): State<Shared>,
    Path((id, file)): Path<(String, String)>,
) -> Result<Response, Fail> {
    safe_name(&file)?;
    send_file(app.store.game_dir(&id).join(&file), &file).await
}

async fn layer(State(app): State<Shared>, Path(file): Path<String>) -> Result<Response, Fail> {
    safe_name(&file)?;
    send_file(app.store.layer_path(&file), &file).await
}

/// Stream an upload body to a file. The name comes from `X-File-Name`.
async fn save_body(
    dir: &std::path::Path,
    headers: &HeaderMap,
    body: Body,
) -> Result<std::path::PathBuf, Fail> {
    let name = headers
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            Fail(
                StatusCode::BAD_REQUEST,
                "X-File-Name header required".into(),
            )
        })?
        .to_string();
    safe_name(&name)?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let tmp = dir.join(format!("{name}.upload"));
    let mut f = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut stream = body.into_data_stream();
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Fail(StatusCode::BAD_REQUEST, e.to_string()))?;
        f.write_all(&chunk)
            .await
            .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    f.flush().await.ok();
    drop(f);
    let dest = dir.join(&name);
    tokio::fs::rename(&tmp, &dest)
        .await
        .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(dest)
}

async fn upload_game(
    State(app): State<Shared>,
    headers: HeaderMap,
    body: Body,
) -> R<cell_hub_api::Game> {
    admin(&app, &headers)?;
    let incoming = app.store.root.join("incoming");
    let path = save_body(&incoming, &headers, body).await?;
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    if !name.ends_with(".cell") {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(Fail(StatusCode::BAD_REQUEST, "upload a .cell file".into()));
    }
    let store = app.store.clone();
    let game = tokio::task::spawn_blocking(move || store.import(&path, &name))
        .await
        .map_err(|e| Fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(game))
}

async fn upload_layer(
    State(app): State<Shared>,
    headers: HeaderMap,
    body: Body,
) -> R<serde_json::Value> {
    admin(&app, &headers)?;
    let dir = app.store.root.join("layers");
    let path = save_body(&dir, &headers, body).await?;
    let size = tokio::fs::metadata(&path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(Json(serde_json::json!({
        "file": path.file_name().unwrap().to_string_lossy(),
        "size": size
    })))
}

async fn upload_sidecar(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> R<serde_json::Value> {
    admin(&app, &headers)?;
    app.store.load(&id)?;
    let path = save_body(&app.store.game_dir(&id), &headers, body).await?;
    Ok(Json(
        serde_json::json!({ "file": path.file_name().unwrap().to_string_lossy() }),
    ))
}

// ---- servers ----------------------------------------------------------------------

async fn servers(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> R<Vec<cell_hub_api::ServerInstance>> {
    caller(&app, &headers)?;
    Ok(Json(app.servers.list().await))
}

async fn server(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<cell_hub_api::ServerInstance> {
    caller(&app, &headers)?;
    Ok(Json(app.servers.get(&id).await?))
}

async fn server_create(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<CreateServer>,
) -> R<cell_hub_api::ServerInstance> {
    admin(&app, &headers)?;
    Ok(Json(
        app.servers
            .create(&req.game_id, &req.name, req.join_args)
            .await?,
    ))
}

async fn server_update(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateServer>,
) -> R<cell_hub_api::ServerInstance> {
    operator(&app, &headers, &id)?;
    Ok(Json(
        app.servers.update(&id, req.name, req.join_args).await?,
    ))
}

async fn server_delete(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<serde_json::Value> {
    admin(&app, &headers)?;
    app.servers.remove(&id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn server_start(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<cell_hub_api::ServerInstance> {
    operator(&app, &headers, &id)?;
    Ok(Json(app.servers.start(&id).await?))
}

async fn server_stop(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<cell_hub_api::ServerInstance> {
    operator(&app, &headers, &id)?;
    Ok(Json(app.servers.stop(&id).await?))
}

async fn server_restart(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<cell_hub_api::ServerInstance> {
    operator(&app, &headers, &id)?;
    app.servers.stop(&id).await?;
    Ok(Json(app.servers.start(&id).await?))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    500
}

async fn server_log(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>,
) -> R<cell_hub_api::LogPage> {
    caller(&app, &headers)?;
    Ok(Json(
        app.servers.log(&id, q.after, q.limit.min(3000)).await?,
    ))
}

async fn server_input(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ConsoleInput>,
) -> R<serde_json::Value> {
    operator(&app, &headers, &id)?;
    app.servers.input(&id, &req.line).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Live console: the hub sends log lines as JSON; the client sends text
/// lines for stdin.
async fn server_console(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Result<Response, Fail> {
    let c = caller_from_token(&app, q.get("token").map(|s| s.as_str())).ok_or_else(|| {
        Fail(
            StatusCode::UNAUTHORIZED,
            "sign in with a paired profile".into(),
        )
    })?;
    let token_ok = c.can_control(&id);
    let (page, rx) = app.servers.subscribe(&id).await?;
    Ok(ws.on_upgrade(move |socket| console_session(socket, app, id, page, rx, token_ok)))
}

async fn console_session(
    socket: WebSocket,
    app: Shared,
    id: String,
    page: cell_hub_api::LogPage,
    mut rx: tokio::sync::broadcast::Receiver<cell_hub_api::LogLine>,
    token_ok: bool,
) {
    let (mut tx, mut rx_ws) = socket.split();
    for line in page.lines {
        if tx
            .send(Message::Text(serde_json::to_string(&line).unwrap().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    loop {
        tokio::select! {
            got = rx.recv() => match got {
                Ok(line) => {
                    if tx.send(Message::Text(serde_json::to_string(&line).unwrap().into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            msg = rx_ws.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    if token_ok {
                        let _ = app.servers.input(&id, &t).await;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}

#[derive(Deserialize)]
struct PathQuery {
    #[serde(default)]
    path: String,
}

async fn server_files(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> R<Vec<cell_hub_api::FileEntry>> {
    operator(&app, &headers, &id)?;
    Ok(Json(app.servers.list_files(&id, &q.path).await?))
}

async fn server_file_get(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> R<FileContent> {
    operator(&app, &headers, &id)?;
    let text = app.servers.read_file(&id, &q.path).await?;
    Ok(Json(FileContent { path: q.path, text }))
}

async fn server_file_put(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<FileContent>,
) -> R<serde_json::Value> {
    operator(&app, &headers, &id)?;
    app.servers.write_file(&id, &req.path, &req.text).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn backups(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<Vec<cell_hub_api::Backup>> {
    operator(&app, &headers, &id)?;
    Ok(Json(app.servers.backups(&id).await?))
}

async fn backup_create(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<cell_hub_api::Backup> {
    operator(&app, &headers, &id)?;
    Ok(Json(app.servers.backup(&id).await?))
}

async fn backup_download(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, Fail> {
    // Browsers cannot set headers on a link, so the token may come as a query.
    let tok = bearer(&headers).or_else(|| q.get("token").cloned());
    let c = caller_from_token(&app, tok.as_deref())
        .ok_or_else(|| Fail(StatusCode::UNAUTHORIZED, "sign in".into()))?;
    if !c.can_control(&id) {
        return Err(Fail(StatusCode::FORBIDDEN, "operator role required".into()));
    }
    let p = app.servers.backup_path(&id, &name)?;
    send_file(p, &name).await
}

async fn backup_restore(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path((id, name)): Path<(String, String)>,
) -> R<serde_json::Value> {
    operator(&app, &headers, &id)?;
    app.servers.restore(&id, &name).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn backup_delete(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path((id, name)): Path<(String, String)>,
) -> R<serde_json::Value> {
    operator(&app, &headers, &id)?;
    app.servers.delete_backup(&id, &name).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- identity, pairing, sessions -------------------------------------------------------

async fn identity(State(app): State<Shared>) -> R<cell_hub_api::HubIdentity> {
    Ok(Json(app.identity.hub_identity()))
}

async fn pair(State(app): State<Shared>, Json(req): Json<PairRequest>) -> R<PairResponse> {
    let member = app.identity.pair(&req)?;
    Ok(Json(PairResponse {
        member,
        hub: app.identity.hub_identity(),
    }))
}

async fn auth_challenge(
    State(app): State<Shared>,
    Json(req): Json<ChallengeRequest>,
) -> R<cell_hub_api::Challenge> {
    Ok(Json(app.identity.challenge(&req.pubkey)?))
}

async fn auth_login(
    State(app): State<Shared>,
    Json(req): Json<LoginRequest>,
) -> R<cell_hub_api::Session> {
    Ok(Json(app.identity.login(&req)?))
}

async fn auth_logout(State(app): State<Shared>, headers: HeaderMap) -> R<serde_json::Value> {
    if let Some(t) = bearer(&headers) {
        app.identity.logout(&t)?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn me(State(app): State<Shared>, headers: HeaderMap) -> R<cell_hub_api::Member> {
    Ok(Json(caller(&app, &headers)?.member))
}

async fn members(State(app): State<Shared>, headers: HeaderMap) -> R<Vec<cell_hub_api::Member>> {
    admin(&app, &headers)?;
    Ok(Json(app.identity.members()))
}

async fn member_update(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(pubkey): Path<String>,
    Json(req): Json<UpdateMember>,
) -> R<cell_hub_api::Member> {
    admin(&app, &headers)?;
    let role = match req.role.as_deref() {
        Some(r) => Some(Role::parse(r).ok_or_else(|| {
            Fail(
                StatusCode::BAD_REQUEST,
                "role is admin, operator or member".into(),
            )
        })?),
        None => None,
    };
    Ok(Json(app.identity.update_member(
        &pubkey,
        role,
        req.servers,
    )?))
}

async fn member_delete(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(pubkey): Path<String>,
) -> R<serde_json::Value> {
    let c = admin(&app, &headers)?;
    if c.member.pubkey == pubkey {
        return Err(Fail(
            StatusCode::BAD_REQUEST,
            "you cannot remove yourself".into(),
        ));
    }
    app.identity.remove_member(&pubkey)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn invites(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> R<Vec<cell_hub_api::InviteCode>> {
    admin(&app, &headers)?;
    Ok(Json(app.identity.invites()))
}

async fn invite_create(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<CreateInvite>,
) -> R<cell_hub_api::InviteCode> {
    admin(&app, &headers)?;
    let role = Role::parse(&req.role).ok_or_else(|| {
        Fail(
            StatusCode::BAD_REQUEST,
            "role is admin, operator or member".into(),
        )
    })?;
    Ok(Json(app.identity.create_invite(role, req.servers)?))
}

// ---- presence and peers ----------------------------------------------------------------

async fn presence(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(p): Json<Presence>,
) -> R<serde_json::Value> {
    let c = caller(&app, &headers)?;
    app.identity.set_presence(&c.member.pubkey, p);
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn peers(State(app): State<Shared>, headers: HeaderMap) -> R<Vec<cell_hub_api::Peer>> {
    let c = caller(&app, &headers)?;
    Ok(Json(app.identity.peers(&c.member.pubkey)))
}

async fn peer_code(State(app): State<Shared>, headers: HeaderMap) -> R<cell_hub_api::PeerCode> {
    let c = caller(&app, &headers)?;
    Ok(Json(app.identity.peer_code(&c.member.pubkey)))
}

async fn peer_pair(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<PairPeer>,
) -> R<cell_hub_api::Member> {
    let c = caller(&app, &headers)?;
    Ok(Json(app.identity.pair_peer(&c.member.pubkey, &req.code)?))
}

async fn peer_remove(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(pubkey): Path<String>,
) -> R<serde_json::Value> {
    let c = caller(&app, &headers)?;
    app.identity.remove_peer(&c.member.pubkey, &pubkey)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn invite_send(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<SendInvite>,
) -> R<serde_json::Value> {
    let c = caller(&app, &headers)?;
    if !app.identity.is_peer(&c.member.pubkey, &req.to) {
        return Err(Fail(
            StatusCode::FORBIDDEN,
            "you can only invite paired peers".into(),
        ));
    }
    let (server_name, game_id, game_name, join_args) = match &req.server_id {
        Some(id) => {
            let rec = app.servers.record(id).await?;
            let game = app.store.load(&rec.game_id)?;
            (rec.name, rec.game_id, game.name, rec.join_args)
        }
        None => (String::new(), String::new(), String::new(), Vec::new()),
    };
    let invite = cell_hub_api::GameInvite {
        id: cell_identity::random_token(),
        from: c.member.clone(),
        server_id: req.server_id.clone(),
        server_name,
        game_id,
        game_name,
        join_args,
        message: req.message.clone(),
        created_at: crate::store::now(),
    };
    app.identity.push_invite(&req.to, invite);
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn inbox(State(app): State<Shared>, headers: HeaderMap) -> R<Vec<cell_hub_api::GameInvite>> {
    let c = caller(&app, &headers)?;
    Ok(Json(app.identity.inbox(&c.member.pubkey)))
}

async fn inbox_dismiss(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> R<serde_json::Value> {
    let c = caller(&app, &headers)?;
    app.identity.dismiss(&c.member.pubkey, &id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- web UI -----------------------------------------------------------------------

#[derive(rust_embed::Embed)]
#[folder = "../../apps/hub-web/dist"]
struct WebAssets;

async fn web(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match WebAssets::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], f.data.into_owned()).into_response()
        }
        None => match WebAssets::get("index.html") {
            // Single-page app: unknown paths render the app.
            Some(f) => ([(header::CONTENT_TYPE, "text/html")], f.data.into_owned()).into_response(),
            None => (StatusCode::NOT_FOUND, "not found").into_response(),
        },
    }
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/api/info", get(info))
        .route("/api/games", get(games).post(upload_game))
        .route("/api/games/{id}", get(game).delete(game_delete))
        .route("/api/games/{id}/art/{kind}", get(art))
        .route("/api/games/{id}/download", get(download))
        .route("/api/games/{id}/sidecar/{file}", get(sidecar))
        .route("/api/games/{id}/sidecar", post(upload_sidecar))
        .route("/api/layers", post(upload_layer))
        .route("/api/layers/{file}", get(layer))
        .route("/api/identity", get(identity))
        .route("/api/pair", post(pair))
        .route("/api/auth/challenge", post(auth_challenge))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/me", get(me))
        .route("/api/members", get(members))
        .route(
            "/api/members/{pubkey}",
            axum::routing::patch(member_update).delete(member_delete),
        )
        .route("/api/invites", get(invites).post(invite_create))
        .route("/api/presence", post(presence))
        .route("/api/peers", get(peers))
        .route("/api/peers/code", post(peer_code))
        .route("/api/peers/pair", post(peer_pair))
        .route("/api/peers/{pubkey}", axum::routing::delete(peer_remove))
        .route("/api/peers/invite", post(invite_send))
        .route("/api/inbox", get(inbox))
        .route("/api/inbox/{id}", axum::routing::delete(inbox_dismiss))
        .route("/api/servers", get(servers).post(server_create))
        .route(
            "/api/servers/{id}",
            get(server).patch(server_update).delete(server_delete),
        )
        .route("/api/servers/{id}/start", post(server_start))
        .route("/api/servers/{id}/stop", post(server_stop))
        .route("/api/servers/{id}/restart", post(server_restart))
        .route("/api/servers/{id}/log", get(server_log))
        .route("/api/servers/{id}/input", post(server_input))
        .route("/api/servers/{id}/console", get(server_console))
        .route("/api/servers/{id}/files", get(server_files))
        .route(
            "/api/servers/{id}/file",
            get(server_file_get).put(server_file_put),
        )
        .route(
            "/api/servers/{id}/backups",
            get(backups).post(backup_create),
        )
        .route(
            "/api/servers/{id}/backups/{name}",
            get(backup_download).delete(backup_delete),
        )
        .route(
            "/api/servers/{id}/backups/{name}/restore",
            post(backup_restore),
        )
        .fallback(web)
        .layer(CorsLayer::permissive())
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(app)
}
