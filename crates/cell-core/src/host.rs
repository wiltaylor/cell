//! What the host offers. The launcher probes once and decides its tier.

#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct HostProbe {
    pub kernel: String,
    pub kernel_tuple: (u32, u32),
    pub userns: bool,
    pub fuse: bool,
    pub wayland: Option<PathBuf>,
    pub x11_socket: Option<PathBuf>,
    pub display: Option<String>,
    pub xauthority: Option<PathBuf>,
    pub pipewire: Option<PathBuf>,
    pub pulse: Option<PathBuf>,
    pub dri: bool,
    pub nvidia_version: Option<String>,
    pub nvidia_devices: Vec<PathBuf>,
    pub dbus_session: Option<PathBuf>,
    pub runtime_dir: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[cfg(target_os = "linux")]
pub fn probe() -> HostProbe {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let runtime_dir = env("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut parts = kernel
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty());
    let kernel_tuple = (
        parts.next().and_then(|s| s.parse().ok()).unwrap_or(0),
        parts.next().and_then(|s| s.parse().ok()).unwrap_or(0),
    );
    let userns = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
        && std::fs::read_to_string("/proc/sys/user/max_user_namespaces")
            .map(|v| v.trim() != "0")
            .unwrap_or(true);
    let wayland = env("WAYLAND_DISPLAY").map(|d| {
        if d.starts_with('/') {
            PathBuf::from(d)
        } else {
            runtime_dir.join(d)
        }
    });
    let wayland = wayland.filter(|p| p.exists());
    let display = env("DISPLAY");
    let x11_socket = display
        .as_deref()
        .and_then(crate::xauth::display_number)
        .map(|n| PathBuf::from(format!("/tmp/.X11-unix/X{n}")))
        .filter(|p| p.exists());
    let xauthority = env("XAUTHORITY")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".Xauthority")))
        .filter(|p| p.exists());
    let pipewire = Some(runtime_dir.join("pipewire-0")).filter(|p| p.exists());
    let pulse = Some(runtime_dir.join("pulse/native")).filter(|p| p.exists());
    let nvidia_version = std::fs::read_to_string("/sys/module/nvidia/version")
        .ok()
        .map(|v| v.trim().to_string());
    let mut nvidia_devices = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/dev") {
        for e in rd.flatten() {
            let n = e.file_name();
            if n.to_string_lossy().starts_with("nvidia") {
                nvidia_devices.push(e.path());
            }
        }
    }
    nvidia_devices.sort();
    let dbus_session = env("DBUS_SESSION_BUS_ADDRESS")
        .and_then(|a| {
            a.split(',')
                .find_map(|part| part.strip_prefix("unix:path=").map(PathBuf::from))
        })
        .or_else(|| Some(runtime_dir.join("bus")))
        .filter(|p| p.exists());
    let user = env("USER").unwrap_or_else(|| "user".to_string());
    let home = env("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/home/{user}")));
    HostProbe {
        kernel,
        kernel_tuple,
        userns,
        fuse: Path::new("/dev/fuse").exists(),
        wayland,
        x11_socket,
        display,
        xauthority,
        pipewire,
        pulse,
        dri: Path::new("/dev/dri").exists(),
        nvidia_version,
        nvidia_devices,
        dbus_session,
        runtime_dir,
        uid,
        gid,
        user,
        home,
    }
}

/// `$XDG_DATA_HOME/cell` or `~/.local/share/cell`; on Windows
/// `%LOCALAPPDATA%\cell`.
#[cfg(windows)]
pub fn data_home() -> PathBuf {
    local_app_data().join("cell")
}

/// `%LOCALAPPDATA%\cell\cache` on Windows.
#[cfg(windows)]
pub fn cache_home() -> PathBuf {
    local_app_data().join("cell").join("cache")
}

#[cfg(windows)]
fn local_app_data() -> PathBuf {
    env("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| env("USERPROFILE").map(|u| PathBuf::from(u).join("AppData").join("Local")))
        .unwrap_or_else(|| PathBuf::from("C:\\cell"))
}

#[cfg(not(windows))]
pub fn data_home() -> PathBuf {
    env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env("HOME").unwrap_or_else(|| "/tmp".into())).join(".local/share")
        })
        .join("cell")
}

/// `$XDG_CACHE_HOME/cell` or `~/.cache/cell`.
#[cfg(not(windows))]
pub fn cache_home() -> PathBuf {
    env("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env("HOME").unwrap_or_else(|| "/tmp".into())).join(".cache")
        })
        .join("cell")
}
