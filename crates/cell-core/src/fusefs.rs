//! A FUSE server that exposes one squashfs layer read-only.
//!
//! The mount is made with a direct `mount(2)` on `/dev/fuse`, which works
//! inside an unprivileged user namespace. No `fusermount` is involved.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory,
    ReplyEntry, ReplyOpen, Request, FUSE_ROOT_ID,
};

use crate::squashfs::{DirEntry, Inode, InodeKind, InodeRef, Squashfs};

const TTL: Duration = Duration::from_secs(86400);

pub struct LayerFs {
    fs: Arc<Squashfs>,
    refs: Vec<Option<InodeRef>>,
    root_number: u32,
    inodes: HashMap<u32, Arc<Inode>>,
    dirs: HashMap<u32, Arc<Vec<DirEntry>>>,
}

impl LayerFs {
    pub fn new(fs: Squashfs) -> Result<Self> {
        let refs = fs.index()?;
        let root = fs.read_inode(fs.root_ref())?;
        Ok(LayerFs {
            fs: Arc::new(fs),
            refs,
            root_number: root.number,
            inodes: HashMap::new(),
            dirs: HashMap::new(),
        })
    }

    /// FUSE ino <-> squashfs inode number. Root must be ino 1, so the two
    /// numbers swap places. The map is its own inverse.
    fn swap(&self, n: u64) -> u64 {
        if n == FUSE_ROOT_ID {
            self.root_number as u64
        } else if n == self.root_number as u64 {
            FUSE_ROOT_ID
        } else {
            n
        }
    }

    fn inode(&mut self, ino: u64) -> Option<Arc<Inode>> {
        let num = self.swap(ino) as u32;
        if let Some(i) = self.inodes.get(&num) {
            return Some(i.clone());
        }
        let r = (*self.refs.get(num as usize)?)?;
        let inode = Arc::new(self.fs.read_inode(r).ok()?);
        self.inodes.insert(num, inode.clone());
        Some(inode)
    }

    fn listing(&mut self, ino: u64) -> Option<Arc<Vec<DirEntry>>> {
        let num = self.swap(ino) as u32;
        if let Some(d) = self.dirs.get(&num) {
            return Some(d.clone());
        }
        let inode = self.inode(ino)?;
        let list = Arc::new(self.fs.read_dir(&inode).ok()?);
        self.dirs.insert(num, list.clone());
        Some(list)
    }

    fn attr(&self, inode: &Inode) -> FileAttr {
        let (kind, rdev) = match &inode.kind {
            InodeKind::Dir { .. } => (FileType::Directory, 0),
            InodeKind::File { .. } => (FileType::RegularFile, 0),
            InodeKind::Symlink(_) => (FileType::Symlink, 0),
            InodeKind::CharDev(d) => (FileType::CharDevice, *d),
            InodeKind::BlockDev(d) => (FileType::BlockDevice, *d),
            InodeKind::Fifo => (FileType::NamedPipe, 0),
            InodeKind::Socket => (FileType::Socket, 0),
        };
        let t = UNIX_EPOCH + Duration::from_secs(inode.mtime as u64);
        let size = inode.size();
        FileAttr {
            ino: self.swap(inode.number as u64),
            size,
            blocks: size.div_ceil(512),
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind,
            perm: inode.perm,
            nlink: inode.nlink.max(1),
            uid: inode.uid,
            gid: inode.gid,
            rdev,
            blksize: self.fs.sb.block_size,
            flags: 0,
        }
    }
}

fn ftype(kind: u16) -> FileType {
    match kind {
        1 | 8 => FileType::Directory,
        2 | 9 => FileType::RegularFile,
        3 | 10 => FileType::Symlink,
        4 | 11 => FileType::BlockDevice,
        5 | 12 => FileType::CharDevice,
        6 | 13 => FileType::NamedPipe,
        _ => FileType::Socket,
    }
}

impl Filesystem for LayerFs {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let Some(list) = self.listing(parent) else {
            reply.error(libc::ENOENT);
            return;
        };
        let Some(e) = list.iter().find(|e| e.name.as_slice() == name.as_bytes()) else {
            reply.error(libc::ENOENT);
            return;
        };
        let ino = self.swap(e.inode_number as u64);
        match self.inode(ino) {
            Some(i) => reply.entry(&TTL, &self.attr(&i), 0),
            None => reply.error(libc::ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        match self.inode(ino) {
            Some(i) => reply.attr(&TTL, &self.attr(&i)),
            None => reply.error(libc::ENOENT),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.inode(ino) {
            Some(i) => match &i.kind {
                InodeKind::Symlink(t) => reply.data(t),
                _ => reply.error(libc::EINVAL),
            },
            None => reply.error(libc::ENOENT),
        }
    }

    fn open(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: ReplyOpen) {
        reply.opened(0, fuser::consts::FOPEN_KEEP_CACHE);
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let Some(i) = self.inode(ino) else {
            reply.error(libc::ENOENT);
            return;
        };
        match self.fs.read_file(&i, offset.max(0) as u64, size as usize) {
            Ok(d) => reply.data(&d),
            Err(_) => reply.error(libc::EIO),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let Some(list) = self.listing(ino) else {
            reply.error(libc::ENOTDIR);
            return;
        };
        let parent = match self.inode(ino).map(|i| i.kind.clone()) {
            Some(InodeKind::Dir { parent, .. }) => {
                if ino == FUSE_ROOT_ID {
                    FUSE_ROOT_ID
                } else {
                    self.swap(parent as u64)
                }
            }
            _ => FUSE_ROOT_ID,
        };
        let mut idx = offset as usize;
        loop {
            let full = match idx {
                0 => reply.add(ino, 1, FileType::Directory, "."),
                1 => reply.add(parent, 2, FileType::Directory, ".."),
                n => {
                    let Some(e) = list.get(n - 2) else { break };
                    let child = self.swap(e.inode_number as u64);
                    reply.add(child, (n + 1) as i64, ftype(e.kind), OsStr::from_bytes(&e.name))
                }
            };
            if full {
                break;
            }
            idx += 1;
        }
        reply.ok();
    }
}

/// Mount a squashfs image (at `base` inside `file`) on `mountpoint`. The
/// returned session must stay alive for as long as the mount is in use.
pub fn mount_layer(file: std::fs::File, base: u64, mountpoint: &Path, name: &str) -> Result<fuser::BackgroundSession> {
    let fs = Squashfs::open(file, base).context("open squashfs layer")?;
    let layer = LayerFs::new(fs)?;
    let opts = [
        MountOption::RO,
        MountOption::FSName(format!("cell:{name}")),
        MountOption::Subtype("cell".to_string()),
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
    ];
    fuser::spawn_mount2(layer, mountpoint, &opts)
        .with_context(|| format!("mount layer {name} at {}", mountpoint.display()))
}
