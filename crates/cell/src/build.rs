//! The build: fetch the base, make the runtime layer, run the build steps
//! into the app layer, pack the data layers, and assemble the `.cell`.

use std::fs;
use std::io::{Read, Seek, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use cell_core::format::{
    self, LayerMeta, LayerRole, Manifest, Placement, Trailer, FORMAT_VERSION, TRAILER_LEN,
};
use cell_core::sandbox::{self, BindSpec, Layer, LayerSource, Spec};

use crate::model::{BuildStep, PlacementKind, Plan};
use crate::unpack;

pub struct Options {
    pub out: PathBuf,
    pub launcher: PathBuf,
    pub no_cache: bool,
}

fn sh(cmd: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), cmd.into()]
}

fn hash_str(parts: &[&str]) -> String {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(b"\0");
    }
    h.finalize().to_hex()[..24].to_string()
}

fn status(msg: &str) {
    eprintln!("cell: {msg}");
}

// ---- base ------------------------------------------------------------------

/// Return a directory that holds the extracted base rootfs, and a string
/// that identifies its content for the runtime cache key.
fn base_rootfs(plan: &Plan, no_cache: bool) -> Result<(PathBuf, String)> {
    let base = &plan.runtime.base;
    if let Some(d) = base.strip_prefix("dir://") {
        return Ok((plan.root.join(d), base.clone()));
    }
    if let Some(image) = base.strip_prefix("oci://") {
        let (rootfs, digest) = crate::oci::rootfs(image, &plan.arch, no_cache)?;
        return Ok((rootfs, format!("oci://{image}@{digest}")));
    }
    let cache = cell_core::host::cache_home().join("base");
    fs::create_dir_all(&cache)?;
    let key = hash_str(&[base]);
    let rootfs = cache.join(&key).join("rootfs");
    let stamp = cache.join(&key).join("ok");
    if stamp.exists() && !no_cache {
        status(&format!("base {base}: cached"));
        return Ok((rootfs, base.clone()));
    }
    let _ = fs::remove_dir_all(cache.join(&key));
    fs::create_dir_all(&rootfs)?;
    let tar_path = cache.join(&key).join("base.tar");
    if let Some(image) = base.strip_prefix("docker://") {
        status(&format!("base {base}: exporting with docker"));
        let out = Command::new("docker")
            .args(["create", "--pull=missing", image, "/bin/true"])
            .output()
            .context("docker create (is docker installed and usable without root?)")?;
        if !out.status.success() {
            bail!(
                "docker create {image}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let st = Command::new("docker")
            .args(["export", "-o"])
            .arg(&tar_path)
            .arg(&id)
            .status()?;
        let _ = Command::new("docker").args(["rm", &id]).output();
        if !st.success() {
            bail!("docker export failed");
        }
    } else if let Some(src) = base.strip_prefix("tar://") {
        if src.starts_with("http://") || src.starts_with("https://") {
            status(&format!("base {base}: downloading"));
            let st = Command::new("curl")
                .args(["-L", "--fail", "-o"])
                .arg(&tar_path)
                .arg(src)
                .status()?;
            if !st.success() {
                bail!("download failed");
            }
        } else {
            fs::copy(plan.root.join(src), &tar_path)?;
        }
    } else {
        bail!("unknown base scheme in {base}");
    }
    status("base: extracting");
    let reader = unpack::open_compressed(&tar_path, base)?;
    // Arch bootstrap tarballs wrap everything in `root.x86_64/`.
    let opts = unpack::Options {
        strip_root_dir: true,
        whiteouts: false,
    };
    unpack::unpack_tar(reader, &rootfs, &opts)?;
    let _ = fs::remove_file(&tar_path);
    fs::write(&stamp, b"ok")?;
    Ok((rootfs, base.clone()))
}

// ---- sandbox steps ----------------------------------------------------------

fn build_env() -> Vec<(String, String)> {
    vec![
        (
            "PATH".into(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
        ),
        ("HOME".into(), "/root".into()),
        (
            "TERM".into(),
            std::env::var("TERM").unwrap_or_else(|_| "xterm".into()),
        ),
        ("LANG".into(), "C.UTF-8".into()),
        ("DEBIAN_FRONTEND".into(), "noninteractive".into()),
        ("CELL_BUILD".into(), "1".into()),
    ]
}

/// Run one shell command as root in a sandbox over `lower` with `upper`.
fn run_step(lower: LayerSource, upper: &Path, workdir: &Path, cmd: &str, sbx: &Path) -> Result<()> {
    let mut spec = Spec {
        layers: vec![Layer {
            source: lower,
            mount: "/".into(),
        }],
        upper: Some((upper.to_path_buf(), workdir.to_path_buf())),
        env: build_env(),
        exec: sh(cmd),
        new_pid: true,
        new_net: false,
        new_ipc: false,
        host_proc: false,
        ..Default::default()
    };
    for f in ["/etc/resolv.conf", "/etc/hosts"] {
        spec.binds.push(BindSpec {
            host: PathBuf::from(f),
            guest: f.into(),
            mode: format::BindMode::Ro,
            optional: true,
        });
    }
    status(&format!("run: {cmd}"));
    let code = sandbox::run(&spec, sbx)?;
    teardown(sbx);
    if code != 0 {
        bail!("step failed with exit status {code}: {cmd}");
    }
    Ok(())
}

fn teardown(sbx: &Path) {
    let c = std::ffi::CString::new(sbx.to_string_lossy().as_bytes()).unwrap();
    unsafe {
        libc::umount2(c.as_ptr(), libc::MNT_DETACH);
    }
}

fn fresh_dir(p: &Path) -> Result<()> {
    if p.exists() {
        fs::remove_dir_all(p).with_context(|| format!("clear {}", p.display()))?;
    }
    fs::create_dir_all(p)?;
    Ok(())
}

fn mksquashfs(src: &Path, out: &Path, compress: bool, one_fs: bool) -> Result<()> {
    let _ = fs::remove_file(out);
    let mut c = Command::new("mksquashfs");
    c.arg(src).arg(out);
    c.args([
        "-noappend",
        "-no-recovery",
        "-quiet",
        "-no-progress",
        "-b",
        "1M",
        "-comp",
        "zstd",
    ]);
    if compress {
        c.args(["-Xcompression-level", "15"]);
    } else {
        c.args(["-noD", "-noF"]);
    }
    if one_fs {
        c.arg("-one-file-system");
    }
    status(&format!("pack: {} -> {}", src.display(), out.display()));
    let st = c
        .status()
        .context("run mksquashfs (install squashfs-tools)")?;
    if !st.success() {
        bail!("mksquashfs failed: {st}");
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        let t = fs::read_link(from)?;
        let _ = fs::remove_file(to);
        std::os::unix::fs::symlink(t, to)?;
    } else if meta.is_dir() {
        fs::create_dir_all(to)?;
        fs::set_permissions(to, meta.permissions())?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        if let Some(p) = to.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(from, to)?;
    }
    Ok(())
}

fn package_command(base: &Path, plan: &Plan) -> Option<String> {
    let pk = &plan.runtime.packages;
    if pk.is_empty() {
        return None;
    }
    let list = pk.join(" ");
    if base.join("usr/bin/pacman").exists() {
        // pacman 7 downloads as the `alpm` user and chowns its download
        // directory to it. Inside a build namespace with one mapped id
        // that chown fails, so the sandbox download user is turned off.
        Some(format!(
            "sed -i -e 's/^DownloadUser/#DownloadUser/' -e 's/^#DisableSandbox/DisableSandbox/' /etc/pacman.conf; \
             pacman -Syu --noconfirm --needed {list}"
        ))
    } else if base.join("sbin/apk").exists() {
        Some(format!("apk add --no-cache {list}"))
    } else if base.join("usr/bin/apt-get").exists() {
        Some(format!(
            "apt-get update && apt-get install -y --no-install-recommends {list}"
        ))
    } else if base.join("usr/bin/dnf").exists() {
        Some(format!("dnf install -y {list}"))
    } else {
        Some(format!(
            "echo 'no known package manager in the base' >&2; exit 1 # {list}"
        ))
    }
}

// ---- the build -------------------------------------------------------------

pub fn run_build(plan: &Plan, opts: &Options) -> Result<i32> {
    let work = cell_core::host::cache_home().join("build").join(&plan.id);
    fs::create_dir_all(&work)?;
    if plan.is_windows() {
        return run_build_windows(plan, opts, &work);
    }
    let (base, base_id) = base_rootfs(plan, opts.no_cache)?;

    // Runtime layer.
    let mut setup = plan.runtime.setup.clone();
    if plan.runtime.multilib && base.join("usr/bin/pacman").exists() {
        setup.insert(
            0,
            "sed -i '/^#\\[multilib\\]/,/^#Include/ s/^#//' /etc/pacman.conf; grep -q '^\\[multilib\\]' /etc/pacman.conf || printf '\\n[multilib]\\nInclude = /etc/pacman.d/mirrorlist\\n' >> /etc/pacman.conf".into(),
        );
    }
    // The key uses the resolved base identity, so a moved `oci://` tag
    // makes a new runtime layer.
    let mut key_parts: Vec<&str> = vec![&base_id, &plan.arch];
    let pk = plan.runtime.packages.join(" ");
    key_parts.push(&pk);
    let su = setup.join("\n");
    key_parts.push(&su);
    let rt_key = hash_str(&key_parts);
    let runtimes = cell_core::host::cache_home().join("runtimes");
    fs::create_dir_all(&runtimes)?;
    let runtime_sqfs = runtimes.join(format!("{rt_key}.sqfs"));
    if !runtime_sqfs.exists() || opts.no_cache {
        status("runtime: building");
        let upper = work.join("runtime-upper");
        let workd = work.join("runtime-work");
        fresh_dir(&upper)?;
        fresh_dir(&workd)?;
        let mut cmds = setup.clone();
        if let Some(c) = package_command(&base, plan) {
            cmds.push(c);
        }
        // Package caches do not belong in the image.
        cmds.push("rm -rf /var/cache/pacman/pkg/* /var/lib/apt/lists/* /var/cache/apk/* /root/.cache 2>/dev/null; true".into());
        for (i, c) in cmds.iter().enumerate() {
            run_step(
                LayerSource::Dir(base.clone()),
                &upper,
                &workd,
                c,
                &work.join(format!("sbx-rt-{i}")),
            )?;
        }
        let merged = work.join("runtime-merged");
        let workd2 = work.join("runtime-work2");
        fresh_dir(&merged)?;
        fresh_dir(&workd2)?;
        let data = format!(
            "lowerdir={},upperdir={},workdir={},userxattr",
            base.display(),
            upper.display(),
            workd2.display()
        );
        sandbox::mount("overlay", &merged, Some("overlay"), 0, Some(&data))
            .context("mount merged runtime")?;
        let tmp = runtimes.join(format!("{rt_key}.partial"));
        mksquashfs(&merged, &tmp, true, true)?;
        teardown(&merged);
        fs::rename(&tmp, &runtime_sqfs)?;
    } else {
        status(&format!("runtime: cached ({})", runtime_sqfs.display()));
    }

    // App layer.
    let app_upper = work.join("app-upper");
    let app_work = work.join("app-work");
    fresh_dir(&app_upper)?;
    fresh_dir(&app_work)?;
    for (i, step) in plan.steps.iter().enumerate() {
        match step {
            BuildStep::Copy { from, to } => {
                let dest = app_upper.join(to.trim_start_matches('/'));
                status(&format!("copy: {} -> {to}", from.display()));
                copy_tree(from, &dest)?;
            }
            BuildStep::Run(cmd) => {
                let f = fs::File::open(&runtime_sqfs)?;
                run_step(
                    LayerSource::Squashfs {
                        file: f,
                        base: 0,
                        name: "runtime".into(),
                    },
                    &app_upper,
                    &app_work,
                    cmd,
                    &work.join(format!("sbx-app-{i}")),
                )?;
            }
        }
    }
    let app_sqfs = work.join("app.sqfs");
    mksquashfs(&app_upper, &app_sqfs, true, false)?;

    // Data layers.
    let mut data_files = Vec::new();
    for l in &plan.data_layers {
        let out = work.join(format!("data-{}.sqfs", l.name));
        mksquashfs(&l.source, &out, l.compress, false)?;
        data_files.push((l, out));
    }

    // Assemble.
    assemble(plan, opts, Some(&runtime_sqfs), &app_sqfs, &data_files)?;
    Ok(0)
}

/// A Windows cell: no package manager and no sandbox. The runtime layer,
/// when there is one, is a `dir://` or `tar://` tree packed as is; the app
/// layer is the copy steps; data layers pack like on Linux.
fn run_build_windows(plan: &Plan, opts: &Options, work: &Path) -> Result<i32> {
    let runtime_sqfs = if plan.runtime.base.is_empty() {
        None
    } else {
        let (base, base_id) = base_rootfs(plan, opts.no_cache)?;
        let rt_key = hash_str(&[&base_id, &plan.arch, "windows"]);
        let runtimes = cell_core::host::cache_home().join("runtimes");
        fs::create_dir_all(&runtimes)?;
        let out = runtimes.join(format!("{rt_key}.sqfs"));
        if !out.exists() || opts.no_cache {
            let tmp = runtimes.join(format!("{rt_key}.partial"));
            mksquashfs(&base, &tmp, true, false)?;
            fs::rename(&tmp, &out)?;
        } else {
            status(&format!("runtime: cached ({})", out.display()));
        }
        Some(out)
    };
    let app_upper = work.join("app-upper");
    fresh_dir(&app_upper)?;
    for step in &plan.steps {
        match step {
            BuildStep::Copy { from, to } => {
                let dest = app_upper.join(to.trim_start_matches('/'));
                status(&format!("copy: {} -> {to}", from.display()));
                copy_tree(from, &dest)?;
            }
            BuildStep::Run(cmd) => bail!("os = windows: cannot run `{cmd}` on the build host"),
        }
    }
    let app_sqfs = work.join("app.sqfs");
    mksquashfs(&app_upper, &app_sqfs, true, false)?;
    let mut data_files = Vec::new();
    for l in &plan.data_layers {
        let out = work.join(format!("data-{}.sqfs", l.name));
        mksquashfs(&l.source, &out, l.compress, false)?;
        data_files.push((l, out));
    }
    assemble(plan, opts, runtime_sqfs.as_deref(), &app_sqfs, &data_files)?;
    Ok(0)
}

type DataFile<'a> = (&'a crate::model::DataLayer, PathBuf);

fn blake3_file(p: &Path) -> Result<(String, u64)> {
    let mut f = fs::File::open(p)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n_total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        n_total += n as u64;
    }
    Ok((h.finalize().to_hex().to_string(), n_total))
}

fn append_aligned(out: &mut fs::File, src: &Path) -> Result<u64> {
    let pos = out.stream_position()?;
    let aligned = pos.div_ceil(4096) * 4096;
    if aligned > pos {
        out.write_all(&vec![0u8; (aligned - pos) as usize])?;
    }
    let mut f = fs::File::open(src)?;
    std::io::copy(&mut f, out)?;
    Ok(aligned)
}

fn assemble(
    plan: &Plan,
    opts: &Options,
    runtime_sqfs: Option<&Path>,
    app_sqfs: &Path,
    data: &[DataFile<'_>],
) -> Result<()> {
    let out_path = if plan.is_windows()
        && !opts
            .out
            .extension()
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false)
    {
        // Windows runs a file by its extension: `name.cell` becomes
        // `name.cell.exe`.
        let mut s = opts.out.as_os_str().to_owned();
        s.push(".exe");
        PathBuf::from(s)
    } else {
        opts.out.clone()
    };
    status(&format!("assemble: {}", out_path.display()));
    let launcher = fs::read(&opts.launcher)
        .with_context(|| format!("read launcher {}", opts.launcher.display()))?;
    if plan.is_windows() != launcher.starts_with(b"MZ") {
        bail!(
            "launcher {} is not a {} launcher (pass --launcher or set {})",
            opts.launcher.display(),
            if plan.is_windows() {
                "Windows"
            } else {
                "Linux"
            },
            if plan.is_windows() {
                "CELL_LAUNCHER_WINDOWS"
            } else {
                "CELL_LAUNCHER"
            }
        );
    }
    let tmp_out = out_path.with_extension("cell.partial");
    let mut out = fs::File::create(&tmp_out)?;
    out.write_all(&launcher)?;
    let sidecar_dir = format::sidecar_dir_for(&out_path);
    let out_dir = out_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut layers = Vec::new();
    let mut place = |out: &mut fs::File,
                     name: &str,
                     role: LayerRole,
                     mount: &str,
                     src: &Path,
                     kind: PlacementKind|
     -> Result<()> {
        let (hash, size) = blake3_file(src)?;
        let (placement, offset) = match kind {
            PlacementKind::Sidecar => {
                fs::create_dir_all(&sidecar_dir)?;
                let file = format!("{name}.layer");
                fs::copy(src, sidecar_dir.join(&file))?;
                (Placement::Sidecar { file }, 0)
            }
            PlacementKind::Referenced => {
                // Content-addressed and shared: a second cell in the same
                // directory with the same layer bytes reuses the file.
                let file = format::referenced_file_name(name, &hash);
                let dest = out_dir.join(&file);
                if dest.is_file() && fs::metadata(&dest)?.len() == size {
                    status(&format!(
                        "layer {name}: {file} already beside the output, shared"
                    ));
                } else {
                    let tmp = out_dir.join(format!("{file}.partial"));
                    fs::copy(src, &tmp)?;
                    fs::rename(&tmp, &dest)?;
                    status(&format!(
                        "layer {name}: written as {file} beside the output"
                    ));
                }
                (Placement::Referenced { file }, 0)
            }
            PlacementKind::Embedded => (Placement::Embedded, append_aligned(out, src)?),
        };
        layers.push(LayerMeta {
            name: name.to_string(),
            role,
            mount: mount.to_string(),
            placement,
            offset,
            size,
            blake3: hash,
        });
        Ok(())
    };
    if let Some(runtime_sqfs) = runtime_sqfs {
        place(
            &mut out,
            "runtime",
            LayerRole::Runtime,
            "/",
            runtime_sqfs,
            plan.runtime.placement,
        )?;
    }
    place(
        &mut out,
        "app",
        LayerRole::App,
        "/",
        app_sqfs,
        PlacementKind::Embedded,
    )?;
    for (l, p) in data {
        place(&mut out, &l.name, LayerRole::Data, &l.mount, p, l.placement)?;
    }
    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        app: format::AppInfo {
            id: plan.id.clone(),
            name: plan.name.clone(),
            version: plan.version.clone(),
            arch: plan.arch.clone(),
            os: plan.os.clone(),
        },
        entry: plan.entry.clone(),
        isolation: plan.isolation.clone(),
        integration: {
            let mut i = plan.integration.clone();
            i.desktop = plan.desktop.clone();
            i.steam = plan.steam.clone();
            i
        },
        layers,
        registry: plan.registry.clone(),
        wine: plan.wine.clone(),
    };
    let cbor = manifest.to_cbor()?;
    let manifest_off = out.stream_position()?;
    out.write_all(&cbor)?;
    let trailer = Trailer {
        version: FORMAT_VERSION,
        flags: 0,
        manifest_off,
        manifest_len: cbor.len() as u64,
        launcher_len: launcher.len() as u64,
    };
    out.write_all(&trailer.to_bytes())?;
    let total = out.stream_position()?;
    drop(out);
    fs::set_permissions(&tmp_out, fs::Permissions::from_mode(0o755))?;
    fs::rename(&tmp_out, &out_path)?;
    let _ = TRAILER_LEN;
    status(&format!(
        "done: {} ({} MiB)",
        out_path.display(),
        total / (1024 * 1024)
    ));
    Ok(())
}

/// Print the manifest of an existing `.cell`.
pub fn inspect(path: &Path) -> Result<()> {
    let cell = format::CellFile::open(path)?;
    let m = &cell.manifest;
    println!(
        "{} {} ({}, {} {})",
        m.app.name, m.app.version, m.app.id, m.app.os, m.app.arch
    );
    println!("launcher: {} bytes", cell.trailer.launcher_len);
    for l in &m.layers {
        println!(
            "layer {:<12} {:?} mount={} offset={} size={} {:?}",
            l.name, l.role, l.mount, l.offset, l.size, l.placement
        );
    }
    println!("entry: {:?}", m.entry.exec);
    println!("isolation: {:?}", m.isolation);
    for line in cell_core::registry::describe(&m.registry) {
        println!("registry: {line}");
    }
    if let Some(w) = &m.wine {
        println!("wine: runs Windows cells through {:?}", w.command);
    }
    println!(
        "integration: display={:?} audio={:?} input={:?} dbus={:?} gpu={:?}",
        m.integration.display,
        m.integration.audio,
        m.integration.input,
        m.integration.dbus,
        m.integration.gpu
    );
    Ok(())
}
