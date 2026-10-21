//! `CELL_CMD=setup`: the one-time, elevated step that lets sandboxed
//! cells use WinFsp volumes.
//!
//! WinFsp creates each volume's device object with a fixed security
//! descriptor that grants Everyone read access and nothing to app
//! packages. An AppContainer token must pass a second access check in
//! which only its package SID, its capabilities and `ALL APPLICATION
//! PACKAGES` count, so every open through the device fails before the
//! file system sees it. `IoCreateDeviceSecure` lets device class security
//! in the registry override that default, and a volume device is created
//! on every mount, so the change takes effect at the next launch without
//! a reboot. The key lives under `HKLM\SYSTEM\...\Control\Class\<guid>\
//! Properties`, which only SYSTEM may write; an administrator writes it
//! through `SeRestorePrivilege` and `REG_OPTION_BACKUP_RESTORE`.

use std::ffi::c_void;

use anyhow::{bail, Context, Result};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE, LUID};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetKernelObjectSecurity, GetSecurityDescriptorLength,
    GetTokenInformation, LookupPrivilegeValueW, TokenElevation, DACL_SECURITY_INFORMATION,
    LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE,
    REG_BINARY, REG_OPTION_BACKUP_RESTORE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// WinFsp's virtual disk device class, `FspFsvrtDeviceClassGuid`.
const CLASS_KEY: &str =
    "SYSTEM\\CurrentControlSet\\Control\\Class\\{b48171c3-dd50-4852-83a3-344c50d93b17}\\Properties";

/// WinFsp's default, plus full access for app packages.
const DEVICE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGX;;;WD)(A;;GA;;;AC)";

/// The argument a self-elevated launcher gets instead of `CELL_CMD`.
pub const SETUP_ARG: &str = "--cell-setup";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn sddl_bytes(sddl: &str) -> Result<Vec<u8>> {
    let w = wide(sddl);
    let mut sd: *mut c_void = std::ptr::null_mut();
    // SAFETY: a valid SDDL string; the descriptor is freed below.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            bail!("cannot parse the security descriptor");
        }
        let n = GetSecurityDescriptorLength(sd) as usize;
        let bytes = std::slice::from_raw_parts(sd as *const u8, n).to_vec();
        LocalFree(sd as _);
        Ok(bytes)
    }
}

pub fn is_elevated() -> bool {
    // SAFETY: documented token query.
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let mut e: TOKEN_ELEVATION = std::mem::zeroed();
        let mut n = 0u32;
        let ok = GetTokenInformation(
            tok,
            TokenElevation,
            &mut e as *mut _ as *mut c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut n,
        );
        CloseHandle(tok);
        ok != 0 && e.TokenIsElevated != 0
    }
}

fn enable_privilege(name: &str) -> Result<()> {
    // SAFETY: documented privilege adjustment on our own token.
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut tok,
        ) == 0
        {
            bail!("OpenProcessToken: {}", GetLastError());
        }
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        let w = wide(name);
        if LookupPrivilegeValueW(std::ptr::null(), w.as_ptr(), &mut luid) == 0 {
            CloseHandle(tok);
            bail!("LookupPrivilegeValue({name}): {}", GetLastError());
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let ok = AdjustTokenPrivileges(tok, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
        let err = GetLastError();
        CloseHandle(tok);
        if ok == 0 || err != 0 {
            bail!("{name} is not held by this token (run as administrator)");
        }
    }
    Ok(())
}

/// Write the class security. Needs an elevated token.
pub fn apply() -> Result<()> {
    if !is_elevated() {
        bail!("setup needs an administrator token");
    }
    enable_privilege("SeRestorePrivilege").context("enable SeRestorePrivilege")?;
    enable_privilege("SeBackupPrivilege").ok();
    let bytes = sddl_bytes(DEVICE_SDDL)?;
    let key = wide(CLASS_KEY);
    let value = wide("Security");
    // SAFETY: valid strings and buffers; the key is closed below.
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_BACKUP_RESTORE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        );
        if rc != 0 {
            bail!("cannot open HKLM\\{CLASS_KEY}: error {rc}");
        }
        let rc = RegSetValueExW(
            hkey,
            value.as_ptr(),
            0,
            REG_BINARY,
            bytes.as_ptr(),
            bytes.len() as u32,
        );
        RegCloseKey(hkey);
        if rc != 0 {
            bail!("cannot write Security under HKLM\\{CLASS_KEY}: error {rc}");
        }
    }
    Ok(())
}

/// Relaunch this executable elevated to run the setup, through the UAC
/// prompt. Returns once the prompt has been answered.
pub fn elevate_and_apply() -> Result<()> {
    let exe = std::env::current_exe()?;
    let w_exe = wide(&exe.to_string_lossy());
    let w_arg = wide(SETUP_ARG);
    let verb = wide("runas");
    // SAFETY: valid NUL-terminated strings.
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            w_exe.as_ptr(),
            w_arg.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (r as isize) <= 32 {
        bail!(
            "elevation was refused or failed (ShellExecute returned {})",
            r as isize
        );
    }
    Ok(())
}

/// The security descriptor of the volume device object, as SDDL, read
/// through a volume handle. `None` when the device cannot be opened.
pub fn device_sddl(volume_name: &str) -> Option<String> {
    let path = wide(&format!("\\\\?\\GLOBALROOT{volume_name}"));
    // SAFETY: a valid path; handles and descriptors are released below.
    unsafe {
        let h = CreateFileW(
            path.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if h as isize == -1 {
            return None;
        }
        let mut need = 0u32;
        GetKernelObjectSecurity(
            h,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut need,
        );
        let mut buf = vec![0u8; need as usize + 8];
        let ok = GetKernelObjectSecurity(
            h,
            DACL_SECURITY_INFORMATION,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
            &mut need,
        );
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let mut s: *mut u16 = std::ptr::null_mut();
        let mut len = 0u32;
        if ConvertSecurityDescriptorToStringSecurityDescriptorW(
            buf.as_mut_ptr() as *mut c_void,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut s,
            &mut len,
        ) == 0
        {
            return None;
        }
        let out = cell_core::winfsp::from_wide(s);
        LocalFree(s as _);
        Some(out)
    }
}

/// Whether app packages can reach the volume: its device descriptor
/// names `ALL APPLICATION PACKAGES` (`AC`) or a package SID (`S-1-15-2-`).
pub fn device_allows_app_packages(volume_name: &str) -> Option<bool> {
    device_sddl(volume_name).map(|s| s.contains(";;;AC)") || s.contains("S-1-15-2-"))
}
