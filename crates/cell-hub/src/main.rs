//! Cell Hub: hosts cells for download, runs game servers from cells, and
//! serves a web admin UI. `cell-hub console` is a terminal console for the
//! game servers.

mod api;
mod console;
mod servers;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cell-hub",
    version,
    about = "Cell Hub: a game library and game-server host for .cell files"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the hub server.
    Serve {
        /// Address to listen on.
        #[arg(long, default_value = "0.0.0.0:8787", env = "CELL_HUB_LISTEN")]
        listen: String,
        /// Data directory for games, layers, servers and backups.
        #[arg(long, default_value = "./hub-data", env = "CELL_HUB_DATA")]
        data: PathBuf,
        /// Admin token for uploads and server control. Without it every call is allowed.
        #[arg(long, env = "CELL_HUB_TOKEN")]
        token: Option<String>,
        /// Name shown in the UI.
        #[arg(long, default_value = "Cell Hub", env = "CELL_HUB_NAME")]
        name: String,
    },
    /// Terminal console for the game servers of a hub.
    Console {
        /// Hub URL.
        #[arg(long, default_value = "http://127.0.0.1:8787", env = "CELL_HUB_URL")]
        url: String,
        #[arg(long, env = "CELL_HUB_TOKEN")]
        token: Option<String>,
    },
    /// Upload a cell (and its referenced layers from the same directory) to a hub.
    Upload {
        file: PathBuf,
        #[arg(long, default_value = "http://127.0.0.1:8787", env = "CELL_HUB_URL")]
        url: String,
        #[arg(long, env = "CELL_HUB_TOKEN")]
        token: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve {
            listen,
            data,
            token,
            name,
        } => serve(listen, data, token, name),
        Cmd::Console { url, token } => console::run(&url, token),
        Cmd::Upload { file, url, token } => upload(&file, &url, token),
    }
}

fn serve(listen: String, data: PathBuf, token: Option<String>, name: String) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let store = store::Store::new(&data)?;
        let servers = tokio::task::spawn_blocking({
            let store = store.clone();
            move || servers::Manager::new(store)
        })
        .await??;
        let app = Arc::new(api::App {
            store,
            servers,
            token: token.clone(),
            name,
        });
        if token.is_none() {
            eprintln!("cell-hub: no admin token set; every call is allowed (set --token or CELL_HUB_TOKEN)");
        }
        let listener = tokio::net::TcpListener::bind(&listen).await?;
        eprintln!("cell-hub: listening on http://{listen}  data: {}", data.display());
        axum::serve(listener, api::router(app)).await?;
        Ok(())
    })
}

fn upload(file: &std::path::Path, url: &str, token: Option<String>) -> Result<()> {
    let url = url.trim_end_matches('/');
    let cell = cell_core::format::CellFile::open(file)?;
    let client = reqwest::blocking::Client::builder().timeout(None).build()?;
    let send = |path: &str, local: &std::path::Path, name: &str| -> Result<String> {
        let f = std::fs::File::open(local)?;
        let len = f.metadata()?.len();
        eprintln!("uploading {name} ({} MiB)", len / (1024 * 1024));
        let mut r = client
            .post(format!("{url}{path}"))
            .header("X-File-Name", name)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(f);
        if let Some(t) = &token {
            r = r.bearer_auth(t);
        }
        let resp = r.send()?;
        let status = resp.status();
        let text = resp.text()?;
        if !status.is_success() {
            anyhow::bail!("{name}: {status} {text}");
        }
        Ok(text)
    };
    // Referenced layers first, so the game is complete when it appears.
    for l in &cell.manifest.layers {
        if let cell_core::format::Placement::Referenced { file: lf } = &l.placement {
            let (p, _) = cell.layer_location(l)?;
            send("/api/layers", &p, lf)?;
        }
    }
    let name = file.file_name().unwrap().to_string_lossy().to_string();
    let text = send("/api/games", file, &name)?;
    let game: cell_hub_api::Game = serde_json::from_str(&text)?;
    for l in &cell.manifest.layers {
        if let cell_core::format::Placement::Sidecar { file: sf } = &l.placement {
            let (p, _) = cell.layer_location(l)?;
            send(&format!("/api/games/{}/sidecar", game.id), &p, sf)?;
        }
    }
    println!("uploaded {} {} ({})", game.name, game.version, game.id);
    Ok(())
}
