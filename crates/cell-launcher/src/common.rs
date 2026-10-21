//! Pieces both launchers share: hash verification and the entry point
//! selection.

use anyhow::{Context, Result};
use cell_core::format::{CellFile, Manifest};

/// Check every layer's BLAKE3 hash. Returns the exit code.
pub fn verify(cell: &CellFile) -> Result<i32> {
    use std::io::{Read, Seek, SeekFrom};
    let mut bad = 0;
    for l in &cell.manifest.layers {
        let (p, off) = cell.layer_location(l)?;
        let mut f = std::fs::File::open(&p)?;
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

/// The command to run: the entry point, or the named alternative from
/// `CELL_COMMAND=name`.
pub fn select_exec(m: &Manifest) -> Result<Vec<String>> {
    match std::env::var("CELL_COMMAND").ok().filter(|s| !s.is_empty()) {
        Some(name) => {
            let c = m
                .entry
                .commands
                .iter()
                .find(|c| c.name == name)
                .with_context(|| format!("no command named {name}"))?;
            Ok(c.exec.clone())
        }
        None => Ok(m.entry.exec.clone()),
    }
}
