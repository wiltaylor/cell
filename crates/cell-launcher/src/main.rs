//! The Cell launcher. It heads every `.cell` file: it reads the trailer and
//! manifest from its own executable, mounts the layers, builds the sandbox
//! and runs the application.
//!
//! Control commands come through `CELL_CMD` so they never collide with the
//! application's own arguments:
//!
//! - `CELL_CMD=info`    print host probe, grants and layers
//! - `CELL_CMD=shell`   open a shell inside the sandbox
//! - `CELL_CMD=verify`  check layer hashes
//! - `CELL_CMD=install` / `uninstall`  desktop entry, icon, URL schemes
//! - `CELL_CMD=steam-add` / `steam-remove` / `steam-list`  Steam shortcut and art
//! - `CELL_FILE=<path>` read the payload from another file (noexec mounts)

mod desktop;
mod grants;
mod steam;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use cell_core::format::{CellFile, LayerRole};
use cell_core::sandbox::{self, Layer, LayerSource};

fn main() {
    match real_main() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("cell: {e:#}");
            std::process::exit(125);
        }
    }
}

fn self_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CELL_FILE") {
        return Ok(PathBuf::from(p));
    }
    std::fs::read_link("/proc/self/exe").context("read /proc/self/exe")
}

fn real_main() -> Result<i32> {
    let path = self_path()?;
    let cell = CellFile::open(&path)?;
    let cmd = std::env::var("CELL_CMD").unwrap_or_default();
    let host = cell_core::host::probe();
    let m = &cell.manifest;

    match cmd.as_str() {
        "verify" => return verify(&cell),
        "manifest" => {
            println!("{:#?}", m);
            return Ok(0);
        }
        "install" => {
            desktop::install(m, &path)?;
            return Ok(0);
        }
        "uninstall" => {
            desktop::uninstall(m)?;
            return Ok(0);
        }
        "steam-add" => {
            steam::add(m, &path)?;
            return Ok(0);
        }
        "steam-remove" => {
            steam::remove(m, &path)?;
            return Ok(0);
        }
        "steam-list" => {
            steam::list()?;
            return Ok(0);
        }
        "desktop-entry" => {
            print!("{}", desktop::desktop_entry(m, &path.canonicalize()?));
            return Ok(0);
        }
        _ => {}
    }

    let state = grants::state_dirs(m, &path)?;
    let exec_override = match cmd.as_str() {
        "shell" => Some(vec!["/bin/sh".to_string()]),
        "" | "run" | "info" => {
            // `CELL_COMMAND=name` picks a named alternative entry point.
            match std::env::var("CELL_COMMAND").ok() {
                Some(name) => {
                    let c = m
                        .entry
                        .commands
                        .iter()
                        .find(|c| c.name == name)
                        .with_context(|| format!("no command named {name}"))?;
                    Some(c.exec.clone())
                }
                None => None,
            }
        }
        other => bail!("unknown CELL_CMD {other}"),
    };
    let (mut spec, report) = grants::build_spec(m, &host, &state, exec_override);

    if cmd == "info" {
        println!("{} {} ({})", m.app.name, m.app.version, m.app.id);
        println!("file: {}", path.display());
        println!(
            "host: kernel {}, userns {}, fuse {}",
            host.kernel, host.userns, host.fuse
        );
        println!(
            "host: wayland {:?}, x11 {:?}, nvidia {:?}",
            host.wayland.as_ref().map(|p| p.display().to_string()),
            host.display,
            host.nvidia_version
        );
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
        for line in &report.lines {
            println!("grant: {line}");
        }
        if let Some(d) = &m.integration.desktop {
            println!(
                "desktop: icon {}, categories {:?}, mime {:?}, protocols {:?}",
                d.icon_png
                    .as_ref()
                    .map(|b| format!("{} bytes", b.len()))
                    .unwrap_or("none".into()),
                d.categories,
                d.mime,
                d.protocols
            );
        }
        if let Some(st) = &m.integration.steam {
            println!(
                "steam: portrait {}, wide {}, hero {}, logo {}, tags {:?}",
                st.portrait.is_some(),
                st.wide.is_some(),
                st.hero.is_some(),
                st.logo.is_some(),
                st.tags
            );
        }
        println!("exec: {:?}", spec.exec);
        return Ok(0);
    }

    // Append user arguments to the entry point.
    let args: Vec<String> = std::env::args().skip(1).collect();
    spec.exec.extend(args);

    // Layers.
    for l in &m.layers {
        let (file_path, off) = cell.layer_location(l)?;
        let file = std::fs::File::open(&file_path)
            .with_context(|| format!("open layer {} at {}", l.name, file_path.display()))?;
        let mount = match l.role {
            LayerRole::Runtime | LayerRole::App => "/".to_string(),
            LayerRole::Data => l.mount.clone(),
        };
        spec.layers.push(Layer {
            source: LayerSource::Squashfs {
                file,
                base: off,
                name: l.name.clone(),
            },
            mount,
        });
    }
    spec.squash_ids = Some((0, 0));

    // One instance owns the overlay upper directory. A second instance of
    // the same app runs with a throwaway upper, so writes outside the home
    // directory do not persist for it. The home bind is shared and is
    // outside the overlay, so both instances see the same saves.
    let lock = grants::instance_lock(&state)?;
    if lock.is_some() {
        spec.upper = Some((state.upper.clone(), state.work.clone()));
    } else {
        eprintln!(
            "cell: another instance of {} is running; this one keeps no changes outside $HOME",
            m.app.id
        );
        spec.upper = None;
    }

    if !host.fuse {
        bail!("/dev/fuse is missing on this host (load the fuse module)");
    }
    let work = host
        .runtime_dir
        .join("cell")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&work)?;

    spec.drop_to = Some((host.uid, host.gid));
    sandbox::enter_userns_root(spec.new_net, spec.new_ipc)?;
    let code = sandbox::run(&spec, &work)?;
    drop(lock);
    Ok(code)
}

fn verify(cell: &CellFile) -> Result<i32> {
    use std::io::Read;
    let mut bad = 0;
    for l in &cell.manifest.layers {
        let (p, off) = cell.layer_location(l)?;
        let mut f = std::fs::File::open(&p)?;
        use std::io::{Seek, SeekFrom};
        f.seek(SeekFrom::Start(off))?;
        let mut h = blake3::Hasher::new();
        let mut left = l.size;
        let mut buf = vec![0u8; 1 << 20];
        while left > 0 {
            let want = left.min(buf.len() as u64) as usize;
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            left -= n as u64;
        }
        let got = h.finalize().to_hex().to_string();
        let ok = got == l.blake3;
        println!("{} {} {}", if ok { "ok  " } else { "BAD " }, l.name, got);
        if !ok {
            bad += 1;
        }
    }
    Ok(if bad == 0 { 0 } else { 1 })
}
