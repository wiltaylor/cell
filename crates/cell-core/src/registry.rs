//! Declared registry keys of a Windows cell.
//!
//! A cell names the `HKEY_CURRENT_USER` keys its app needs. The build
//! tool reads them from the build file or from a `.reg` file, and the
//! launcher writes them before the app starts and lets the sandbox reach
//! them. Every path in the manifest is relative to `HKEY_CURRENT_USER`,
//! with `\` between the parts.
//!
//! This module is plain Rust: the parser and the `.reg` writer run on the
//! Linux build host and in the Windows launcher alike.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The data of one value, by registry type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RegData {
    /// `REG_SZ`
    String(String),
    /// `REG_EXPAND_SZ`: `%VAR%` references expand when the app reads it.
    Expand(String),
    /// `REG_DWORD`
    Dword(u32),
    /// `REG_QWORD`
    Qword(u64),
    /// `REG_BINARY`
    Binary(Vec<u8>),
    /// `REG_MULTI_SZ`
    Strings(Vec<String>),
}

impl RegData {
    /// True when the launcher must rewrite the value at every start
    /// because it names the mounted root, which can move.
    pub fn mentions_root(&self) -> bool {
        match self {
            RegData::String(s) | RegData::Expand(s) => s.contains("$CELL_ROOT"),
            RegData::Strings(v) => v.iter().any(|s| s.contains("$CELL_ROOT")),
            _ => false,
        }
    }

    /// The same data with `f` applied to every string.
    pub fn map_strings(&self, f: impl Fn(&str) -> String) -> RegData {
        match self {
            RegData::String(s) => RegData::String(f(s)),
            RegData::Expand(s) => RegData::Expand(f(s)),
            RegData::Strings(v) => RegData::Strings(v.iter().map(|s| f(s)).collect()),
            other => other.clone(),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            RegData::String(_) => "REG_SZ",
            RegData::Expand(_) => "REG_EXPAND_SZ",
            RegData::Dword(_) => "REG_DWORD",
            RegData::Qword(_) => "REG_QWORD",
            RegData::Binary(_) => "REG_BINARY",
            RegData::Strings(_) => "REG_MULTI_SZ",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegValue {
    /// The value name. Empty names the key's default value (`@` in a
    /// `.reg` file).
    pub name: String,
    pub data: RegData,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegKey {
    /// Relative to `HKEY_CURRENT_USER`, for example `Software\Vendor\App`.
    pub path: String,
    pub values: Vec<RegValue>,
}

/// Everything a cell declares about the registry.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Registry {
    /// Keys the launcher creates. A value is written when it is missing,
    /// or at every start when it names `$CELL_ROOT`.
    pub keys: Vec<RegKey>,
    /// Keys the sandbox may read and write, with nothing written to
    /// them. Relative to `HKEY_CURRENT_USER`.
    pub grants: Vec<String>,
}

impl Registry {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.grants.is_empty()
    }

    /// Add a key. Values of a key that is already present are merged in,
    /// and a value with the same name replaces the old one.
    pub fn add_key(&mut self, key: RegKey) {
        if let Some(k) = self
            .keys
            .iter_mut()
            .find(|k| k.path.eq_ignore_ascii_case(&key.path))
        {
            for v in key.values {
                k.values
                    .retain(|old| !old.name.eq_ignore_ascii_case(&v.name));
                k.values.push(v);
            }
        } else {
            self.keys.push(key);
        }
    }

    /// Every path the launcher grants: the declared keys and the grants.
    pub fn grant_paths(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.keys.iter().map(|k| k.path.as_str()).collect();
        out.extend(self.grants.iter().map(String::as_str));
        out
    }

    /// Render the declaration as a `.reg` file. Strings keep `$CELL_ROOT`
    /// as written unless `expand` replaces it.
    pub fn to_reg(&self, expand: impl Fn(&str) -> String) -> String {
        let mut out = String::from("Windows Registry Editor Version 5.00\r\n");
        for k in &self.keys {
            out.push_str(&format!("\r\n[HKEY_CURRENT_USER\\{}]\r\n", k.path));
            for v in &k.values {
                let name = if v.name.is_empty() {
                    "@".to_string()
                } else {
                    format!("\"{}\"", reg_escape(&v.name))
                };
                out.push_str(&format!(
                    "{name}={}\r\n",
                    reg_data_text(&v.data.map_strings(&expand))
                ));
            }
        }
        out
    }
}

/// A `.reg` text as the bytes regedit writes: UTF-16LE with a byte order
/// mark. Wine's regedit reads `hex(2)` and `hex(7)` data as ANSI unless
/// the file is UTF-16.
pub fn reg_utf16(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    out.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
    out
}

/// Normalise a key path from a build file or a `.reg` file into the
/// manifest form: relative to `HKEY_CURRENT_USER`, `\` separated. Other
/// hives are refused: a cell runs as the user and cannot write them.
pub fn hkcu_subkey(path: &str) -> Result<String> {
    let p = path.replace('/', "\\");
    let p = p.trim_matches('\\');
    let (root, rest) = match p.split_once('\\') {
        Some((r, rest)) => (r, rest.trim_matches('\\')),
        None => (p, ""),
    };
    match root.to_ascii_uppercase().as_str() {
        "HKEY_CURRENT_USER" | "HKCU" => {}
        "HKEY_LOCAL_MACHINE" | "HKLM" => {
            bail!("registry key `{path}`: HKEY_LOCAL_MACHINE needs administrator rights; a cell writes HKEY_CURRENT_USER only")
        }
        "HKEY_CLASSES_ROOT" | "HKCR" => {
            bail!("registry key `{path}`: write HKEY_CURRENT_USER\\Software\\Classes\\... instead of HKEY_CLASSES_ROOT")
        }
        "HKEY_USERS" | "HKU" | "HKEY_CURRENT_CONFIG" | "HKCC" => {
            bail!("registry key `{path}`: only HKEY_CURRENT_USER keys are supported")
        }
        _ => bail!("registry key `{path}` must start with HKCU\\ or HKEY_CURRENT_USER\\"),
    }
    if rest.is_empty() {
        bail!("registry key `{path}` names the whole hive; give a key under it, for example HKCU\\Software\\Vendor\\App");
    }
    let mut parts = Vec::new();
    for part in rest.split('\\') {
        let part = part.trim();
        if part.is_empty() {
            bail!("registry key `{path}` has an empty part");
        }
        parts.push(part);
    }
    Ok(parts.join("\\"))
}

// ---- .reg files --------------------------------------------------------

/// Decode a `.reg` file's bytes. Regedit writes UTF-16LE with a byte
/// order mark; hand-written files are UTF-8.
pub fn decode_reg(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        let s = String::from_utf8_lossy(bytes).into_owned();
        s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s)
    }
}

/// Parse a `.reg` file into keys. Deletions (`[-KEY]`, `"name"=-`) are
/// refused: a cell declares what it needs, not what it removes.
pub fn parse_reg(text: &str) -> Result<Vec<RegKey>> {
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    let header = lines.next().map(str::trim).context("empty .reg file")?;
    let ansi = match header {
        "Windows Registry Editor Version 5.00" => false,
        "REGEDIT4" => true,
        other => bail!("line 1: `{other}` is not a .reg header (expected `Windows Registry Editor Version 5.00`)"),
    };
    let mut keys: Vec<RegKey> = Vec::new();
    let mut lineno = 1usize;
    let mut pending: Option<String> = None;
    let mut pending_line = 0usize;
    for raw in lines {
        lineno += 1;
        let line = raw.trim();
        // A `\` at the end of a line continues a hex list.
        let line = match pending.take() {
            Some(mut acc) => {
                acc.push_str(line);
                acc
            }
            None => line.to_string(),
        };
        if let Some(stripped) = line.strip_suffix('\\') {
            if pending_line == 0 {
                pending_line = lineno;
            }
            pending = Some(stripped.trim_end().to_string());
            continue;
        }
        let at = if pending_line != 0 {
            pending_line
        } else {
            lineno
        };
        pending_line = 0;
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[') {
            let section = section
                .strip_suffix(']')
                .with_context(|| format!("line {at}: key line has no closing `]`"))?;
            if let Some(gone) = section.strip_prefix('-') {
                bail!("line {at}: `[-{gone}]` deletes a key; a cell cannot declare a deletion");
            }
            let path = hkcu_subkey(section).with_context(|| format!("line {at}"))?;
            keys.push(RegKey {
                path,
                values: Vec::new(),
            });
            continue;
        }
        let key = keys
            .last_mut()
            .with_context(|| format!("line {at}: a value before any [KEY] line"))?;
        let (name, rest) = parse_value_name(&line).with_context(|| format!("line {at}"))?;
        let data = parse_value_data(rest, ansi).with_context(|| format!("line {at}"))?;
        key.values.retain(|v| !v.name.eq_ignore_ascii_case(&name));
        key.values.push(RegValue { name, data });
    }
    if pending.is_some() {
        bail!("line {pending_line}: the file ends inside a continued line");
    }
    Ok(keys)
}

/// `@=` or `"name"=`; returns the name and the text after `=`.
fn parse_value_name(line: &str) -> Result<(String, &str)> {
    if let Some(rest) = line.strip_prefix("@=") {
        return Ok((String::new(), rest.trim_start()));
    }
    let Some(body) = line.strip_prefix('"') else {
        bail!("`{line}` is not a `\"name\"=value` line");
    };
    let (name, after) = take_quoted(body)?;
    let rest = after
        .trim_start()
        .strip_prefix('=')
        .with_context(|| format!("`{line}` has no `=` after the value name"))?;
    Ok((name, rest.trim_start()))
}

/// Read a quoted string body up to its closing quote, with `\\` and `\"`
/// unescaped. Returns the text and what follows the closing quote.
fn take_quoted(body: &str) -> Result<(String, &str)> {
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, e)) => out.push(e),
                None => bail!("a `\\` at the end of a quoted string"),
            },
            '"' => return Ok((out, &body[i + 1..])),
            c => out.push(c),
        }
    }
    bail!("a quoted string with no closing quote")
}

fn parse_value_data(text: &str, ansi: bool) -> Result<RegData> {
    if text == "-" {
        bail!("`=-` deletes a value; a cell cannot declare a deletion");
    }
    if let Some(body) = text.strip_prefix('"') {
        let (s, after) = take_quoted(body)?;
        if !after.trim().is_empty() {
            bail!("text after the closing quote: `{}`", after.trim());
        }
        return Ok(RegData::String(s));
    }
    if let Some(hex) = text.strip_prefix("dword:") {
        let n = u32::from_str_radix(hex.trim(), 16)
            .with_context(|| format!("`dword:{hex}` is not 8 hex digits"))?;
        return Ok(RegData::Dword(n));
    }
    let (kind, list) = if let Some(rest) = text.strip_prefix("hex(") {
        let (k, rest) = rest.split_once("):").context("`hex(` without `):`")?;
        (
            u32::from_str_radix(k.trim(), 16).context("bad hex type")?,
            rest,
        )
    } else if let Some(rest) = text.strip_prefix("hex:") {
        (3u32, rest)
    } else {
        bail!("`{text}` is not a string, dword or hex value");
    };
    let mut bytes = Vec::new();
    for b in list.split(',') {
        let b = b.trim();
        if b.is_empty() {
            continue;
        }
        bytes.push(u8::from_str_radix(b, 16).with_context(|| format!("`{b}` is not a hex byte"))?);
    }
    Ok(match kind {
        0 | 3 => RegData::Binary(bytes),
        1 => RegData::String(decode_sz(&bytes, ansi)),
        2 => RegData::Expand(decode_sz(&bytes, ansi)),
        4 => RegData::Dword(u32::from_le_bytes(
            bytes
                .as_slice()
                .try_into()
                .context("hex(4) needs 4 bytes")?,
        )),
        7 => RegData::Strings(decode_multi_sz(&bytes, ansi)),
        0xb => RegData::Qword(u64::from_le_bytes(
            bytes
                .as_slice()
                .try_into()
                .context("hex(b) needs 8 bytes")?,
        )),
        other => bail!("registry type hex({other:x}) is not supported"),
    })
}

fn decode_units(bytes: &[u8], ansi: bool) -> Vec<u32> {
    if ansi {
        bytes.iter().map(|&b| b as u32).collect()
    } else {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c) as u32)
            .collect()
    }
}

fn units_to_string(units: &[u32], ansi: bool) -> String {
    if ansi {
        units.iter().map(|&u| u as u8 as char).collect()
    } else {
        let u16s: Vec<u16> = units.iter().map(|&u| u as u16).collect();
        String::from_utf16_lossy(&u16s)
    }
}

fn decode_sz(bytes: &[u8], ansi: bool) -> String {
    let units = decode_units(bytes, ansi);
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    units_to_string(&units[..end], ansi)
}

fn decode_multi_sz(bytes: &[u8], ansi: bool) -> Vec<String> {
    let units = decode_units(bytes, ansi);
    let mut out = Vec::new();
    for part in units.split(|&u| u == 0) {
        if part.is_empty() {
            break;
        }
        out.push(units_to_string(part, ansi));
    }
    out
}

fn reg_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn hex_list(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn utf16_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(|u| u.to_le_bytes())
        .collect()
}

fn reg_data_text(d: &RegData) -> String {
    match d {
        RegData::String(s) => format!("\"{}\"", reg_escape(s)),
        RegData::Expand(s) => format!("hex(2):{}", hex_list(&utf16_bytes(s))),
        RegData::Dword(n) => format!("dword:{n:08x}"),
        RegData::Qword(n) => format!("hex(b):{}", hex_list(&n.to_le_bytes())),
        RegData::Binary(b) => format!("hex:{}", hex_list(b)),
        RegData::Strings(v) => {
            let mut bytes: Vec<u8> = v.iter().flat_map(|s| utf16_bytes(s)).collect();
            bytes.extend([0, 0]);
            format!("hex(7):{}", hex_list(&bytes))
        }
    }
}

/// One line per value, for `info` and `inspect`.
pub fn describe(reg: &Registry) -> Vec<String> {
    let mut out = Vec::new();
    for k in &reg.keys {
        out.push(format!("HKCU\\{} ({} values)", k.path, k.values.len()));
        for v in &k.values {
            let name = if v.name.is_empty() {
                "(default)"
            } else {
                &v.name
            };
            let data = match &v.data {
                RegData::String(s) | RegData::Expand(s) => format!("\"{s}\""),
                RegData::Dword(n) => format!("{n:#x}"),
                RegData::Qword(n) => format!("{n:#x}"),
                RegData::Binary(b) => format!("{} bytes", b.len()),
                RegData::Strings(v) => format!("{v:?}"),
            };
            out.push(format!("  {name} {} = {data}", v.data.type_name()));
        }
    }
    for g in &reg.grants {
        out.push(format!("HKCU\\{g} (grant only)"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subkey_forms() {
        assert_eq!(hkcu_subkey("HKCU/Software/X").unwrap(), "Software\\X");
        assert_eq!(
            hkcu_subkey("HKEY_CURRENT_USER\\Software\\X\\").unwrap(),
            "Software\\X"
        );
        assert_eq!(hkcu_subkey("hkcu\\Software").unwrap(), "Software");
        assert!(hkcu_subkey("HKLM\\Software").is_err());
        assert!(hkcu_subkey("HKCR\\.txt").is_err());
        assert!(hkcu_subkey("Software\\X").is_err());
        assert!(hkcu_subkey("HKCU").is_err());
        assert!(hkcu_subkey("HKCU\\Software\\\\X").is_err());
    }

    #[test]
    fn parse_every_type() {
        let text = "Windows Registry Editor Version 5.00\r\n\
            \r\n\
            ; a comment\r\n\
            [HKEY_CURRENT_USER\\Software\\Test]\r\n\
            @=\"def\"\r\n\
            \"Str\"=\"a \\\"b\\\" c\\\\d\"\r\n\
            \"Num\"=dword:0000000a\r\n\
            \"Q\"=hex(b):ff,ff,ff,ff,00,00,00,00\r\n\
            \"Bin\"=hex:01,02,\\\r\n\
            \x20 03\r\n\
            \"Exp\"=hex(2):25,00,41,00,25,00,00,00\r\n\
            \"Multi\"=hex(7):61,00,00,00,62,00,63,00,00,00,00,00\r\n";
        let keys = parse_reg(text).unwrap();
        assert_eq!(keys.len(), 1);
        let k = &keys[0];
        assert_eq!(k.path, "Software\\Test");
        let get = |n: &str| &k.values.iter().find(|v| v.name == n).unwrap().data;
        assert_eq!(get(""), &RegData::String("def".into()));
        assert_eq!(get("Str"), &RegData::String("a \"b\" c\\d".into()));
        assert_eq!(get("Num"), &RegData::Dword(10));
        assert_eq!(get("Q"), &RegData::Qword(0xffff_ffff));
        assert_eq!(get("Bin"), &RegData::Binary(vec![1, 2, 3]));
        assert_eq!(get("Exp"), &RegData::Expand("%A%".into()));
        assert_eq!(
            get("Multi"),
            &RegData::Strings(vec!["a".into(), "bc".into()])
        );
    }

    #[test]
    fn refuses_deletions_and_other_hives() {
        let base = "Windows Registry Editor Version 5.00\n";
        assert!(parse_reg(&format!("{base}[-HKCU\\Software\\X]\n")).is_err());
        assert!(parse_reg(&format!("{base}[HKCU\\Software\\X]\n\"a\"=-\n")).is_err());
        assert!(parse_reg(&format!("{base}[HKLM\\Software\\X]\n")).is_err());
        assert!(parse_reg("REGEDIT5\n").is_err());
        assert!(parse_reg(&format!("{base}\"a\"=\"b\"\n")).is_err());
    }

    #[test]
    fn utf16_round_trip() {
        let mut reg = Registry::default();
        reg.add_key(RegKey {
            path: "Software\\Test".into(),
            values: vec![
                RegValue {
                    name: "Dir".into(),
                    data: RegData::String("$CELL_ROOT\\app".into()),
                },
                RegValue {
                    name: "Exp".into(),
                    data: RegData::Expand("%APPDATA%\\x".into()),
                },
                RegValue {
                    name: "L".into(),
                    data: RegData::Strings(vec!["one".into(), "two".into()]),
                },
                RegValue {
                    name: "Q".into(),
                    data: RegData::Qword(1 << 40),
                },
            ],
        });
        let text = reg.to_reg(|s| s.to_string());
        let bytes = reg_utf16(&text);
        let back = parse_reg(&decode_reg(&bytes)).unwrap();
        assert_eq!(back, reg.keys);
    }

    #[test]
    fn add_key_merges() {
        let mut reg = Registry::default();
        let v = |n: &str, d: u32| RegValue {
            name: n.into(),
            data: RegData::Dword(d),
        };
        reg.add_key(RegKey {
            path: "Software\\A".into(),
            values: vec![v("x", 1), v("y", 2)],
        });
        reg.add_key(RegKey {
            path: "software\\a".into(),
            values: vec![v("X", 3)],
        });
        assert_eq!(reg.keys.len(), 1);
        assert_eq!(reg.keys[0].values.len(), 2);
        assert!(reg.keys[0]
            .values
            .iter()
            .any(|v| v.data == RegData::Dword(3)));
        assert!(RegData::String("$CELL_ROOT/x".into()).mentions_root());
        assert!(!RegData::Dword(1).mentions_root());
    }
}
