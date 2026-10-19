//! A build runs as root inside a user namespace mapped with the caller's
//! subordinate uid and gid ranges (`/etc/subuid`, `/etc/subgid`), the way
//! rootless podman does. Files the package manager chowns to other users
//! land on subordinate ids on the host.

use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;

use anyhow::{bail, Context, Result};

fn pipe() -> Result<(std::fs::File, std::fs::File)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        bail!("pipe: {}", std::io::Error::last_os_error());
    }
    Ok(unsafe { (std::fs::File::from_raw_fd(fds[0]), std::fs::File::from_raw_fd(fds[1])) })
}

fn sub_range(file: &str, user: &str, uid: u32) -> Result<(u32, u32)> {
    let text = std::fs::read_to_string(file).with_context(|| format!("read {file}"))?;
    for line in text.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() == 3 && (parts[0] == user || parts[0] == uid.to_string()) {
            return Ok((parts[1].parse()?, parts[2].parse()?));
        }
    }
    bail!("no entry for {user} in {file}; add one with `usermod --add-subuids 100000-165535 --add-subgids 100000-165535 {user}`")
}

/// How the build namespace maps ids.
enum Mapping {
    /// `newuidmap` maps root to the caller and 1..n to the subordinate
    /// range, so a package manager can chown files to other users.
    Subordinate {
        uid: u32,
        gid: u32,
        sub_uid: (u32, u32),
        sub_gid: (u32, u32),
    },
    /// One id only: the caller becomes root and nothing else is mapped.
    /// A chown to another user fails and the file stays root-owned. That
    /// is fine for a Cell image, because the launcher reports every file
    /// as owned by the user anyway. This is the mode inside a sandbox, for
    /// example when the build tool itself runs from a `.cell`.
    FakeRoot { uid: u32, gid: u32 },
}

fn choose_mapping() -> Mapping {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let user = std::env::var("USER").unwrap_or_else(|_| uid.to_string());
    let has_newuidmap = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| std::path::Path::new(d).join("newuidmap").is_file());
    let inside_cell = std::env::var("CELL_APP_ID").is_ok();
    if has_newuidmap && !inside_cell {
        if let (Ok(su), Ok(sg)) = (
            sub_range("/etc/subuid", &user, uid),
            sub_range("/etc/subgid", &user, uid),
        ) {
            return Mapping::Subordinate {
                uid,
                gid,
                sub_uid: su,
                sub_gid: sg,
            };
        }
    }
    eprintln!(
        "cell: build namespace maps one id only (no usable newuidmap and /etc/subuid); \
         files the package manager gives to other users stay root-owned"
    );
    Mapping::FakeRoot { uid, gid }
}

/// Run `f` as root in a new user and mount namespace. Returns its exit code.
pub fn with_mapped_root<F: FnOnce() -> Result<i32>>(f: F) -> Result<i32> {
    let mapping = choose_mapping();
    let (mut ready_r, mut ready_w) = pipe()?;
    let (mut go_r, mut go_w) = pipe()?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!("fork: {}", std::io::Error::last_os_error());
    }
    if pid == 0 {
        drop(ready_r);
        drop(go_w);
        let code = (|| -> Result<i32> {
            cell_core::sandbox::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS)
                .context("user namespace for the build")?;
            if let Mapping::FakeRoot { uid, gid } = mapping {
                std::fs::write("/proc/self/setgroups", "deny").context("setgroups")?;
                std::fs::write("/proc/self/uid_map", format!("0 {uid} 1\n")).context("uid_map")?;
                std::fs::write("/proc/self/gid_map", format!("0 {gid} 1\n")).context("gid_map")?;
            }
            ready_w.write_all(b"1")?;
            let mut b = [0u8; 1];
            go_r.read_exact(&mut b).context("wait for uid map")?;
            if unsafe { libc::getuid() } != 0 {
                bail!("uid map did not make us root in the namespace");
            }
            cell_core::sandbox::mount(
                "none",
                std::path::Path::new("/"),
                None,
                libc::MS_REC | libc::MS_PRIVATE,
                None,
            )?;
            f()
        })();
        let code = match code {
            Ok(c) => c,
            Err(e) => {
                eprintln!("cell: build failed: {e:#}");
                1
            }
        };
        unsafe { libc::_exit(code) };
    }
    drop(ready_w);
    drop(go_r);
    let mut b = [0u8; 1];
    ready_r.read_exact(&mut b).context("build child did not start")?;
    if let Mapping::Subordinate {
        uid,
        gid,
        sub_uid,
        sub_gid,
    } = mapping
    {
        let pid_s = pid.to_string();
        let run = |cmd: &str, args: Vec<String>| -> Result<()> {
            let st = std::process::Command::new(cmd)
                .args(&args)
                .status()
                .with_context(|| format!("run {cmd}"))?;
            if !st.success() {
                bail!("{cmd} failed: {st}");
            }
            Ok(())
        };
        run(
            "newuidmap",
            vec![
                pid_s.clone(),
                "0".into(),
                uid.to_string(),
                "1".into(),
                "1".into(),
                sub_uid.0.to_string(),
                sub_uid.1.to_string(),
            ],
        )?;
        run(
            "newgidmap",
            vec![
                pid_s,
                "0".into(),
                gid.to_string(),
                "1".into(),
                "1".into(),
                sub_gid.0.to_string(),
                sub_gid.1.to_string(),
            ],
        )?;
    }
    go_w.write_all(b"1")?;
    drop(go_w);
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    Ok(if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        1
    })
}
