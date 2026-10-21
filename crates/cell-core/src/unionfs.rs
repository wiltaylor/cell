//! The union file system a Windows cell runs from, served through WinFsp.
//!
//! Lower layers are squashfs images read straight from the `.cell` file.
//! The upper layer is a plain NTFS directory. The merge rules follow
//! overlayfs: the upper wins, directories merge, a file in a higher layer
//! hides everything below it, and a delete of a lower name writes a
//! whiteout file `.cellwh.<name>` into the upper directory. A whiteout
//! with an upper entry of the same name makes that entry opaque, which is
//! what "remove a directory, then recreate it" needs.
//!
//! Names from the lower layers are matched without case, because the
//! volume reports itself case-insensitive, like NTFS.

#![allow(clippy::missing_safety_doc, clippy::too_many_arguments)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_FILE_NOT_FOUND, FILETIME, HANDLE,
    INVALID_HANDLE_VALUE, NTSTATUS,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::GetSecurityDescriptorLength;
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, DeleteFileW, FileAllocationInfo, FileBasicInfo,
    FileDispositionInfo, FileEndOfFileInfo, FindClose, FindFirstFileW, FindNextFileW,
    FlushFileBuffers, GetDiskFreeSpaceExW, GetFileInformationByHandle, MoveFileExW, ReadFile,
    RemoveDirectoryW, SetFileInformationByHandle, WriteFile, BY_HANDLE_FILE_INFORMATION,
    CREATE_NEW, DELETE, FILE_ALLOCATION_INFO, FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_NORMAL, FILE_BASIC_INFO, FILE_DISPOSITION_INFO, FILE_END_OF_FILE_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_POSIX_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA,
    INVALID_FILE_ATTRIBUTES, MOVEFILE_REPLACE_EXISTING, OPEN_EXISTING, WIN32_FIND_DATAW, WRITE_DAC,
    WRITE_OWNER,
};
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows_sys::Win32::System::IO::OVERLAPPED;

use crate::squashfs::{DirEntry, Inode, InodeKind, InodeRef, Squashfs};
use crate::winfsp::{
    self as fsp, Api, FileInfo, FileSystem, Interface, VolumeInfo, VolumeParams, BOOLEAN,
};

const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const WRITE_MASK: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL;
const FILE_DIRECTORY_FILE: u32 = 0x1;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
const ALLOCATION_UNIT: u64 = 4096;
const WHITEOUT_PREFIX: &str = ".cellwh.";

/// One lower layer to mount.
pub struct LayerSpec {
    pub name: String,
    pub file: File,
    pub base: u64,
    /// `/` for the root stack, or the mount path of a data layer.
    pub mount: String,
}

struct Lower {
    name: String,
    img: Squashfs,
    root: InodeRef,
    /// Mount path components, case-folded. Empty for the root stack.
    mount: Vec<String>,
}

/// A directory's entries indexed by case-folded name.
struct DirIndex {
    by_key: HashMap<String, usize>,
    entries: Vec<DirEntry>,
}

fn fold(s: &str) -> String {
    s.to_lowercase()
}

fn filetime_from_unix(secs: u64) -> u64 {
    (secs + 11_644_473_600) * 10_000_000
}

fn now_filetime() -> u64 {
    let mut ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    // SAFETY: a valid out pointer.
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

fn ft_u64(ft: &FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

fn wide_path(p: &Path) -> Vec<u16> {
    // Long-path prefix so deep game trees do not hit MAX_PATH.
    let s = p.to_string_lossy();
    let s = if s.starts_with("\\\\?\\") {
        s.into_owned()
    } else {
        format!("\\\\?\\{s}")
    };
    fsp::wide(&s)
}

/// The union: lowers plus an upper directory.
pub struct Union {
    lowers: Vec<Lower>,
    upper: PathBuf,
    security: Vec<u8>,
    created: u64,
    dir_cache: Mutex<HashMap<(usize, u64, u16), Arc<DirIndex>>>,
    /// Serialises copy-ups so two opens of one lower file do not race.
    copy_lock: Mutex<()>,
    api: Arc<Api>,
}

/// What a path resolves to in the merged view.
struct Resolved {
    /// The upper path and whether it exists there, as a dir or a file.
    upper: PathBuf,
    upper_kind: Option<bool>, // Some(is_dir)
    /// Lower inodes still visible at this path, top-most first.
    lowers: Vec<(usize, Inode)>,
    /// A directory that exists only because a data layer mounts below it.
    synthetic: bool,
    /// The parent directory exists in the merged view.
    parent_exists: bool,
}

impl Resolved {
    fn exists(&self) -> bool {
        self.upper_kind.is_some() || !self.lowers.is_empty() || self.synthetic
    }
    fn is_dir(&self) -> bool {
        match self.upper_kind {
            Some(d) => d,
            None => match self.lowers.first() {
                Some((_, ino)) => matches!(ino.kind, InodeKind::Dir { .. }),
                None => self.synthetic,
            },
        }
    }
    /// The lower file that backs this path, if the upper has nothing.
    fn lower_file(&self) -> Option<&(usize, Inode)> {
        if self.upper_kind.is_some() {
            return None;
        }
        self.lowers
            .first()
            .filter(|(_, i)| matches!(i.kind, InodeKind::File { .. }))
    }
}

/// Per-open state handed to WinFsp as the file context.
pub struct FileCtx {
    comps: Vec<String>,
    is_dir: bool,
    handle: HANDLE,
    lower: Option<(usize, Inode)>,
    dir_buffer: *mut c_void,
}

// SAFETY: the raw handle and the WinFsp directory buffer are used only
// through the WinFsp callbacks, which synchronise access.
unsafe impl Send for FileCtx {}
unsafe impl Sync for FileCtx {}

fn split_path(p: &str) -> Vec<String> {
    p.split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .map(|c| c.to_string())
        .collect()
}

fn build_security() -> Result<Vec<u8>> {
    // Everyone, SYSTEM, administrators and app containers get full access.
    // The sandbox boundary is not the ACL; the mount is per user anyway.
    let sddl = fsp::wide("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)(A;;FA;;;AC)");
    let mut sd: *mut c_void = std::ptr::null_mut();
    let mut len: u32 = 0;
    // SAFETY: a valid SDDL string and out pointers.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd as *mut *mut c_void as *mut _,
            &mut len,
        )
    };
    if ok == 0 || sd.is_null() {
        bail!("cannot build the volume security descriptor");
    }
    // SAFETY: `sd` is a self-relative descriptor of `len` bytes.
    let n = unsafe { GetSecurityDescriptorLength(sd) } as usize;
    let bytes =
        unsafe { std::slice::from_raw_parts(sd as *const u8, n.max(len as usize)) }.to_vec();
    unsafe { LocalFree(sd as _) };
    Ok(bytes)
}

impl Union {
    pub fn new(api: Arc<Api>, layers: Vec<LayerSpec>, upper: &Path) -> Result<Arc<Self>> {
        std::fs::create_dir_all(upper).with_context(|| format!("create {}", upper.display()))?;
        let mut lowers = Vec::new();
        for l in layers {
            let img =
                Squashfs::open(l.file, l.base).with_context(|| format!("open layer {}", l.name))?;
            let root = img.root_ref();
            let mount: Vec<String> = split_path(&l.mount).iter().map(|c| fold(c)).collect();
            lowers.push(Lower {
                name: l.name,
                img,
                root,
                mount,
            });
        }
        Ok(Arc::new(Union {
            lowers,
            upper: upper.to_path_buf(),
            security: build_security()?,
            created: now_filetime(),
            dir_cache: Mutex::new(HashMap::new()),
            copy_lock: Mutex::new(()),
            api,
        }))
    }

    pub fn layer_names(&self) -> Vec<String> {
        self.lowers.iter().map(|l| l.name.clone()).collect()
    }

    fn status_from_win32(&self, err: u32) -> NTSTATUS {
        // SAFETY: plain function call.
        unsafe { (self.api.FspNtStatusFromWin32)(err) }
    }

    fn last_error(&self) -> NTSTATUS {
        // SAFETY: plain function call.
        self.status_from_win32(unsafe { GetLastError() })
    }

    fn dir_index(&self, li: usize, ino: &Inode, r: InodeRef) -> Result<Arc<DirIndex>> {
        let key = (li, r.block, r.offset);
        if let Some(d) = self.dir_cache.lock().unwrap().get(&key) {
            return Ok(d.clone());
        }
        let entries = self.lowers[li].img.read_dir(ino)?;
        let mut by_key = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            let name = String::from_utf8_lossy(&e.name);
            by_key.entry(fold(&name)).or_insert(i);
        }
        let d = Arc::new(DirIndex { by_key, entries });
        self.dir_cache.lock().unwrap().insert(key, d.clone());
        Ok(d)
    }

    /// Look `name` up in a lower directory, without case.
    fn lower_lookup(
        &self,
        li: usize,
        dir: &Inode,
        dir_ref: InodeRef,
        name: &str,
    ) -> Result<Option<(Inode, InodeRef)>> {
        let idx = self.dir_index(li, dir, dir_ref)?;
        let Some(&i) = idx.by_key.get(&fold(name)) else {
            return Ok(None);
        };
        let e = &idx.entries[i];
        let ino = self.lowers[li].img.read_inode(e.inode_ref)?;
        Ok(Some((ino, e.inode_ref)))
    }

    fn whiteout_path(upper_dir: &Path, name: &str) -> PathBuf {
        upper_dir.join(format!("{WHITEOUT_PREFIX}{name}"))
    }

    /// Resolve `comps` in the merged view.
    fn resolve(&self, comps: &[String]) -> Result<Resolved> {
        if comps
            .last()
            .map(|c| c.starts_with(WHITEOUT_PREFIX))
            .unwrap_or(false)
        {
            // Whiteouts are internal and never visible.
            return Ok(Resolved {
                upper: self.upper.clone(),
                upper_kind: None,
                lowers: Vec::new(),
                synthetic: false,
                parent_exists: true,
            });
        }
        let mut upper_path = self.upper.clone();
        let mut upper_exists = true;
        let mut lower_visible = true;
        // Current position in each lower: the inode and its ref, or None
        // when the lower has nothing at this depth.
        let mut cur: Vec<Option<(Inode, InodeRef)>> = Vec::with_capacity(self.lowers.len());
        for l in &self.lowers {
            if l.mount.is_empty() {
                cur.push(Some((l.img.read_inode(l.root)?, l.root)));
            } else {
                cur.push(None);
            }
        }
        let mut parent_exists = true;
        let depth = comps.len();
        for (d, c) in comps.iter().enumerate() {
            let folded = fold(c);
            // The parent is the merged view at depth d.
            let synthetic_here = self.lowers.iter().any(|l| {
                l.mount.len() > d
                    && l.mount[..d]
                        .iter()
                        .zip(comps.iter())
                        .all(|(m, x)| *m == fold(x))
            });
            parent_exists = upper_exists || cur.iter().any(|x| x.is_some()) || synthetic_here;
            if upper_exists {
                if Self::whiteout_path(&upper_path, c).exists() {
                    lower_visible = false;
                }
                upper_path = upper_path.join(c);
                match std::fs::metadata(&upper_path) {
                    Ok(m) if m.is_dir() => {}
                    Ok(_) => {
                        if d + 1 < depth {
                            // A file in the upper hides the whole subtree.
                            return Ok(Resolved {
                                upper: upper_path,
                                upper_kind: None,
                                lowers: Vec::new(),
                                synthetic: false,
                                parent_exists: false,
                            });
                        }
                    }
                    Err(_) => upper_exists = false,
                }
            } else {
                upper_path = upper_path.join(c);
            }
            // Step every lower.
            let mut top_file = false;
            for (li, l) in self.lowers.iter().enumerate() {
                let next = if !lower_visible {
                    None
                } else if !l.mount.is_empty() && l.mount.len() == d + 1 {
                    // This component is the data layer's mount point.
                    if l.mount[..d]
                        .iter()
                        .zip(comps.iter())
                        .all(|(m, x)| *m == fold(x))
                        && l.mount[d] == folded
                    {
                        Some((l.img.read_inode(l.root)?, l.root))
                    } else {
                        None
                    }
                } else {
                    match &cur[li] {
                        Some((ino, r)) if matches!(ino.kind, InodeKind::Dir { .. }) => {
                            self.lower_lookup(li, ino, *r, c)?
                        }
                        _ => None,
                    }
                };
                // A file at a higher layer hides the layers below it.
                let next = if top_file { None } else { next };
                if let Some((ino, _)) = &next {
                    if !matches!(ino.kind, InodeKind::Dir { .. }) {
                        top_file = true;
                    }
                }
                cur[li] = next;
            }
            // Later lowers are below earlier ones only in reverse order:
            // the stack is runtime, app, data... with the last on top, so
            // apply "top-most file hides the rest" from the end.
            let mut seen_file = false;
            for li in (0..self.lowers.len()).rev() {
                if seen_file {
                    cur[li] = None;
                } else if let Some((ino, _)) = &cur[li] {
                    if !matches!(ino.kind, InodeKind::Dir { .. }) {
                        seen_file = true;
                    }
                }
            }
            if upper_exists && d + 1 == depth {
                // An upper file hides every lower at the leaf as well.
                if let Ok(m) = std::fs::metadata(&upper_path) {
                    if !m.is_dir() {
                        for x in cur.iter_mut() {
                            *x = None;
                        }
                    }
                }
            }
        }
        let upper_kind = if upper_exists {
            std::fs::metadata(&upper_path).ok().map(|m| m.is_dir())
        } else {
            None
        };
        let mut lowers: Vec<(usize, Inode)> = cur
            .into_iter()
            .enumerate()
            .filter_map(|(li, x)| x.map(|(ino, _)| (li, ino)))
            .collect();
        lowers.reverse(); // top-most first
        let synthetic = self.lowers.iter().any(|l| {
            l.mount.len() > depth
                && l.mount[..depth]
                    .iter()
                    .zip(comps.iter())
                    .all(|(m, x)| *m == fold(x))
        });
        Ok(Resolved {
            upper: upper_path,
            upper_kind,
            lowers,
            synthetic,
            parent_exists,
        })
    }

    /// Lower directory inode refs at `comps` (for listings), top-most first.
    fn lower_dirs(&self, comps: &[String]) -> Result<Vec<(usize, Inode, InodeRef)>> {
        // Re-walk keeping refs. Cheap enough: listings are rare next to reads.
        let mut out = Vec::new();
        let r = self.resolve(comps)?;
        for (li, ino) in r.lowers {
            if !matches!(ino.kind, InodeKind::Dir { .. }) {
                continue;
            }
            // Find the ref by walking this one lower again.
            if let Some(rf) = self.lower_ref(li, comps)? {
                out.push((li, ino, rf));
            }
        }
        Ok(out)
    }

    fn lower_ref(&self, li: usize, comps: &[String]) -> Result<Option<InodeRef>> {
        let l = &self.lowers[li];
        let start = l.mount.len();
        if comps.len() < start {
            return Ok(None);
        }
        let mut ino = l.img.read_inode(l.root)?;
        let mut r = l.root;
        for c in &comps[start..] {
            match self.lower_lookup(li, &ino, r, c)? {
                Some((i, rr)) => {
                    ino = i;
                    r = rr;
                }
                None => return Ok(None),
            }
        }
        Ok(Some(r))
    }

    fn info_from_inode(&self, li: usize, ino: &Inode) -> FileInfo {
        let is_dir = matches!(ino.kind, InodeKind::Dir { .. });
        let size = if is_dir { 0 } else { ino.size() };
        let t = filetime_from_unix(ino.mtime as u64);
        FileInfo {
            file_attributes: if is_dir {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            },
            reparse_tag: 0,
            allocation_size: size.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT,
            file_size: size,
            creation_time: t,
            last_access_time: t,
            last_write_time: t,
            change_time: t,
            index_number: ((li as u64 + 1) << 48) | ino.number as u64,
            hard_links: 0,
            ea_size: 0,
        }
    }

    fn info_synthetic(&self) -> FileInfo {
        FileInfo {
            file_attributes: FILE_ATTRIBUTE_DIRECTORY,
            creation_time: self.created,
            last_access_time: self.created,
            last_write_time: self.created,
            change_time: self.created,
            ..Default::default()
        }
    }

    fn info_from_handle(&self, h: HANDLE) -> Result<FileInfo, NTSTATUS> {
        // SAFETY: zeroed plain-data struct and a valid handle.
        let mut bh: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(h, &mut bh) } == 0 {
            return Err(self.last_error());
        }
        let size = ((bh.nFileSizeHigh as u64) << 32) | bh.nFileSizeLow as u64;
        Ok(FileInfo {
            file_attributes: bh.dwFileAttributes,
            reparse_tag: 0,
            allocation_size: size.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT,
            file_size: size,
            creation_time: ft_u64(&bh.ftCreationTime),
            last_access_time: ft_u64(&bh.ftLastAccessTime),
            last_write_time: ft_u64(&bh.ftLastWriteTime),
            change_time: ft_u64(&bh.ftLastWriteTime),
            index_number: ((bh.nFileIndexHigh as u64) << 32) | bh.nFileIndexLow as u64,
            hard_links: 0,
            ea_size: 0,
        })
    }

    fn info_from_find(data: &WIN32_FIND_DATAW) -> FileInfo {
        let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
        FileInfo {
            file_attributes: data.dwFileAttributes,
            reparse_tag: 0,
            allocation_size: size.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT,
            file_size: size,
            creation_time: ft_u64(&data.ftCreationTime),
            last_access_time: ft_u64(&data.ftLastAccessTime),
            last_write_time: ft_u64(&data.ftLastWriteTime),
            change_time: ft_u64(&data.ftLastWriteTime),
            index_number: 0,
            hard_links: 0,
            ea_size: 0,
        }
    }

    /// File info for a resolved path without opening it.
    fn info_for(&self, r: &Resolved) -> Result<FileInfo, NTSTATUS> {
        if r.upper_kind.is_some() {
            let h = self.open_upper(&r.upper, 0, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS)?;
            let i = self.info_from_handle(h);
            unsafe { CloseHandle(h) };
            return i;
        }
        if let Some((li, ino)) = r.lowers.first() {
            return Ok(self.info_from_inode(*li, ino));
        }
        if r.synthetic {
            return Ok(self.info_synthetic());
        }
        Err(fsp::STATUS_OBJECT_NAME_NOT_FOUND)
    }

    fn open_upper(
        &self,
        p: &Path,
        access: u32,
        disposition: u32,
        flags: u32,
    ) -> Result<HANDLE, NTSTATUS> {
        let w = wide_path(p);
        // SAFETY: a NUL-terminated path.
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                disposition,
                flags,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(self.last_error());
        }
        Ok(h)
    }

    /// Make sure `comps` exists as a directory in the upper. Parents that
    /// exist only in a lower are created, which is the directory copy-up.
    fn ensure_upper_dir(&self, comps: &[String]) -> Result<PathBuf> {
        let mut p = self.upper.clone();
        for c in comps {
            p = p.join(c);
            if !p.is_dir() {
                std::fs::create_dir(&p).with_context(|| format!("mkdir {}", p.display()))?;
            }
        }
        Ok(p)
    }

    /// Copy a lower file into the upper so it can be written.
    fn copy_up(&self, comps: &[String], li: usize, ino: &Inode) -> Result<PathBuf> {
        let _g = self.copy_lock.lock().unwrap();
        let parent = self.ensure_upper_dir(&comps[..comps.len() - 1])?;
        let dest = parent.join(comps.last().map(String::as_str).unwrap_or(""));
        if dest.exists() {
            return Ok(dest);
        }
        let tmp = parent.join(format!(".cellcopy.{}", std::process::id()));
        {
            use std::io::Write;
            let mut out = File::create(&tmp)?;
            let size = ino.size();
            let mut off = 0u64;
            while off < size {
                let want = (size - off).min(4 << 20) as usize;
                let data = self.lowers[li].img.read_file(ino, off, want)?;
                if data.is_empty() {
                    break;
                }
                out.write_all(&data)?;
                off += data.len() as u64;
            }
        }
        std::fs::rename(&tmp, &dest)?;
        Ok(dest)
    }

    /// Copy a whole lower directory tree into the upper (for renames).
    fn copy_up_tree(&self, comps: &[String]) -> Result<()> {
        let dirs = self.lower_dirs(comps)?;
        let dest = self.ensure_upper_dir(comps)?;
        // Bottom-most first, so higher layers overwrite.
        for (li, ino, r) in dirs.iter().rev() {
            let idx = self.dir_index(*li, ino, *r)?;
            for e in &idx.entries {
                let name = String::from_utf8_lossy(&e.name).into_owned();
                if Self::whiteout_path(&dest, &name).exists() {
                    continue;
                }
                let child = self.lowers[*li].img.read_inode(e.inode_ref)?;
                let mut sub = comps.to_vec();
                sub.push(name.clone());
                match child.kind {
                    InodeKind::Dir { .. } => self.copy_up_tree(&sub)?,
                    InodeKind::File { .. } if !dest.join(&name).exists() => {
                        self.copy_up(&sub, *li, &child)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn write_whiteout(&self, comps: &[String]) -> Result<()> {
        let Some(name) = comps.last() else {
            return Ok(());
        };
        let parent = self.ensure_upper_dir(&comps[..comps.len() - 1])?;
        File::create(Self::whiteout_path(&parent, name))?;
        Ok(())
    }

    /// Any lower has an entry at `comps`.
    fn lowers_have(&self, comps: &[String]) -> bool {
        (0..self.lowers.len()).any(|li| self.lower_ref(li, comps).ok().flatten().is_some())
    }

    /// The merged listing of a directory: name and info, sorted.
    fn list_dir(&self, comps: &[String]) -> Result<Vec<(String, FileInfo)>> {
        let mut seen: HashMap<String, (String, FileInfo)> = HashMap::new();
        let mut whiteouts: Vec<String> = Vec::new();
        let r = self.resolve(comps)?;
        // Upper entries.
        if r.upper_kind == Some(true) {
            let pattern = wide_path(&r.upper.join("*"));
            // SAFETY: zeroed plain data and a valid pattern.
            let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
            let h = unsafe { FindFirstFileW(pattern.as_ptr(), &mut data) };
            if h != INVALID_HANDLE_VALUE {
                loop {
                    let n = data
                        .cFileName
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(data.cFileName.len());
                    let name = String::from_utf16_lossy(&data.cFileName[..n]);
                    if name != "." && name != ".." {
                        if let Some(rest) = name.strip_prefix(WHITEOUT_PREFIX) {
                            whiteouts.push(fold(rest));
                        } else if !name.starts_with(".cellcopy.") {
                            seen.insert(fold(&name), (name, Self::info_from_find(&data)));
                        }
                    }
                    if unsafe { FindNextFileW(h, &mut data) } == 0 {
                        break;
                    }
                }
                unsafe { FindClose(h) };
            }
        }
        // Lower entries, top-most layer first so it wins among lowers.
        for (li, ino) in &r.lowers {
            if !matches!(ino.kind, InodeKind::Dir { .. }) {
                continue;
            }
            let Some(rf) = self.lower_ref(*li, comps)? else {
                continue;
            };
            let idx = self.dir_index(*li, ino, rf)?;
            for e in &idx.entries {
                let name = String::from_utf8_lossy(&e.name).into_owned();
                let key = fold(&name);
                if seen.contains_key(&key) || whiteouts.contains(&key) {
                    continue;
                }
                // Skip names Windows cannot represent, and special files.
                if crate::extract::windows_name_problem(&name).is_some() {
                    continue;
                }
                match e.kind {
                    1 | 2 | 8 | 9 => {}
                    _ => continue,
                }
                let child = self.lowers[*li].img.read_inode(e.inode_ref)?;
                seen.insert(key, (name, self.info_from_inode(*li, &child)));
            }
        }
        // Mount points of data layers below this directory.
        for l in &self.lowers {
            if l.mount.len() == comps.len() + 1
                && l.mount[..comps.len()]
                    .iter()
                    .zip(comps.iter())
                    .all(|(m, x)| *m == fold(x))
            {
                let key = l.mount[comps.len()].clone();
                seen.entry(key.clone())
                    .or_insert((key, self.info_synthetic()));
            }
        }
        let mut out: Vec<(String, FileInfo)> = seen.into_values().collect();
        out.sort_by_key(|a| fold(&a.0));
        Ok(out)
    }
}

// ---- the WinFsp callbacks ----------------------------------------------------

unsafe fn union_of<'a>(fs: *mut FileSystem) -> &'a Union {
    &*(fsp::user_context(fs) as *const Union)
}

unsafe fn ctx_of<'a>(p: *mut c_void) -> &'a mut FileCtx {
    &mut *(p as *mut FileCtx)
}

fn err_status(e: &anyhow::Error) -> NTSTATUS {
    eprintln!("cell: unionfs: {e:#}");
    fsp::STATUS_UNSUCCESSFUL
}

unsafe extern "C" fn cb_get_volume_info(fs: *mut FileSystem, vi: *mut VolumeInfo) -> NTSTATUS {
    let u = union_of(fs);
    let w = wide_path(&u.upper);
    let mut total: u64 = 0;
    let mut free: u64 = 0;
    if GetDiskFreeSpaceExW(w.as_ptr(), std::ptr::null_mut(), &mut total, &mut free) == 0 {
        return u.last_error();
    }
    (*vi).total_size = total;
    (*vi).free_size = free;
    let label: Vec<u16> = "Cell".encode_utf16().collect();
    let vi = &mut *vi;
    vi.volume_label[..label.len()].copy_from_slice(&label);
    vi.volume_label_length = (label.len() * 2) as u16;
    fsp::STATUS_SUCCESS
}

unsafe fn write_security(u: &Union, sd: *mut c_void, psize: *mut usize) -> NTSTATUS {
    if psize.is_null() {
        return fsp::STATUS_SUCCESS;
    }
    let need = u.security.len();
    if *psize < need || sd.is_null() {
        *psize = need;
        return if sd.is_null() {
            fsp::STATUS_SUCCESS
        } else {
            fsp::STATUS_BUFFER_OVERFLOW
        };
    }
    std::ptr::copy_nonoverlapping(u.security.as_ptr(), sd as *mut u8, need);
    *psize = need;
    fsp::STATUS_SUCCESS
}

unsafe extern "C" fn cb_get_security_by_name(
    fs: *mut FileSystem,
    name: fsp::PWSTR,
    pattrs: *mut u32,
    sd: *mut c_void,
    psize: *mut usize,
) -> NTSTATUS {
    let u = union_of(fs);
    let comps = split_path(&fsp::from_wide(name));
    let r = match u.resolve(&comps) {
        Ok(r) => r,
        Err(e) => return err_status(&e),
    };
    if !r.exists() {
        return if r.parent_exists {
            fsp::STATUS_OBJECT_NAME_NOT_FOUND
        } else {
            fsp::STATUS_OBJECT_PATH_NOT_FOUND
        };
    }
    if !pattrs.is_null() {
        match u.info_for(&r) {
            Ok(i) => *pattrs = i.file_attributes,
            Err(s) => return s,
        }
    }
    write_security(u, sd, psize)
}

unsafe extern "C" fn cb_open(
    fs: *mut FileSystem,
    name: fsp::PWSTR,
    create_options: u32,
    granted: u32,
    pctx: *mut *mut c_void,
    info: *mut FileInfo,
) -> NTSTATUS {
    let u = union_of(fs);
    let comps = split_path(&fsp::from_wide(name));
    let r = match u.resolve(&comps) {
        Ok(r) => r,
        Err(e) => return err_status(&e),
    };
    if !r.exists() {
        return if r.parent_exists {
            fsp::STATUS_OBJECT_NAME_NOT_FOUND
        } else {
            fsp::STATUS_OBJECT_PATH_NOT_FOUND
        };
    }
    let is_dir = r.is_dir();
    if is_dir && create_options & FILE_NON_DIRECTORY_FILE != 0 {
        return fsp::STATUS_FILE_IS_A_DIRECTORY;
    }
    if !is_dir && create_options & FILE_DIRECTORY_FILE != 0 {
        return fsp::STATUS_NOT_A_DIRECTORY;
    }
    let wants_write = granted & WRITE_MASK != 0;
    let mut ctx = FileCtx {
        comps: comps.clone(),
        is_dir,
        handle: INVALID_HANDLE_VALUE,
        lower: None,
        dir_buffer: std::ptr::null_mut(),
    };
    if is_dir {
        if wants_write && r.upper_kind.is_none() && !comps.is_empty() {
            if let Err(e) = u.ensure_upper_dir(&comps) {
                return err_status(&e);
            }
        }
        if r.upper_kind.is_some() || (wants_write && !comps.is_empty()) {
            match u.open_upper(&r.upper, granted, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS) {
                Ok(h) => ctx.handle = h,
                Err(s) => return s,
            }
            match u.info_from_handle(ctx.handle) {
                Ok(i) => *info = i,
                Err(s) => {
                    CloseHandle(ctx.handle);
                    return s;
                }
            }
        } else {
            ctx.lower = r.lowers.first().cloned();
            *info = match r.lowers.first() {
                Some((li, ino)) => u.info_from_inode(*li, ino),
                None => u.info_synthetic(),
            };
        }
    } else if let Some((li, ino)) = r.lower_file() {
        if wants_write {
            match u.copy_up(&comps, *li, ino) {
                Ok(p) => match u.open_upper(&p, granted, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS)
                {
                    Ok(h) => ctx.handle = h,
                    Err(s) => return s,
                },
                Err(e) => return err_status(&e),
            }
            match u.info_from_handle(ctx.handle) {
                Ok(i) => *info = i,
                Err(s) => {
                    CloseHandle(ctx.handle);
                    return s;
                }
            }
        } else {
            ctx.lower = Some((*li, ino.clone()));
            *info = u.info_from_inode(*li, ino);
        }
    } else {
        match u.open_upper(&r.upper, granted, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS) {
            Ok(h) => ctx.handle = h,
            Err(s) => return s,
        }
        match u.info_from_handle(ctx.handle) {
            Ok(i) => *info = i,
            Err(s) => {
                CloseHandle(ctx.handle);
                return s;
            }
        }
    }
    *pctx = Box::into_raw(Box::new(ctx)) as *mut c_void;
    fsp::STATUS_SUCCESS
}

unsafe extern "C" fn cb_create(
    fs: *mut FileSystem,
    name: fsp::PWSTR,
    create_options: u32,
    granted: u32,
    mut attrs: u32,
    _sd: *mut c_void,
    _alloc: u64,
    pctx: *mut *mut c_void,
    info: *mut FileInfo,
) -> NTSTATUS {
    let u = union_of(fs);
    let comps = split_path(&fsp::from_wide(name));
    if comps.is_empty() {
        return fsp::STATUS_OBJECT_NAME_COLLISION;
    }
    if comps
        .last()
        .map(|c| c.starts_with(WHITEOUT_PREFIX) || c.starts_with(".cellcopy."))
        .unwrap_or(false)
    {
        return fsp::STATUS_OBJECT_NAME_INVALID;
    }
    let r = match u.resolve(&comps) {
        Ok(r) => r,
        Err(e) => return err_status(&e),
    };
    if r.exists() {
        return fsp::STATUS_OBJECT_NAME_COLLISION;
    }
    if !r.parent_exists {
        return fsp::STATUS_OBJECT_PATH_NOT_FOUND;
    }
    let parent = match u.ensure_upper_dir(&comps[..comps.len() - 1]) {
        Ok(p) => p,
        Err(e) => return err_status(&e),
    };
    let path = parent.join(comps.last().unwrap());
    let is_dir = create_options & FILE_DIRECTORY_FILE != 0;
    let w = wide_path(&path);
    let h = if is_dir {
        if CreateDirectoryW(w.as_ptr(), std::ptr::null()) == 0 {
            return u.last_error();
        }
        match u.open_upper(&path, granted, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS) {
            Ok(h) => h,
            Err(s) => return s,
        }
    } else {
        attrs &= !FILE_ATTRIBUTE_DIRECTORY;
        if attrs == 0 {
            attrs = FILE_ATTRIBUTE_NORMAL;
        }
        match u.open_upper(
            &path,
            granted,
            CREATE_NEW,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_POSIX_SEMANTICS | attrs,
        ) {
            Ok(h) => h,
            Err(s) => return s,
        }
    };
    match u.info_from_handle(h) {
        Ok(i) => *info = i,
        Err(s) => {
            CloseHandle(h);
            return s;
        }
    }
    let ctx = FileCtx {
        comps,
        is_dir,
        handle: h,
        lower: None,
        dir_buffer: std::ptr::null_mut(),
    };
    *pctx = Box::into_raw(Box::new(ctx)) as *mut c_void;
    fsp::STATUS_SUCCESS
}

unsafe extern "C" fn cb_overwrite(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    mut attrs: u32,
    replace: BOOLEAN,
    _alloc: u64,
    info: *mut FileInfo,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    if ctx.handle == INVALID_HANDLE_VALUE {
        return fsp::STATUS_ACCESS_DENIED;
    }
    if replace != 0 {
        if attrs == 0 {
            attrs = FILE_ATTRIBUTE_NORMAL;
        }
        let mut bi: FILE_BASIC_INFO = std::mem::zeroed();
        bi.FileAttributes = attrs;
        if SetFileInformationByHandle(
            ctx.handle,
            FileBasicInfo,
            &bi as *const _ as *const c_void,
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        ) == 0
        {
            return u.last_error();
        }
    }
    let ai = FILE_ALLOCATION_INFO { AllocationSize: 0 };
    if SetFileInformationByHandle(
        ctx.handle,
        FileAllocationInfo,
        &ai as *const _ as *const c_void,
        std::mem::size_of::<FILE_ALLOCATION_INFO>() as u32,
    ) == 0
    {
        return u.last_error();
    }
    match u.info_from_handle(ctx.handle) {
        Ok(i) => {
            *info = i;
            fsp::STATUS_SUCCESS
        }
        Err(s) => s,
    }
}

unsafe extern "C" fn cb_cleanup(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    _name: fsp::PWSTR,
    flags: u32,
) {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    if flags & fsp::FSP_CLEANUP_DELETE == 0 {
        return;
    }
    let in_lower = u.lowers_have(&ctx.comps);
    if ctx.is_dir {
        if ctx.handle != INVALID_HANDLE_VALUE {
            CloseHandle(ctx.handle);
            ctx.handle = INVALID_HANDLE_VALUE;
        }
        let p = u.upper.join(ctx.comps.join("\\"));
        if p.is_dir() {
            let w = wide_path(&p);
            // Whiteouts inside are ours: clear them so the dir is empty.
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().starts_with(WHITEOUT_PREFIX) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            RemoveDirectoryW(w.as_ptr());
        }
    } else if ctx.handle != INVALID_HANDLE_VALUE {
        let di = FILE_DISPOSITION_INFO { DeleteFile: true };
        if SetFileInformationByHandle(
            ctx.handle,
            FileDispositionInfo,
            &di as *const _ as *const c_void,
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        ) == 0
        {
            let p = u.upper.join(ctx.comps.join("\\"));
            let w = wide_path(&p);
            CloseHandle(ctx.handle);
            ctx.handle = INVALID_HANDLE_VALUE;
            DeleteFileW(w.as_ptr());
        }
    }
    if in_lower {
        if let Err(e) = u.write_whiteout(&ctx.comps) {
            eprintln!("cell: unionfs: whiteout: {e:#}");
        }
    }
}

unsafe extern "C" fn cb_close(fs: *mut FileSystem, pctx: *mut c_void) {
    let u = union_of(fs);
    let ctx = Box::from_raw(pctx as *mut FileCtx);
    if ctx.handle != INVALID_HANDLE_VALUE {
        CloseHandle(ctx.handle);
    }
    let mut buf = ctx.dir_buffer;
    (u.api.FspFileSystemDeleteDirectoryBuffer)(&mut buf);
}

unsafe extern "C" fn cb_read(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    buf: *mut c_void,
    offset: u64,
    len: u32,
    pbytes: *mut u32,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    if ctx.handle != INVALID_HANDLE_VALUE {
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.Anonymous.Anonymous.Offset = offset as u32;
        ov.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        if ReadFile(ctx.handle, buf as *mut u8, len, pbytes, &mut ov) == 0 {
            return u.last_error();
        }
        return fsp::STATUS_SUCCESS;
    }
    let Some((li, ino)) = &ctx.lower else {
        return fsp::STATUS_INVALID_DEVICE_REQUEST;
    };
    if offset >= ino.size() {
        return fsp::STATUS_END_OF_FILE;
    }
    match u.lowers[*li].img.read_file(ino, offset, len as usize) {
        Ok(data) => {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf as *mut u8, data.len());
            *pbytes = data.len() as u32;
            fsp::STATUS_SUCCESS
        }
        Err(e) => err_status(&e),
    }
}

unsafe extern "C" fn cb_write(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    buf: *mut c_void,
    mut offset: u64,
    mut len: u32,
    to_end: BOOLEAN,
    constrained: BOOLEAN,
    pbytes: *mut u32,
    info: *mut FileInfo,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    if ctx.handle == INVALID_HANDLE_VALUE {
        return fsp::STATUS_ACCESS_DENIED;
    }
    let cur = match u.info_from_handle(ctx.handle) {
        Ok(i) => i,
        Err(s) => return s,
    };
    if to_end != 0 {
        offset = cur.file_size;
    }
    if constrained != 0 {
        if offset >= cur.file_size {
            *pbytes = 0;
            *info = cur;
            return fsp::STATUS_SUCCESS;
        }
        if offset + len as u64 > cur.file_size {
            len = (cur.file_size - offset) as u32;
        }
    }
    let mut ov: OVERLAPPED = std::mem::zeroed();
    ov.Anonymous.Anonymous.Offset = offset as u32;
    ov.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    if WriteFile(ctx.handle, buf as *const u8, len, pbytes, &mut ov) == 0 {
        return u.last_error();
    }
    match u.info_from_handle(ctx.handle) {
        Ok(i) => {
            *info = i;
            fsp::STATUS_SUCCESS
        }
        Err(s) => s,
    }
}

unsafe extern "C" fn cb_flush(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    info: *mut FileInfo,
) -> NTSTATUS {
    let u = union_of(fs);
    if pctx.is_null() {
        return fsp::STATUS_SUCCESS;
    }
    let ctx = ctx_of(pctx);
    if ctx.handle != INVALID_HANDLE_VALUE {
        if FlushFileBuffers(ctx.handle) == 0 {
            return u.last_error();
        }
        return match u.info_from_handle(ctx.handle) {
            Ok(i) => {
                *info = i;
                fsp::STATUS_SUCCESS
            }
            Err(s) => s,
        };
    }
    cb_get_file_info(fs, pctx, info)
}

unsafe extern "C" fn cb_get_file_info(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    info: *mut FileInfo,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    if ctx.handle != INVALID_HANDLE_VALUE {
        return match u.info_from_handle(ctx.handle) {
            Ok(i) => {
                *info = i;
                fsp::STATUS_SUCCESS
            }
            Err(s) => s,
        };
    }
    if let Some((li, ino)) = &ctx.lower {
        *info = u.info_from_inode(*li, ino);
        return fsp::STATUS_SUCCESS;
    }
    *info = u.info_synthetic();
    fsp::STATUS_SUCCESS
}

unsafe extern "C" fn cb_set_basic_info(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    mut attrs: u32,
    ctime: u64,
    atime: u64,
    wtime: u64,
    _chtime: u64,
    info: *mut FileInfo,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    if ctx.handle == INVALID_HANDLE_VALUE {
        return fsp::STATUS_ACCESS_DENIED;
    }
    if attrs == INVALID_FILE_ATTRIBUTES {
        attrs = 0;
    } else if attrs == 0 {
        attrs = FILE_ATTRIBUTE_NORMAL;
    }
    let mut bi: FILE_BASIC_INFO = std::mem::zeroed();
    bi.FileAttributes = attrs;
    bi.CreationTime = ctime as i64;
    bi.LastAccessTime = atime as i64;
    bi.LastWriteTime = wtime as i64;
    if SetFileInformationByHandle(
        ctx.handle,
        FileBasicInfo,
        &bi as *const _ as *const c_void,
        std::mem::size_of::<FILE_BASIC_INFO>() as u32,
    ) == 0
    {
        return u.last_error();
    }
    match u.info_from_handle(ctx.handle) {
        Ok(i) => {
            *info = i;
            fsp::STATUS_SUCCESS
        }
        Err(s) => s,
    }
}

unsafe extern "C" fn cb_set_file_size(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    new_size: u64,
    set_alloc: BOOLEAN,
    info: *mut FileInfo,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    if ctx.handle == INVALID_HANDLE_VALUE {
        return fsp::STATUS_ACCESS_DENIED;
    }
    let ok = if set_alloc != 0 {
        let ai = FILE_ALLOCATION_INFO {
            AllocationSize: new_size as i64,
        };
        SetFileInformationByHandle(
            ctx.handle,
            FileAllocationInfo,
            &ai as *const _ as *const c_void,
            std::mem::size_of::<FILE_ALLOCATION_INFO>() as u32,
        )
    } else {
        let ei = FILE_END_OF_FILE_INFO {
            EndOfFile: new_size as i64,
        };
        SetFileInformationByHandle(
            ctx.handle,
            FileEndOfFileInfo,
            &ei as *const _ as *const c_void,
            std::mem::size_of::<FILE_END_OF_FILE_INFO>() as u32,
        )
    };
    if ok == 0 {
        return u.last_error();
    }
    match u.info_from_handle(ctx.handle) {
        Ok(i) => {
            *info = i;
            fsp::STATUS_SUCCESS
        }
        Err(s) => s,
    }
}

unsafe extern "C" fn cb_can_delete(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    _name: fsp::PWSTR,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    if !ctx.is_dir {
        return fsp::STATUS_SUCCESS;
    }
    match u.list_dir(&ctx.comps) {
        Ok(l) if l.is_empty() => fsp::STATUS_SUCCESS,
        Ok(_) => fsp::STATUS_DIRECTORY_NOT_EMPTY,
        Err(e) => err_status(&e),
    }
}

unsafe extern "C" fn cb_rename(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    _name: fsp::PWSTR,
    new_name: fsp::PWSTR,
    replace: BOOLEAN,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    let new_comps = split_path(&fsp::from_wide(new_name));
    if new_comps.is_empty() || ctx.comps.is_empty() {
        return fsp::STATUS_ACCESS_DENIED;
    }
    let target = match u.resolve(&new_comps) {
        Ok(r) => r,
        Err(e) => return err_status(&e),
    };
    if !target.parent_exists {
        return fsp::STATUS_OBJECT_PATH_NOT_FOUND;
    }
    let same = new_comps.len() == ctx.comps.len()
        && new_comps
            .iter()
            .zip(ctx.comps.iter())
            .all(|(a, b)| fold(a) == fold(b));
    if target.exists() && !same && (replace == 0 || target.is_dir()) {
        return fsp::STATUS_OBJECT_NAME_COLLISION;
    }
    let src_in_lower = u.lowers_have(&ctx.comps);
    // Make sure the source is fully present in the upper.
    if ctx.is_dir {
        if let Err(e) = u.copy_up_tree(&ctx.comps) {
            return err_status(&e);
        }
    } else if ctx.handle == INVALID_HANDLE_VALUE {
        if let Some((li, ino)) = ctx.lower.clone() {
            if let Err(e) = u.copy_up(&ctx.comps, li, &ino) {
                return err_status(&e);
            }
        }
    }
    let new_parent = match u.ensure_upper_dir(&new_comps[..new_comps.len() - 1]) {
        Ok(p) => p,
        Err(e) => return err_status(&e),
    };
    let src = u.upper.join(ctx.comps.join("\\"));
    let dst = new_parent.join(new_comps.last().unwrap());
    let ws = wide_path(&src);
    let wd = wide_path(&dst);
    let flags = if replace != 0 {
        MOVEFILE_REPLACE_EXISTING
    } else {
        0
    };
    if MoveFileExW(ws.as_ptr(), wd.as_ptr(), flags) == 0 {
        return u.last_error();
    }
    if src_in_lower {
        if let Err(e) = u.write_whiteout(&ctx.comps) {
            return err_status(&e);
        }
    }
    // The target name may have been a lower entry that the upper now
    // shadows; a whiteout keeps a lower directory from merging into it.
    if u.lowers_have(&new_comps) {
        let _ = u.write_whiteout(&new_comps);
    }
    ctx.comps = new_comps;
    fsp::STATUS_SUCCESS
}

unsafe extern "C" fn cb_get_security(
    fs: *mut FileSystem,
    _pctx: *mut c_void,
    sd: *mut c_void,
    psize: *mut usize,
) -> NTSTATUS {
    write_security(union_of(fs), sd, psize)
}

unsafe extern "C" fn cb_set_security(
    _fs: *mut FileSystem,
    _pctx: *mut c_void,
    _si: u32,
    _sd: *mut c_void,
) -> NTSTATUS {
    // ACL changes are accepted and dropped: one descriptor serves the volume.
    fsp::STATUS_SUCCESS
}

unsafe extern "C" fn cb_read_directory(
    fs: *mut FileSystem,
    pctx: *mut c_void,
    _pattern: fsp::PWSTR,
    marker: fsp::PWSTR,
    buf: *mut c_void,
    len: u32,
    pbytes: *mut u32,
) -> NTSTATUS {
    let u = union_of(fs);
    let ctx = ctx_of(pctx);
    let mut result: NTSTATUS = fsp::STATUS_SUCCESS;
    if (u.api.FspFileSystemAcquireDirectoryBuffer)(
        &mut ctx.dir_buffer,
        (marker.is_null()) as u8,
        &mut result,
    ) != 0
    {
        let fill = |u: &Union, ctx: &mut FileCtx| -> Result<NTSTATUS> {
            let mut st = fsp::STATUS_SUCCESS;
            if !ctx.comps.is_empty() {
                let parent = u.resolve(&ctx.comps[..ctx.comps.len() - 1])?;
                let here = u.resolve(&ctx.comps)?;
                let dot = u.info_for(&here).unwrap_or_else(|_| u.info_synthetic());
                let dotdot = u.info_for(&parent).unwrap_or_else(|_| u.info_synthetic());
                for (n, i) in [(".", dot), ("..", dotdot)] {
                    let mut bytes = fsp::dir_info_bytes(n, &i);
                    if (u.api.FspFileSystemFillDirectoryBuffer)(
                        &mut ctx.dir_buffer,
                        bytes.as_mut_ptr() as *mut c_void,
                        &mut st,
                    ) == 0
                    {
                        return Ok(st);
                    }
                }
            }
            for (name, info) in u.list_dir(&ctx.comps)? {
                let mut bytes = fsp::dir_info_bytes(&name, &info);
                if (u.api.FspFileSystemFillDirectoryBuffer)(
                    &mut ctx.dir_buffer,
                    bytes.as_mut_ptr() as *mut c_void,
                    &mut st,
                ) == 0
                {
                    break;
                }
            }
            Ok(st)
        };
        match fill(u, ctx) {
            Ok(st) => result = st,
            Err(e) => result = err_status(&e),
        }
        (u.api.FspFileSystemReleaseDirectoryBuffer)(&mut ctx.dir_buffer);
    }
    if result < 0 {
        return result;
    }
    (u.api.FspFileSystemReadDirectoryBuffer)(&mut ctx.dir_buffer, marker, buf, len, pbytes);
    fsp::STATUS_SUCCESS
}

static INTERFACE: Interface = Interface {
    get_volume_info: Some(cb_get_volume_info),
    set_volume_label: None,
    get_security_by_name: Some(cb_get_security_by_name),
    create: Some(cb_create),
    open: Some(cb_open),
    overwrite: Some(cb_overwrite),
    cleanup: Some(cb_cleanup),
    close: Some(cb_close),
    read: Some(cb_read),
    write: Some(cb_write),
    flush: Some(cb_flush),
    get_file_info: Some(cb_get_file_info),
    set_basic_info: Some(cb_set_basic_info),
    set_file_size: Some(cb_set_file_size),
    can_delete: Some(cb_can_delete),
    rename: Some(cb_rename),
    get_security: Some(cb_get_security),
    set_security: Some(cb_set_security),
    read_directory: Some(cb_read_directory),
    resolve_reparse_points: None,
    get_reparse_point: None,
    set_reparse_point: None,
    delete_reparse_point: None,
    get_stream_info: None,
    get_dir_info_by_name: None,
    control: None,
    set_delete: None,
    create_ex: None,
    overwrite_ex: None,
    get_ea: None,
    set_ea: None,
    obsolete0: None,
    dispatcher_stopped: None,
    reserved: [None; 31],
};

/// A mounted union. Dropping it stops the dispatcher and removes the
/// mount point.
pub struct Mounted {
    fs: *mut FileSystem,
    api: Arc<Api>,
    _union: Arc<Union>,
    pub mount_point: PathBuf,
    /// The kernel name of the volume, `\Device\Volume{...}`.
    pub volume_name: String,
}

unsafe impl Send for Mounted {}

impl Drop for Mounted {
    fn drop(&mut self) {
        // SAFETY: `fs` came from FspFileSystemCreate and is still live.
        unsafe {
            (self.api.FspFileSystemStopDispatcher)(self.fs);
            (self.api.FspFileSystemDelete)(self.fs);
        }
        let _ = std::fs::remove_dir(&self.mount_point);
    }
}

/// Mount the union at `mount_point`, which must not exist yet.
pub fn mount(union: Arc<Union>, mount_point: &Path, debug: bool) -> Result<Mounted> {
    let api = union.api.clone();
    if let Some(parent) = mount_point.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A stale mount point from an unclean exit is an empty directory or a
    // dangling reparse point; both go away with remove_dir.
    let _ = std::fs::remove_dir(mount_point);
    if mount_point.exists() {
        bail!(
            "mount point {} exists and is not empty",
            mount_point.display()
        );
    }
    let mut vp = VolumeParams {
        version: 0,
        sector_size: ALLOCATION_UNIT as u16,
        sectors_per_allocation_unit: 1,
        max_component_length: 255,
        volume_creation_time: union.created,
        volume_serial_number: (union.created & 0xFFFF_FFFF) as u32,
        file_info_timeout: 1000,
        flags: fsp::VP_CASE_PRESERVED_NAMES
            | fsp::VP_UNICODE_ON_DISK
            | fsp::VP_PERSISTENT_ACLS
            | fsp::VP_POST_CLEANUP_WHEN_MODIFIED_ONLY
            | fsp::VP_UM_FILE_CONTEXT_IS_USER_CONTEXT2,
        ..Default::default()
    };
    // The FAQ: a program run as administrator from a WinFsp volume needs
    // the volume to call itself NTFS.
    for (i, c) in "NTFS".encode_utf16().enumerate() {
        vp.file_system_name[i] = c;
    }
    let dev = fsp::disk_device_path();
    let mut fs: *mut FileSystem = std::ptr::null_mut();
    // SAFETY: valid pointers; INTERFACE is 'static.
    let st = unsafe { (api.FspFileSystemCreate)(dev.as_ptr(), &vp, &INTERFACE, &mut fs) };
    if st < 0 || fs.is_null() {
        bail!("FspFileSystemCreate failed: 0x{:08x}", st as u32);
    }
    let raw = Arc::into_raw(union.clone()) as *mut c_void;
    unsafe { fsp::set_user_context(fs, raw) };
    if debug {
        if let (Some(set), Some(handle)) = (api.FspFileSystemSetDebugLogF, api.FspDebugLogSetHandle)
        {
            use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
            unsafe {
                handle(GetStdHandle(STD_ERROR_HANDLE));
                set(fs, u32::MAX);
            }
        }
    }
    let mp = fsp::wide(&mount_point.to_string_lossy());
    let st = unsafe { (api.FspFileSystemSetMountPoint)(fs, mp.as_ptr()) };
    if st < 0 {
        unsafe { (api.FspFileSystemDelete)(fs) };
        bail!(
            "cannot mount at {}: 0x{:08x}",
            mount_point.display(),
            st as u32
        );
    }
    let st = unsafe { (api.FspFileSystemStartDispatcher)(fs, 0) };
    if st < 0 {
        unsafe { (api.FspFileSystemDelete)(fs) };
        bail!("cannot start the file system: 0x{:08x}", st as u32);
    }
    let _ = ERROR_FILE_NOT_FOUND;
    // SAFETY: `fs` is live.
    let volume_name = unsafe { fsp::volume_name(fs) };
    Ok(Mounted {
        fs,
        api,
        _union: union,
        mount_point: mount_point.to_path_buf(),
        volume_name,
    })
}
