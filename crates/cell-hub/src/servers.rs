//! Game servers: instances of a cell run by the hub with the `server`
//! command, each with its own state directory, a log ring buffer, stdin for
//! console commands, backups and a file view of its config.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use cell_hub_api::{Backup, FileEntry, LogLine, LogPage, ServerInstance};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex};

use crate::store::{now, Store};

const LOG_CAP: usize = 3000;

/// Archive a state directory, skipping the overlay's scratch `work`
/// directories (the kernel owns them, mode 000) and instance locks.
fn append_state<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    rel: &Path,
) -> Result<()> {
    tar.append_dir(rel, dir)?;
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name();
        let name_s = name.to_string_lossy().to_string();
        let path = e.path();
        let rel_child = rel.join(&name);
        let md = std::fs::symlink_metadata(&path)?;
        if md.is_dir() {
            let parent_is_apps = dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n == "apps")
                .unwrap_or(false);
            if parent_is_apps && name_s == "work" {
                continue;
            }
            append_state(tar, &path, &rel_child)?;
        } else if name_s == "instance.lock" {
            continue;
        } else if md.file_type().is_symlink() {
            tar.append_path_with_name(&path, &rel_child)?;
        } else {
            let mut f = std::fs::File::open(&path)?;
            tar.append_file(&rel_child, &mut f)?;
        }
    }
    Ok(())
}

/// Delete a state directory. The overlay's `work` directories are mode
/// 000, so every directory is made writable first.
fn remove_state(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    fn open_up(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    open_up(&e.path());
                }
            }
        }
    }
    open_up(dir);
    std::fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub id: String,
    pub game_id: String,
    pub name: String,
    pub created_at: u64,
    /// Arguments a client passes to the game to join, e.g. `["-n", "host:3979"]`.
    #[serde(default)]
    pub join_args: Vec<String>,
}

pub struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    pid: u32,
    started_at: u64,
}

pub struct Logs {
    lines: VecDeque<LogLine>,
    seq: u64,
    tx: broadcast::Sender<LogLine>,
}

impl Logs {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Logs {
            lines: VecDeque::new(),
            seq: 0,
            tx,
        }
    }
    fn push(&mut self, stream: &str, text: String) {
        self.seq += 1;
        let line = LogLine {
            seq: self.seq,
            ts: now(),
            stream: stream.to_string(),
            text,
        };
        self.lines.push_back(line.clone());
        while self.lines.len() > LOG_CAP {
            self.lines.pop_front();
        }
        let _ = self.tx.send(line);
    }
    pub fn page(&self, after: u64, limit: usize) -> LogPage {
        let lines: Vec<LogLine> = self
            .lines
            .iter()
            .filter(|l| l.seq > after)
            .take(limit)
            .cloned()
            .collect();
        LogPage {
            lines,
            seq: self.seq,
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.tx.subscribe()
    }
}

pub struct Instance {
    pub record: InstanceRecord,
    pub running: Option<Running>,
    pub logs: Logs,
}

#[derive(Clone)]
pub struct Manager {
    store: Store,
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<Instance>>>>>,
}

fn id_for(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    let salt: u32 = rand::random();
    format!(
        "{}-{:06x}",
        if slug.is_empty() {
            "server".into()
        } else {
            slug
        },
        salt & 0xFF_FFFF
    )
}

impl Manager {
    pub fn new(store: Store) -> Result<Self> {
        let m = Manager {
            store,
            inner: Arc::new(Mutex::new(HashMap::new())),
        };
        m.load_all()?;
        Ok(m)
    }

    fn servers_dir(&self) -> PathBuf {
        self.store.root.join("servers")
    }

    pub fn instance_dir(&self, id: &str) -> PathBuf {
        self.servers_dir().join(id)
    }

    fn load_all(&self) -> Result<()> {
        let mut map = self.inner.blocking_lock();
        if let Ok(rd) = std::fs::read_dir(self.servers_dir()) {
            for e in rd.flatten() {
                let p = e.path().join("instance.json");
                if let Ok(b) = std::fs::read(&p) {
                    if let Ok(rec) = serde_json::from_slice::<InstanceRecord>(&b) {
                        map.insert(
                            rec.id.clone(),
                            Arc::new(Mutex::new(Instance {
                                record: rec,
                                running: None,
                                logs: Logs::new(),
                            })),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn create(
        &self,
        game_id: &str,
        name: &str,
        join_args: Vec<String>,
    ) -> Result<ServerInstance> {
        let game = self.store.load(game_id)?;
        let id = id_for(name);
        let dir = self.instance_dir(&id);
        std::fs::create_dir_all(dir.join("xdg"))?;
        let rec = InstanceRecord {
            id: id.clone(),
            game_id: game.id.clone(),
            name: name.to_string(),
            created_at: now(),
            join_args,
        };
        std::fs::write(dir.join("instance.json"), serde_json::to_vec_pretty(&rec)?)?;
        let inst = Arc::new(Mutex::new(Instance {
            record: rec,
            running: None,
            logs: Logs::new(),
        }));
        self.inner.lock().await.insert(id.clone(), inst);
        self.get(&id).await
    }

    async fn find(&self, id: &str) -> Result<Arc<Mutex<Instance>>> {
        self.inner
            .lock()
            .await
            .get(id)
            .cloned()
            .with_context(|| format!("no server {id}"))
    }

    async fn summary(&self, inst: &Instance) -> ServerInstance {
        let game = self.store.load(&inst.record.game_id).ok();
        ServerInstance {
            id: inst.record.id.clone(),
            game_id: inst.record.game_id.clone(),
            game_name: game.as_ref().map(|g| g.name.clone()).unwrap_or_default(),
            name: inst.record.name.clone(),
            state: if inst.running.is_some() {
                "running".into()
            } else {
                "stopped".into()
            },
            pid: inst.running.as_ref().map(|r| r.pid),
            started_at: inst.running.as_ref().map(|r| r.started_at),
            created_at: inst.record.created_at,
            log_seq: inst.logs.seq,
            command: if game.map(|g| g.has_server).unwrap_or(false) {
                "server".into()
            } else {
                "entry".into()
            },
            join_args: inst.record.join_args.clone(),
        }
    }

    pub async fn update(
        &self,
        id: &str,
        name: Option<String>,
        join_args: Option<Vec<String>>,
    ) -> Result<ServerInstance> {
        let inst = self.find(id).await?;
        let mut guard = inst.lock().await;
        if let Some(n) = name {
            if !n.trim().is_empty() {
                guard.record.name = n;
            }
        }
        if let Some(j) = join_args {
            guard.record.join_args = j;
        }
        std::fs::write(
            self.instance_dir(id).join("instance.json"),
            serde_json::to_vec_pretty(&guard.record)?,
        )?;
        Ok(self.summary(&guard).await)
    }

    pub async fn record(&self, id: &str) -> Result<InstanceRecord> {
        let inst = self.find(id).await?;
        let guard = inst.lock().await;
        Ok(guard.record.clone())
    }

    pub async fn get(&self, id: &str) -> Result<ServerInstance> {
        let inst = self.find(id).await?;
        let mut guard = inst.lock().await;
        self.reap(&mut guard).await;
        Ok(self.summary(&guard).await)
    }

    pub async fn list(&self) -> Vec<ServerInstance> {
        let all: Vec<Arc<Mutex<Instance>>> = self.inner.lock().await.values().cloned().collect();
        let mut out = Vec::new();
        for inst in all {
            let mut guard = inst.lock().await;
            self.reap(&mut guard).await;
            out.push(self.summary(&guard).await);
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Notice a child that exited on its own.
    async fn reap(&self, inst: &mut Instance) {
        if let Some(r) = inst.running.as_mut() {
            if let Ok(Some(status)) = r.child.try_wait() {
                inst.logs.push("hub", format!("server exited: {status}"));
                inst.running = None;
            }
        }
    }

    pub async fn start(&self, id: &str) -> Result<ServerInstance> {
        let inst = self.find(id).await?;
        let mut guard = inst.lock().await;
        self.reap(&mut guard).await;
        if guard.running.is_some() {
            bail!("server {id} is already running");
        }
        let game = self.store.load(&guard.record.game_id)?;
        let cell = self.store.cell_path(&game.id)?;
        let dir = self.instance_dir(id);
        let xdg = dir.join("xdg");
        std::fs::create_dir_all(&xdg)?;
        let mut cmd = Command::new(&cell);
        cmd.current_dir(&dir)
            .env("XDG_DATA_HOME", &xdg)
            .env("CELL_NO_GAMESCOPE", "1")
            // A dedicated server draws nothing; never fetch GPU drivers for it.
            .env("CELL_NVIDIA", "off")
            .env(
                "CELL_LAYER_PATH",
                self.store.root.join("layers").to_string_lossy().to_string(),
            )
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if game.has_server {
            cmd.env("CELL_COMMAND", "server");
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("start {}", cell.display()))?;
        let pid = child.id().unwrap_or(0);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        guard.logs.push(
            "hub",
            format!(
                "started {} ({}) as pid {pid}",
                game.name,
                if game.has_server {
                    "server command"
                } else {
                    "entry point"
                }
            ),
        );
        guard.running = Some(Running {
            child,
            stdin,
            pid,
            started_at: now(),
        });
        for (stream, reader) in [
            ("out", stdout.map(|s| Box::pin(s) as _)),
            ("err", stderr.map(|s| Box::pin(s) as _)),
        ] {
            let reader: Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>> = reader;
            if let Some(r) = reader {
                let inst2 = inst.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        inst2.lock().await.logs.push(stream, line);
                    }
                });
            }
        }
        Ok(self.summary(&guard).await)
    }

    pub async fn stop(&self, id: &str) -> Result<ServerInstance> {
        let inst = self.find(id).await?;
        let mut guard = inst.lock().await;
        self.reap(&mut guard).await;
        if let Some(mut r) = guard.running.take() {
            guard.logs.push("hub", "stopping (SIGTERM)".into());
            unsafe {
                libc::kill(r.pid as i32, libc::SIGTERM);
            }
            let waited =
                tokio::time::timeout(std::time::Duration::from_secs(15), r.child.wait()).await;
            if waited.is_err() {
                guard
                    .logs
                    .push("hub", "did not exit in 15 s, killing".into());
                let _ = r.child.kill().await;
            }
            guard.logs.push("hub", "stopped".into());
        }
        Ok(self.summary(&guard).await)
    }

    pub async fn input(&self, id: &str, line: &str) -> Result<()> {
        let inst = self.find(id).await?;
        let mut guard = inst.lock().await;
        let text = format!("{}\n", line.trim_end_matches('\n'));
        let Some(r) = guard.running.as_mut() else {
            bail!("server {id} is not running");
        };
        let Some(stdin) = r.stdin.as_mut() else {
            bail!("no stdin for server {id}");
        };
        stdin.write_all(text.as_bytes()).await?;
        stdin.flush().await?;
        guard.logs.push("hub", format!("> {}", line.trim_end()));
        Ok(())
    }

    pub async fn log(&self, id: &str, after: u64, limit: usize) -> Result<LogPage> {
        let inst = self.find(id).await?;
        let guard = inst.lock().await;
        Ok(guard.logs.page(after, limit))
    }

    pub async fn subscribe(&self, id: &str) -> Result<(LogPage, broadcast::Receiver<LogLine>)> {
        let inst = self.find(id).await?;
        let guard = inst.lock().await;
        Ok((guard.logs.page(0, 200), guard.logs.subscribe()))
    }

    pub async fn remove(&self, id: &str) -> Result<()> {
        self.stop(id).await?;
        self.inner.lock().await.remove(id);
        let dir = self.instance_dir(id);
        remove_state(&dir)?;
        Ok(())
    }

    // ---- files ---------------------------------------------------------------

    /// The instance's home directory inside its state: where the game keeps
    /// its config and saves.
    pub async fn home_dir(&self, id: &str) -> Result<PathBuf> {
        let inst = self.find(id).await?;
        let game_id = inst.lock().await.record.game_id.clone();
        let home = self
            .instance_dir(id)
            .join("xdg/cell/apps")
            .join(game_id)
            .join("home");
        std::fs::create_dir_all(&home)?;
        Ok(home)
    }

    fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
        let rel = rel.trim_start_matches('/');
        if rel.split('/').any(|c| c == "..") {
            bail!("path escapes the server directory");
        }
        Ok(root.join(rel))
    }

    pub async fn list_files(&self, id: &str, rel: &str) -> Result<Vec<FileEntry>> {
        let root = self.home_dir(id).await?;
        let dir = Self::resolve(&root, rel)?;
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dir)
            .with_context(|| format!("list {rel}"))?
            .flatten()
        {
            let md = e.metadata()?;
            out.push(FileEntry {
                name: e.file_name().to_string_lossy().to_string(),
                is_dir: md.is_dir(),
                size: md.len(),
            });
        }
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
        Ok(out)
    }

    pub async fn read_file(&self, id: &str, rel: &str) -> Result<String> {
        let root = self.home_dir(id).await?;
        let p = Self::resolve(&root, rel)?;
        let md = std::fs::metadata(&p).with_context(|| format!("read {rel}"))?;
        if md.len() > 1 << 20 {
            bail!("file is larger than 1 MiB");
        }
        let bytes = std::fs::read(&p)?;
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("not a text file"))
    }

    pub async fn write_file(&self, id: &str, rel: &str, text: &str) -> Result<()> {
        let root = self.home_dir(id).await?;
        let p = Self::resolve(&root, rel)?;
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, text).with_context(|| format!("write {rel}"))?;
        Ok(())
    }

    // ---- backups ---------------------------------------------------------------

    fn backups_dir(&self, id: &str) -> PathBuf {
        self.store.root.join("backups").join(id)
    }

    pub async fn backups(&self, id: &str) -> Result<Vec<Backup>> {
        self.find(id).await?;
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.backups_dir(id)) {
            for e in rd.flatten() {
                let md = e.metadata()?;
                out.push(Backup {
                    name: e.file_name().to_string_lossy().to_string(),
                    size: md.len(),
                    created_at: md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                });
            }
        }
        out.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(out)
    }

    pub async fn backup(&self, id: &str) -> Result<Backup> {
        let inst = self.find(id).await?;
        let dir = self.backups_dir(id);
        std::fs::create_dir_all(&dir)?;
        let name = format!("{}.tar.gz", now());
        let path = dir.join(&name);
        let xdg = self.instance_dir(id).join("xdg");
        {
            let f = std::fs::File::create(&path)?;
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut tar = tar::Builder::new(enc);
            tar.follow_symlinks(false);
            append_state(&mut tar, &xdg, Path::new("xdg")).context("archive the state")?;
            tar.into_inner()?.finish()?;
        }
        let size = std::fs::metadata(&path)?.len();
        inst.lock().await.logs.push(
            "hub",
            format!("backup written: {name} ({} KiB)", size / 1024),
        );
        Ok(Backup {
            name,
            size,
            created_at: now(),
        })
    }

    pub fn backup_path(&self, id: &str, name: &str) -> Result<PathBuf> {
        crate::store::safe_name(name)?;
        let p = self.backups_dir(id).join(name);
        if !p.is_file() {
            bail!("no backup {name}");
        }
        Ok(p)
    }

    pub async fn restore(&self, id: &str, name: &str) -> Result<()> {
        let path = self.backup_path(id, name)?;
        self.stop(id).await?;
        let inst = self.find(id).await?;
        let xdg = self.instance_dir(id).join("xdg");
        remove_state(&xdg)?;
        let f = std::fs::File::open(&path)?;
        let dec = flate2::read::GzDecoder::new(f);
        let mut ar = tar::Archive::new(dec);
        ar.unpack(self.instance_dir(id))
            .context("unpack the backup")?;
        inst.lock()
            .await
            .logs
            .push("hub", format!("restored from {name}"));
        Ok(())
    }

    pub async fn delete_backup(&self, id: &str, name: &str) -> Result<()> {
        let p = self.backup_path(id, name)?;
        std::fs::remove_file(p)?;
        Ok(())
    }
}
