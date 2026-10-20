//! Cell Hub: hosts cells for download, runs game servers from cells, and
//! serves a web admin UI. `cell-hub console` is a terminal console for the
//! game servers.

mod api;
mod console;
mod members;
mod servers;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
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
enum ProfileCmd {
    /// Create a profile (refuses to overwrite one).
    Init {
        #[arg(long)]
        name: String,
    },
    /// Show the profile's name, public key and fingerprint.
    Show,
    /// Export the profile to a passphrase-protected file.
    Export {
        file: PathBuf,
        #[arg(long, env = "CELL_PROFILE_PASSPHRASE")]
        passphrase: String,
    },
    /// Import a profile from an export file (refuses to overwrite one).
    Import {
        file: PathBuf,
        #[arg(long, env = "CELL_PROFILE_PASSPHRASE")]
        passphrase: String,
    },
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
    /// Terminal console for the game servers of a hub. Signs in with your profile.
    Console {
        /// Hub URL.
        #[arg(long, default_value = "http://127.0.0.1:8787", env = "CELL_HUB_URL")]
        url: String,
        /// A legacy admin token instead of the profile.
        #[arg(long, env = "CELL_HUB_TOKEN")]
        token: Option<String>,
    },
    /// Your profile: the keypair that identifies you to hubs and peers.
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    /// Pair your profile with a hub using an invite code.
    Pair {
        #[arg(long, default_value = "http://127.0.0.1:8787", env = "CELL_HUB_URL")]
        url: String,
        code: String,
    },
    /// Make an invite code for a new member of this hub (reads the data directory).
    Invite {
        /// `admin`, `operator` or `member`.
        #[arg(long, default_value = "member")]
        role: String,
        /// Server ids an operator may control, comma separated.
        #[arg(long, default_value = "")]
        servers: String,
        #[arg(long, default_value = "./hub-data", env = "CELL_HUB_DATA")]
        data: PathBuf,
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
        Cmd::Console { url, token } => {
            let token = match token {
                Some(t) => t,
                None => {
                    cell_identity::client::token_for(&url, &cell_identity::client::profile_path())?
                }
            };
            console::run(&url, Some(token))
        }
        Cmd::Profile { cmd } => profile_cmd(cmd),
        Cmd::Pair { url, code } => {
            let path = cell_identity::client::profile_path();
            let profile = cell_identity::Profile::load(&path).with_context(|| {
                format!(
                    "no profile at {} (run `cell-hub profile init --name <you>`)",
                    path.display()
                )
            })?;
            let hub = cell_identity::client::pair(&url, &code, &profile)?;
            println!("paired with {} ({})", hub.name, hub.fingerprint);
            let s = cell_identity::client::login(&url, &profile)?;
            let mut sessions = cell_identity::client::Sessions::load();
            sessions
                .by_hub
                .insert(url.trim_end_matches('/').to_string(), s.token);
            sessions.save()?;
            println!("signed in; session saved");
            Ok(())
        }
        Cmd::Invite {
            role,
            servers,
            data,
        } => {
            let id = members::Identity::open(&data, "Cell Hub")?;
            let role = members::Role::parse(&role)
                .ok_or_else(|| anyhow::anyhow!("role is admin, operator or member"))?;
            let servers: Vec<String> = servers
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let inv = id.create_invite(role, servers)?;
            println!("{}", inv.code);
            eprintln!(
                "invite for a new {} of hub {} (expires in 1 h)",
                inv.role,
                cell_identity::fingerprint(&id.hub_pubkey)
            );
            Ok(())
        }
        Cmd::Upload { file, url, token } => upload(&file, &url, token),
    }
}

fn profile_cmd(cmd: ProfileCmd) -> Result<()> {
    use cell_identity::{client::profile_path, fingerprint, Profile};
    let path = profile_path();
    match cmd {
        ProfileCmd::Init { name } => {
            if path.exists() {
                anyhow::bail!("a profile already exists at {}", path.display());
            }
            let p = Profile::generate(&name);
            p.save(&path)?;
            println!(
                "created {} for {} ({})",
                path.display(),
                p.name,
                fingerprint(&p.pubkey()?)
            );
        }
        ProfileCmd::Show => {
            let p = Profile::load(&path)?;
            println!("name:        {}", p.name);
            println!("public key:  {}", p.pubkey()?);
            println!("fingerprint: {}", fingerprint(&p.pubkey()?));
            println!("file:        {}", path.display());
        }
        ProfileCmd::Export { file, passphrase } => {
            let p = Profile::load(&path)?;
            std::fs::write(&file, p.export(&passphrase)?)?;
            println!("exported to {}", file.display());
        }
        ProfileCmd::Import { file, passphrase } => {
            if path.exists() {
                anyhow::bail!("a profile already exists at {}", path.display());
            }
            let text = std::fs::read_to_string(&file)?;
            let p = Profile::import(&text, &passphrase)?;
            p.save(&path)?;
            println!("imported {} ({})", p.name, fingerprint(&p.pubkey()?));
        }
    }
    Ok(())
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
        let identity = members::Identity::open(&data, &name)?;
        eprintln!(
            "cell-hub: identity {} ({})",
            cell_identity::fingerprint(&identity.hub_pubkey),
            identity.hub_pubkey
        );
        if let Some(inv) = identity.ensure_first_admin_invite()? {
            eprintln!("cell-hub: no admin yet. Pair your profile with this invite code: {}", inv.code);
        }
        if token.is_some() {
            eprintln!("cell-hub: legacy admin token is set; sessions from paired profiles are the normal way in");
        }
        let app = Arc::new(api::App {
            store,
            servers,
            token: token.clone(),
            name,
            identity,
        });
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
