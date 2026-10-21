//! Unpack a squashfs layer into a directory.
//!
//! This is the no-mount tier: a host without FUSE (Linux) or WinFsp
//! (Windows) pays for one extraction per layer and runs the application
//! from a plain directory. Later layers overwrite earlier ones, which gives
//! the same top-layer-wins view the mounted stack has.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::squashfs::{Inode, InodeKind, Squashfs};

/// Names that are legal in a squashfs image but cannot exist on Windows.
/// The build tool rejects them; the extractor skips them with a notice so
/// one bad file does not stop a whole layer.
pub fn windows_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("empty");
    }
    if name.chars().any(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 32
    }) {
        return Some("contains a character Windows forbids (< > : \" / \\ | ? * or a control)");
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Some("ends with a space or a dot");
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Some("is a reserved device name");
    }
    None
}

/// Extract the whole image under `dest`. Existing files are overwritten.
/// Symlinks are skipped on Windows and recreated on Unix. Returns the
/// number of files written.
pub fn extract_all(img: &Squashfs, dest: &Path) -> Result<u64> {
    fs::create_dir_all(dest)?;
    let root = img.read_inode(img.root_ref())?;
    let mut count = 0;
    extract_dir(img, &root, dest, &mut count)?;
    Ok(count)
}

fn extract_dir(img: &Squashfs, dir: &Inode, dest: &Path, count: &mut u64) -> Result<()> {
    for e in img.read_dir(dir)? {
        let name = String::from_utf8_lossy(&e.name).into_owned();
        if cfg!(windows) {
            if let Some(why) = windows_name_problem(&name) {
                eprintln!("cell: skip {}: name {why}", dest.join(&name).display());
                continue;
            }
        }
        let inode = img.read_inode(e.inode_ref)?;
        let target = dest.join(&name);
        match &inode.kind {
            InodeKind::Dir { .. } => {
                fs::create_dir_all(&target)?;
                extract_dir(img, &inode, &target, count)?;
            }
            InodeKind::File { .. } => {
                write_file(img, &inode, &target)?;
                *count += 1;
            }
            InodeKind::Symlink(t) => {
                #[cfg(unix)]
                {
                    let _ = fs::remove_file(&target);
                    std::os::unix::fs::symlink(String::from_utf8_lossy(t).as_ref(), &target)?;
                }
                #[cfg(not(unix))]
                {
                    let _ = t;
                    eprintln!("cell: skip symlink {}", target.display());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn write_file(img: &Squashfs, inode: &Inode, target: &Path) -> Result<()> {
    let size = inode.size();
    let tmp = target.with_extension("cell-partial");
    {
        let mut out =
            fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        let chunk = 4 << 20;
        let mut off = 0u64;
        while off < size {
            let want = (size - off).min(chunk as u64) as usize;
            let data = img.read_file(inode, off, want)?;
            if data.is_empty() {
                bail!("short read in {}", target.display());
            }
            out.write_all(&data)?;
            off += data.len() as u64;
        }
    }
    let _ = fs::remove_file(target);
    fs::rename(&tmp, target).with_context(|| format!("rename to {}", target.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(target, fs::Permissions::from_mode(inode.perm as u32));
    }
    Ok(())
}
