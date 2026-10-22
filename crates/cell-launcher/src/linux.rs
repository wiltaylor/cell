//! The Linux launcher: it reads the trailer and manifest from its own
//! executable, mounts the layers, builds the sandbox and runs the
//! application.
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
//!
//! A cell with a `wine` block runs a Windows cell given as its first
//! argument: `./wine.cell app.cell.exe [args]` (see `wine.rs`).

use crate::{desktop, grants, steam, wine};

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use cell_core::format::{CellFile, LayerRole};
use cell_core::sandbox::{self, Layer, LayerSource, Spec};

fn self_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CELL_FILE") {
        return Ok(PathBuf::from(p));
    }
    std::fs::read_link("/proc/self/exe").context("read /proc/self/exe")
}

/// Mount every layer of `cell` into the spec: root-stack layers join the
/// overlay in order, data layers mount at their path.
fn push_layers(cell: &CellFile, spec: &mut Spec) -> Result<()> {
    for l in &cell.manifest.layers {
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
    Ok(())
}

fn print_layers(cell: &CellFile) {
    for l in &cell.manifest.layers {
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
}

pub fn real_main() -> Result<i32> {
    let path = self_path()?;
    let cell = CellFile::open(&path)?;
    let cmd = std::env::var("CELL_CMD").unwrap_or_default();
    let host = cell_core::host::probe();
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // A Wine runner given a Windows cell runs that cell.
    let wrapped = match (&cell.manifest.wine, wine::guest_arg(&args)) {
        (Some(w), Some(guest_path)) => {
            let guest = CellFile::open(&guest_path)?;
            args.remove(0);
            Some(wine::wrap(&cell.manifest, w, guest)?)
        }
        _ => None,
    };
    let m = match &wrapped {
        Some(w) => &w.manifest,
        None => &cell.manifest,
    };
    let state_path = match &wrapped {
        Some(w) => w.guest.path.clone(),
        None => path.clone(),
    };
    if wrapped.is_some()
        && matches!(
            cmd.as_str(),
            "install" | "uninstall" | "steam-add" | "steam-remove" | "desktop-entry"
        )
    {
        bail!("CELL_CMD={cmd} is not supported for a Windows cell run through Wine yet");
    }

    match cmd.as_str() {
        "verify" => {
            let code = crate::common::verify(&cell)?;
            return match &wrapped {
                Some(w) => Ok(code.max(crate::common::verify(&w.guest)?)),
                None => Ok(code),
            };
        }
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

    let state = grants::state_dirs(m, &state_path)?;
    let exec_override = match cmd.as_str() {
        "shell" => Some(vec!["/bin/sh".to_string()]),
        "" | "run" | "info" => {
            // `CELL_COMMAND=name` picks a named alternative entry point.
            if std::env::var("CELL_COMMAND")
                .map(|v| !v.is_empty())
                .unwrap_or(false)
            {
                Some(crate::common::select_exec(m)?)
            } else {
                None
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
        print_layers(&cell);
        if let Some(w) = &wrapped {
            println!(
                "wine: runs {} ({}) through {:?}",
                w.guest.path.display(),
                w.guest.manifest.app.id,
                cell.manifest.wine.as_ref().map(|x| &x.command)
            );
            print_layers(&w.guest);
            for line in cell_core::registry::describe(&w.guest.manifest.registry) {
                println!("registry: {line}");
            }
        } else if cell.manifest.wine.is_some() {
            println!(
                "wine: give a Windows cell as the first argument to run it: {} app.cell.exe",
                path.display()
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
    spec.exec.extend(args);

    // Layers: the runner's, then the guest's on top.
    push_layers(&cell, &mut spec)?;
    if let Some(w) = &wrapped {
        push_layers(&w.guest, &mut spec)?;
        // The guest's registry keys go into the Wine prefix once per
        // declaration, as the user, before the entry point.
        if let Some(reg) = &w.reg {
            let stamp =
                wine::registry_stamp(&state.home, &w.guest.manifest.app.id, &cell.manifest.app.id);
            if wine::registry_pending(reg, &stamp) {
                let reg_path = "/run/cell/registry.reg";
                spec.files.push((
                    reg_path.to_string(),
                    cell_core::registry::reg_utf16(reg),
                    0o644,
                ));
                let mut cmd = cell
                    .manifest
                    .wine
                    .as_ref()
                    .map(|x| x.command.clone())
                    .unwrap_or_default();
                cmd.extend([
                    "regedit".to_string(),
                    "/S".to_string(),
                    reg_path.to_string(),
                ]);
                spec.user_pre_exec.push(cmd);
                wine::mark_registry_applied(reg, &stamp)?;
                eprintln!("cell: applying the registry keys of {}", m.app.id);
            }
        }
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
