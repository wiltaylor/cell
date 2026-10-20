//! Cell Launcher backend: scans a games folder for `.cell` files, reads
//! their manifests and art, launches them, and talks to a Cell Hub.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine;
use cell_core::format::{CellFile, Placement};
use cell_hub_api::{Game, ServerInstance};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub games_dir: String,
    pub hub_url: String,
    /// Legacy admin token; sessions from the profile are the normal way in.
    #[serde(default)]
    pub hub_token: String,
}

impl Default for Settings {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        Settings {
            games_dir: home.join("Games/cells").to_string_lossy().to_string(),
            // No hub until the user connects one; the library works alone.
            hub_url: String::new(),
            hub_token: String::new(),
        }
    }
}

fn settings_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("settings.json")
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Settings {
    std::fs::read(settings_path(&app))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    let p = settings_path(&app);
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&p, serde_json::to_vec_pretty(&settings).unwrap()).map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(&settings.games_dir);
    Ok(())
}

/// A cell in the games folder, as the library shows it.
#[derive(Debug, Clone, Serialize)]
pub struct LocalGame {
    pub id: String,
    pub name: String,
    pub version: String,
    pub comment: Option<String>,
    pub categories: Vec<String>,
    pub commands: Vec<String>,
    pub has_server: bool,
    pub path: String,
    pub file: String,
    pub size: u64,
    pub total_size: u64,
    pub art: Vec<String>,
    pub missing_layers: Vec<String>,
    pub grants: cell_hub_api::GrantSummary,
    pub installed_at: u64,
}

fn grants(m: &cell_core::format::Manifest) -> cell_hub_api::GrantSummary {
    cell_hub_api::GrantSummary {
        network: m.isolation.network,
        home: format!("{:?}", m.isolation.home).to_lowercase(),
        display: m
            .integration
            .display
            .iter()
            .map(|d| format!("{d:?}").to_lowercase())
            .collect(),
        audio: m
            .integration
            .audio
            .iter()
            .map(|a| format!("{a:?}").to_lowercase())
            .collect(),
        input: m
            .integration
            .input
            .iter()
            .map(|a| format!("{a:?}").to_lowercase())
            .collect(),
        gpu: m.integration.gpu.enable,
        nvidia: format!("{:?}", m.integration.gpu.nvidia).to_lowercase(),
        dbus: format!("{:?}", m.integration.dbus).to_lowercase(),
    }
}

fn read_game(path: &Path) -> Option<LocalGame> {
    let cell = CellFile::open(path).ok()?;
    let m = &cell.manifest;
    let md = std::fs::metadata(path).ok()?;
    let mut total = md.len();
    let mut missing = Vec::new();
    for l in &m.layers {
        if let Placement::Embedded = l.placement {
            continue;
        }
        match cell.layer_location(l) {
            Ok(_) => total += l.size,
            Err(_) => {
                if let Placement::Referenced { file } | Placement::Sidecar { file } = &l.placement {
                    missing.push(file.clone());
                }
            }
        }
    }
    let d = m.integration.desktop.clone().unwrap_or_default();
    let st = m.integration.steam.clone().unwrap_or_default();
    let mut art = Vec::new();
    if d.icon_png.is_some() {
        art.push("icon".into());
    }
    for (k, v) in [
        ("portrait", &st.portrait),
        ("wide", &st.wide),
        ("hero", &st.hero),
        ("logo", &st.logo),
    ] {
        if v.is_some() {
            art.push(k.into());
        }
    }
    let commands: Vec<String> = m.entry.commands.iter().map(|c| c.name.clone()).collect();
    Some(LocalGame {
        id: m.app.id.clone(),
        name: m.app.name.clone(),
        version: m.app.version.clone(),
        comment: d.comment.clone(),
        categories: d.categories.clone(),
        has_server: commands.iter().any(|c| c == "server"),
        commands,
        path: path.to_string_lossy().to_string(),
        file: path.file_name().unwrap().to_string_lossy().to_string(),
        size: md.len(),
        total_size: total,
        art,
        missing_layers: missing,
        grants: grants(m),
        installed_at: md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
    })
}

#[tauri::command]
fn scan_library(dir: String) -> Result<Vec<LocalGame>, String> {
    let _ = std::fs::create_dir_all(&dir);
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir)
        .map_err(|e| format!("{dir}: {e}"))?
        .flatten()
    {
        let p = e.path();
        if p.extension().map(|x| x == "cell").unwrap_or(false) {
            if let Some(g) = read_game(&p) {
                out.push(g);
            }
        }
    }
    out.sort_by_key(|g| g.name.to_lowercase());
    Ok(out)
}

/// Art as a data URL, read straight from the manifest.
#[tauri::command]
fn game_art(path: String, kind: String) -> Result<String, String> {
    let cell = CellFile::open(Path::new(&path)).map_err(|e| e.to_string())?;
    let m = &cell.manifest;
    let d = m.integration.desktop.clone().unwrap_or_default();
    let st = m.integration.steam.clone().unwrap_or_default();
    let bytes = match kind.as_str() {
        "icon" => d.icon_png,
        "portrait" => st.portrait,
        "wide" => st.wide,
        "hero" => st.hero,
        "logo" => st.logo,
        _ => None,
    }
    .ok_or_else(|| format!("no {kind} art"))?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Start a cell detached from the launcher. `command` picks a named
/// command (`CELL_COMMAND`).
#[tauri::command]
fn play(path: String, command: Option<String>) -> Result<u32, String> {
    let mut cmd = Command::new(&path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = Path::new(&path).parent() {
        cmd.current_dir(dir);
    }
    if let Some(c) = command.filter(|c| !c.is_empty()) {
        cmd.env("CELL_COMMAND", c);
    }
    let child = cmd.spawn().map_err(|e| format!("start {path}: {e}"))?;
    Ok(child.id())
}

/// Run `CELL_CMD=<what>` and return its output (install, steam-add, info).
#[tauri::command]
fn cell_cmd(path: String, what: String) -> Result<String, String> {
    let out = Command::new(&path)
        .env("CELL_CMD", &what)
        .output()
        .map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

#[tauri::command]
fn remove_game(path: String) -> Result<(), String> {
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

// ---- profile ---------------------------------------------------------------------

use cell_identity::{client::profile_path, fingerprint, Profile};

#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub name: String,
    pub pubkey: String,
    pub fingerprint: String,
    pub created_at: u64,
    pub path: String,
}

fn view(p: &Profile) -> Result<ProfileView, String> {
    let pubkey = p.pubkey().map_err(|e| e.to_string())?;
    Ok(ProfileView {
        name: p.name.clone(),
        fingerprint: fingerprint(&pubkey),
        pubkey,
        created_at: p.created_at,
        path: profile_path().to_string_lossy().to_string(),
    })
}

#[tauri::command]
fn profile_get() -> Result<Option<ProfileView>, String> {
    let p = profile_path();
    if !p.exists() {
        return Ok(None);
    }
    Ok(Some(view(&Profile::load(&p).map_err(|e| e.to_string())?)?))
}

#[tauri::command]
fn profile_create(name: String) -> Result<ProfileView, String> {
    let path = profile_path();
    if path.exists() {
        return Err("a profile already exists".into());
    }
    let p = Profile::generate(name.trim());
    p.save(&path).map_err(|e| e.to_string())?;
    view(&p)
}

#[tauri::command]
fn profile_import(file: String, passphrase: String) -> Result<ProfileView, String> {
    let path = profile_path();
    if path.exists() {
        return Err("a profile already exists; forget it first".into());
    }
    let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let p = Profile::import(&text, &passphrase).map_err(|e| e.to_string())?;
    p.save(&path).map_err(|e| e.to_string())?;
    view(&p)
}

#[tauri::command]
fn profile_export(file: String, passphrase: String) -> Result<(), String> {
    let p = Profile::load(&profile_path()).map_err(|e| e.to_string())?;
    std::fs::write(&file, p.export(&passphrase).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn profile_forget() -> Result<(), String> {
    let p = profile_path();
    if p.exists() {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(cell_identity::client::sessions_path());
    Ok(())
}

// ---- hub -----------------------------------------------------------------------

fn client() -> reqwest::Client {
    reqwest::Client::builder().build().unwrap()
}

async fn fail_of(resp: reqwest::Response) -> String {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    serde_json::from_str::<cell_hub_api::ApiError>(&text)
        .map(|e| e.error)
        .unwrap_or_else(|_| format!("{status} {text}"))
}

async fn get_json<T: serde::de::DeserializeOwned>(
    url: String,
    token: Option<&str>,
) -> Result<T, String> {
    let mut r = client().get(url);
    if let Some(t) = token {
        r = r.bearer_auth(t);
    }
    let resp = r.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(fail_of(resp).await);
    }
    resp.json().await.map_err(|e| e.to_string())
}

async fn post_json<T: serde::de::DeserializeOwned>(
    url: String,
    token: Option<&str>,
    body: serde_json::Value,
) -> Result<T, String> {
    let mut r = client().post(url).json(&body);
    if let Some(t) = token {
        r = r.bearer_auth(t);
    }
    let resp = r.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(fail_of(resp).await);
    }
    resp.json().await.map_err(|e| e.to_string())
}

fn base(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

#[tauri::command]
async fn hub_identity(url: String) -> Result<cell_hub_api::HubIdentity, String> {
    get_json(format!("{}/api/identity", base(&url)), None).await
}

/// Sign in with the profile and return a session token. Cached per hub.
async fn session_token(url: &str, fresh: bool) -> Result<String, String> {
    let url = base(url);
    let mut sessions = cell_identity::client::Sessions::load();
    if !fresh {
        if let Some(t) = sessions.by_hub.get(&url) {
            if get_json::<cell_hub_api::Member>(format!("{url}/api/me"), Some(t))
                .await
                .is_ok()
            {
                return Ok(t.clone());
            }
        }
    }
    let profile = Profile::load(&profile_path()).map_err(|_| "no profile yet".to_string())?;
    let hub: cell_hub_api::HubIdentity = get_json(format!("{url}/api/identity"), None).await?;
    let pubkey = profile.pubkey().map_err(|e| e.to_string())?;
    let ch: cell_hub_api::Challenge = post_json(
        format!("{url}/api/auth/challenge"),
        None,
        serde_json::json!({ "pubkey": pubkey }),
    )
    .await?;
    let sig = profile
        .sign(&cell_identity::login_message(&hub.pubkey, &ch.nonce))
        .map_err(|e| e.to_string())?;
    let s: cell_hub_api::Session = post_json(
        format!("{url}/api/auth/login"),
        None,
        serde_json::json!({ "pubkey": pubkey, "nonce": ch.nonce, "signature": sig }),
    )
    .await?;
    sessions.by_hub.insert(url, s.token.clone());
    sessions.save().map_err(|e| e.to_string())?;
    Ok(s.token)
}

#[tauri::command]
async fn hub_me(url: String) -> Result<Option<cell_hub_api::Member>, String> {
    if !profile_path().exists() {
        return Ok(None);
    }
    match session_token(&url, false).await {
        Ok(t) => Ok(get_json(format!("{}/api/me", base(&url)), Some(&t))
            .await
            .ok()),
        Err(_) => Ok(None),
    }
}

#[tauri::command]
async fn hub_pair(url: String, code: String) -> Result<cell_hub_api::PairResponse, String> {
    let profile = Profile::load(&profile_path()).map_err(|_| "make a profile first".to_string())?;
    let url = base(&url);
    let hub: cell_hub_api::HubIdentity = get_json(format!("{url}/api/identity"), None).await?;
    let code = cell_identity::normalize_code(&code);
    let pubkey = profile.pubkey().map_err(|e| e.to_string())?;
    let sig = profile
        .sign(&cell_identity::pair_message(&hub.pubkey, &code))
        .map_err(|e| e.to_string())?;
    let out: cell_hub_api::PairResponse = post_json(
        format!("{url}/api/pair"),
        None,
        serde_json::json!({ "code": code, "pubkey": pubkey, "name": profile.name, "avatar": profile.avatar, "signature": sig }),
    )
    .await?;
    session_token(&url, true).await?;
    Ok(out)
}

#[tauri::command]
async fn hub_presence(url: String, status: String, game_id: Option<String>) -> Result<(), String> {
    let t = session_token(&url, false).await?;
    let _: serde_json::Value = post_json(
        format!("{}/api/presence", base(&url)),
        Some(&t),
        serde_json::json!({ "status": status, "game_id": game_id }),
    )
    .await?;
    Ok(())
}

#[tauri::command]
async fn hub_peers(url: String) -> Result<Vec<cell_hub_api::Peer>, String> {
    let t = session_token(&url, false).await?;
    get_json(format!("{}/api/peers", base(&url)), Some(&t)).await
}

#[tauri::command]
async fn hub_peer_code(url: String) -> Result<cell_hub_api::PeerCode, String> {
    let t = session_token(&url, false).await?;
    post_json(
        format!("{}/api/peers/code", base(&url)),
        Some(&t),
        serde_json::json!({}),
    )
    .await
}

#[tauri::command]
async fn hub_peer_pair(url: String, code: String) -> Result<cell_hub_api::Member, String> {
    let t = session_token(&url, false).await?;
    post_json(
        format!("{}/api/peers/pair", base(&url)),
        Some(&t),
        serde_json::json!({ "code": code }),
    )
    .await
}

#[tauri::command]
async fn hub_invite(
    url: String,
    to: String,
    server_id: Option<String>,
    message: String,
) -> Result<(), String> {
    let t = session_token(&url, false).await?;
    let _: serde_json::Value = post_json(
        format!("{}/api/peers/invite", base(&url)),
        Some(&t),
        serde_json::json!({ "to": to, "server_id": server_id, "message": message }),
    )
    .await?;
    Ok(())
}

#[tauri::command]
async fn hub_inbox(url: String) -> Result<Vec<cell_hub_api::GameInvite>, String> {
    let t = session_token(&url, false).await?;
    get_json(format!("{}/api/inbox", base(&url)), Some(&t)).await
}

#[tauri::command]
async fn hub_dismiss(url: String, id: String) -> Result<(), String> {
    let t = session_token(&url, false).await?;
    let resp = client()
        .delete(format!("{}/api/inbox/{id}", base(&url)))
        .bearer_auth(&t)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(fail_of(resp).await);
    }
    Ok(())
}

#[tauri::command]
async fn hub_games(url: String) -> Result<Vec<Game>, String> {
    client()
        .get(format!("{}/api/games", url.trim_end_matches('/')))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn hub_servers(url: String) -> Result<Vec<ServerInstance>, String> {
    let t = session_token(&url, false).await?;
    get_json(format!("{}/api/servers", base(&url)), Some(&t)).await
}

#[derive(Clone, Serialize)]
struct Progress {
    id: String,
    file: String,
    done: u64,
    total: u64,
    finished: bool,
    error: Option<String>,
}

async fn fetch_to(
    app: &AppHandle,
    id: &str,
    url: &str,
    dest: &Path,
    total_hint: u64,
) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let resp = client().get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{url}: {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(total_hint);
    let tmp = dest.with_extension("part");
    let mut f = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut done = 0u64;
    let file = dest.file_name().unwrap().to_string_lossy().to_string();
    let mut last = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        f.write_all(&chunk).await.map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        if last.elapsed().as_millis() > 150 {
            let _ = app.emit(
                "download",
                Progress {
                    id: id.into(),
                    file: file.clone(),
                    done,
                    total,
                    finished: false,
                    error: None,
                },
            );
            last = std::time::Instant::now();
        }
    }
    f.flush().await.ok();
    drop(f);
    tokio::fs::rename(&tmp, dest)
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "download",
        Progress {
            id: id.into(),
            file,
            done,
            total,
            finished: false,
            error: None,
        },
    );
    Ok(())
}

/// Download a game and its layers from the hub into the games folder.
/// Progress arrives on the `download` event.
#[tauri::command]
async fn hub_download(
    app: AppHandle,
    url: String,
    id: String,
    games_dir: String,
) -> Result<String, String> {
    let base = url.trim_end_matches('/').to_string();
    let game: Game = client()
        .get(format!("{base}/api/games/{id}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let dir = PathBuf::from(&games_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result: Result<(), String> = async {
        for l in &game.layers {
            let Some(file) = &l.file else { continue };
            match l.placement.as_str() {
                "referenced" => {
                    let dest = dir.join(file);
                    if dest.exists()
                        && std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == l.size
                    {
                        continue;
                    }
                    fetch_to(
                        &app,
                        &id,
                        &format!("{base}/api/layers/{file}"),
                        &dest,
                        l.size,
                    )
                    .await?;
                }
                "sidecar" => {
                    let sdir = dir.join(format!("{}.d", game.file));
                    std::fs::create_dir_all(&sdir).map_err(|e| e.to_string())?;
                    fetch_to(
                        &app,
                        &id,
                        &format!("{base}/api/games/{id}/sidecar/{file}"),
                        &sdir.join(file),
                        l.size,
                    )
                    .await?;
                }
                _ => {}
            }
        }
        let dest = dir.join(&game.file);
        fetch_to(
            &app,
            &id,
            &format!("{base}/api/games/{id}/download"),
            &dest,
            game.size,
        )
        .await?;
        std::fs::set_permissions(&dest, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            let _ = app.emit(
                "download",
                Progress {
                    id: id.clone(),
                    file: game.file.clone(),
                    done: 1,
                    total: 1,
                    finished: true,
                    error: None,
                },
            );
            Ok(dir.join(&game.file).to_string_lossy().to_string())
        }
        Err(e) => {
            let _ = app.emit(
                "download",
                Progress {
                    id: id.clone(),
                    file: game.file.clone(),
                    done: 0,
                    total: 1,
                    finished: true,
                    error: Some(e.clone()),
                },
            );
            Err(e)
        }
    }
}

/// Upload a local cell and its referenced layers to the hub.
#[tauri::command]
async fn hub_upload(url: String, token: String, path: String) -> Result<Game, String> {
    let token = if token.is_empty() {
        session_token(&url, false).await?
    } else {
        token
    };
    let base = url.trim_end_matches('/').to_string();
    let cell = CellFile::open(Path::new(&path)).map_err(|e| e.to_string())?;
    let send = |api: String, local: PathBuf, name: String| {
        let base = base.clone();
        let token = token.clone();
        async move {
            let bytes = tokio::fs::read(&local).await.map_err(|e| e.to_string())?;
            let mut r = client()
                .post(format!("{base}{api}"))
                .header("X-File-Name", name.clone())
                .body(bytes);
            if !token.is_empty() {
                r = r.bearer_auth(&token);
            }
            let resp = r.send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(format!("{name}: {status} {text}"));
            }
            Ok(text)
        }
    };
    for l in &cell.manifest.layers {
        if let Placement::Referenced { file } = &l.placement {
            let (p, _) = cell.layer_location(l).map_err(|e| e.to_string())?;
            send("/api/layers".into(), p, file.clone()).await?;
        }
    }
    let name = Path::new(&path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let text = send("/api/games".into(), PathBuf::from(&path), name).await?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            scan_library,
            game_art,
            play,
            cell_cmd,
            remove_game,
            hub_games,
            hub_servers,
            hub_download,
            hub_upload,
            profile_get,
            profile_create,
            profile_import,
            profile_export,
            profile_forget,
            hub_identity,
            hub_me,
            hub_pair,
            hub_presence,
            hub_peers,
            hub_peer_code,
            hub_peer_pair,
            hub_invite,
            hub_inbox,
            hub_dismiss,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
