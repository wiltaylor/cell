//! `CELL_CMD=steam-add` and `CELL_CMD=steam-remove`: the cell as a non-Steam
//! game in the Steam library, with the artwork from the manifest. Works on a
//! desktop Steam install and on a Steam Deck in desktop mode.
//!
//! Steam keeps shortcuts in a binary KeyValues file,
//! `userdata/<user>/config/shortcuts.vdf`, and reads it at start-up only.
//! Steam must not be running while the file is edited.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use cell_core::format::Manifest;

// ---- binary KeyValues --------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Map(Vec<(String, Value)>),
    Str(String),
    Int(u32),
}

fn read_cstr(b: &[u8], pos: &mut usize) -> Result<String> {
    let start = *pos;
    while *pos < b.len() && b[*pos] != 0 {
        *pos += 1;
    }
    if *pos >= b.len() {
        bail!("unterminated string in shortcuts.vdf");
    }
    let s = String::from_utf8_lossy(&b[start..*pos]).to_string();
    *pos += 1;
    Ok(s)
}

fn parse_map(b: &[u8], pos: &mut usize) -> Result<Vec<(String, Value)>> {
    let mut out = Vec::new();
    loop {
        if *pos >= b.len() {
            bail!("unterminated map in shortcuts.vdf");
        }
        let t = b[*pos];
        *pos += 1;
        if t == 0x08 {
            return Ok(out);
        }
        let key = read_cstr(b, pos)?;
        let v = match t {
            0x00 => Value::Map(parse_map(b, pos)?),
            0x01 => Value::Str(read_cstr(b, pos)?),
            0x02 => {
                if *pos + 4 > b.len() {
                    bail!("truncated int in shortcuts.vdf");
                }
                let v = u32::from_le_bytes(b[*pos..*pos + 4].try_into().unwrap());
                *pos += 4;
                Value::Int(v)
            }
            other => bail!("unknown value type {other:#x} in shortcuts.vdf"),
        };
        out.push((key, v));
    }
}

pub fn parse(b: &[u8]) -> Result<Vec<(String, Value)>> {
    let mut pos = 0;
    parse_map(b, &mut pos)
}

fn write_map(out: &mut Vec<u8>, m: &[(String, Value)]) {
    for (k, v) in m {
        match v {
            Value::Map(inner) => {
                out.push(0x00);
                out.extend_from_slice(k.as_bytes());
                out.push(0);
                write_map(out, inner);
            }
            Value::Str(s) => {
                out.push(0x01);
                out.extend_from_slice(k.as_bytes());
                out.push(0);
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
            Value::Int(i) => {
                out.push(0x02);
                out.extend_from_slice(k.as_bytes());
                out.push(0);
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
    }
    out.push(0x08);
}

pub fn serialize(m: &[(String, Value)]) -> Vec<u8> {
    let mut out = Vec::new();
    write_map(&mut out, m);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (!(crc & 1)).wrapping_add(1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// The app id Steam derives for a shortcut: crc32 of the quoted exe plus
/// the name, with the high bit set. Artwork files are named after it.
pub fn shortcut_appid(exe_quoted: &str, name: &str) -> u32 {
    crc32(format!("{exe_quoted}{name}").as_bytes()) | 0x8000_0000
}

// ---- Steam on this host ------------------------------------------------------

fn steam_root() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("CELL_STEAM_DIR") {
        return Some(PathBuf::from(d));
    }
    let home = PathBuf::from(std::env::var("HOME").ok()?);
    [
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
    ]
    .into_iter()
    .find(|p| p.join("userdata").is_dir())
}

/// The user's `userdata/<id>` directory: `CELL_STEAM_USER`, or the one whose
/// config changed most recently.
fn user_dir(root: &Path) -> Result<PathBuf> {
    let userdata = root.join("userdata");
    if let Ok(u) = std::env::var("CELL_STEAM_USER") {
        return Ok(userdata.join(u));
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&userdata)
        .context("read Steam userdata")?
        .flatten()
    {
        let name = e.file_name().to_string_lossy().to_string();
        if name == "0" || name == "ac" || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let cfg = e.path().join("config");
        let t = std::fs::metadata(&cfg)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
            best = Some((t, e.path()));
        }
    }
    best.map(|(_, p)| p)
        .context("no Steam user found under userdata; log in to Steam once")
}

fn steam_running() -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return false;
    };
    for e in rd.flatten() {
        if let Ok(c) = std::fs::read_to_string(e.path().join("comm")) {
            let c = c.trim();
            if c == "steam" || c == "steamwebhelper" {
                return true;
            }
        }
    }
    false
}

fn load_shortcuts(path: &Path) -> Result<Vec<(String, Value)>> {
    if !path.exists() {
        return Ok(vec![("shortcuts".into(), Value::Map(Vec::new()))]);
    }
    let bytes = std::fs::read(path)?;
    parse(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn entries_mut(root: &mut Vec<(String, Value)>) -> &mut Vec<(String, Value)> {
    if !root.iter().any(|(k, _)| k == "shortcuts") {
        root.push(("shortcuts".into(), Value::Map(Vec::new())));
    }
    match root.iter_mut().find(|(k, _)| k == "shortcuts") {
        Some((_, Value::Map(m))) => m,
        _ => unreachable!(),
    }
}

fn field<'a>(entry: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    entry
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn renumber(list: &mut [(String, Value)]) {
    for (i, (k, _)) in list.iter_mut().enumerate() {
        *k = i.to_string();
    }
}

fn save_shortcuts(path: &Path, root: &[(String, Value)]) -> Result<()> {
    if path.exists() {
        let backup = path.with_extension("vdf.cell-backup");
        std::fs::copy(path, &backup)?;
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("vdf.partial");
    std::fs::write(&tmp, serialize(root))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn check_not_running() -> Result<()> {
    if steam_running() && std::env::var("CELL_FORCE").is_err() {
        bail!(
            "Steam is running. It rewrites shortcuts.vdf on exit, so quit Steam first \
             (on a Steam Deck, switch to desktop mode and quit Steam), or set CELL_FORCE=1"
        );
    }
    Ok(())
}

/// Add or replace the shortcut and write the artwork.
pub fn add(m: &Manifest, cell: &Path) -> Result<()> {
    check_not_running()?;
    let cell = cell.canonicalize().context("resolve the cell path")?;
    let root = steam_root().context("no Steam installation found (set CELL_STEAM_DIR)")?;
    let user = user_dir(&root)?;
    let vdf = user.join("config/shortcuts.vdf");
    let mut data = load_shortcuts(&vdf)?;
    let exe = format!("\"{}\"", cell.display());
    let start_dir = format!("\"{}\"", cell.parent().unwrap_or(Path::new("/")).display());
    let name = m.app.name.clone();
    let appid = shortcut_appid(&exe, &name);
    let st = m.integration.steam.clone().unwrap_or_default();

    let grid = user.join("config/grid");
    std::fs::create_dir_all(&grid)?;
    let mut icon_path = String::new();
    if let Some(png) = m
        .integration
        .desktop
        .as_ref()
        .and_then(|d| d.icon_png.as_ref())
    {
        let p = grid.join(format!("{appid}_icon.png"));
        std::fs::write(&p, png)?;
        icon_path = p.to_string_lossy().to_string();
    }
    for (bytes, file) in [
        (&st.portrait, format!("{appid}p.png")),
        (&st.wide, format!("{appid}.png")),
        (&st.hero, format!("{appid}_hero.png")),
        (&st.logo, format!("{appid}_logo.png")),
    ] {
        if let Some(b) = bytes {
            let p = grid.join(&file);
            std::fs::write(&p, b)?;
            println!("wrote {}", p.display());
        }
    }

    let mut tags: Vec<(String, Value)> = Vec::new();
    for (i, t) in st.tags.iter().enumerate() {
        tags.push((i.to_string(), Value::Str(t.clone())));
    }
    let entry = Value::Map(vec![
        ("appid".into(), Value::Int(appid)),
        ("AppName".into(), Value::Str(name.clone())),
        ("Exe".into(), Value::Str(exe.clone())),
        ("StartDir".into(), Value::Str(start_dir)),
        ("icon".into(), Value::Str(icon_path)),
        ("ShortcutPath".into(), Value::Str(String::new())),
        (
            "LaunchOptions".into(),
            Value::Str(st.launch_options.clone().unwrap_or_default()),
        ),
        ("IsHidden".into(), Value::Int(0)),
        ("AllowDesktopConfig".into(), Value::Int(1)),
        ("AllowOverlay".into(), Value::Int(1)),
        ("OpenVR".into(), Value::Int(0)),
        ("Devkit".into(), Value::Int(0)),
        ("DevkitGameID".into(), Value::Str(String::new())),
        ("DevkitOverrideAppID".into(), Value::Int(0)),
        ("LastPlayTime".into(), Value::Int(0)),
        ("FlatpakAppID".into(), Value::Str(String::new())),
        ("tags".into(), Value::Map(tags)),
    ]);

    let list = entries_mut(&mut data);
    let same = |e: &Value| match e {
        Value::Map(f) => {
            field(f, "Exe") == Some(&Value::Str(exe.clone()))
                || field(f, "AppName") == Some(&Value::Str(name.clone()))
        }
        _ => false,
    };
    let replaced = list.iter().any(|(_, e)| same(e));
    list.retain(|(_, e)| !same(e));
    list.push((list.len().to_string(), entry));
    renumber(list);
    save_shortcuts(&vdf, &data)?;
    println!(
        "{} \"{}\" in {}",
        if replaced { "replaced" } else { "added" },
        name,
        vdf.display()
    );
    let run_id = ((appid as u64) << 32) | 0x0200_0000;
    println!("start Steam to see it; launch URL: steam://rungameid/{run_id}");
    Ok(())
}

/// Remove the shortcut and its artwork.
pub fn remove(m: &Manifest, cell: &Path) -> Result<()> {
    check_not_running()?;
    let cell = cell.canonicalize().context("resolve the cell path")?;
    let root = steam_root().context("no Steam installation found (set CELL_STEAM_DIR)")?;
    let user = user_dir(&root)?;
    let vdf = user.join("config/shortcuts.vdf");
    let mut data = load_shortcuts(&vdf)?;
    let exe = format!("\"{}\"", cell.display());
    let name = m.app.name.clone();
    let appid = shortcut_appid(&exe, &name);
    let list = entries_mut(&mut data);
    let before = list.len();
    list.retain(|(_, e)| match e {
        Value::Map(f) => {
            field(f, "Exe") != Some(&Value::Str(exe.clone()))
                && field(f, "AppName") != Some(&Value::Str(name.clone()))
        }
        _ => true,
    });
    renumber(list);
    let removed = before - list.len();
    save_shortcuts(&vdf, &data)?;
    let grid = user.join("config/grid");
    for file in [
        format!("{appid}_icon.png"),
        format!("{appid}p.png"),
        format!("{appid}.png"),
        format!("{appid}_hero.png"),
        format!("{appid}_logo.png"),
    ] {
        let _ = std::fs::remove_file(grid.join(file));
    }
    println!(
        "removed {removed} shortcut(s) named \"{name}\" from {}",
        vdf.display()
    );
    Ok(())
}

/// Print the shortcuts for `CELL_CMD=steam-list`.
pub fn list() -> Result<()> {
    let root = steam_root().context("no Steam installation found (set CELL_STEAM_DIR)")?;
    let user = user_dir(&root)?;
    let data = load_shortcuts(&user.join("config/shortcuts.vdf"))?;
    if let Some((_, Value::Map(list))) = data.iter().find(|(k, _)| k == "shortcuts") {
        for (_, e) in list {
            if let Value::Map(f) = e {
                let name = match field(f, "AppName") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                let exe = match field(f, "Exe") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                let id = match field(f, "appid") {
                    Some(Value::Int(i)) => *i,
                    _ => 0,
                };
                println!("{id:>10}  {name}  {exe}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let root = vec![(
            "shortcuts".to_string(),
            Value::Map(vec![(
                "0".to_string(),
                Value::Map(vec![
                    ("appid".to_string(), Value::Int(0x8000_0001)),
                    ("AppName".to_string(), Value::Str("x".into())),
                    ("tags".to_string(), Value::Map(vec![])),
                ]),
            )]),
        )];
        let bytes = serialize(&root);
        assert_eq!(parse(&bytes).unwrap(), root);
    }

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
