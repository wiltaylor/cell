//! The catalog: cells uploaded to the hub, their manifests and their art.
//!
//! Layout under the data directory:
//! ```text
//! games/<id>/<file>.cell      the cell
//! games/<id>/meta.json        the Game summary
//! games/<id>/art/<kind>.png   icon, portrait, wide, hero, logo
//! layers/<file>.layer         referenced layers, shared by every game
//! ```

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use cell_core::format::{CellFile, Display, HomeMode, Manifest, NvidiaMode, Placement};
use cell_hub_api::{Game, GrantSummary, LayerInfo};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}

/// Only plain file names: no separators, no `..`.
pub fn safe_name(name: &str) -> Result<&str> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
        || name.contains('\0')
    {
        bail!("invalid file name");
    }
    Ok(name)
}

fn grants(m: &Manifest) -> GrantSummary {
    GrantSummary {
        network: m.isolation.network,
        home: format!("{:?}", m.isolation.home).to_lowercase(),
        display: m
            .integration
            .display
            .iter()
            .map(|d| match d {
                Display::Wayland => "wayland".into(),
                Display::X11 => "x11".into(),
                Display::X11Fallback => "x11_fallback".into(),
            })
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
        nvidia: match m.integration.gpu.nvidia {
            NvidiaMode::Auto => "auto".into(),
            NvidiaMode::Fetch => "fetch".into(),
            NvidiaMode::Host => "host".into(),
            NvidiaMode::None => "off".into(),
        },
        dbus: format!("{:?}", m.integration.dbus).to_lowercase(),
    }
}

impl Store {
    pub fn new(root: &Path) -> Result<Self> {
        for d in ["games", "layers", "servers", "backups"] {
            std::fs::create_dir_all(root.join(d))?;
        }
        Ok(Store {
            root: root.to_path_buf(),
        })
    }

    pub fn game_dir(&self, id: &str) -> PathBuf {
        self.root.join("games").join(id)
    }

    pub fn layer_path(&self, file: &str) -> PathBuf {
        self.root.join("layers").join(file)
    }

    /// Register an uploaded cell. The file is already at `path`.
    pub fn import(&self, path: &Path, file_name: &str) -> Result<Game> {
        let cell = CellFile::open(path)?;
        let m = &cell.manifest;
        let id = m.app.id.clone();
        if id.is_empty() || id.contains('/') || id.contains("..") {
            bail!("the cell has no usable app id");
        }
        let dir = self.game_dir(&id);
        std::fs::create_dir_all(dir.join("art"))?;
        // Replace any older file for this id.
        for e in std::fs::read_dir(&dir)?.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.ends_with(".cell") && n != file_name {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let dest = dir.join(file_name);
        if dest != path {
            std::fs::rename(path, &dest).or_else(|_| std::fs::copy(path, &dest).map(|_| ()))?;
        }
        std::fs::set_permissions(&dest, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        let size = std::fs::metadata(&dest)?.len();
        let mut art = Vec::new();
        let mut put = |kind: &str, bytes: &Option<Vec<u8>>| {
            if let Some(b) = bytes {
                if std::fs::write(dir.join("art").join(format!("{kind}.png")), b).is_ok() {
                    art.push(kind.to_string());
                }
            }
        };
        let d = m.integration.desktop.clone().unwrap_or_default();
        put("icon", &d.icon_png);
        let st = m.integration.steam.clone().unwrap_or_default();
        put("portrait", &st.portrait);
        put("wide", &st.wide);
        put("hero", &st.hero);
        put("logo", &st.logo);
        let layers = m
            .layers
            .iter()
            .map(|l| {
                let (placement, file, available) = match &l.placement {
                    Placement::Embedded => ("embedded".to_string(), None, true),
                    Placement::Sidecar { file } => (
                        "sidecar".to_string(),
                        Some(file.clone()),
                        dir.join(file).exists(),
                    ),
                    Placement::Referenced { file } => (
                        "referenced".to_string(),
                        Some(file.clone()),
                        self.layer_path(file).exists(),
                    ),
                };
                LayerInfo {
                    name: l.name.clone(),
                    role: format!("{:?}", l.role).to_lowercase(),
                    placement,
                    file,
                    size: l.size,
                    available,
                }
            })
            .collect();
        let commands: Vec<String> = m.entry.commands.iter().map(|c| c.name.clone()).collect();
        let game = Game {
            id: id.clone(),
            name: m.app.name.clone(),
            version: m.app.version.clone(),
            arch: m.app.arch.clone(),
            comment: d.comment.clone(),
            categories: d.categories.clone(),
            protocols: d.protocols.clone(),
            has_server: commands.iter().any(|c| c == "server"),
            commands,
            grants: grants(m),
            file: file_name.to_string(),
            size,
            layers,
            art,
            uploaded_at: now(),
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&game)?)?;
        let _ = m.isolation.home == HomeMode::Private;
        Ok(game)
    }

    pub fn load(&self, id: &str) -> Result<Game> {
        let p = self.game_dir(id).join("meta.json");
        let bytes = std::fs::read(&p).with_context(|| format!("no game {id}"))?;
        let mut g: Game = serde_json::from_slice(&bytes)?;
        for l in &mut g.layers {
            if let Some(f) = &l.file {
                l.available = match l.placement.as_str() {
                    "referenced" => self.layer_path(f).exists(),
                    "sidecar" => self.game_dir(id).join(f).exists(),
                    _ => true,
                };
            }
        }
        Ok(g)
    }

    pub fn list(&self) -> Vec<Game> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.root.join("games")) {
            for e in rd.flatten() {
                let id = e.file_name().to_string_lossy().to_string();
                if let Ok(g) = self.load(&id) {
                    out.push(g);
                }
            }
        }
        out.sort_by_key(|g| g.name.to_lowercase());
        out
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.game_dir(id);
        if !dir.join("meta.json").exists() {
            bail!("no game {id}");
        }
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// The cell file, made executable (uploads arrive without that bit).
    pub fn cell_path(&self, id: &str) -> Result<PathBuf> {
        let g = self.load(id)?;
        let p = self.game_dir(id).join(g.file);
        let _ = std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755));
        Ok(p)
    }
}
