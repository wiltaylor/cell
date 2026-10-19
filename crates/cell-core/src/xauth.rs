//! Xauthority handling. The sandbox gets a fresh cookie file that holds the
//! host's cookie for the display in use, rewritten with the wildcard
//! address family so it matches from any hostname.

use anyhow::{Context, Result};

const FAMILY_WILD: u16 = 0xFFFF;

struct Entry {
    family: u16,
    address: Vec<u8>,
    number: Vec<u8>,
    name: Vec<u8>,
    data: Vec<u8>,
}

fn read_field(b: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    if *pos + 2 > b.len() {
        return None;
    }
    let len = u16::from_be_bytes([b[*pos], b[*pos + 1]]) as usize;
    *pos += 2;
    if *pos + len > b.len() {
        return None;
    }
    let v = b[*pos..*pos + len].to_vec();
    *pos += len;
    Some(v)
}

fn parse(b: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 2 <= b.len() {
        let family = u16::from_be_bytes([b[pos], b[pos + 1]]);
        pos += 2;
        let (Some(address), Some(number), Some(name), Some(data)) = (
            read_field(b, &mut pos),
            read_field(b, &mut pos),
            read_field(b, &mut pos),
            read_field(b, &mut pos),
        ) else {
            break;
        };
        out.push(Entry {
            family,
            address,
            number,
            name,
            data,
        });
    }
    out
}

fn write_field(out: &mut Vec<u8>, v: &[u8]) {
    out.extend_from_slice(&(v.len() as u16).to_be_bytes());
    out.extend_from_slice(v);
}

/// Display number from a `DISPLAY` value such as `:1`, `:1.0` or `host:2`.
pub fn display_number(display: &str) -> Option<String> {
    let after = display.rsplit(':').next()?;
    let num = after.split('.').next()?;
    if num.is_empty() {
        None
    } else {
        Some(num.to_string())
    }
}

/// Build a wildcard Xauthority file for one display from the host file.
pub fn wildcard_cookie(host_file: &[u8], display: &str) -> Result<Vec<u8>> {
    let number = display_number(display).context("DISPLAY has no display number")?;
    let mut out = Vec::new();
    for e in parse(host_file) {
        if e.number != number.as_bytes() && !e.number.is_empty() {
            continue;
        }
        let _ = e.family;
        let _ = &e.address;
        out.extend_from_slice(&FAMILY_WILD.to_be_bytes());
        write_field(&mut out, b"");
        write_field(&mut out, number.as_bytes());
        write_field(&mut out, &e.name);
        write_field(&mut out, &e.data);
    }
    Ok(out)
}
