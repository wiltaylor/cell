//! The declared registry keys of a Windows cell, applied to the real
//! `HKEY_CURRENT_USER` before the app starts.
//!
//! There is no virtual hive: the app reads and writes the user's own
//! registry, and the cell says which keys it needs. The launcher creates
//! those keys, writes each declared value once (or at every start when
//! it names `$CELL_ROOT`, because the mounted root can move), and gives
//! the AppContainer an ACE on every declared key, since a lowbox token
//! cannot otherwise write anywhere under `HKCU`.

use anyhow::{bail, Result};
use cell_core::registry::{RegData, Registry};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_ALL_ACCESS, REG_BINARY, REG_CREATED_NEW_KEY, REG_DWORD, REG_EXPAND_SZ,
    REG_MULTI_SZ, REG_OPTION_NON_VOLATILE, REG_QWORD, REG_SZ,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An open key that closes with the value.
struct Key(HKEY);

impl Key {
    /// Open or create `subkey` under `HKEY_CURRENT_USER`. Returns the key
    /// and whether it was new.
    fn create(subkey: &str) -> Result<(Key, bool)> {
        let path = wide(subkey);
        let mut h: HKEY = std::ptr::null_mut();
        let mut disposition: u32 = 0;
        // SAFETY: plain registry call with valid pointers.
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut h,
                &mut disposition,
            )
        };
        if rc != ERROR_SUCCESS {
            bail!("create HKCU\\{subkey}: error {rc}");
        }
        Ok((Key(h), disposition == REG_CREATED_NEW_KEY))
    }

    fn has_value(&self, name: &str) -> bool {
        let n = wide(name);
        // SAFETY: a size query, no buffers.
        let rc = unsafe {
            RegQueryValueExW(
                self.0,
                n.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        rc != ERROR_FILE_NOT_FOUND
    }

    fn set(&self, name: &str, data: &RegData) -> Result<()> {
        let (kind, bytes) = encode(data);
        let n = wide(name);
        // SAFETY: the buffer outlives the call.
        let rc = unsafe {
            RegSetValueExW(
                self.0,
                n.as_ptr(),
                0,
                kind,
                bytes.as_ptr(),
                bytes.len() as u32,
            )
        };
        if rc != ERROR_SUCCESS {
            bail!("set value `{name}`: error {rc}");
        }
        Ok(())
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle is ours.
        unsafe { RegCloseKey(self.0) };
    }
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(|u| u.to_le_bytes())
        .collect()
}

/// The registry type and the raw bytes of a value.
fn encode(d: &RegData) -> (u32, Vec<u8>) {
    match d {
        RegData::String(s) => (REG_SZ, utf16(s)),
        RegData::Expand(s) => (REG_EXPAND_SZ, utf16(s)),
        RegData::Dword(n) => (REG_DWORD, n.to_le_bytes().to_vec()),
        RegData::Qword(n) => (REG_QWORD, n.to_le_bytes().to_vec()),
        RegData::Binary(b) => (REG_BINARY, b.clone()),
        RegData::Strings(v) => {
            let mut out: Vec<u8> = v.iter().flat_map(|s| utf16(s)).collect();
            out.extend([0, 0]);
            (REG_MULTI_SZ, out)
        }
    }
}

/// Create the declared keys and write their values. `expand` replaces
/// `$CELL_ROOT` in strings. Returns how many values were written.
pub fn apply(reg: &Registry, expand: impl Fn(&str) -> String) -> Result<usize> {
    let mut written = 0;
    for k in &reg.keys {
        let (key, new) = Key::create(&k.path)?;
        for v in &k.values {
            if !new && !v.data.mentions_root() && key.has_value(&v.name) {
                continue;
            }
            key.set(&v.name, &v.data.map_strings(&expand))
                .map_err(|e| anyhow::anyhow!("HKCU\\{}: {e}", k.path))?;
            written += 1;
        }
    }
    for g in &reg.grants {
        // The sandbox gets an ACE on it below; the key must exist first.
        Key::create(g)?;
    }
    Ok(written)
}

/// Give the container access to every declared key and every grant.
pub fn grant_all(reg: &Registry, sid: &crate::appcontainer::Sid) -> Result<()> {
    for path in reg.grant_paths() {
        crate::appcontainer::grant_key(path, sid, true)
            .map_err(|e| anyhow::anyhow!("grant the sandbox access to HKCU\\{path}: {e}"))?;
    }
    Ok(())
}

/// `CELL_CMD=registry-clean`: delete the declared keys and all they hold.
/// Grant-only keys belong to someone else and stay.
pub fn clean(reg: &Registry) -> Result<usize> {
    let mut removed = 0;
    for k in &reg.keys {
        let path = wide(&k.path);
        // SAFETY: a plain registry call.
        let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };
        match rc {
            ERROR_SUCCESS => {
                println!("removed HKCU\\{}", k.path);
                removed += 1;
            }
            ERROR_FILE_NOT_FOUND => {}
            rc => bail!("delete HKCU\\{}: error {rc}", k.path),
        }
    }
    Ok(removed)
}
