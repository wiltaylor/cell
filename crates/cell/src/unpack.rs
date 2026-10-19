//! One tar unpack routine for every base source.
//!
//! The routine writes a tar stream into a rootfs directory. It keeps
//! permissions, ownership, symlinks, device nodes and xattrs. It resolves
//! every path inside the destination, so a symlink in the image cannot point
//! the writer at the host. With `whiteouts` set, it applies the OCI layer
//! rules: `.wh.<name>` deletes `<name>`, and `.wh..wh..opq` makes the
//! directory opaque.

use std::collections::HashSet;
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use tar::EntryType;

/// How to unpack one tar stream.
#[derive(Default)]
pub struct Options {
    /// Drop a top-level `root.*` directory (Arch bootstrap tarballs).
    pub strip_root_dir: bool,
    /// Apply OCI whiteout entries instead of writing them as files.
    pub whiteouts: bool,
}

/// Open a tar file and wrap it in the decoder that its content needs.
/// The magic bytes decide. `hint` (a file name or a media type) breaks a tie.
pub fn open_compressed(path: &Path, hint: &str) -> Result<Box<dyn Read>> {
    let mut probe = fs::File::open(path)?;
    let mut magic = [0u8; 4];
    let n = probe.read(&mut magic).unwrap_or(0);
    let f = fs::File::open(path)?;
    let zstd_magic = n >= 4 && magic == [0x28, 0xB5, 0x2F, 0xFD];
    let gzip_magic = n >= 2 && magic[..2] == [0x1F, 0x8B];
    let reader: Box<dyn Read> = if zstd_magic || (!gzip_magic && hint.ends_with("zst")) {
        Box::new(zstd::stream::read::Decoder::new(f)?)
    } else if gzip_magic || hint.ends_with("gz") || hint.ends_with("gzip") {
        Box::new(flate2::read::GzDecoder::new(f))
    } else {
        Box::new(f)
    };
    Ok(reader)
}

/// Write the tar stream `reader` into `dest`.
pub fn unpack_tar(reader: impl Read, dest: &Path, opts: &Options) -> Result<()> {
    let mut ar = tar::Archive::new(reader);
    ar.set_preserve_permissions(true);
    ar.set_preserve_ownerships(true);
    ar.set_unpack_xattrs(true);
    ar.set_overwrite(true);
    // Paths this layer wrote. An opaque marker keeps them.
    let mut written: HashSet<PathBuf> = HashSet::new();
    for e in ar.entries()? {
        let mut e = e?;
        let raw = e.path()?.into_owned();
        let Some(rel) = clean_path(&raw, opts.strip_root_dir) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let kind = e.header().entry_type();
        if opts.whiteouts {
            if let Some(name) = rel.file_name().and_then(OsStr::to_str) {
                if let Some(victim) = name.strip_prefix(".wh.") {
                    let parent = rel.parent().unwrap_or(Path::new(""));
                    apply_whiteout(dest, parent, victim, &written);
                    continue;
                }
            }
        }
        let target = resolve(dest, &rel);
        if let Some(p) = target.parent() {
            fs::create_dir_all(p).with_context(|| format!("mkdir {}", p.display()))?;
        }
        written.insert(rel.clone());
        match kind {
            EntryType::Directory => {
                // A symlink to a directory stays. Anything else makes way.
                if let Ok(m) = fs::symlink_metadata(&target) {
                    if !m.is_dir() && !target.is_dir() {
                        let _ = fs::remove_file(&target);
                    }
                }
                if let Err(err) = e.unpack(&target) {
                    eprintln!("cell: warning: {}: {err}", rel.display());
                }
            }
            EntryType::Link => {
                // tar-rs resolves hard links against the working directory;
                // resolve them inside the destination instead.
                let Some(link) = e.link_name()? else { continue };
                let Some(link_rel) = clean_path(&link, opts.strip_root_dir) else { continue };
                let src = resolve(dest, &link_rel);
                remove_any(&target);
                if let Err(err) = fs::hard_link(&src, &target) {
                    eprintln!("cell: warning: hard link {}: {err}", rel.display());
                }
            }
            EntryType::Char | EntryType::Block | EntryType::Fifo => {
                remove_any(&target);
                // A device node that the namespace cannot make is not an error.
                let _ = make_node(&target, e.header(), kind);
            }
            _ => {
                remove_any(&target);
                if let Err(err) = e.unpack(&target) {
                    eprintln!("cell: warning: {}: {err}", rel.display());
                }
            }
        }
    }
    Ok(())
}

/// Turn a tar path into a clean relative path with normal components only.
/// Return `None` for a path that climbs out of the tree.
fn clean_path(raw: &Path, strip_root_dir: bool) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut first = true;
    for c in raw.components() {
        match c {
            Component::Normal(x) => {
                let skip = first
                    && strip_root_dir
                    && x.to_string_lossy().starts_with("root.");
                first = false;
                if !skip {
                    out.push(x);
                }
            }
            Component::ParentDir => return None,
            _ => {}
        }
    }
    Some(out)
}

/// Join `rel` onto `root` and follow symlinks in every directory component,
/// with `root` as the file system root. The last component stays literal.
pub fn resolve(root: &Path, rel: &Path) -> PathBuf {
    let comps: Vec<&OsStr> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(x) => Some(x),
            _ => None,
        })
        .collect();
    let mut cur = root.to_path_buf();
    let n = comps.len();
    for (i, c) in comps.iter().enumerate() {
        let next = cur.join(c);
        if i + 1 == n {
            return next;
        }
        cur = follow(root, next, 0);
    }
    cur
}

/// Follow `p` while it is a symlink. An absolute link target starts again
/// at `root`. A `..` never climbs above `root`.
fn follow(root: &Path, p: PathBuf, depth: u32) -> PathBuf {
    if depth > 40 {
        return p;
    }
    let Ok(t) = fs::read_link(&p) else { return p };
    let mut cur = if t.is_absolute() {
        root.to_path_buf()
    } else {
        p.parent().map(Path::to_path_buf).unwrap_or_else(|| root.to_path_buf())
    };
    for c in t.components() {
        match c {
            Component::Normal(x) => cur = follow(root, cur.join(x), depth + 1),
            Component::ParentDir if cur != root => {
                cur.pop();
            }
            _ => {}
        }
    }
    cur
}

fn remove_any(p: &Path) {
    if let Ok(m) = fs::symlink_metadata(p) {
        if m.is_dir() {
            let _ = fs::remove_dir_all(p);
        } else {
            let _ = fs::remove_file(p);
        }
    }
}

/// Apply one whiteout entry found in directory `parent`.
fn apply_whiteout(dest: &Path, parent: &Path, victim: &str, written: &HashSet<PathBuf>) {
    if victim == ".wh..opq" {
        let dir = resolve(dest, parent);
        let Ok(rd) = fs::read_dir(&dir) else { return };
        for ent in rd.flatten() {
            let rel = parent.join(ent.file_name());
            if written.iter().any(|w| w.starts_with(&rel)) {
                continue;
            }
            remove_any(&ent.path());
        }
    } else {
        remove_any(&resolve(dest, &parent.join(victim)));
    }
}

/// Make a character device, a block device or a fifo from a tar header.
fn make_node(target: &Path, header: &tar::Header, kind: EntryType) -> Result<()> {
    let mode = header.mode().unwrap_or(0o600);
    let ftype = match kind {
        EntryType::Char => libc::S_IFCHR,
        EntryType::Block => libc::S_IFBLK,
        _ => libc::S_IFIFO,
    };
    let major = header.device_major()?.unwrap_or(0);
    let minor = header.device_minor()?.unwrap_or(0);
    let dev = libc::makedev(major, minor);
    let c = CString::new(target.as_os_str().as_bytes())?;
    let rc = unsafe { libc::mknod(c.as_ptr(), ftype | mode, dev) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let uid = header.uid().unwrap_or(0) as libc::uid_t;
    let gid = header.gid().unwrap_or(0) as libc::gid_t;
    unsafe {
        libc::lchown(c.as_ptr(), uid, gid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(entries: &[(&str, EntryType, &str)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (path, kind, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(*kind);
            h.set_mode(if *kind == EntryType::Directory { 0o755 } else { 0o644 });
            match kind {
                EntryType::Symlink => {
                    h.set_size(0);
                    b.append_link(&mut h, path, data).unwrap();
                }
                _ => {
                    h.set_size(data.len() as u64);
                    b.append_data(&mut h, path, data.as_bytes()).unwrap();
                }
            }
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn whiteouts_and_opaque_dirs() {
        let dest = std::env::temp_dir().join(format!("cell-unpack-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(&dest).unwrap();
        let opts = Options { strip_root_dir: false, whiteouts: true };
        let d = EntryType::Directory;
        let f = EntryType::Regular;
        let l1 = layer(&[
            ("etc/", d, ""),
            ("etc/keep", f, "1"),
            ("etc/gone", f, "1"),
            ("opt/", d, ""),
            ("opt/old", f, "1"),
            ("opt/sub/", d, ""),
            ("opt/sub/x", f, "1"),
            ("lib", EntryType::Symlink, "usr/lib"),
            ("usr/lib/", d, ""),
        ]);
        unpack_tar(&l1[..], &dest, &opts).unwrap();
        let l2 = layer(&[
            ("etc/.wh.gone", f, ""),
            ("opt/.wh..wh..opq", f, ""),
            ("opt/new", f, "2"),
            ("lib/through-link", f, "2"),
        ]);
        unpack_tar(&l2[..], &dest, &opts).unwrap();
        assert!(dest.join("etc/keep").exists());
        assert!(!dest.join("etc/gone").exists());
        assert!(!dest.join("opt/old").exists());
        assert!(!dest.join("opt/sub").exists());
        assert_eq!(fs::read_to_string(dest.join("opt/new")).unwrap(), "2");
        // A write through the `lib` symlink lands inside the destination.
        assert!(dest.join("usr/lib/through-link").exists());
        assert!(fs::symlink_metadata(dest.join("lib")).unwrap().is_symlink());
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn absolute_symlink_stays_inside_root() {
        let dest = std::env::temp_dir().join(format!("cell-resolve-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(dest.join("usr/bin")).unwrap();
        std::os::unix::fs::symlink("/usr/bin", dest.join("bin")).unwrap();
        std::os::unix::fs::symlink("../../..", dest.join("usr/up")).unwrap();
        assert_eq!(resolve(&dest, Path::new("bin/sh")), dest.join("usr/bin/sh"));
        assert_eq!(resolve(&dest, Path::new("usr/up/etc/x")), dest.join("etc/x"));
        assert_eq!(clean_path(Path::new("../x"), false), None);
        assert_eq!(clean_path(Path::new("./root.x86_64/etc"), true), Some(PathBuf::from("etc")));
        let _ = fs::remove_dir_all(&dest);
    }
}
