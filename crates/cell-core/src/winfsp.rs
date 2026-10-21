//! A small binding to the WinFsp DLL, loaded at run time.
//!
//! The launcher must start on a host without WinFsp and fall back to the
//! extract tier, so nothing here links against `winfsp-x64.lib`. The DLL
//! is found through the registry key the installer writes, then loaded
//! with `LoadLibraryW`, and the few functions the union file system needs
//! are resolved with `GetProcAddress`. Struct layouts come from
//! `winfsp/fsctl.h` and `winfsp/winfsp.h` (WinFsp 2.x); the sizes are
//! asserted below.

#![allow(non_snake_case, non_camel_case_types, clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::path::PathBuf;

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{HANDLE, NTSTATUS};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

pub type PSECURITY_DESCRIPTOR = *mut c_void;
pub type PWSTR = *mut u16;
pub type BOOLEAN = u8;

/// `FSP_FILE_SYSTEM` is opaque here except for `UserContext`, the second
/// field, which the union file system uses to find itself.
pub type FileSystem = c_void;

pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_BUFFER_OVERFLOW: NTSTATUS = 0x8000_0005_u32 as i32;
pub const STATUS_UNSUCCESSFUL: NTSTATUS = 0xC000_0001_u32 as i32;
pub const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000D_u32 as i32;
pub const STATUS_END_OF_FILE: NTSTATUS = 0xC000_0011_u32 as i32;
pub const STATUS_ACCESS_DENIED: NTSTATUS = 0xC000_0022_u32 as i32;
pub const STATUS_OBJECT_NAME_INVALID: NTSTATUS = 0xC000_0033_u32 as i32;
pub const STATUS_OBJECT_NAME_NOT_FOUND: NTSTATUS = 0xC000_0034_u32 as i32;
pub const STATUS_OBJECT_NAME_COLLISION: NTSTATUS = 0xC000_0035_u32 as i32;
pub const STATUS_OBJECT_PATH_NOT_FOUND: NTSTATUS = 0xC000_003A_u32 as i32;
pub const STATUS_INSUFFICIENT_RESOURCES: NTSTATUS = 0xC000_009A_u32 as i32;
pub const STATUS_FILE_IS_A_DIRECTORY: NTSTATUS = 0xC000_00BA_u32 as i32;
pub const STATUS_DIRECTORY_NOT_EMPTY: NTSTATUS = 0xC000_0101_u32 as i32;
pub const STATUS_NOT_A_DIRECTORY: NTSTATUS = 0xC000_0103_u32 as i32;
pub const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010_u32 as i32;
pub const STATUS_MEDIA_WRITE_PROTECTED: NTSTATUS = 0xC000_00A2_u32 as i32;

pub const FSP_CLEANUP_DELETE: u32 = 0x01;

/// `FSP_FSCTL_VOLUME_PARAMS`, V0 and V1 fields, 504 bytes.
#[repr(C)]
pub struct VolumeParams {
    pub version: u16,
    pub sector_size: u16,
    pub sectors_per_allocation_unit: u16,
    pub max_component_length: u16,
    pub volume_creation_time: u64,
    pub volume_serial_number: u32,
    pub transact_timeout: u32,
    pub irp_timeout: u32,
    pub irp_capacity: u32,
    pub file_info_timeout: u32,
    /// The bit field: see the `VP_*` constants.
    pub flags: u32,
    pub prefix: [u16; 192],
    pub file_system_name: [u16; 16],
    pub flags2: u32,
    pub volume_info_timeout: u32,
    pub dir_info_timeout: u32,
    pub security_timeout: u32,
    pub stream_info_timeout: u32,
    pub ea_timeout: u32,
    pub fsext_control_code: u32,
    pub reserved32: u32,
    pub reserved64: [u64; 2],
}
const _: () = assert!(std::mem::size_of::<VolumeParams>() == 504);

pub const VP_CASE_SENSITIVE_SEARCH: u32 = 1 << 0;
pub const VP_CASE_PRESERVED_NAMES: u32 = 1 << 1;
pub const VP_UNICODE_ON_DISK: u32 = 1 << 2;
pub const VP_PERSISTENT_ACLS: u32 = 1 << 3;
pub const VP_POST_CLEANUP_WHEN_MODIFIED_ONLY: u32 = 1 << 10;
pub const VP_PASS_QUERY_DIRECTORY_PATTERN: u32 = 1 << 11;
pub const VP_FLUSH_AND_PURGE_ON_CLEANUP: u32 = 1 << 14;
pub const VP_UM_FILE_CONTEXT_IS_USER_CONTEXT2: u32 = 1 << 16;

impl Default for VolumeParams {
    fn default() -> Self {
        // SAFETY: all-zero is a valid value for this plain-data struct.
        unsafe { std::mem::zeroed() }
    }
}

/// `FSP_FSCTL_FILE_INFO`, 72 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FileInfo {
    pub file_attributes: u32,
    pub reparse_tag: u32,
    pub allocation_size: u64,
    pub file_size: u64,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub change_time: u64,
    pub index_number: u64,
    pub hard_links: u32,
    pub ea_size: u32,
}
const _: () = assert!(std::mem::size_of::<FileInfo>() == 72);

/// `FSP_FSCTL_VOLUME_INFO`, 88 bytes.
#[repr(C)]
pub struct VolumeInfo {
    pub total_size: u64,
    pub free_size: u64,
    pub volume_label_length: u16,
    pub volume_label: [u16; 32],
}
const _: () = assert!(std::mem::size_of::<VolumeInfo>() == 88);

/// The fixed head of `FSP_FSCTL_DIR_INFO` (104 bytes); the UTF-16 name
/// follows it in memory without a terminator.
#[repr(C)]
pub struct DirInfoHead {
    pub size: u16,
    _pad: [u16; 3],
    pub file_info: FileInfo,
    pub next_offset: [u8; 24],
}
const _: () = assert!(std::mem::size_of::<DirInfoHead>() == 104);

/// Build one directory entry: the head followed by the name.
pub fn dir_info_bytes(name: &str, info: &FileInfo) -> Vec<u8> {
    let wide: Vec<u16> = name.encode_utf16().collect();
    let total = std::mem::size_of::<DirInfoHead>() + wide.len() * 2;
    let mut out = vec![0u8; total.div_ceil(8) * 8];
    let head = DirInfoHead {
        size: total as u16,
        _pad: [0; 3],
        file_info: *info,
        next_offset: [0; 24],
    };
    // SAFETY: DirInfoHead is plain data of exactly 104 bytes.
    let head_bytes: [u8; 104] = unsafe { std::mem::transmute(head) };
    out[..104].copy_from_slice(&head_bytes);
    for (i, w) in wide.iter().enumerate() {
        out[104 + i * 2..104 + i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// `FSP_FILE_SYSTEM_INTERFACE`: 33 named slots plus 31 reserved.
#[repr(C)]
pub struct Interface {
    pub get_volume_info: Option<unsafe extern "C" fn(*mut FileSystem, *mut VolumeInfo) -> NTSTATUS>,
    pub set_volume_label:
        Option<unsafe extern "C" fn(*mut FileSystem, PWSTR, *mut VolumeInfo) -> NTSTATUS>,
    pub get_security_by_name: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            PWSTR,
            *mut u32,
            PSECURITY_DESCRIPTOR,
            *mut usize,
        ) -> NTSTATUS,
    >,
    pub create: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            PWSTR,
            u32,
            u32,
            u32,
            PSECURITY_DESCRIPTOR,
            u64,
            *mut *mut c_void,
            *mut FileInfo,
        ) -> NTSTATUS,
    >,
    pub open: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            PWSTR,
            u32,
            u32,
            *mut *mut c_void,
            *mut FileInfo,
        ) -> NTSTATUS,
    >,
    pub overwrite: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            *mut c_void,
            u32,
            BOOLEAN,
            u64,
            *mut FileInfo,
        ) -> NTSTATUS,
    >,
    pub cleanup: Option<unsafe extern "C" fn(*mut FileSystem, *mut c_void, PWSTR, u32)>,
    pub close: Option<unsafe extern "C" fn(*mut FileSystem, *mut c_void)>,
    pub read: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            *mut c_void,
            *mut c_void,
            u64,
            u32,
            *mut u32,
        ) -> NTSTATUS,
    >,
    pub write: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            *mut c_void,
            *mut c_void,
            u64,
            u32,
            BOOLEAN,
            BOOLEAN,
            *mut u32,
            *mut FileInfo,
        ) -> NTSTATUS,
    >,
    pub flush:
        Option<unsafe extern "C" fn(*mut FileSystem, *mut c_void, *mut FileInfo) -> NTSTATUS>,
    pub get_file_info:
        Option<unsafe extern "C" fn(*mut FileSystem, *mut c_void, *mut FileInfo) -> NTSTATUS>,
    pub set_basic_info: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            *mut c_void,
            u32,
            u64,
            u64,
            u64,
            u64,
            *mut FileInfo,
        ) -> NTSTATUS,
    >,
    pub set_file_size: Option<
        unsafe extern "C" fn(*mut FileSystem, *mut c_void, u64, BOOLEAN, *mut FileInfo) -> NTSTATUS,
    >,
    pub can_delete: Option<unsafe extern "C" fn(*mut FileSystem, *mut c_void, PWSTR) -> NTSTATUS>,
    pub rename: Option<
        unsafe extern "C" fn(*mut FileSystem, *mut c_void, PWSTR, PWSTR, BOOLEAN) -> NTSTATUS,
    >,
    pub get_security: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            *mut c_void,
            PSECURITY_DESCRIPTOR,
            *mut usize,
        ) -> NTSTATUS,
    >,
    pub set_security: Option<
        unsafe extern "C" fn(*mut FileSystem, *mut c_void, u32, PSECURITY_DESCRIPTOR) -> NTSTATUS,
    >,
    pub read_directory: Option<
        unsafe extern "C" fn(
            *mut FileSystem,
            *mut c_void,
            PWSTR,
            PWSTR,
            *mut c_void,
            u32,
            *mut u32,
        ) -> NTSTATUS,
    >,
    pub resolve_reparse_points: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub get_reparse_point: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub set_reparse_point: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub delete_reparse_point: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub get_stream_info: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub get_dir_info_by_name: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub control: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub set_delete: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub create_ex: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub overwrite_ex: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub get_ea: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub set_ea: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub obsolete0: Option<unsafe extern "C" fn() -> NTSTATUS>,
    pub dispatcher_stopped: Option<unsafe extern "C" fn(*mut FileSystem, BOOLEAN)>,
    pub reserved: [Option<unsafe extern "C" fn() -> NTSTATUS>; 31],
}
const _: () = assert!(std::mem::size_of::<Interface>() == 64 * 8);

/// Read `FileSystem->UserContext`, the second field of `FSP_FILE_SYSTEM`
/// (a `UINT16` then pointer alignment puts it at offset 8).
pub unsafe fn user_context(fs: *mut FileSystem) -> *mut c_void {
    *((fs as *mut u8).add(8) as *mut *mut c_void)
}

/// `FileSystem->VolumeName`, the `\Device\Volume{...}` name of the
/// mounted volume (a wide string at offset 16, up to 256 characters).
pub unsafe fn volume_name(fs: *mut FileSystem) -> String {
    let p = (fs as *const u8).add(16) as *const u16;
    let mut n = 0;
    while n < 256 && *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}

pub unsafe fn set_user_context(fs: *mut FileSystem, ctx: *mut c_void) {
    *((fs as *mut u8).add(8) as *mut *mut c_void) = ctx;
}

type FnCreate = unsafe extern "C" fn(
    *const u16,
    *const VolumeParams,
    *const Interface,
    *mut *mut FileSystem,
) -> NTSTATUS;
type FnFs = unsafe extern "C" fn(*mut FileSystem);
type FnFsStatus = unsafe extern "C" fn(*mut FileSystem) -> NTSTATUS;
type FnSetMount = unsafe extern "C" fn(*mut FileSystem, *const u16) -> NTSTATUS;
type FnStart = unsafe extern "C" fn(*mut FileSystem, u32) -> NTSTATUS;
type FnAcquire = unsafe extern "C" fn(*mut *mut c_void, BOOLEAN, *mut NTSTATUS) -> BOOLEAN;
type FnFill = unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut NTSTATUS) -> BOOLEAN;
type FnBuf = unsafe extern "C" fn(*mut *mut c_void);
type FnReadBuf = unsafe extern "C" fn(*mut *mut c_void, *const u16, *mut c_void, u32, *mut u32);
type FnFromWin32 = unsafe extern "C" fn(u32) -> NTSTATUS;
type FnDebugLog = unsafe extern "C" fn(*mut FileSystem, u32);
type FnDebugHandle = unsafe extern "C" fn(HANDLE);

/// The resolved entry points.
pub struct Api {
    pub dll_path: PathBuf,
    pub FspFileSystemCreate: FnCreate,
    pub FspFileSystemDelete: FnFs,
    pub FspFileSystemSetMountPoint: FnSetMount,
    pub FspFileSystemRemoveMountPoint: FnFs,
    pub FspFileSystemStartDispatcher: FnStart,
    pub FspFileSystemStopDispatcher: FnFs,
    pub FspFileSystemAcquireDirectoryBuffer: FnAcquire,
    pub FspFileSystemFillDirectoryBuffer: FnFill,
    pub FspFileSystemReleaseDirectoryBuffer: FnBuf,
    pub FspFileSystemReadDirectoryBuffer: FnReadBuf,
    pub FspFileSystemDeleteDirectoryBuffer: FnBuf,
    pub FspNtStatusFromWin32: FnFromWin32,
    pub FspFileSystemSetDebugLogF: Option<FnDebugLog>,
    pub FspDebugLogSetHandle: Option<FnDebugHandle>,
    _preflight: Option<FnFsStatus>,
}

// SAFETY: the function pointers are plain code addresses in a DLL that
// stays loaded for the life of the process.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Where the installer put the DLL, or `None` when WinFsp is not installed.
pub fn dll_path() -> Option<PathBuf> {
    let arch_dll = if cfg!(target_arch = "x86_64") {
        "winfsp-x64.dll"
    } else if cfg!(target_arch = "aarch64") {
        "winfsp-a64.dll"
    } else {
        "winfsp-x86.dll"
    };
    let mut candidates = Vec::new();
    if let Some(dir) = registry_install_dir() {
        candidates.push(PathBuf::from(dir).join("bin").join(arch_dll));
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Ok(pf) = std::env::var(var) {
            candidates.push(PathBuf::from(pf).join("WinFsp").join("bin").join(arch_dll));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn registry_install_dir() -> Option<String> {
    let key = wide("SOFTWARE\\WOW6432Node\\WinFsp");
    let value = wide("InstallDir");
    let mut buf = vec![0u16; 1024];
    let mut len: u32 = (buf.len() * 2) as u32;
    // SAFETY: valid, NUL-terminated wide strings and a sized buffer.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1).min(buf.len());
    Some(
        String::from_utf16_lossy(&buf[..n])
            .trim_end_matches('\0')
            .to_string(),
    )
}

/// Load the DLL and resolve the entry points.
pub fn load() -> Result<Api> {
    let Some(path) = dll_path() else {
        bail!("WinFsp is not installed (https://winfsp.dev)");
    };
    let w = wide(&path.to_string_lossy());
    // SAFETY: `w` is NUL-terminated.
    let module = unsafe { LoadLibraryW(w.as_ptr()) };
    if module.is_null() {
        bail!("cannot load {}", path.display());
    }
    macro_rules! sym {
        ($name:literal, $ty:ty) => {{
            let n = concat!($name, "\0");
            // SAFETY: a valid module and a NUL-terminated name.
            let p = unsafe { GetProcAddress(module, n.as_ptr()) };
            match p {
                Some(f) => unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(f)
                },
                None => bail!("{} lacks {}", path.display(), $name),
            }
        }};
    }
    macro_rules! optsym {
        ($name:literal, $ty:ty) => {{
            let n = concat!($name, "\0");
            // SAFETY: as above.
            unsafe { GetProcAddress(module, n.as_ptr()) }.map(|f| unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(f)
            })
        }};
    }
    Ok(Api {
        FspFileSystemCreate: sym!("FspFileSystemCreate", FnCreate),
        FspFileSystemDelete: sym!("FspFileSystemDelete", FnFs),
        FspFileSystemSetMountPoint: sym!("FspFileSystemSetMountPoint", FnSetMount),
        FspFileSystemRemoveMountPoint: sym!("FspFileSystemRemoveMountPoint", FnFs),
        FspFileSystemStartDispatcher: sym!("FspFileSystemStartDispatcher", FnStart),
        FspFileSystemStopDispatcher: sym!("FspFileSystemStopDispatcher", FnFs),
        FspFileSystemAcquireDirectoryBuffer: sym!("FspFileSystemAcquireDirectoryBuffer", FnAcquire),
        FspFileSystemFillDirectoryBuffer: sym!("FspFileSystemFillDirectoryBuffer", FnFill),
        FspFileSystemReleaseDirectoryBuffer: sym!("FspFileSystemReleaseDirectoryBuffer", FnBuf),
        FspFileSystemReadDirectoryBuffer: sym!("FspFileSystemReadDirectoryBuffer", FnReadBuf),
        FspFileSystemDeleteDirectoryBuffer: sym!("FspFileSystemDeleteDirectoryBuffer", FnBuf),
        FspNtStatusFromWin32: sym!("FspNtStatusFromWin32", FnFromWin32),
        FspFileSystemSetDebugLogF: optsym!("FspFileSystemSetDebugLogF", FnDebugLog),
        FspDebugLogSetHandle: optsym!("FspDebugLogSetHandle", FnDebugHandle),
        _preflight: None,
        dll_path: path,
    })
}

/// Device path for a disk-style file system.
pub fn disk_device_path() -> Vec<u16> {
    wide("WinFsp.Disk")
}

/// Convert a NUL-terminated wide string from WinFsp.
pub unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}
