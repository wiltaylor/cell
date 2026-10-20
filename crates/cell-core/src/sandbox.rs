//! The sandbox: user namespace, mount namespace, layer mounts, overlay,
//! grants, `pivot_root` and the final `exec`. Every step is a syscall from
//! this process. No `bwrap`, `fusermount` or setuid helper is involved.
//!
//! The caller enters the user namespace first (see [`enter_userns_same_uid`]
//! or the `newuidmap` path in the build tool), then calls [`run`].

use std::ffi::{CString, OsStr};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

use anyhow::{bail, Context, Result};

use crate::format::BindMode;

pub enum LayerSource {
    /// A squashfs image at `base` inside `file`.
    Squashfs {
        file: fs::File,
        base: u64,
        name: String,
    },
    /// A plain directory, bound read-only.
    Dir(PathBuf),
}

pub struct Layer {
    pub source: LayerSource,
    /// `/` for the root stack, otherwise a mount path inside the root.
    pub mount: String,
}

#[derive(Debug, Clone)]
pub struct BindSpec {
    pub host: PathBuf,
    pub guest: String,
    pub mode: BindMode,
    pub optional: bool,
}

#[derive(Default)]
pub struct Spec {
    pub layers: Vec<Layer>,
    /// Overlay upper and work directories. `None` means a throwaway tmpfs.
    pub upper: Option<(PathBuf, PathBuf)>,
    pub binds: Vec<BindSpec>,
    /// Guest paths that get a fresh tmpfs.
    pub tmpfs: Vec<String>,
    /// Files created in the sandbox (guest path, bytes, mode). The path's
    /// parent must be on a tmpfs the sandbox owns, or an existing file on
    /// the root that gets a file bind on top.
    pub files: Vec<(String, Vec<u8>, u32)>,
    /// Symlinks created in the sandbox (guest link path, target).
    pub symlinks: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
    pub exec: Vec<String>,
    pub cwd: Option<String>,
    pub new_pid: bool,
    pub new_net: bool,
    pub new_ipc: bool,
    /// Commands run inside the new root before `exec`, in order. A failure
    /// is reported but does not stop the launch.
    pub pre_exec: Vec<Vec<String>>,
    /// File binds done after `pivot_root` (source, destination), both inside
    /// the root. Used for the regenerated `ld.so.cache`.
    pub late_file_binds: Vec<(String, String)>,
    /// Keep the host `/proc` and pid namespace.
    pub host_proc: bool,
    /// Report every file in a squashfs layer as owned by this (uid, gid).
    /// The launcher sets `Some((0, 0))`: the outer root, which the inner
    /// namespace shows as the real user. The build tool leaves it `None` so
    /// the real ownership reaches `mksquashfs`.
    pub squash_ids: Option<(u32, u32)>,
    /// Before `exec`, enter an inner user namespace that maps the current
    /// uid 0 (the outer root) to this (uid, gid). The application then runs
    /// as the real user while every mount was made with root powers.
    pub drop_to: Option<(u32, u32)>,
}

// ---- raw syscall helpers ---------------------------------------------------

fn cstr(p: impl AsRef<OsStr>) -> CString {
    CString::new(p.as_ref().as_bytes()).expect("path with NUL")
}

pub fn mount(
    src: &str,
    target: &Path,
    fstype: Option<&str>,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<()> {
    let s = cstr(src);
    let t = cstr(target);
    let f = fstype.map(cstr);
    let d = data.map(cstr);
    let r = unsafe {
        libc::mount(
            s.as_ptr(),
            t.as_ptr(),
            f.as_ref().map_or(std::ptr::null(), |x| x.as_ptr()),
            flags,
            d.as_ref()
                .map_or(std::ptr::null(), |x| x.as_ptr() as *const libc::c_void),
        )
    };
    if r != 0 {
        let e = std::io::Error::last_os_error();
        bail!(
            "mount {} -> {} (type {:?}, data {:?}): {e}",
            src,
            target.display(),
            fstype,
            data
        );
    }
    Ok(())
}

#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

const MOUNT_ATTR_RDONLY: u64 = 0x1;
const AT_RECURSIVE: libc::c_uint = 0x8000;
const SYS_MOUNT_SETATTR: libc::c_long = 442;

/// Make a mount tree read-only, recursively (kernel 5.12+). Falls back to a
/// plain remount of the top mount on older kernels.
pub fn make_readonly(target: &Path) -> Result<()> {
    let t = cstr(target);
    let attr = MountAttr {
        attr_set: MOUNT_ATTR_RDONLY,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    let r = unsafe {
        libc::syscall(
            SYS_MOUNT_SETATTR,
            libc::AT_FDCWD,
            t.as_ptr(),
            AT_RECURSIVE,
            &attr as *const MountAttr,
            std::mem::size_of::<MountAttr>(),
        )
    };
    if r == 0 {
        return Ok(());
    }
    mount(
        "none",
        target,
        None,
        libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
        None,
    )
}

pub fn bind(src: &Path, dst: &Path, mode: BindMode) -> Result<()> {
    let s = src.to_string_lossy();
    mount(&s, dst, None, libc::MS_BIND | libc::MS_REC, None)?;
    match mode {
        BindMode::Ro => make_readonly(dst)?,
        BindMode::Rw | BindMode::Dev => {}
    }
    Ok(())
}

fn ensure_target(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::metadata(src).with_context(|| format!("stat {}", src.display()))?;
    if meta.is_dir() {
        fs::create_dir_all(dst).with_context(|| format!("mkdir {}", dst.display()))?;
    } else {
        if let Some(p) = dst.parent() {
            fs::create_dir_all(p)?;
        }
        if !dst.exists() {
            fs::File::create(dst).with_context(|| format!("create {}", dst.display()))?;
        }
    }
    Ok(())
}

pub fn unshare(flags: libc::c_int) -> Result<()> {
    if unsafe { libc::unshare(flags) } != 0 {
        bail!("unshare({flags:#x}): {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// Enter a new user namespace in which the current uid and gid become root,
/// plus a new mount namespace (and optional net and ipc). The process must
/// be single-threaded. Pair with [`Spec::drop_to`] so the application runs
/// as the real user again.
pub fn enter_userns_root(new_net: bool, new_ipc: bool) -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let mut flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNS;
    if new_net {
        flags |= libc::CLONE_NEWNET;
    }
    if new_ipc {
        flags |= libc::CLONE_NEWIPC;
    }
    unshare(flags).context(
        "cannot create a user namespace. The host must allow unprivileged user namespaces \
         (kernel.unprivileged_userns_clone=1, user.max_user_namespaces>0, and on Ubuntu \
         kernel.apparmor_restrict_unprivileged_userns=0)",
    )?;
    fs::write("/proc/self/setgroups", "deny").context("write setgroups")?;
    fs::write("/proc/self/uid_map", format!("0 {uid} 1\n")).context("write uid_map")?;
    fs::write("/proc/self/gid_map", format!("0 {gid} 1\n")).context("write gid_map")?;
    if new_net {
        if let Err(e) = loopback_up() {
            eprintln!("cell: warning: cannot bring up loopback: {e}");
        }
    }
    Ok(())
}

/// Enter an inner user namespace that maps the current uid 0 to `uid`.
fn drop_to_user(uid: u32, gid: u32) -> Result<()> {
    unshare(libc::CLONE_NEWUSER).context("inner user namespace")?;
    fs::write("/proc/self/setgroups", "deny").context("inner setgroups")?;
    fs::write("/proc/self/uid_map", format!("{uid} 0 1\n")).context("inner uid_map")?;
    fs::write("/proc/self/gid_map", format!("{gid} 0 1\n")).context("inner gid_map")?;
    Ok(())
}

/// Bring `lo` up in the current network namespace.
pub fn loopback_up() -> Result<()> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        bail!("socket: {}", std::io::Error::last_os_error());
    }
    let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
    for (i, b) in b"lo".iter().enumerate() {
        req.ifr_name[i] = *b as libc::c_char;
    }
    unsafe {
        req.ifr_ifru.ifru_flags = (libc::IFF_UP | libc::IFF_RUNNING) as i16;
        let r = libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &req);
        libc::close(sock);
        if r != 0 {
            bail!("SIOCSIFFLAGS: {}", std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn pivot_root(new_root: &Path, put_old: &Path) -> Result<()> {
    let n = cstr(new_root);
    let o = cstr(put_old);
    let r = unsafe { libc::syscall(libc::SYS_pivot_root, n.as_ptr(), o.as_ptr()) };
    if r != 0 {
        bail!("pivot_root: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

fn chdir(p: &Path) -> Result<()> {
    let c = cstr(p);
    if unsafe { libc::chdir(c.as_ptr()) } != 0 {
        bail!("chdir {}: {}", p.display(), std::io::Error::last_os_error());
    }
    Ok(())
}

fn umount_detach(p: &Path) -> Result<()> {
    let c = cstr(p);
    if unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) } != 0 {
        bail!(
            "umount {}: {}",
            p.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

fn mkdir_p(p: &Path) -> Result<()> {
    fs::create_dir_all(p).with_context(|| format!("mkdir {}", p.display()))
}

fn guest(root: &Path, g: &str) -> PathBuf {
    root.join(g.trim_start_matches('/'))
}

// ---- the run ---------------------------------------------------------------

static CHILD: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward(sig: libc::c_int) {
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

/// Mount everything under `work` and run `spec.exec`. Returns the child's
/// exit status. Must be called inside a user namespace with a private mount
/// namespace.
pub fn run(spec: &Spec, work: &Path) -> Result<i32> {
    // Nothing we mount may leak to the host.
    mount(
        "none",
        Path::new("/"),
        None,
        libc::MS_REC | libc::MS_PRIVATE,
        None,
    )
    .context("make / private")?;
    mkdir_p(work)?;
    mount("tmpfs", work, Some("tmpfs"), 0, Some("mode=0755")).context("work tmpfs")?;

    // Layers.
    let mut sessions = Vec::new();
    let mut lower_root: Vec<PathBuf> = Vec::new();
    let mut data_mounts: Vec<(PathBuf, String)> = Vec::new();
    for (i, layer) in spec.layers.iter().enumerate() {
        let mp = work.join(format!("layers/{i}"));
        mkdir_p(&mp)?;
        match &layer.source {
            LayerSource::Squashfs { file, base, name } => {
                let f = file.try_clone()?;
                sessions.push(crate::fusefs::mount_layer(
                    f,
                    *base,
                    &mp,
                    name,
                    spec.squash_ids,
                )?);
            }
            LayerSource::Dir(d) => bind(d, &mp, BindMode::Ro)?,
        }
        if layer.mount == "/" {
            lower_root.push(mp);
        } else {
            data_mounts.push((mp, layer.mount.clone()));
        }
    }
    if lower_root.is_empty() {
        bail!("no root layer");
    }

    // Overlay root. Later layers sit on top of earlier ones.
    let root = work.join("root");
    mkdir_p(&root)?;
    let (upper, workdir) = match &spec.upper {
        Some((u, w)) => (u.clone(), w.clone()),
        None => (work.join("upper"), work.join("ovl-work")),
    };
    mkdir_p(&upper)?;
    mkdir_p(&workdir)?;
    let lowers: Vec<String> = lower_root
        .iter()
        .rev()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let data = format!(
        "lowerdir={},upperdir={},workdir={},userxattr",
        lowers.join(":"),
        upper.display(),
        workdir.display()
    );
    mount("overlay", &root, Some("overlay"), 0, Some(&data)).context("mount overlay root")?;

    // Fresh /dev, /tmp, /run; host /sys read-only.
    for d in ["dev", "tmp", "run"] {
        let p = root.join(d);
        mkdir_p(&p)?;
        mount(
            "tmpfs",
            &p,
            Some("tmpfs"),
            libc::MS_NOSUID,
            Some("mode=0755"),
        )?;
    }
    fs::set_permissions(root.join("tmp"), fs::Permissions::from_mode(0o1777))?;
    let sys = root.join("sys");
    if sys.exists() {
        if let Err(e) = bind(Path::new("/sys"), &sys, BindMode::Ro) {
            eprintln!("cell: warning: /sys: {e}");
        }
    }
    for dev in ["null", "zero", "full", "random", "urandom", "tty"] {
        let src = PathBuf::from("/dev").join(dev);
        let dst = root.join("dev").join(dev);
        if src.exists() {
            ensure_target(&src, &dst)?;
            bind(&src, &dst, BindMode::Dev)?;
        }
    }
    let pts = root.join("dev/pts");
    mkdir_p(&pts)?;
    if let Err(e) = mount(
        "devpts",
        &pts,
        Some("devpts"),
        libc::MS_NOSUID | libc::MS_NOEXEC,
        Some("newinstance,ptmxmode=0666,mode=0620"),
    ) {
        eprintln!("cell: warning: devpts: {e}");
    } else {
        let _ = std::os::unix::fs::symlink("pts/ptmx", root.join("dev/ptmx"));
    }
    let shm = root.join("dev/shm");
    mkdir_p(&shm)?;
    mount(
        "tmpfs",
        &shm,
        Some("tmpfs"),
        libc::MS_NOSUID | libc::MS_NODEV,
        Some("mode=1777"),
    )?;
    let _ = std::os::unix::fs::symlink("/proc/self/fd", root.join("dev/fd"));
    let _ = std::os::unix::fs::symlink("/proc/self/fd/0", root.join("dev/stdin"));
    let _ = std::os::unix::fs::symlink("/proc/self/fd/1", root.join("dev/stdout"));
    let _ = std::os::unix::fs::symlink("/proc/self/fd/2", root.join("dev/stderr"));
    mkdir_p(&root.join("proc"))?;

    // Data layers, extra tmpfs, binds, files, symlinks.
    for (mp, at) in &data_mounts {
        let dst = guest(&root, at);
        mkdir_p(&dst)?;
        bind(mp, &dst, BindMode::Ro)?;
    }
    for t in &spec.tmpfs {
        let p = guest(&root, t);
        mkdir_p(&p)?;
        mount(
            "tmpfs",
            &p,
            Some("tmpfs"),
            libc::MS_NOSUID,
            Some("mode=0755"),
        )?;
    }
    for b in &spec.binds {
        if !b.host.exists() {
            if b.optional {
                continue;
            }
            bail!("bind source {} does not exist", b.host.display());
        }
        let dst = guest(&root, &b.guest);
        ensure_target(&b.host, &dst)?;
        bind(&b.host, &dst, b.mode).with_context(|| format!("bind {}", b.guest))?;
    }
    let files_dir = work.join("files");
    mkdir_p(&files_dir)?;
    for (i, (g, bytes, mode)) in spec.files.iter().enumerate() {
        let dst = guest(&root, g);
        let parent = dst.parent().map(Path::to_path_buf).unwrap_or_default();
        if !parent.exists() {
            if is_on_tmpfs_we_own(&parent, &root) {
                mkdir_p(&parent)?;
            } else {
                // Parent is not there in the image: skip quietly.
                continue;
            }
        }
        let staged = files_dir.join(i.to_string());
        fs::write(&staged, bytes)?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(*mode))?;
        if is_on_tmpfs_we_own(&parent, &root) {
            fs::write(&dst, bytes)?;
            fs::set_permissions(&dst, fs::Permissions::from_mode(*mode))?;
        } else {
            if !dst.exists() {
                fs::File::create(&dst)?;
            }
            bind(&staged, &dst, BindMode::Ro)?;
        }
    }
    for (link, target) in &spec.symlinks {
        let p = guest(&root, link);
        if let Some(parent) = p.parent() {
            mkdir_p(parent)?;
        }
        let _ = fs::remove_file(&p);
        std::os::unix::fs::symlink(target, &p)
            .with_context(|| format!("symlink {link} -> {target}"))?;
    }

    // Child: new pid namespace, new mount namespace copy, proc, pivot, then
    // a small init that runs the entry point. `unshare(CLONE_NEWPID)` may
    // only be called once per process, so the child is cloned straight into
    // its pid namespace instead.
    let mut clone_flags: libc::c_long = libc::SIGCHLD as libc::c_long;
    if spec.new_pid && !spec.host_proc {
        clone_flags |= libc::CLONE_NEWPID as libc::c_long;
    }
    let pid = unsafe { libc::syscall(libc::SYS_clone, clone_flags, 0usize, 0usize, 0usize, 0usize) }
        as libc::pid_t;
    if pid < 0 {
        bail!("clone: {}", std::io::Error::last_os_error());
    }
    if pid == 0 {
        let code = match child(spec, &root) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("cell: {e:#}");
                125
            }
        };
        unsafe { libc::_exit(code) };
    }
    CHILD.store(pid, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, forward as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, forward as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, forward as *const () as libc::sighandler_t);
    }
    let mut status = 0;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r == pid {
            break;
        }
        if r < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        bail!("waitpid: {}", std::io::Error::last_os_error());
    }
    // Keep the FUSE sessions until the child is gone, then let the kernel
    // tear the namespaces down with us.
    std::mem::forget(sessions);
    Ok(exit_code(status))
}

/// The shell convention for a `wait` status: the exit code, or 128 plus
/// the signal number.
fn exit_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

fn is_on_tmpfs_we_own(parent: &Path, root: &Path) -> bool {
    let rel = parent.strip_prefix(root).unwrap_or(parent);
    let first = rel
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_string());
    matches!(first.as_deref(), Some("dev") | Some("tmp") | Some("run"))
}

/// Pid 1 of the sandbox. Returns the exit code of the entry point.
fn child(spec: &Spec, root: &Path) -> Result<i32> {
    unshare(libc::CLONE_NEWNS).context("child mount namespace")?;
    mount(
        "none",
        Path::new("/"),
        None,
        libc::MS_REC | libc::MS_PRIVATE,
        None,
    )?;
    let proc_dir = root.join("proc");
    if spec.host_proc {
        bind(Path::new("/proc"), &proc_dir, BindMode::Rw)?;
    } else {
        mount(
            "proc",
            &proc_dir,
            Some("proc"),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            None,
        )
        .context("mount /proc")?;
    }
    chdir(root)?;
    pivot_root(Path::new("."), Path::new("."))?;
    umount_detach(Path::new("."))?;
    chdir(Path::new("/"))?;

    let env: Vec<CString> = spec
        .env
        .iter()
        .map(|(k, v)| CString::new(format!("{k}={v}")).unwrap())
        .collect();

    for cmd in &spec.pre_exec {
        if let Err(e) = run_wait(cmd, &env) {
            eprintln!("cell: warning: {}: {e}", cmd.join(" "));
        }
    }
    for (src, dst) in &spec.late_file_binds {
        let s = Path::new(src);
        let d = Path::new(dst);
        if s.exists() && d.parent().map(|p| p.exists()).unwrap_or(false) {
            if !d.exists() {
                let _ = fs::File::create(d);
            }
            if let Err(e) = bind(s, d, BindMode::Ro) {
                eprintln!("cell: warning: {e}");
            }
        }
    }
    if let Some((uid, gid)) = spec.drop_to {
        drop_to_user(uid, gid)?;
    }
    if let Some(c) = &spec.cwd {
        if let Err(e) = chdir(Path::new(c)) {
            eprintln!("cell: warning: {e}");
        }
    }
    unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        // The sandbox dies with its launcher: pid 1 of the pid namespace
        // gets SIGKILL when the launcher goes away, which takes every
        // other process in the namespace with it.
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
        if libc::getppid() == 1 {
            libc::_exit(0);
        }
    }
    if spec.exec.is_empty() {
        bail!("nothing to run");
    }
    let argv: Vec<CString> = spec
        .exec
        .iter()
        .map(|a| CString::new(a.as_str()).unwrap())
        .collect();
    init(&argv, &env)
}

/// Run the entry point as a child and act as the init of the pid namespace,
/// the way `bwrap` does. Pid 1 reaps every orphan and returns only when no
/// process is left. If the entry point were pid 1 itself, its exit would
/// make the kernel SIGKILL every other process in the namespace at once.
/// Daemons that outlive their client, such as Wine's `wineserver`, would
/// then lose unsaved state (the Wine registry, for one). Signals from the
/// launcher go to the entry point, not to its helpers.
fn init(argv: &[CString], env: &[CString]) -> Result<i32> {
    let entry = unsafe { libc::fork() };
    if entry < 0 {
        bail!("fork: {}", std::io::Error::last_os_error());
    }
    if entry == 0 {
        if let Err(e) = exec(argv, env) {
            eprintln!("cell: {e:#}");
        }
        unsafe { libc::_exit(125) };
    }
    CHILD.store(entry, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, forward as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, forward as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, forward as *const () as libc::sighandler_t);
    }
    let mut code = 0;
    loop {
        let mut status = 0;
        let r = unsafe { libc::wait(&mut status) };
        if r == entry {
            code = exit_code(status);
        } else if r < 0 {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::ECHILD) => break,
                _ => bail!("wait: {}", std::io::Error::last_os_error()),
            }
        }
    }
    Ok(code)
}

fn exec(argv: &[CString], env: &[CString]) -> Result<()> {
    let mut argp: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argp.push(std::ptr::null());
    let mut envp: Vec<*const libc::c_char> = env.iter().map(|a| a.as_ptr()).collect();
    envp.push(std::ptr::null());
    let path = argv[0].to_str().unwrap_or("");
    let resolved = if path.contains('/') {
        Some(PathBuf::from(path))
    } else {
        let path_var = env
            .iter()
            .find_map(|e| e.to_str().ok()?.strip_prefix("PATH=").map(String::from))
            .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_string());
        path_var
            .split(':')
            .map(|d| Path::new(d).join(path))
            .find(|p| p.is_file())
    };
    let Some(resolved) = resolved else {
        bail!("command not found: {path}");
    };
    let r = cstr(&resolved);
    unsafe { libc::execve(r.as_ptr(), argp.as_ptr(), envp.as_ptr()) };
    bail!(
        "exec {}: {}",
        resolved.display(),
        std::io::Error::last_os_error()
    );
}

fn run_wait(cmd: &[String], env: &[CString]) -> Result<()> {
    let argv: Vec<CString> = cmd
        .iter()
        .map(|a| CString::new(a.as_str()).unwrap())
        .collect();
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!("fork: {}", std::io::Error::last_os_error());
    }
    if pid == 0 {
        if let Err(e) = exec(&argv, env) {
            eprintln!("cell: {e}");
        }
        unsafe { libc::_exit(127) };
    }
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        Ok(())
    } else {
        bail!("exit status {}", libc::WEXITSTATUS(status))
    }
}

/// Write `content` to `path`, creating parents.
pub fn write_file(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        mkdir_p(p)?;
    }
    let mut f = fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(content)?;
    Ok(())
}
