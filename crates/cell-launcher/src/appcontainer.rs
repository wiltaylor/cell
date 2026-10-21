//! The Windows process sandbox: an AppContainer.
//!
//! An AppContainer token has its own SID and a short list of capability
//! SIDs. Securable objects that grant nothing to that SID or to
//! `ALL APPLICATION PACKAGES` are invisible to the process, the token is
//! low integrity, and without the network capabilities every socket call
//! fails. No administrator rights are needed to create the profile or to
//! spawn into it.
//!
//! Cell's grants map onto it like this: `network = true` adds the
//! `internetClient`, `internetClientServer` and
//! `privateNetworkClientServer` capabilities; a `bind` adds an ACE for the
//! container SID on the host path (`guest` has no meaning without a mount
//! namespace); the app's state directory and the mount point get ACEs so
//! the app can reach its own files.

use std::ffi::c_void;
use std::os::windows::io::FromRawHandle;
use std::path::Path;

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, SetHandleInformation, GENERIC_ALL, GENERIC_EXECUTE,
    GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SetEntriesInAclW,
    SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, GRANT_ACCESS, SDDL_REVISION_1, SE_FILE_OBJECT,
    TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, FreeSid, WinCapabilityInternetClientServerSid,
    WinCapabilityInternetClientSid, WinCapabilityPrivateNetworkClientServerSid, ACL,
    DACL_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
    SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, WELL_KNOWN_SID_TYPE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// `SE_GROUP_ENABLED`: the capability is active in the token.
const SE_GROUP_ENABLED: u32 = 0x4;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A SID owned by this process.
pub struct Sid {
    ptr: PSID,
    /// `FreeSid` for profile SIDs, plain `Vec` memory for well-known ones.
    from_api: bool,
    _buf: Vec<u8>,
}

impl Drop for Sid {
    fn drop(&mut self) {
        if self.from_api && !self.ptr.is_null() {
            // SAFETY: the SID came from the AppContainer API.
            unsafe { FreeSid(self.ptr) };
        }
    }
}

/// The AppContainer profile name for an app id: the id, limited to the
/// characters and the length Windows accepts.
pub fn profile_name(app_id: &str) -> String {
    let cleaned: String = app_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = format!("cell.{cleaned}");
    if name.len() <= 64 {
        return name;
    }
    let h = blake3::hash(app_id.as_bytes()).to_hex();
    format!("cell.{}.{}", &cleaned[..40], &h[..16])
}

/// Create the profile if needed and return its SID.
pub fn profile_sid(app_id: &str, display: &str) -> Result<Sid> {
    let name = wide(&profile_name(app_id));
    let disp = wide(display);
    let desc = wide("Cell portable application");
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: valid wide strings and an out pointer.
    let hr = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            disp.as_ptr(),
            desc.as_ptr(),
            std::ptr::null(),
            0,
            &mut sid,
        )
    };
    const ALREADY_EXISTS: i32 = 0x800700B7_u32 as i32;
    if hr == ALREADY_EXISTS {
        let hr = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if hr < 0 {
            bail!(
                "DeriveAppContainerSidFromAppContainerName: 0x{:08x}",
                hr as u32
            );
        }
    } else if hr < 0 {
        bail!("CreateAppContainerProfile: 0x{:08x}", hr as u32);
    }
    Ok(Sid {
        ptr: sid,
        from_api: true,
        _buf: Vec::new(),
    })
}

fn well_known(kind: WELL_KNOWN_SID_TYPE) -> Result<Sid> {
    let mut buf = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut len = buf.len() as u32;
    // SAFETY: a sized buffer.
    if unsafe {
        CreateWellKnownSid(
            kind,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as PSID,
            &mut len,
        )
    } == 0
    {
        bail!("CreateWellKnownSid({kind}) failed: {}", unsafe {
            GetLastError()
        });
    }
    let ptr = buf.as_mut_ptr() as PSID;
    Ok(Sid {
        ptr,
        from_api: false,
        _buf: buf,
    })
}

/// The capability SIDs for the manifest's grants.
pub fn capabilities(network: bool) -> Result<Vec<Sid>> {
    let mut out = Vec::new();
    if network {
        for k in [
            WinCapabilityInternetClientSid,
            WinCapabilityInternetClientServerSid,
            WinCapabilityPrivateNetworkClientServerSid,
        ] {
            out.push(well_known(k)?);
        }
    }
    Ok(out)
}

/// Add an inheritable ACE for `sid` on `path`. `full` grants everything,
/// otherwise read and execute.
pub fn grant(path: &Path, sid: &Sid, full: bool) -> Result<()> {
    let name = wide(&path.to_string_lossy());
    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid pointers; the SD and the new ACL are freed below.
    unsafe {
        let rc = GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_dacl,
            std::ptr::null_mut(),
            &mut sd,
        );
        if rc != 0 {
            bail!("GetNamedSecurityInfo({}): {rc}", path.display());
        }
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: if full {
                GENERIC_ALL
            } else {
                GENERIC_READ | GENERIC_EXECUTE
            },
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: 3, // SUB_CONTAINERS_AND_OBJECTS_INHERIT
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.ptr as *mut u16,
            },
        };
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let rc = SetEntriesInAclW(1, &ea, old_dacl, &mut new_dacl);
        if rc != 0 {
            LocalFree(sd as _);
            bail!("SetEntriesInAcl({}): {rc}", path.display());
        }
        let rc = SetNamedSecurityInfoW(
            name.as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null_mut(),
        );
        LocalFree(new_dacl as _);
        LocalFree(sd as _);
        if rc != 0 {
            bail!("SetNamedSecurityInfo({}): {rc}", path.display());
        }
    }
    Ok(())
}

/// Quote one argument the way the Windows C runtime parses it.
fn push_arg(out: &mut Vec<u16>, arg: &str) {
    let quoted = arg.is_empty() || arg.chars().any(|c| c == ' ' || c == '\t' || c == '"');
    if quoted {
        out.push(b'"' as u16);
    }
    let mut backslashes = 0;
    for c in arg.encode_utf16() {
        if c == b'\\' as u16 {
            backslashes += 1;
        } else {
            if c == b'"' as u16 {
                out.extend(std::iter::repeat_n(b'\\' as u16, backslashes + 1));
            }
            backslashes = 0;
        }
        out.push(c);
    }
    if quoted {
        out.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
        out.push(b'"' as u16);
    }
}

fn env_block(vars: &[(String, String)]) -> Vec<u16> {
    let mut all: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !vars.iter().any(|(vk, _)| vk.eq_ignore_ascii_case(k)))
        .collect();
    all.extend(vars.iter().cloned());
    all.sort_by_key(|(k, _)| k.to_uppercase());
    let mut out = Vec::new();
    for (k, v) in all {
        out.extend(format!("{k}={v}").encode_utf16());
        out.push(0);
    }
    out.push(0);
    out
}

/// A pipe whose far end the container may use. Handles a child inherits
/// are access-checked against its AppContainer token, so the agent's or
/// the terminal's own stdio handles never arrive; these pipes do, and the
/// launcher copies them to its own stdout and stderr.
struct Pipe {
    ours: HANDLE,
    theirs: HANDLE,
}

fn container_pipe(child_reads: bool) -> Result<Pipe> {
    let sddl = wide("D:(A;;GA;;;WD)(A;;GA;;;AC)");
    let mut sd: *mut c_void = std::ptr::null_mut();
    // SAFETY: a valid SDDL string; the descriptor is freed below.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            bail!("cannot build the pipe security descriptor");
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 1,
        };
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        let ok = CreatePipe(&mut read, &mut write, &sa, 0);
        LocalFree(sd as _);
        if ok == 0 {
            bail!("CreatePipe: {}", GetLastError());
        }
        let (ours, theirs) = if child_reads {
            (write, read)
        } else {
            (read, write)
        };
        SetHandleInformation(ours, HANDLE_FLAG_INHERIT, 0);
        Ok(Pipe { ours, theirs })
    }
}

/// Run `program` with `args` inside the container and wait for it.
pub fn run(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    container: &Sid,
    caps: &[Sid],
) -> Result<i32> {
    let mut cmdline = Vec::new();
    push_arg(&mut cmdline, &program.to_string_lossy());
    for a in args {
        cmdline.push(b' ' as u16);
        push_arg(&mut cmdline, a);
    }
    cmdline.push(0);
    let app = wide(&program.to_string_lossy());
    let dir = wide(&cwd.to_string_lossy());
    let mut block = env_block(env);
    let mut cap_attrs: Vec<SID_AND_ATTRIBUTES> = caps
        .iter()
        .map(|s| SID_AND_ATTRIBUTES {
            Sid: s.ptr,
            Attributes: SE_GROUP_ENABLED,
        })
        .collect();
    let mut sc = SECURITY_CAPABILITIES {
        AppContainerSid: container.ptr,
        Capabilities: if cap_attrs.is_empty() {
            std::ptr::null_mut()
        } else {
            cap_attrs.as_mut_ptr()
        },
        CapabilityCount: cap_attrs.len() as u32,
        Reserved: 0,
    };
    // SAFETY: documented use of the extended startup info.
    unsafe {
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut list_buf = vec![0u8; size];
        let list = list_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            bail!("InitializeProcThreadAttributeList: {}", GetLastError());
        }
        if UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            &mut sc as *mut _ as *mut c_void,
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
        {
            DeleteProcThreadAttributeList(list);
            bail!("UpdateProcThreadAttribute: {}", GetLastError());
        }
        let stdin = container_pipe(true)?;
        let stdout = container_pipe(false)?;
        let stderr = container_pipe(false)?;
        crate::windows::trace("appcontainer: pipes ready");
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = stdin.theirs;
        si.StartupInfo.hStdOutput = stdout.theirs;
        si.StartupInfo.hStdError = stderr.theirs;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            app.as_ptr(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            block.as_mut_ptr() as *mut c_void,
            dir.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        );
        let err = GetLastError();
        crate::windows::trace(&format!(
            "appcontainer: CreateProcessW ok={ok} err={err} pid={}",
            pi.dwProcessId
        ));
        DeleteProcThreadAttributeList(list);
        // The child's ends stay with the child only.
        for h in [stdin.theirs, stdout.theirs, stderr.theirs] {
            CloseHandle(h);
        }
        // Nothing feeds the child's stdin: it reads end-of-file.
        CloseHandle(stdin.ours);
        if ok == 0 {
            CloseHandle(stdout.ours);
            CloseHandle(stderr.ours);
            bail!(
                "CreateProcess({}) in AppContainer: error {err}",
                program.display()
            );
        }
        CloseHandle(pi.hThread);
        crate::windows::trace("appcontainer: handles closed, waiting");
        let debug = std::env::var("CELL_DEBUG").is_ok();
        if debug {
            eprintln!(
                "cell: appcontainer: started pid {} ({})",
                pi.dwProcessId,
                program.display()
            );
        }
        let pump_out = pump(stdout.ours, false);
        let pump_err = pump(stderr.ours, true);
        WaitForSingleObject(pi.hProcess, INFINITE);
        let mut code: u32 = 0;
        GetExitCodeProcess(pi.hProcess, &mut code);
        crate::windows::trace(&format!("appcontainer: wait done, code 0x{code:08x}"));
        CloseHandle(pi.hProcess);
        if debug {
            eprintln!(
                "cell: appcontainer: pid {} exited with 0x{code:08x}",
                pi.dwProcessId
            );
        }
        let _ = pump_out.join();
        let _ = pump_err.join();
        Ok(code as i32)
    }
}

/// Copy a pipe to this process's stdout or stderr until the far end
/// closes, which happens when the last process holding it exits.
fn pump(h: HANDLE, to_stderr: bool) -> std::thread::JoinHandle<()> {
    // SAFETY: `h` is an owned, open pipe handle.
    let mut from = unsafe { std::fs::File::from_raw_handle(h as _) };
    std::thread::spawn(move || {
        if to_stderr {
            let _ = std::io::copy(&mut from, &mut std::io::stderr());
        } else {
            let _ = std::io::copy(&mut from, &mut std::io::stdout());
        }
    })
}
