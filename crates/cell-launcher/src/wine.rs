//! A Linux cell that runs Windows cells: `./wine.cell app.cell.exe [args]`.
//!
//! The runner's manifest supplies the runtime (Wine and its libraries),
//! the grants and the host integration. The Windows cell supplies the
//! identity, the files, the entry point and the registry keys. Its layers
//! join the root stack below the upper, so the app sees its files at
//! `/app/...`, which Wine shows as `Z:\app\...`. `$CELL_ROOT` is `Z:\`.
//!
//! The state (upper, home, Wine prefix) belongs to the Windows cell's id,
//! so each game keeps its own prefix and saves whichever runner starts it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use cell_core::format::{CellFile, Manifest, NamedCommand, Wine};

/// The Wine path of the layer root. Wine maps `/` to `Z:`.
pub const ROOT: &str = "Z:\\";

/// The first argument when it names a Windows cell that exists.
pub fn guest_arg(args: &[String]) -> Option<PathBuf> {
    let first = args.first()?;
    if !first.to_ascii_lowercase().ends_with(".cell.exe") {
        return None;
    }
    let p = PathBuf::from(first);
    p.is_file().then_some(p)
}

/// `$CELL_ROOT` in an argument or a value, as the Wine root.
pub fn expand(s: &str) -> String {
    s.replace("${CELL_ROOT}", ROOT).replace("$CELL_ROOT", ROOT)
}

/// A manifest path (`/app/x.exe`) as the Wine path `Z:\app\x.exe`; any
/// other text has `$CELL_ROOT` expanded.
pub fn win_path(p: &str) -> String {
    match p.strip_prefix('/') {
        Some(rel) => format!("{ROOT}{}", rel.replace('/', "\\")),
        None => expand(p),
    }
}

/// `wine Z:\app\x.exe args...` from a Windows cell's exec list.
fn translate_exec(wine: &Wine, exec: &[String]) -> Vec<String> {
    let mut out = wine.command.clone();
    if let Some(p) = exec.first() {
        out.push(win_path(p));
    }
    out.extend(exec.iter().skip(1).map(|a| expand(a)));
    out
}

/// A Windows cell's working directory as a Linux path inside the root.
fn translate_cwd(cwd: Option<&str>, exec: &[String]) -> String {
    match cwd {
        Some(c) if c.starts_with('/') || c.contains("$PWD") => c.to_string(),
        Some(c) => {
            let p = c.replace("${CELL_ROOT}", "").replace("$CELL_ROOT", "");
            let p = p.replace('\\', "/");
            if p.starts_with('/') {
                p
            } else {
                format!("/{p}")
            }
        }
        None => exec
            .first()
            .filter(|p| p.starts_with('/'))
            .and_then(|p| Path::new(p).parent())
            .map(|d| d.to_string_lossy().into_owned())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/".to_string()),
    }
}

pub struct Wrapped {
    pub guest: CellFile,
    /// The manifest the run uses: the runner's grants with the guest's
    /// identity, entry point, home mode and network wish.
    pub manifest: Manifest,
    /// The guest's registry keys as a `.reg` file, for `regedit /S`.
    pub reg: Option<String>,
}

pub fn wrap(runner: &Manifest, wine: &Wine, guest: CellFile) -> Result<Wrapped> {
    let g = &guest.manifest;
    if !g.app.is_windows() {
        bail!(
            "{} is a {} cell, not a Windows cell",
            guest.path.display(),
            g.app.os
        );
    }
    let mut m = runner.clone();
    m.app = g.app.clone();
    m.entry.exec = translate_exec(wine, &g.entry.exec);
    m.entry.cwd = Some(translate_cwd(g.entry.cwd.as_deref(), &g.entry.exec));
    for (k, v) in &g.entry.env {
        m.entry.env.retain(|(n, _)| n != k);
        m.entry.env.push((k.clone(), expand(v)));
    }
    m.entry.env.push(("CELL_ROOT".into(), ROOT.into()));
    m.entry.env.push(("CELL_SANDBOX".into(), "linux".into()));
    // The runner's commands (winecfg and friends) first, then the guest's.
    for c in &g.entry.commands {
        m.entry.commands.retain(|r| r.name != c.name);
        m.entry.commands.push(NamedCommand {
            name: c.name.clone(),
            exec: translate_exec(wine, &c.exec),
        });
    }
    // The runner says what the host allows; the guest says what it wants.
    m.isolation.network = runner.isolation.network && g.isolation.network;
    m.isolation.home = g.isolation.home;
    let reg = if g.registry.is_empty() {
        None
    } else {
        Some(g.registry.to_reg(expand))
    };
    Ok(Wrapped {
        guest,
        manifest: m,
        reg,
    })
}

/// The guest's keys are applied with `regedit /S` once per declaration:
/// the stamp in the home holds the hash of the last `.reg` applied, so a
/// reset home (a new prefix) gets them again and a changed declaration
/// is applied on top. Runners keep separate prefixes in the same home
/// (Wine in `.wine`, Proton under `proton/`), so the stamp names both.
pub fn registry_stamp(home: &Path, app_id: &str, runner_id: &str) -> PathBuf {
    home.join(".cell")
        .join(format!("registry-{app_id}-{runner_id}.stamp"))
}

pub fn registry_pending(reg: &str, stamp: &Path) -> bool {
    let want = blake3::hash(reg.as_bytes()).to_hex().to_string();
    std::fs::read_to_string(stamp).ok().as_deref() != Some(want.as_str())
}

pub fn mark_registry_applied(reg: &str, stamp: &Path) -> Result<()> {
    if let Some(d) = stamp.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(stamp, blake3::hash(reg.as_bytes()).to_hex().to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(win_path("/app/x.exe"), "Z:\\app\\x.exe");
        assert_eq!(expand("$CELL_ROOT/app/data"), "Z:\\/app/data");
        assert_eq!(translate_cwd(Some("/app"), &[]), "/app");
        assert_eq!(translate_cwd(Some("$CELL_ROOT/app"), &[]), "/app");
        assert_eq!(translate_cwd(None, &["/app/x/y.exe".into()]), "/app/x");
        assert_eq!(translate_cwd(None, &[]), "/");
        let w = Wine::default();
        assert_eq!(
            translate_exec(
                &w,
                &["/app/g.exe".into(), "-d".into(), "$CELL_ROOT/app".into()]
            ),
            vec!["wine", "Z:\\app\\g.exe", "-d", "Z:\\/app"]
        );
    }
}
