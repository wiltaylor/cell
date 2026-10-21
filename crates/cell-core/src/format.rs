//! On-disk layout of a `.cell` file and the manifest it carries.
//!
//! ```text
//! | launcher ELF | layer 0 | layer 1 | ... | manifest (CBOR) | trailer (64 B) |
//! ```
//!
//! The launcher finds the trailer by reading the last 64 bytes of its own
//! executable. The manifest lists every layer with its offset and size.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const TRAILER_LEN: u64 = 64;
pub const MAGIC: &[u8; 8] = b"CELLv1\0\0";
pub const FORMAT_VERSION: u32 = 1;

/// The fixed 64-byte trailer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trailer {
    pub version: u32,
    pub flags: u32,
    pub manifest_off: u64,
    pub manifest_len: u64,
    pub launcher_len: u64,
}

impl Trailer {
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut b = [0u8; 64];
        b[..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&self.version.to_le_bytes());
        b[12..16].copy_from_slice(&self.flags.to_le_bytes());
        b[16..24].copy_from_slice(&self.manifest_off.to_le_bytes());
        b[24..32].copy_from_slice(&self.manifest_len.to_le_bytes());
        b[32..40].copy_from_slice(&self.launcher_len.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        if b.len() < 64 || &b[..8] != MAGIC {
            bail!("no Cell trailer: this file has no application payload");
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        Ok(Trailer {
            version: u32_at(8),
            flags: u32_at(12),
            manifest_off: u64_at(16),
            manifest_len: u64_at(24),
            launcher_len: u64_at(32),
        })
    }
}

// ---- manifest ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LayerRole {
    /// Part of the root filesystem stack (overlay lower).
    Runtime,
    App,
    /// Mounted at `mount` inside the root.
    Data,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Placement {
    Embedded,
    /// A file beside the `.cell`, in `<name>.cell.d/<file>`.
    Sidecar {
        file: String,
    },
    /// A content-addressed file, `<layer>-<hash16>.layer`, that lives in
    /// the cell's directory and can be shared by every cell there. The
    /// launcher also looks in `CELL_LAYER_PATH`, `<name>.cell.d/`,
    /// `$XDG_DATA_HOME/cell/layers` and `$XDG_CACHE_HOME/cell/layers`.
    Referenced {
        file: String,
    },
}

/// The file name of a referenced layer: the layer name plus the first 16
/// hex digits of its BLAKE3 hash.
pub fn referenced_file_name(layer_name: &str, blake3_hex: &str) -> String {
    format!(
        "{layer_name}-{}.layer",
        &blake3_hex[..16.min(blake3_hex.len())]
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerMeta {
    pub name: String,
    pub role: LayerRole,
    /// Mount path inside the sandbox. `/` for root-stack layers.
    pub mount: String,
    pub placement: Placement,
    /// Offset from the start of the file (embedded) or of the sidecar.
    pub offset: u64,
    pub size: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub arch: String,
    /// `linux` or `windows`. Decides which launcher heads the file and
    /// how the entry point's paths are read. Older manifests have no
    /// field, and they are Linux cells.
    #[serde(default = "default_os")]
    pub os: String,
}

fn default_os() -> String {
    "linux".to_string()
}

impl AppInfo {
    pub fn is_windows(&self) -> bool {
        self.os == "windows"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NamedCommand {
    pub name: String,
    pub exec: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Entry {
    pub exec: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub commands: Vec<NamedCommand>,
    pub gamescope: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum HomeMode {
    #[default]
    Private,
    Portable,
    Host,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum PidMode {
    #[default]
    Private,
    Host,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum BindMode {
    #[default]
    Ro,
    Rw,
    Dev,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bind {
    pub mode: BindMode,
    pub host: String,
    pub guest: String,
    /// Skip quietly when the host path is missing.
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Isolation {
    pub network: bool,
    pub home: HomeMode,
    pub pid: PidMode,
    pub shm: bool,
    pub binds: Vec<Bind>,
}

impl Default for Isolation {
    fn default() -> Self {
        Isolation {
            network: false,
            home: HomeMode::Private,
            pid: PidMode::Private,
            shm: false,
            binds: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Display {
    Wayland,
    X11,
    X11Fallback,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Audio {
    Pipewire,
    Pulse,
    Alsa,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Input {
    Gamepad,
    Uinput,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Dbus {
    #[default]
    None,
    Session,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum NvidiaMode {
    #[default]
    Auto,
    Fetch,
    Host,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gpu {
    pub enable: bool,
    pub nvidia: NvidiaMode,
    pub vaapi: bool,
}

impl Default for Gpu {
    fn default() -> Self {
        Gpu {
            enable: true,
            nvidia: NvidiaMode::Auto,
            vaapi: true,
        }
    }
}

/// Desktop entry data: the icon, the menu categories, the MIME types and
/// URL schemes the app handles. `CELL_CMD=install` writes it out.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Desktop {
    pub icon_png: Option<Vec<u8>>,
    pub categories: Vec<String>,
    pub mime: Vec<String>,
    #[serde(default)]
    pub comment: Option<String>,
    /// URL schemes, registered as `x-scheme-handler/<scheme>`.
    #[serde(default)]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// Steam library artwork and shortcut options. `CELL_CMD=steam-add` adds
/// the cell as a non-Steam game with this art.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Steam {
    /// Library capsule, 600x900.
    pub portrait: Option<Vec<u8>>,
    /// Header capsule, 920x430 (or 460x215).
    pub wide: Option<Vec<u8>>,
    /// Library hero, 1920x620 (or 3840x1240).
    pub hero: Option<Vec<u8>>,
    /// Logo over the hero, transparent PNG.
    pub logo: Option<Vec<u8>>,
    pub tags: Vec<String>,
    pub launch_options: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Integration {
    pub display: Vec<Display>,
    pub audio: Vec<Audio>,
    pub input: Vec<Input>,
    pub dbus: Dbus,
    pub fonts: bool,
    pub themes: bool,
    pub gpu: Gpu,
    pub desktop: Option<Desktop>,
    #[serde(default)]
    pub steam: Option<Steam>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub app: AppInfo,
    pub entry: Entry,
    pub isolation: Isolation,
    pub integration: Integration,
    pub layers: Vec<LayerMeta>,
}

impl Manifest {
    pub fn to_cbor(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        ciborium::into_writer(self, &mut out).context("encode manifest")?;
        Ok(out)
    }

    pub fn from_cbor(bytes: &[u8]) -> Result<Self> {
        ciborium::from_reader(bytes).context("decode manifest")
    }
}

/// The sidecar directory of a cell at `path`: `<name>.cell.d`, with a
/// trailing `.exe` removed first.
pub fn sidecar_dir_for(path: &Path) -> std::path::PathBuf {
    let base = match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("exe") => path.with_extension(""),
        _ => path.to_path_buf(),
    };
    let mut s = base.as_os_str().to_owned();
    s.push(".d");
    std::path::PathBuf::from(s)
}

/// A `.cell` file opened for reading: the trailer and the manifest.
pub struct CellFile {
    pub path: std::path::PathBuf,
    pub file: File,
    pub trailer: Trailer,
    pub manifest: Manifest,
}

impl CellFile {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let len = file.metadata()?.len();
        if len < TRAILER_LEN {
            bail!("{} is too small to be a .cell file", path.display());
        }
        let mut tb = [0u8; 64];
        crate::pread::read_exact_at(&file, &mut tb, len - TRAILER_LEN)?;
        let trailer = Trailer::from_bytes(&tb)?;
        if trailer.version != FORMAT_VERSION {
            bail!(
                "unsupported Cell format version {} (this launcher reads {})",
                trailer.version,
                FORMAT_VERSION
            );
        }
        let mut mb = vec![0u8; trailer.manifest_len as usize];
        file.seek(SeekFrom::Start(trailer.manifest_off))?;
        file.read_exact(&mut mb)?;
        let manifest = Manifest::from_cbor(&mb)?;
        Ok(CellFile {
            path: path.to_path_buf(),
            file,
            trailer,
            manifest,
        })
    }

    /// Where a layer's bytes live: (file path, offset). A referenced layer
    /// is searched for; a missing one is an error that names every
    /// directory that was tried.
    pub fn layer_location(&self, layer: &LayerMeta) -> Result<(std::path::PathBuf, u64)> {
        match &layer.placement {
            Placement::Embedded => Ok((self.path.clone(), layer.offset)),
            Placement::Sidecar { file } => Ok((self.sidecar_dir().join(file), layer.offset)),
            Placement::Referenced { file } => {
                let dirs = self.referenced_search_dirs();
                for d in &dirs {
                    let p = d.join(file);
                    if p.is_file() {
                        return Ok((p, layer.offset));
                    }
                }
                let list: Vec<String> = dirs.iter().map(|d| format!("  {}", d.display())).collect();
                bail!(
                    "layer `{}` is referenced, not embedded: put `{}` ({} MiB) beside this cell. \
                     Searched:\n{}",
                    layer.name,
                    file,
                    layer.size / (1024 * 1024),
                    list.join("\n")
                )
            }
        }
    }

    /// Directories searched for referenced layers, in order.
    pub fn referenced_search_dirs(&self) -> Vec<std::path::PathBuf> {
        let mut dirs = Vec::new();
        if let Ok(v) = std::env::var("CELL_LAYER_PATH") {
            let sep = if cfg!(windows) { ';' } else { ':' };
            for d in v.split(sep).filter(|d| !d.is_empty()) {
                dirs.push(std::path::PathBuf::from(d));
            }
        }
        if let Some(parent) = self.path.parent() {
            dirs.push(parent.to_path_buf());
        }
        dirs.push(self.sidecar_dir());
        dirs.push(crate::host::data_home().join("layers"));
        dirs.push(crate::host::cache_home().join("layers"));
        dirs
    }

    /// `<name>.cell.d` beside the file. A Windows cell is `<name>.cell.exe`
    /// and its sidecar directory drops the `.exe`, so both platforms use
    /// `<name>.cell.d`.
    pub fn sidecar_dir(&self) -> std::path::PathBuf {
        sidecar_dir_for(&self.path)
    }
}
