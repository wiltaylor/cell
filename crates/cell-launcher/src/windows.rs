//! The Windows launcher.
//!
//! Tier 0 serves the layers through WinFsp as one union volume mounted
//! under `%LOCALAPPDATA%\cell\mnt\<id>`: reads come straight from the
//! `.cell` file and writes go to the app's upper directory. Tier 1, used
//! when WinFsp is not installed or `CELL_TIER=extract` is set, extracts
//! the layers once into `%LOCALAPPDATA%\cell\apps\<id>\root` and runs the
//! app from there.
//!
//! The entry point's paths are the same `/app/...` paths a Linux cell
//! uses; `/` is the root of the layer stack. `$CELL_ROOT` in an argument,
//! the working directory or an environment value expands to that root on
//! the host. There is no process sandbox yet: the job object only makes
//! sure the app's process tree dies with the launcher.

use std::fs::{self, File};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use cell_core::format::{sidecar_dir_for, CellFile, HomeMode, LayerRole, Manifest};
use cell_core::unionfs::{self, LayerSpec, Union};
use cell_core::winfsp;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Where an app keeps its writable state.
struct State {
    base: PathBuf,
    upper: PathBuf,
    home: PathBuf,
    root: PathBuf,
    lock: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    WinFsp,
    Extract,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn self_path() -> Result<PathBuf> {
    if let Some(p) = env("CELL_FILE") {
        return Ok(PathBuf::from(p));
    }
    std::env::current_exe().context("locate own executable")
}

fn state_dirs(m: &Manifest, cell_path: &Path) -> Result<State> {
    let base = match m.isolation.home {
        HomeMode::Portable => sidecar_dir_for(cell_path).join("state"),
        _ => cell_core::host::data_home().join("apps").join(&m.app.id),
    };
    let s = State {
        upper: base.join("upper"),
        home: base.join("home"),
        root: base.join("root"),
        lock: base.join("instance.lock"),
        base,
    };
    fs::create_dir_all(&s.base).with_context(|| format!("create {}", s.base.display()))?;
    Ok(s)
}

/// Hold `instance.lock` open with no sharing: a second launcher of the
/// same app fails to open it and knows it is not alone.
fn instance_lock(state: &State) -> Option<File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(&state.lock)
        .ok()
}

/// A job that kills every process in it when the last handle closes. The
/// launcher joins it itself, so every child and grandchild inherits it and
/// dies with the launcher, like the Linux init does.
struct Job(HANDLE);

impl Job {
    fn new() -> Result<Self> {
        // SAFETY: plain Win32 calls with valid arguments.
        unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                bail!("CreateJobObject failed");
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                CloseHandle(h);
                bail!("SetInformationJobObject failed");
            }
            if AssignProcessToJobObject(h, GetCurrentProcess()) == 0 {
                // Already in a job that forbids nesting (older hosts):
                // children still run, they just outlive a killed launcher.
                eprintln!(
                    "cell: note: cannot join a job object; children may outlive the launcher"
                );
            }
            Ok(Job(h))
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is ours.
        unsafe { CloseHandle(self.0) };
    }
}

fn choose_tier() -> Tier {
    match env("CELL_TIER").as_deref() {
        Some("extract") => Tier::Extract,
        Some("winfsp") => Tier::WinFsp,
        _ => {
            if winfsp::dll_path().is_some() {
                Tier::WinFsp
            } else {
                Tier::Extract
            }
        }
    }
}

/// Map a manifest path (`/app/x`) onto the host root.
fn host_path(root: &Path, p: &str) -> PathBuf {
    let rel = p.trim_start_matches('/').replace('/', "\\");
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// `$CELL_ROOT` and `$PWD` in an argument or environment value.
fn expand(s: &str, root: &Path, pwd: &Path) -> String {
    s.replace("$CELL_ROOT", &root.to_string_lossy())
        .replace("${CELL_ROOT}", &root.to_string_lossy())
        .replace("$PWD", &pwd.to_string_lossy())
}

/// Extract every layer into `root` once; a stamp of the layer hashes says
/// when it is current.
fn extract_tier(cell: &CellFile, state: &State) -> Result<PathBuf> {
    let m = &cell.manifest;
    let want: String = m
        .layers
        .iter()
        .map(|l| format!("{} {}\n", l.name, l.blake3))
        .collect();
    let stamp = state.base.join("root.stamp");
    if state.root.is_dir() && fs::read_to_string(&stamp).ok().as_deref() == Some(want.as_str()) {
        return Ok(state.root.clone());
    }
    if winfsp::dll_path().is_some() {
        eprintln!("cell: CELL_TIER=extract: extracting the layers once");
    } else {
        eprintln!("cell: WinFsp is not installed; extracting the layers once (install WinFsp from https://winfsp.dev to run without this step)");
    }
    let _ = fs::remove_file(&stamp);
    if state.root.exists() {
        fs::remove_dir_all(&state.root)
            .with_context(|| format!("clear {}", state.root.display()))?;
    }
    fs::create_dir_all(&state.root)?;
    for l in &m.layers {
        let (file_path, off) = cell.layer_location(l)?;
        let file = File::open(&file_path).with_context(|| format!("open layer {}", l.name))?;
        let img = cell_core::squashfs::Squashfs::open(file, off)?;
        let dest = match l.role {
            LayerRole::Runtime | LayerRole::App => state.root.clone(),
            LayerRole::Data => host_path(&state.root, &l.mount),
        };
        let n = cell_core::extract::extract_all(&img, &dest)?;
        eprintln!("cell: layer {}: {n} files", l.name);
    }
    fs::write(&stamp, want)?;
    Ok(state.root.clone())
}

/// Mount the union; the mount lives until the returned value drops.
fn winfsp_tier(
    cell: &CellFile,
    state: &State,
    upper: &Path,
    mount_name: &str,
) -> Result<unionfs::Mounted> {
    let api = Arc::new(winfsp::load()?);
    let mut layers = Vec::new();
    for l in &cell.manifest.layers {
        let (file_path, off) = cell.layer_location(l)?;
        let file = File::open(&file_path).with_context(|| format!("open layer {}", l.name))?;
        layers.push(LayerSpec {
            name: l.name.clone(),
            file,
            base: off,
            mount: match l.role {
                LayerRole::Runtime | LayerRole::App => "/".to_string(),
                LayerRole::Data => l.mount.clone(),
            },
        });
    }
    let union = Union::new(api, layers, upper)?;
    let mount_point = cell_core::host::data_home().join("mnt").join(mount_name);
    let _ = &state.base;
    unionfs::mount(union, &mount_point, env("CELL_DEBUG").is_some())
}

fn home_env(m: &Manifest, state: &State) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    match m.isolation.home {
        HomeMode::Host => {}
        HomeMode::Private | HomeMode::Portable => {
            let home = &state.home;
            let roaming = home.join("AppData").join("Roaming");
            let local = home.join("AppData").join("Local");
            let temp = local.join("Temp");
            for d in [
                &roaming,
                &local,
                &temp,
                &home.join("Documents"),
                &home.join("Desktop"),
            ] {
                fs::create_dir_all(d)?;
            }
            let s = |p: &Path| p.to_string_lossy().into_owned();
            out.push(("USERPROFILE".into(), s(home)));
            out.push(("APPDATA".into(), s(&roaming)));
            out.push(("LOCALAPPDATA".into(), s(&local)));
            out.push(("TEMP".into(), s(&temp)));
            out.push(("TMP".into(), s(&temp)));
        }
    }
    Ok(out)
}

pub fn real_main() -> Result<i32> {
    let path = self_path()?;
    let cell = CellFile::open(&path)?;
    let m = &cell.manifest;
    if !m.app.is_windows() {
        bail!(
            "{} is a Linux cell (os = {}); it cannot run on Windows",
            path.display(),
            m.app.os
        );
    }
    let cmd = env("CELL_CMD").unwrap_or_default();
    match cmd.as_str() {
        "verify" => return crate::common::verify(&cell),
        "manifest" => {
            println!("{:#?}", m);
            return Ok(0);
        }
        "install" | "uninstall" | "steam-add" | "steam-remove" | "steam-list" | "desktop-entry" => {
            bail!("CELL_CMD={cmd} is not implemented on Windows yet");
        }
        "" | "run" | "info" | "shell" => {}
        other => bail!("unknown CELL_CMD {other}"),
    }

    let state = state_dirs(m, &path)?;
    let tier = choose_tier();
    let exec = if cmd == "shell" {
        vec![env("COMSPEC").unwrap_or_else(|| "cmd.exe".into())]
    } else {
        crate::common::select_exec(m)?
    };
    if exec.is_empty() {
        bail!("the manifest has no entry point");
    }

    if cmd == "info" {
        println!("{} {} ({})", m.app.name, m.app.version, m.app.id);
        println!("file: {}", path.display());
        println!("os: windows, tier: {:?}", tier);
        match winfsp::dll_path() {
            Some(p) => println!("winfsp: {}", p.display()),
            None => {
                println!("winfsp: not installed (https://winfsp.dev); the extract tier applies")
            }
        }
        println!("state: {}", state.base.display());
        for l in &m.layers {
            let found = match cell.layer_location(l) {
                Ok((p, _)) => p.display().to_string(),
                Err(e) => format!("MISSING: {e}"),
            };
            println!(
                "layer: {} ({:?}) at {} size {} MiB {:?} -> {}",
                l.name,
                l.role,
                l.mount,
                l.size / (1024 * 1024),
                l.placement,
                found
            );
        }
        println!("home: {:?}", m.isolation.home);
        if m.isolation.network {
            println!("network: host (no process sandbox on Windows yet)");
        } else {
            println!("network: manifest says none, but Windows has no process sandbox yet; the app has host network");
        }
        println!("exec: {:?}", exec);
        return Ok(0);
    }

    // Ctrl-C goes to the app; the launcher waits, then unmounts.
    // SAFETY: documented use of the API.
    unsafe { SetConsoleCtrlHandler(None, 1) };
    let _job = Job::new()?;

    let lock = instance_lock(&state);
    let (upper, mount_name) = if lock.is_some() {
        (state.upper.clone(), m.app.id.clone())
    } else {
        eprintln!(
            "cell: another instance of {} is running; this one keeps no changes outside its home",
            m.app.id
        );
        let scratch =
            std::env::temp_dir()
                .join("cell")
                .join(format!("{}-{}", m.app.id, std::process::id()));
        (scratch, format!("{}-{}", m.app.id, std::process::id()))
    };

    let mut mounted = None;
    let root = match tier {
        Tier::WinFsp => {
            let mnt = winfsp_tier(&cell, &state, &upper, &mount_name)?;
            let root = mnt.mount_point.clone();
            mounted = Some(mnt);
            root
        }
        Tier::Extract => extract_tier(&cell, &state)?,
    };

    let pwd = std::env::current_dir().unwrap_or_else(|_| root.clone());
    let program = if exec[0].starts_with('/') {
        host_path(&root, &exec[0])
    } else {
        PathBuf::from(expand(&exec[0], &root, &pwd))
    };
    let args: Vec<String> = exec[1..]
        .iter()
        .map(|a| expand(a, &root, &pwd))
        .chain(std::env::args().skip(1))
        .collect();
    let cwd = match m.entry.cwd.as_deref() {
        Some(c) if c.starts_with('/') => host_path(&root, c),
        Some(c) => PathBuf::from(expand(c, &root, &pwd)),
        None => program
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.clone()),
    };

    let mut c = Command::new(&program);
    c.args(&args).current_dir(&cwd);
    c.env("CELL_ROOT", &root);
    c.env("CELL_APP_ID", &m.app.id);
    for (k, v) in home_env(m, &state)? {
        c.env(k, v);
    }
    for (k, v) in &m.entry.env {
        c.env(k, expand(v, &root, &pwd));
    }
    let status = c
        .status()
        .with_context(|| format!("run {}", program.display()))?;
    let code = status.code().unwrap_or(1);
    drop(mounted);
    drop(lock);
    Ok(code)
}
