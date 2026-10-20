//! NVIDIA `:fetch`: download the driver archive that matches the loaded
//! kernel module and unpack its userspace libraries into a cache.
//!
//! The proprietary userspace must match the kernel module exactly. The
//! launcher reads the module version, downloads
//! `NVIDIA-Linux-x86_64-<version>.run` from NVIDIA once and keeps the
//! libraries under `$XDG_CACHE_HOME/cell/drivers/nvidia-<version>/`:
//!
//! - `lib/`    64-bit libraries, with `lib/gbm/` and `lib/vdpau/`
//! - `lib32/`  32-bit libraries from the archive's `32/` directory
//! - `share/`  the ICD and vendor JSON files, for reference
//! - `done`    marker; the cache is complete when it exists
//!
//! The `.run` file is a makeself archive: a shell header, then a compressed
//! tar. The header line `skip=N` gives the first payload line. Drivers since
//! 550 compress with zstd; older ones with xz. The archive holds no `.so.N`
//! links; `nvidia-installer` makes them from each library's SONAME, and so
//! does this module.
//!
//! This is the model Flatpak uses for `org.freedesktop.Platform.GL.nvidia`.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// JSON files the archive ships that describe the driver to loaders.
const SHARE_JSON: &[&str] = &[
    "nvidia_icd.json",
    "nvidia_layers.json",
    "10_nvidia.json",
    "10_nvidia_wayland.json",
    "15_nvidia_gbm.json",
];

/// A complete driver cache for one kernel module version.
#[derive(Debug, Clone)]
pub struct FetchedDriver {
    pub version: String,
    pub dir: PathBuf,
}

impl FetchedDriver {
    pub fn lib(&self) -> PathBuf {
        self.dir.join("lib")
    }
    pub fn lib32(&self) -> PathBuf {
        self.dir.join("lib32")
    }
    pub fn share(&self) -> PathBuf {
        self.dir.join("share")
    }
}

/// Where the cache for `version` lives.
pub fn cache_dir(version: &str) -> PathBuf {
    crate::host::cache_home()
        .join("drivers")
        .join(format!("nvidia-{version}"))
}

/// The cache for `version`, if it is complete.
pub fn cached(version: &str) -> Option<FetchedDriver> {
    let dir = cache_dir(version);
    dir.join("done").exists().then(|| FetchedDriver {
        version: version.to_string(),
        dir,
    })
}

/// The download URLs to try, in order.
pub fn download_urls(version: &str) -> Vec<String> {
    let file = format!("NVIDIA-Linux-x86_64-{version}.run");
    vec![
        format!("https://download.nvidia.com/XFree86/Linux-x86_64/{version}/{file}"),
        format!("https://us.download.nvidia.com/XFree86/Linux-x86_64/{version}/{file}"),
        format!("https://us.download.nvidia.com/tesla/{version}/{file}"),
    ]
}

/// Return the cache for `version`. Download and unpack the driver first
/// when the cache is not complete. This needs host network access, so
/// call it before the launcher enters its namespaces.
pub fn fetch(version: &str) -> Result<FetchedDriver> {
    if let Some(d) = cached(version) {
        return Ok(d);
    }
    let dir = cache_dir(version);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    // One launcher at a time fills a cache. A second launcher waits here
    // and then finds the marker.
    let lock = File::create(dir.join("lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error()).context("lock driver cache");
    }
    if let Some(d) = cached(version) {
        return Ok(d);
    }

    let run = dir.join(format!("NVIDIA-Linux-x86_64-{version}.run"));
    if !run.exists() {
        download(version, &run)?;
    }
    let out = FetchedDriver {
        version: version.to_string(),
        dir: dir.clone(),
    };
    eprintln!("cell: nvidia: unpacking {}", run.display());
    let kept = unpack(&run, &out).with_context(|| format!("unpack {}", run.display()))?;
    eprintln!(
        "cell: nvidia: kept {} 64-bit and {} 32-bit libraries in {}",
        kept.0,
        kept.1,
        dir.display()
    );
    let _ = fs::remove_file(&run);
    fs::write(
        dir.join("done"),
        format!("nvidia {version}\nunpacked by cell-launcher\n"),
    )?;
    Ok(out)
}

// ---- download ----------------------------------------------------------------

/// Download the `.run` file to `dest`, through a `.partial` file. Try each
/// URL in turn; the first that answers is used. A `.partial` left by an
/// earlier run continues with a `Range` request.
///
/// `ureq` with `rustls` and `ring` builds for `x86_64-unknown-linux-musl`
/// through the zig wrapper, so the static launcher needs no host `curl`.
fn download(version: &str, dest: &Path) -> Result<()> {
    let partial = dest.with_extension("run.partial");
    let agent = ureq::config::Config::builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .user_agent("cell-launcher")
        .build()
        .new_agent();
    let mut errors = Vec::new();
    for url in download_urls(version) {
        let have = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
        let mut req = agent.get(&url);
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("{url}: {e}"));
                continue;
            }
        };
        let status = resp.status().as_u16();
        let resume = status == 206 && have > 0;
        if status != 200 && !resume {
            errors.push(format!("{url}: HTTP {status}"));
            continue;
        }
        let remaining: Option<u64> = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let start = if resume { have } else { 0 };
        let total = remaining.map(|r| r + start);
        eprintln!(
            "cell: nvidia: downloading {url}{}",
            match (total, resume) {
                (Some(t), true) => format!(" ({} MiB, resuming at {} MiB)", t >> 20, start >> 20),
                (Some(t), false) => format!(" ({} MiB)", t >> 20),
                (None, _) => String::new(),
            }
        );
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(!resume)
            .append(resume)
            .open(&partial)?;
        let mut body = resp.into_body().into_reader();
        copy_with_progress(&mut body, &mut file, start, total)
            .with_context(|| format!("read {url}"))?;
        file.flush()?;
        drop(file);
        if let Some(t) = total {
            let got = fs::metadata(&partial)?.len();
            if got != t {
                bail!("{url}: got {got} bytes, expected {t}");
            }
        }
        fs::rename(&partial, dest)?;
        return Ok(());
    }
    bail!(
        "no download for NVIDIA driver {version} ({})",
        errors.join("; ")
    )
}

/// Copy `src` to `dst` and print a line on stderr at each 10%.
fn copy_with_progress(
    src: &mut impl Read,
    dst: &mut impl Write,
    start: u64,
    total: Option<u64>,
) -> Result<()> {
    let mut buf = vec![0u8; 1 << 20];
    let mut done = start;
    let mut next_pct = 10;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(t) = total.filter(|t| *t > 0) {
            let pct = done * 100 / t;
            while pct >= next_pct && next_pct <= 100 {
                eprintln!("cell: nvidia: {next_pct}% ({} MiB)", done >> 20);
                next_pct += 10;
            }
        }
    }
    Ok(())
}

// ---- makeself header -------------------------------------------------------

/// Where the compressed payload starts: the byte offset of line `skip`
/// (1-based, as `tail -n +$skip` counts) from the makeself header.
fn payload_offset(run: &Path) -> Result<u64> {
    let mut r = BufReader::new(File::open(run)?);
    let mut skip: Option<u64> = None;
    let mut line = String::new();
    for _ in 0..64 {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            break;
        }
        if let Some(v) = line.trim().strip_prefix("skip=") {
            skip = Some(v.parse().context("parse skip= in makeself header")?);
            break;
        }
    }
    let skip = skip.context("no skip= line in the makeself header")?;
    r.seek(SeekFrom::Start(0))?;
    let mut offset = 0u64;
    let mut lines = 1u64;
    let mut buf = vec![0u8; 1 << 16];
    'outer: while lines < skip {
        let n = r.read(&mut buf)?;
        if n == 0 {
            bail!("makeself header ends before line {skip}");
        }
        for b in &buf[..n] {
            offset += 1;
            if *b == b'\n' {
                lines += 1;
                if lines == skip {
                    break 'outer;
                }
            }
        }
    }
    Ok(offset)
}

enum Payload {
    Zstd,
    Xz,
}

fn payload_kind(run: &Path, offset: u64) -> Result<Payload> {
    let mut f = File::open(run)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut magic = [0u8; 6];
    f.read_exact(&mut magic)?;
    if magic[..4] == [0x28, 0xB5, 0x2F, 0xFD] {
        Ok(Payload::Zstd)
    } else if magic == [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00] {
        Ok(Payload::Xz)
    } else {
        bail!("payload at offset {offset} is not zstd or xz (magic {magic:02x?})")
    }
}

/// A reader over the decompressed tar stream. The xz path runs `lzma-rs`
/// on a thread and streams through a pipe, because its decoder writes to a
/// `Write` and does not implement `Read`.
fn payload_reader(run: &Path, offset: u64) -> Result<Box<dyn Read>> {
    let mut f = File::open(run)?;
    f.seek(SeekFrom::Start(offset))?;
    match payload_kind(run, offset)? {
        Payload::Zstd => Ok(Box::new(zstd::stream::read::Decoder::new(f)?)),
        Payload::Xz => {
            let (rx, tx) = io::pipe()?;
            std::thread::spawn(move || {
                let mut src = BufReader::with_capacity(1 << 20, f);
                let mut dst = io::BufWriter::with_capacity(1 << 20, tx);
                if let Err(e) = lzma_rs::xz_decompress(&mut src, &mut dst) {
                    eprintln!("cell: nvidia: xz: {e}");
                }
                let _ = dst.flush();
            });
            Ok(Box::new(rx))
        }
    }
}

// ---- unpack ------------------------------------------------------------------

/// Where an archive entry goes in the cache, or `None` to skip it.
fn destination(rel: &Path, out: &FetchedDriver) -> Option<PathBuf> {
    let mut parts = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string());
    let first = parts.next()?;
    let second = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (base, name) = match second {
        None => (out.lib(), first),
        Some(name) if first == "32" => (out.lib32(), name),
        Some(_) => return None,
    };
    if base == out.lib() && SHARE_JSON.contains(&name.as_str()) {
        return Some(out.share().join(name));
    }
    if !name.contains(".so") {
        return None;
    }
    if name.starts_with("libvdpau_nvidia.") {
        return Some(base.join("vdpau").join(name));
    }
    crate::gpu::is_nvidia_lib(&name).then(|| base.join(name))
}

/// Unpack the libraries from `run` into `out`. Return the count of 64-bit
/// and 32-bit files kept.
fn unpack(run: &Path, out: &FetchedDriver) -> Result<(usize, usize)> {
    for d in [out.lib(), out.lib32(), out.share()] {
        if d.exists() {
            fs::remove_dir_all(&d)?;
        }
        fs::create_dir_all(&d)?;
    }
    let offset = payload_offset(run)?;
    let reader = payload_reader(run, offset)?;
    let mut archive = tar::Archive::new(reader);
    let mut kept = (0usize, 0usize);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let rel = path.strip_prefix(".").unwrap_or(&path).to_path_buf();
        let Some(dest) = destination(&rel, out) else {
            continue;
        };
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_symlink()) {
            continue;
        }
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        entry
            .unpack(&dest)
            .with_context(|| format!("unpack {}", rel.display()))?;
        if kind.is_file() {
            let mode = entry.header().mode().unwrap_or(0o644);
            fs::set_permissions(&dest, fs::Permissions::from_mode(mode & 0o777))?;
        }
        if dest.starts_with(out.lib32()) {
            kept.1 += 1;
        } else if dest.starts_with(out.lib()) {
            kept.0 += 1;
        }
    }
    if kept.0 == 0 {
        bail!("the archive holds no NVIDIA libraries");
    }
    eprintln!("cell: nvidia: creating SONAME links");
    for d in [out.lib(), out.lib32()] {
        soname_links(&d)?;
        soname_links(&d.join("vdpau"))?;
        gbm_link(&d)?;
    }
    Ok(kept)
}

/// Make `SONAME -> file` links in `dir`, as `nvidia-installer` does.
fn soname_links(dir: &Path) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !e.file_type()?.is_file() || !name.contains(".so") {
            continue;
        }
        let Some(soname) = elf_soname(&e.path()) else {
            continue;
        };
        if soname == name || soname.contains('/') {
            continue;
        }
        let link = dir.join(&soname);
        if !link.exists() && fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(&name, &link)
                .with_context(|| format!("link {}", link.display()))?;
        }
    }
    Ok(())
}

/// The GBM backend is the allocator library under another name:
/// `gbm/nvidia-drm_gbm.so -> ../libnvidia-allocator.so.1`.
fn gbm_link(dir: &Path) -> Result<()> {
    if !dir.join("libnvidia-allocator.so.1").exists() {
        return Ok(());
    }
    let gbm = dir.join("gbm");
    fs::create_dir_all(&gbm)?;
    let link = gbm.join("nvidia-drm_gbm.so");
    if fs::symlink_metadata(&link).is_err() {
        std::os::unix::fs::symlink("../libnvidia-allocator.so.1", &link)?;
    }
    Ok(())
}

// ---- ELF SONAME --------------------------------------------------------------

/// Read `DT_SONAME` from a little-endian ELF shared object.
fn elf_soname(path: &Path) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let mut hdr = [0u8; 64];
    f.read_exact(&mut hdr).ok()?;
    if hdr[..4] != [0x7F, b'E', b'L', b'F'] || hdr[5] != 1 {
        return None;
    }
    let is64 = hdr[4] == 2;
    let u16_at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let (shoff, shentsize, shnum) = if is64 {
        (
            u64_at(&hdr, 0x28),
            u16_at(&hdr, 0x3A) as usize,
            u16_at(&hdr, 0x3C) as usize,
        )
    } else {
        (
            u32_at(&hdr, 0x20) as u64,
            u16_at(&hdr, 0x2E) as usize,
            u16_at(&hdr, 0x30) as usize,
        )
    };
    if shoff == 0 || shentsize == 0 || shnum == 0 {
        return None;
    }
    let mut table = vec![0u8; shentsize * shnum];
    f.seek(SeekFrom::Start(shoff)).ok()?;
    f.read_exact(&mut table).ok()?;
    // (type, offset, size, link) of each section header.
    let section = |i: usize| {
        let s = &table[i * shentsize..(i + 1) * shentsize];
        if is64 {
            (
                u32_at(s, 4),
                u64_at(s, 0x18),
                u64_at(s, 0x20),
                u32_at(s, 0x28) as usize,
            )
        } else {
            (
                u32_at(s, 4),
                u32_at(s, 0x10) as u64,
                u32_at(s, 0x14) as u64,
                u32_at(s, 0x18) as usize,
            )
        }
    };
    let read_section = |f: &mut File, off: u64, size: u64| -> Option<Vec<u8>> {
        if size > 1 << 24 {
            return None;
        }
        let mut v = vec![0u8; size as usize];
        f.seek(SeekFrom::Start(off)).ok()?;
        f.read_exact(&mut v).ok()?;
        Some(v)
    };
    const SHT_DYNAMIC: u32 = 6;
    const DT_SONAME: u64 = 14;
    for i in 0..shnum {
        let (ty, off, size, link) = section(i);
        if ty != SHT_DYNAMIC || link >= shnum {
            continue;
        }
        let dynamic = read_section(&mut f, off, size)?;
        let (_, str_off, str_size, _) = section(link);
        let strtab = read_section(&mut f, str_off, str_size)?;
        let ent = if is64 { 16 } else { 8 };
        for e in dynamic.chunks_exact(ent) {
            let (tag, val) = if is64 {
                (u64_at(e, 0), u64_at(e, 8))
            } else {
                (u32_at(e, 0) as u64, u32_at(e, 4) as u64)
            };
            if tag == 0 {
                break;
            }
            if tag == DT_SONAME {
                let s = strtab.get(val as usize..)?;
                let end = s.iter().position(|b| *b == 0)?;
                return String::from_utf8(s[..end].to_vec()).ok();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_keeps_only_driver_files() {
        let out = FetchedDriver {
            version: "1".into(),
            dir: PathBuf::from("/c"),
        };
        let d = |p: &str| destination(Path::new(p), &out);
        assert_eq!(
            d("libGLX_nvidia.so.615.71.09"),
            Some(PathBuf::from("/c/lib/libGLX_nvidia.so.615.71.09"))
        );
        assert_eq!(
            d("32/libcuda.so.615.71.09"),
            Some(PathBuf::from("/c/lib32/libcuda.so.615.71.09"))
        );
        assert_eq!(
            d("nvidia_icd.json"),
            Some(PathBuf::from("/c/share/nvidia_icd.json"))
        );
        assert_eq!(
            d("libvdpau_nvidia.so.1.0"),
            Some(PathBuf::from("/c/lib/vdpau/libvdpau_nvidia.so.1.0"))
        );
        assert_eq!(d("nvidia_drv.so"), None);
        assert_eq!(d("libGL.so.1.7.0"), None);
        assert_eq!(d("kernel/nvidia/nv.c"), None);
        assert_eq!(d("32/nvidia_icd.json"), None);
        assert_eq!(d("html/gbm.html"), None);
    }

    /// Build a fake `.run`: a makeself header, then `payload`.
    fn fake_run(name: &str, payload: &[u8]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("cell-nvfetch-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.run");
        let mut f = File::create(&p).unwrap();
        f.write_all(b"#! /bin/sh\nskip=4\nlabel=x\n").unwrap();
        f.write_all(payload).unwrap();
        (dir, p)
    }

    /// A tar with the shape of the NVIDIA archive, uncompressed.
    fn fake_tar() -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut add = |path: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, path, data).unwrap();
        };
        add("./libGLX_nvidia.so.1.2", b"gl64");
        add("./32/libcuda.so.9", b"cuda32");
        add("./nvidia_icd.json", b"{}");
        add("./nvidia_drv.so", b"x");
        add("./kernel/nvidia/nv.c", b"c");
        b.into_inner().unwrap()
    }

    fn check_unpacked(dir: &Path, payload: Vec<u8>) {
        let (tmp, run) = fake_run("x", &payload);
        let out = FetchedDriver {
            version: "1".into(),
            dir: dir.to_path_buf(),
        };
        assert_eq!(unpack(&run, &out).unwrap(), (1, 1));
        assert_eq!(
            fs::read(out.lib().join("libGLX_nvidia.so.1.2")).unwrap(),
            b"gl64"
        );
        assert_eq!(
            fs::read(out.lib32().join("libcuda.so.9")).unwrap(),
            b"cuda32"
        );
        assert!(out.share().join("nvidia_icd.json").exists());
        assert!(!out.lib().join("nvidia_drv.so").exists());
        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn unpack_zstd_payload() {
        let payload = zstd::encode_all(&fake_tar()[..], 3).unwrap();
        let (dir, _) = fake_run("zstd-out", b"");
        check_unpacked(&dir, payload);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unpack_xz_payload() {
        let mut payload = Vec::new();
        lzma_rs::xz_compress(&mut &fake_tar()[..], &mut payload).unwrap();
        let (dir, _) = fake_run("xz-out", b"");
        check_unpacked(&dir, payload);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn soname_of_host_libc() {
        for p in [
            "/usr/lib/libc.so.6",
            "/usr/lib64/libc.so.6",
            "/usr/lib/x86_64-linux-gnu/libc.so.6",
        ] {
            if Path::new(p).exists() {
                assert_eq!(elf_soname(Path::new(p)).as_deref(), Some("libc.so.6"));
                return;
            }
        }
    }

    #[test]
    fn payload_offset_counts_lines() {
        let dir = std::env::temp_dir().join(format!("cell-nvfetch-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.run");
        fs::write(&p, b"#! /bin/sh\nskip=4\nlabel=x\nPAYLOAD").unwrap();
        assert_eq!(payload_offset(&p).unwrap(), 26);
        fs::remove_dir_all(&dir).unwrap();
    }
}
