//! Types shared by the Cell Hub server, its terminal console, its web UI
//! and the desktop launcher. Everything here is JSON on the wire.

use serde::{Deserialize, Serialize};

/// One layer of a cell as the hub sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerInfo {
    pub name: String,
    pub role: String,
    /// `embedded`, `sidecar` or `referenced`.
    pub placement: String,
    /// File name for sidecar and referenced layers.
    pub file: Option<String>,
    pub size: u64,
    /// Whether the hub has the file (always true for embedded).
    pub available: bool,
}

/// What a cell grants, summarized for a details page.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GrantSummary {
    pub network: bool,
    pub home: String,
    pub display: Vec<String>,
    pub audio: Vec<String>,
    pub input: Vec<String>,
    pub gpu: bool,
    pub nvidia: String,
    pub dbus: String,
}

/// A game (or any cell) in the hub's catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub version: String,
    pub arch: String,
    pub comment: Option<String>,
    pub categories: Vec<String>,
    pub protocols: Vec<String>,
    /// Named commands from the manifest (`CELL_COMMAND=<name>`).
    pub commands: Vec<String>,
    /// True when the manifest has a `server` command.
    pub has_server: bool,
    pub grants: GrantSummary,
    /// The `.cell` file name on the hub.
    pub file: String,
    pub size: u64,
    pub layers: Vec<LayerInfo>,
    /// Art kinds the hub can serve: icon, portrait, wide, hero, logo.
    pub art: Vec<String>,
    pub uploaded_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInstance {
    pub id: String,
    pub game_id: String,
    pub game_name: String,
    pub name: String,
    /// `running` or `stopped`.
    pub state: String,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub created_at: u64,
    /// Sequence number of the newest log line.
    pub log_seq: u64,
    /// The command the hub runs: `server` or `entry`.
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateServer {
    pub game_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub seq: u64,
    pub ts: u64,
    /// `out`, `err` or `hub`.
    pub stream: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPage {
    pub lines: Vec<LogLine>,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleInput {
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub name: String,
    pub size: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContent {
    pub path: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubInfo {
    pub name: String,
    pub version: String,
    pub games: usize,
    pub servers: usize,
    pub running: usize,
    /// Whether admin calls need a token.
    pub auth_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
