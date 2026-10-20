//! Map the manifest's isolation and integration blocks onto a sandbox
//! [`Spec`]: binds, tmpfs, files, symlinks and environment.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cell_core::format::{
    Audio, BindMode, Dbus, Display, HomeMode, Input, Manifest, NvidiaMode, PidMode,
};
use cell_core::gpu::DriverDir;
use cell_core::host::HostProbe;
use cell_core::sandbox::{BindSpec, Spec};

pub struct StateDirs {
    pub upper: PathBuf,
    pub work: PathBuf,
    pub home: PathBuf,
}

/// Take the app's instance lock. `None` means another instance holds it.
/// The lock lives as long as the returned file.
pub fn instance_lock(state: &StateDirs) -> Result<Option<std::fs::File>> {
    use std::os::unix::io::AsRawFd;
    let path = state
        .upper
        .parent()
        .unwrap_or(&state.upper)
        .join("instance.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if r == 0 {
        Ok(Some(f))
    } else {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(e).context("instance lock")
        }
    }
}

/// Where this app keeps its writable state.
pub fn state_dirs(m: &Manifest, cell_path: &Path) -> Result<StateDirs> {
    let base = match m.isolation.home {
        HomeMode::Portable => {
            let mut s = cell_path.as_os_str().to_owned();
            s.push(".d");
            PathBuf::from(s).join("state")
        }
        _ => cell_core::host::data_home().join("apps").join(&m.app.id),
    };
    let dirs = StateDirs {
        upper: base.join("upper"),
        work: base.join("work"),
        home: base.join("home"),
    };
    for d in [&dirs.upper, &dirs.work, &dirs.home] {
        std::fs::create_dir_all(d)?;
    }
    Ok(dirs)
}

fn bind(spec: &mut Spec, host: impl Into<PathBuf>, guest: impl Into<String>, mode: BindMode) {
    spec.binds.push(BindSpec {
        host: host.into(),
        guest: guest.into(),
        mode,
        optional: true,
    });
}

fn env(spec: &mut Spec, k: &str, v: impl Into<String>) {
    let v = v.into();
    if let Some(slot) = spec.env.iter_mut().find(|(key, _)| key == k) {
        slot.1 = v;
    } else {
        spec.env.push((k.to_string(), v));
    }
}

fn host_env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

pub struct GrantReport {
    pub lines: Vec<String>,
}

/// Build the spec. `exec_override` replaces the entry point (shell mode).
pub fn build_spec(
    m: &Manifest,
    host: &HostProbe,
    state: &StateDirs,
    exec_override: Option<Vec<String>>,
) -> (Spec, GrantReport) {
    let mut spec = Spec::default();
    let mut report = Vec::new();
    let uid = host.uid;
    let run_user = format!("/run/user/{uid}");

    // Identity and base environment.
    let home = host.home.to_string_lossy().to_string();
    env(
        &mut spec,
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/bin:/bin",
    );
    env(&mut spec, "HOME", home.clone());
    env(&mut spec, "USER", host.user.clone());
    env(&mut spec, "LOGNAME", host.user.clone());
    env(&mut spec, "SHELL", "/bin/sh");
    env(
        &mut spec,
        "TERM",
        host_env("TERM").unwrap_or_else(|| "xterm".into()),
    );
    env(
        &mut spec,
        "LANG",
        host_env("LANG").unwrap_or_else(|| "C.UTF-8".into()),
    );
    env(&mut spec, "XDG_RUNTIME_DIR", run_user.clone());
    env(&mut spec, "XDG_DATA_DIRS", "/usr/local/share:/usr/share");
    env(&mut spec, "XDG_CONFIG_DIRS", "/etc/xdg");
    env(&mut spec, "CELL_APP_ID", m.app.id.clone());
    if let Some(tz) = host_env("TZ") {
        env(&mut spec, "TZ", tz);
    }
    spec.tmpfs.push(run_user.clone());
    let passwd = format!(
        "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/:/usr/bin/nologin\n{}:x:{}:{}:{}:{}:/bin/sh\n",
        host.user, uid, host.gid, host.user, home
    );
    let group = format!(
        "root:x:0:\nnobody:x:65534:\nvideo:x:985:{u}\naudio:x:986:{u}\ninput:x:994:{u}\nrender:x:989:{u}\n{u}:x:{g}:\n",
        u = host.user,
        g = host.gid
    );
    spec.files
        .push(("/etc/passwd".into(), passwd.into_bytes(), 0o644));
    spec.files
        .push(("/etc/group".into(), group.into_bytes(), 0o644));

    // Home.
    match m.isolation.home {
        HomeMode::Host => {
            bind(&mut spec, &host.home, home.clone(), BindMode::Rw);
            report.push("home: host $HOME (read-write)".into());
        }
        HomeMode::Private | HomeMode::Portable => {
            bind(&mut spec, &state.home, home.clone(), BindMode::Rw);
            report.push(format!("home: {}", state.home.display()));
        }
    }

    // Network.
    spec.new_net = !m.isolation.network;
    if m.isolation.network {
        for f in ["/etc/resolv.conf", "/etc/hosts"] {
            bind(&mut spec, f, f, BindMode::Ro);
        }
        report.push("network: shared with host".into());
    } else {
        report.push("network: none (loopback only)".into());
    }
    spec.new_pid = m.isolation.pid == PidMode::Private;
    spec.host_proc = m.isolation.pid == PidMode::Host;
    if m.isolation.shm {
        bind(&mut spec, "/dev/shm", "/dev/shm", BindMode::Rw);
    }

    // Display.
    let want_wayland = m.integration.display.contains(&Display::Wayland);
    let want_x11 = m.integration.display.contains(&Display::X11)
        || (m.integration.display.contains(&Display::X11Fallback) && host.wayland.is_none());
    // X11 clients pass MIT-SHM segments to the X server, so an X11 grant
    // keeps the host IPC namespace (Flatpak's `--share=ipc` rule).
    spec.new_ipc = !m.isolation.shm && !(want_x11 && host.x11_socket.is_some());
    if want_wayland {
        if let Some(sock) = &host.wayland {
            let name = sock.file_name().unwrap().to_string_lossy().to_string();
            bind(&mut spec, sock, format!("{run_user}/{name}"), BindMode::Rw);
            env(&mut spec, "WAYLAND_DISPLAY", name.clone());
            env(&mut spec, "XDG_SESSION_TYPE", "wayland");
            report.push(format!("display: wayland ({name})"));
        } else {
            report.push("display: wayland requested, no host socket".into());
        }
    }
    if want_x11 {
        if let (Some(sock), Some(display)) = (&host.x11_socket, &host.display) {
            let name = sock.file_name().unwrap().to_string_lossy().to_string();
            let num = cell_core::xauth::display_number(display).unwrap_or_default();
            bind(
                &mut spec,
                sock,
                format!("/tmp/.X11-unix/{name}"),
                BindMode::Rw,
            );
            env(&mut spec, "DISPLAY", format!(":{num}"));
            if !want_wayland || host.wayland.is_none() {
                env(&mut spec, "XDG_SESSION_TYPE", "x11");
            }
            if let Some(xa) = &host.xauthority {
                if let Ok(bytes) = std::fs::read(xa) {
                    if let Ok(cookie) = cell_core::xauth::wildcard_cookie(&bytes, display) {
                        let p = format!("{run_user}/Xauthority");
                        spec.files.push((p.clone(), cookie, 0o600));
                        env(&mut spec, "XAUTHORITY", p);
                    }
                }
            }
            report.push(format!("display: x11 (:{num})"));
        } else {
            report.push("display: x11 requested, no host socket".into());
        }
    }

    // Audio.
    if m.integration.audio.contains(&Audio::Pipewire) {
        if let Some(p) = &host.pipewire {
            bind(&mut spec, p, format!("{run_user}/pipewire-0"), BindMode::Rw);
            report.push("audio: pipewire".into());
        }
    }
    if m.integration.audio.contains(&Audio::Pulse) {
        if let Some(p) = &host.pulse {
            bind(
                &mut spec,
                p,
                format!("{run_user}/pulse/native"),
                BindMode::Rw,
            );
            env(
                &mut spec,
                "PULSE_SERVER",
                format!("unix:{run_user}/pulse/native"),
            );
            report.push("audio: pulseaudio".into());
        }
    }
    if m.integration.audio.contains(&Audio::Alsa) {
        bind(&mut spec, "/dev/snd", "/dev/snd", BindMode::Dev);
        report.push("audio: alsa (/dev/snd)".into());
    }

    // Input.
    if m.integration.input.contains(&Input::Gamepad) {
        bind(&mut spec, "/dev/input", "/dev/input", BindMode::Dev);
        bind(&mut spec, "/run/udev", "/run/udev", BindMode::Ro);
        if let Ok(rd) = std::fs::read_dir("/dev") {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with("hidraw") {
                    bind(&mut spec, e.path(), format!("/dev/{n}"), BindMode::Dev);
                }
            }
        }
        report.push("input: gamepads (/dev/input, /run/udev)".into());
    }
    if m.integration.input.contains(&Input::Uinput) {
        bind(&mut spec, "/dev/uinput", "/dev/uinput", BindMode::Dev);
    }

    // D-Bus.
    if m.integration.dbus == Dbus::Session {
        if let Some(b) = &host.dbus_session {
            bind(&mut spec, b, format!("{run_user}/bus"), BindMode::Rw);
            env(
                &mut spec,
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={run_user}/bus"),
            );
            report.push("dbus: session bus (unfiltered)".into());
        }
    }

    // Fonts and themes from the host.
    if m.integration.fonts {
        bind(
            &mut spec,
            "/usr/share/fonts",
            "/run/host/fonts",
            BindMode::Ro,
        );
        bind(
            &mut spec,
            host.home.join(".local/share/fonts"),
            "/run/host/user-fonts",
            BindMode::Ro,
        );
        bind(
            &mut spec,
            "/run/current-system/sw/share/X11/fonts",
            "/run/host/nix-fonts",
            BindMode::Ro,
        );
        let conf = "<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n<fontconfig>\n  <dir>/run/host/fonts</dir>\n  <dir>/run/host/user-fonts</dir>\n  <dir>/run/host/nix-fonts</dir>\n</fontconfig>\n";
        spec.files.push((
            "/etc/fonts/conf.d/99-cell-host-fonts.conf".into(),
            conf.as_bytes().to_vec(),
            0o644,
        ));
        report.push("fonts: host fonts under /run/host".into());
    }
    if m.integration.themes {
        bind(
            &mut spec,
            "/usr/share/icons",
            "/run/host/icons",
            BindMode::Ro,
        );
        bind(
            &mut spec,
            "/usr/share/themes",
            "/run/host/themes",
            BindMode::Ro,
        );
        env(
            &mut spec,
            "XCURSOR_PATH",
            "/run/host/icons:/usr/share/icons",
        );
        if let Some(t) = host_env("XCURSOR_THEME") {
            env(&mut spec, "XCURSOR_THEME", t);
        }
        if let Some(s) = host_env("XCURSOR_SIZE") {
            env(&mut spec, "XCURSOR_SIZE", s);
        }
    }

    // GPU.
    if m.integration.gpu.enable {
        if host.dri {
            bind(&mut spec, "/dev/dri", "/dev/dri", BindMode::Dev);
            report.push("gpu: /dev/dri (Mesa from the runtime)".into());
        }
        let nvidia_mode = match host_env("CELL_NVIDIA").as_deref() {
            Some("host") => NvidiaMode::Host,
            Some("none") => NvidiaMode::None,
            Some("fetch") => NvidiaMode::Fetch,
            Some("auto") => NvidiaMode::Auto,
            _ => m.integration.gpu.nvidia,
        };
        if let Some(ver) = &host.nvidia_version {
            if nvidia_mode != NvidiaMode::None {
                for d in &host.nvidia_devices {
                    let n = d.file_name().unwrap().to_string_lossy().to_string();
                    bind(&mut spec, d, format!("/dev/{n}"), BindMode::Dev);
                }
                if let Some(src) = nvidia_source(nvidia_mode, ver, &mut report) {
                    nvidia_userspace(&mut spec, ver, &src, &mut report);
                }
            }
        }
    }

    // Extra binds from the manifest. `$PWD` in a path means the directory
    // the user ran the cell from, so a tool cell can see its working
    // directory wherever that is.
    let host_cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".into());
    for b in &m.isolation.binds {
        let host_path = b.host.replace("$PWD", &host_cwd);
        let guest_path = b.guest.replace("$PWD", &host_cwd);
        spec.binds.push(BindSpec {
            host: PathBuf::from(&host_path),
            guest: guest_path.clone(),
            mode: b.mode,
            optional: b.optional,
        });
        report.push(format!("bind: {host_path} -> {guest_path} ({:?})", b.mode));
    }

    // Entry point.
    for (k, v) in &m.entry.env {
        env(&mut spec, k, v.clone());
    }
    // `$PWD` in `cwd` means the directory the user ran the cell from. It
    // only works when that directory is visible inside (for example under
    // a `home = :host` bind); otherwise the sandbox starts in `/`.
    let host_cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".into());
    let cwd = m.entry.cwd.as_ref().map(|c| c.replace("$PWD", &host_cwd));
    if let Some(c) = &cwd {
        env(&mut spec, "PWD", c.clone());
    }
    spec.cwd = cwd;
    spec.exec = exec_override.unwrap_or_else(|| m.entry.exec.clone());
    if m.entry.gamescope && std::env::var("CELL_NO_GAMESCOPE").is_err() {
        let mut g = vec!["gamescope".to_string(), "--".to_string()];
        g.extend(spec.exec.clone());
        spec.exec = g;
    }

    (spec, GrantReport { lines: report })
}

/// Where the NVIDIA userspace libraries come from.
enum NvidiaSource {
    /// Host directories, bound under `/run/host`.
    Host(Vec<DriverDir>),
    /// The fetch cache, bound at `/run/cell/gpu/fetched`.
    Fetched {
        cache: PathBuf,
        dirs: Vec<DriverDir>,
    },
}

/// Pick the library source for `mode`. `:fetch` uses the cache only.
/// `:auto` tries the cache and the download first, then the host.
/// `:host` uses the host only. `None` means no userspace was found; the
/// report says why.
fn nvidia_source(
    mode: NvidiaMode,
    version: &str,
    report: &mut Vec<String>,
) -> Option<NvidiaSource> {
    let fetched = |report: &mut Vec<String>| match cell_core::nvidia_fetch::fetch(version) {
        Ok(d) => {
            let dirs = cell_core::gpu::fetched_driver_dirs(&d.dir);
            if dirs.is_empty() {
                report.push(format!(
                    "gpu: nvidia {version} cache {} holds no libraries",
                    d.dir.display()
                ));
                None
            } else {
                Some(NvidiaSource::Fetched { cache: d.dir, dirs })
            }
        }
        Err(e) => {
            eprintln!("cell: nvidia: fetch of driver {version} failed: {e:#}");
            report.push(format!("gpu: nvidia {version} fetch failed: {e:#}"));
            None
        }
    };
    let host = |report: &mut Vec<String>| {
        let dirs = cell_core::gpu::find_host_driver_dirs();
        if dirs.is_empty() {
            report.push(format!(
                "gpu: nvidia kernel module {version} loaded but no host userspace found"
            ));
            None
        } else {
            Some(NvidiaSource::Host(dirs))
        }
    };
    match mode {
        NvidiaMode::None => None,
        NvidiaMode::Fetch => fetched(report),
        NvidiaMode::Host => host(report),
        NvidiaMode::Auto => fetched(report).or_else(|| {
            eprintln!("cell: nvidia: falling back to the host libraries");
            host(report).or_else(|| {
                eprintln!(
                    "cell: warning: nvidia {version}: no fetched or host userspace; \
                     the NVIDIA GPU is not usable in this sandbox"
                );
                None
            })
        }),
    }
}

/// Bind the NVIDIA userspace from `src`, build the symlink farm under
/// `/run/cell/gpu/lib{,32}`, write the loader JSON files and regenerate
/// `ld.so.cache` inside the sandbox.
fn nvidia_userspace(spec: &mut Spec, version: &str, src: &NvidiaSource, report: &mut Vec<String>) {
    // (bound directory in the guest, libraries, 32-bit) for each source dir.
    let mut guest_dirs: Vec<(String, &DriverDir)> = Vec::new();
    let mut gbm_dir = None;
    let mut vdpau_dir = None;
    match src {
        NvidiaSource::Host(dirs) => {
            for d in dirs {
                let guest = if d.lib32 {
                    "/run/host/lib32"
                } else {
                    "/run/host/lib"
                };
                bind(spec, &d.dir, guest, BindMode::Ro);
                guest_dirs.push((guest.to_string(), d));
                report.push(format!(
                    "gpu: nvidia {version} userspace from host {} ({} libs)",
                    d.dir.display(),
                    d.libs.len()
                ));
            }
            for (host_path, rel) in cell_core::gpu::host_extra_files() {
                bind(
                    spec,
                    host_path,
                    format!("/run/cell/gpu/extra/{rel}"),
                    BindMode::Ro,
                );
                if rel.starts_with("gbm/") {
                    gbm_dir = Some("/run/cell/gpu/extra/gbm".to_string());
                } else if rel.starts_with("vdpau/") {
                    vdpau_dir = Some("/run/cell/gpu/extra/vdpau".to_string());
                }
            }
        }
        NvidiaSource::Fetched { cache, dirs } => {
            let fetched = "/run/cell/gpu/fetched";
            bind(spec, cache, fetched, BindMode::Ro);
            for d in dirs {
                let guest = format!("{fetched}/{}", if d.lib32 { "lib32" } else { "lib" });
                guest_dirs.push((guest, d));
            }
            if cache.join("lib/gbm/nvidia-drm_gbm.so").exists() {
                gbm_dir = Some(format!("{fetched}/lib/gbm"));
            }
            if cache.join("lib/vdpau").exists() {
                vdpau_dir = Some(format!("{fetched}/lib/vdpau"));
            }
            report.push(format!(
                "gpu: nvidia {version} userspace fetched from NVIDIA (cache {}, {} libs)",
                cache.display(),
                dirs.iter().map(|d| d.libs.len()).sum::<usize>()
            ));
        }
    }
    let mut ld_conf = String::from("include /etc/ld.so.conf.d/*.conf\n");
    for (guest, d) in &guest_dirs {
        let gpu_dir = if d.lib32 {
            "/run/cell/gpu/lib32"
        } else {
            "/run/cell/gpu/lib"
        };
        for lib in &d.libs {
            spec.symlinks
                .push((format!("{gpu_dir}/{lib}"), format!("{guest}/{lib}")));
        }
        ld_conf.push_str(gpu_dir);
        ld_conf.push('\n');
    }
    let share = "/run/cell/gpu/share";
    spec.files.push((
        format!("{share}/glvnd/egl_vendor.d/10_nvidia.json"),
        cell_core::gpu::egl_vendor_json("")
            .replace("\"/libEGL", "\"libEGL")
            .into_bytes(),
        0o644,
    ));
    spec.files.push((
        format!("{share}/vulkan/icd.d/nvidia_icd.json"),
        cell_core::gpu::vulkan_icd_json("", version)
            .replace("\"/libGLX", "\"libGLX")
            .into_bytes(),
        0o644,
    ));
    spec.files.push((
        format!("{share}/egl/egl_external_platform.d/10_nvidia_wayland.json"),
        cell_core::gpu::egl_external_platform_json("")
            .replace("\"/libnvidia", "\"libnvidia")
            .into_bytes(),
        0o644,
    ));
    spec.files
        .push(("/run/cell/ld.so.conf".into(), ld_conf.into_bytes(), 0o644));
    spec.pre_exec.push(vec![
        "ldconfig".into(),
        "-X".into(),
        "-f".into(),
        "/run/cell/ld.so.conf".into(),
        "-C".into(),
        "/run/cell/ld.so.cache".into(),
    ]);
    spec.late_file_binds
        .push(("/run/cell/ld.so.cache".into(), "/etc/ld.so.cache".into()));
    env(
        spec,
        "XDG_DATA_DIRS",
        format!("{share}:/usr/local/share:/usr/share"),
    );
    env(
        spec,
        "__EGL_VENDOR_LIBRARY_DIRS",
        format!("{share}/glvnd/egl_vendor.d:/usr/share/glvnd/egl_vendor.d"),
    );
    env(
        spec,
        "__EGL_EXTERNAL_PLATFORM_CONFIG_DIRS",
        format!("{share}/egl/egl_external_platform.d:/usr/share/egl/egl_external_platform.d"),
    );
    if let Some(g) = gbm_dir {
        env(spec, "GBM_BACKENDS_PATH", format!("{g}:/usr/lib/gbm"));
    }
    if let Some(v) = vdpau_dir {
        env(spec, "VDPAU_DRIVER_PATH", v);
    }
}
