//! `CELL_CMD=install` and `CELL_CMD=uninstall`: a freedesktop `.desktop`
//! entry and icon for this cell, with its MIME types, URL schemes and
//! named commands as desktop actions.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use cell_core::format::Manifest;

fn data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join(".local/share")
        })
}

/// Width and height of a PNG from its IHDR chunk.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

fn quote(p: &Path) -> String {
    format!("\"{}\"", p.to_string_lossy().replace('"', "\\\""))
}

/// The text of the `.desktop` file.
pub fn desktop_entry(m: &Manifest, cell: &Path) -> String {
    let d = m.integration.desktop.clone().unwrap_or_default();
    let id = &m.app.id;
    let mut mime: Vec<String> = d.mime.clone();
    for p in &d.protocols {
        mime.push(format!("x-scheme-handler/{p}"));
    }
    let exec = if mime.is_empty() {
        quote(cell)
    } else {
        format!("{} %U", quote(cell))
    };
    let mut out = String::new();
    out.push_str("[Desktop Entry]\nType=Application\nVersion=1.5\n");
    out.push_str(&format!("Name={}\n", m.app.name));
    if let Some(c) = &d.comment {
        out.push_str(&format!("Comment={c}\n"));
    }
    out.push_str(&format!("Exec={exec}\n"));
    out.push_str(&format!("Icon={id}\n"));
    out.push_str("Terminal=false\nStartupNotify=true\n");
    if !d.categories.is_empty() {
        out.push_str(&format!("Categories={};\n", d.categories.join(";")));
    }
    if !mime.is_empty() {
        out.push_str(&format!("MimeType={};\n", mime.join(";")));
    }
    if !d.keywords.is_empty() {
        out.push_str(&format!("Keywords={};\n", d.keywords.join(";")));
    }
    out.push_str(&format!("StartupWMClass={}\n", m.app.name));
    out.push_str(&format!("X-Cell-Id={id}\nX-Cell-File={}\n", cell.display()));
    if !m.entry.commands.is_empty() {
        let names: Vec<&str> = m.entry.commands.iter().map(|c| c.name.as_str()).collect();
        out.push_str(&format!("Actions={};\n", names.join(";")));
        for c in &m.entry.commands {
            out.push_str(&format!(
                "\n[Desktop Action {}]\nName={}\nExec=env CELL_COMMAND={} {}\n",
                c.name,
                c.name,
                c.name,
                quote(cell)
            ));
        }
    }
    out
}

fn paths(m: &Manifest) -> (PathBuf, PathBuf) {
    let data = data_home();
    (
        data.join("applications")
            .join(format!("{}.desktop", m.app.id)),
        data.join("icons/hicolor"),
    )
}

fn run_quiet(cmd: &str, args: &[&str]) {
    let _ = Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Write the desktop entry and icon, then register the URL schemes.
pub fn install(m: &Manifest, cell: &Path) -> Result<()> {
    let cell = cell.canonicalize().context("resolve the cell path")?;
    let (desktop_file, icons) = paths(m);
    std::fs::create_dir_all(desktop_file.parent().unwrap())?;
    std::fs::write(&desktop_file, desktop_entry(m, &cell))
        .with_context(|| format!("write {}", desktop_file.display()))?;
    println!("wrote {}", desktop_file.display());
    let d = m.integration.desktop.clone().unwrap_or_default();
    if let Some(png) = &d.icon_png {
        let (w, h) = png_size(png).unwrap_or((256, 256));
        let dir = icons.join(format!("{w}x{h}/apps"));
        std::fs::create_dir_all(&dir)?;
        let icon = dir.join(format!("{}.png", m.app.id));
        std::fs::write(&icon, png)?;
        println!("wrote {}", icon.display());
    }
    run_quiet(
        "update-desktop-database",
        &[desktop_file.parent().unwrap().to_str().unwrap()],
    );
    run_quiet(
        "gtk-update-icon-cache",
        &["-q", "-t", icons.to_str().unwrap()],
    );
    let name = desktop_file
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    for p in &d.protocols {
        let scheme = format!("x-scheme-handler/{p}");
        run_quiet("xdg-mime", &["default", &name, &scheme]);
        println!("registered {p}:// links to {name}");
    }
    if !d.mime.is_empty() {
        println!("advertises MIME types: {}", d.mime.join(", "));
    }
    Ok(())
}

/// Remove what `install` wrote.
pub fn uninstall(m: &Manifest) -> Result<()> {
    let (desktop_file, icons) = paths(m);
    if desktop_file.exists() {
        std::fs::remove_file(&desktop_file)?;
        println!("removed {}", desktop_file.display());
    }
    if let Ok(rd) = std::fs::read_dir(&icons) {
        for size in rd.flatten() {
            let icon = size.path().join("apps").join(format!("{}.png", m.app.id));
            if icon.exists() {
                let _ = std::fs::remove_file(&icon);
                println!("removed {}", icon.display());
            }
        }
    }
    run_quiet(
        "update-desktop-database",
        &[desktop_file.parent().unwrap().to_str().unwrap()],
    );
    Ok(())
}
