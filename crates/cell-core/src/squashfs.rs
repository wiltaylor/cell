//! A read-only squashfs 4.0 reader with random access to file data.
//!
//! The image may start at any offset inside a bigger file, which is how
//! layers live inside a `.cell`. Metadata blocks and data blocks are cached
//! in small LRU maps. Only zstd, gzip and uncompressed blocks are read.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Read;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};

const MAGIC: u32 = 0x7371_7368;
const META_BLOCK: usize = 8192;
pub const FRAG_NONE: u32 = 0xFFFF_FFFF;

const COMP_GZIP: u16 = 1;
const COMP_ZSTD: u16 = 6;

const FLAG_UNCOMP_INODES: u16 = 0x0001;
const FLAG_UNCOMP_DATA: u16 = 0x0002;
const FLAG_UNCOMP_FRAGS: u16 = 0x0008;
const FLAG_UNCOMP_IDS: u16 = 0x0800;

#[derive(Debug, Clone, Copy)]
pub struct Superblock {
    pub inode_count: u32,
    pub block_size: u32,
    pub frag_count: u32,
    pub compressor: u16,
    pub flags: u16,
    pub id_count: u16,
    pub root_inode: u64,
    pub bytes_used: u64,
    pub id_table_start: u64,
    pub inode_table_start: u64,
    pub dir_table_start: u64,
    pub frag_table_start: u64,
    pub export_table_start: u64,
}

/// A reference into the inode table: absolute metadata block offset plus
/// the offset inside the decompressed block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InodeRef {
    pub block: u64,
    pub offset: u16,
}

#[derive(Debug, Clone)]
pub enum InodeKind {
    Dir {
        /// Absolute offset of the directory table block.
        block: u64,
        offset: u16,
        /// Listing size in bytes (already minus 3).
        size: u32,
        parent: u32,
    },
    File {
        blocks_start: u64,
        size: u64,
        frag_index: u32,
        frag_offset: u32,
        /// Raw block size words (bit 24 = uncompressed, 0 = sparse).
        block_sizes: Vec<u32>,
        /// Absolute start offset of each block.
        block_offsets: Vec<u64>,
    },
    Symlink(Vec<u8>),
    CharDev(u32),
    BlockDev(u32),
    Fifo,
    Socket,
}

#[derive(Debug, Clone)]
pub struct Inode {
    pub number: u32,
    pub perm: u16,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u32,
    pub nlink: u32,
    pub kind: InodeKind,
}

impl Inode {
    pub fn size(&self) -> u64 {
        match &self.kind {
            InodeKind::File { size, .. } => *size,
            InodeKind::Symlink(t) => t.len() as u64,
            InodeKind::Dir { size, .. } => *size as u64,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub inode_ref: InodeRef,
    pub inode_number: u32,
    /// Squashfs basic type number (1 dir, 2 file, 3 symlink, ...).
    pub kind: u16,
}

struct Lru {
    map: HashMap<u64, Arc<Vec<u8>>>,
    order: VecDeque<u64>,
    cap: usize,
}

impl Lru {
    fn new(cap: usize) -> Self {
        Lru {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap,
        }
    }
    fn get(&self, k: u64) -> Option<Arc<Vec<u8>>> {
        self.map.get(&k).cloned()
    }
    fn put(&mut self, k: u64, v: Arc<Vec<u8>>) {
        if self.map.insert(k, v).is_none() {
            self.order.push_back(k);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

/// A decompressed metadata block and the absolute offset of the next one.
type MetaEntry = (Arc<Vec<u8>>, u64);

pub struct Squashfs {
    file: File,
    base: u64,
    pub sb: Superblock,
    ids: Vec<u32>,
    frags: Vec<(u64, u32)>,
    /// Metadata block cache: abs offset -> (data, next block abs offset).
    meta: Mutex<HashMap<u64, MetaEntry>>,
    data: Mutex<Lru>,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

impl Squashfs {
    /// Open an image that starts at `base` inside `file`.
    pub fn open(file: File, base: u64) -> Result<Self> {
        let mut sbb = [0u8; 96];
        crate::pread::read_exact_at(&file, &mut sbb, base).context("read superblock")?;
        if u32_at(&sbb, 0) != MAGIC {
            bail!("not a squashfs image at offset {base}");
        }
        let sb = Superblock {
            inode_count: u32_at(&sbb, 4),
            block_size: u32_at(&sbb, 12),
            frag_count: u32_at(&sbb, 16),
            compressor: u16_at(&sbb, 20),
            flags: u16_at(&sbb, 24),
            id_count: u16_at(&sbb, 26),
            root_inode: u64_at(&sbb, 32),
            bytes_used: u64_at(&sbb, 40),
            id_table_start: u64_at(&sbb, 48),
            inode_table_start: u64_at(&sbb, 64),
            dir_table_start: u64_at(&sbb, 72),
            frag_table_start: u64_at(&sbb, 80),
            export_table_start: u64_at(&sbb, 88),
        };
        if u16_at(&sbb, 28) != 4 {
            bail!("unsupported squashfs major version {}", u16_at(&sbb, 28));
        }
        if sb.compressor != COMP_ZSTD && sb.compressor != COMP_GZIP {
            bail!("unsupported squashfs compressor id {}", sb.compressor);
        }
        let mut fs = Squashfs {
            file,
            base,
            sb,
            ids: Vec::new(),
            frags: Vec::new(),
            meta: Mutex::new(HashMap::new()),
            data: Mutex::new(Lru::new(48)),
        };
        fs.ids = fs.read_id_table()?;
        fs.frags = fs.read_frag_table()?;
        Ok(fs)
    }

    fn read_at(&self, buf: &mut [u8], off: u64) -> Result<()> {
        crate::pread::read_exact_at(&self.file, buf, self.base + off)
            .with_context(|| format!("read {} bytes at {}", buf.len(), off))
    }

    fn decompress(&self, src: &[u8], cap: usize) -> Result<Vec<u8>> {
        match self.sb.compressor {
            COMP_ZSTD => zstd::bulk::decompress(src, cap).context("zstd block"),
            COMP_GZIP => {
                let mut out = Vec::with_capacity(cap);
                flate2::read::ZlibDecoder::new(src)
                    .read_to_end(&mut out)
                    .context("gzip block")?;
                Ok(out)
            }
            other => bail!("compressor {other} not supported"),
        }
    }

    /// Read one metadata block at an absolute offset. Returns the data and
    /// the absolute offset of the block that follows it.
    fn meta_block(&self, abs: u64, uncompressed_flag: bool) -> Result<MetaEntry> {
        if let Some(hit) = self.meta.lock().unwrap().get(&abs) {
            return Ok(hit.clone());
        }
        let mut hdr = [0u8; 2];
        self.read_at(&mut hdr, abs)?;
        let word = u16::from_le_bytes(hdr);
        let size = (word & 0x7FFF) as usize;
        let raw = word & 0x8000 != 0 || uncompressed_flag;
        let mut buf = vec![0u8; size];
        self.read_at(&mut buf, abs + 2)?;
        let data = if raw {
            buf
        } else {
            self.decompress(&buf, META_BLOCK)?
        };
        let next = abs + 2 + size as u64;
        let entry = (Arc::new(data), next);
        self.meta.lock().unwrap().insert(abs, entry.clone());
        Ok(entry)
    }

    fn cursor(&self, block: u64, offset: u16, table: Table) -> MetaCursor<'_> {
        MetaCursor {
            fs: self,
            block,
            off: offset as usize,
            buf: None,
            next: 0,
            raw: match table {
                Table::Inode | Table::Dir => self.sb.flags & FLAG_UNCOMP_INODES != 0,
            },
        }
    }

    fn read_id_table(&self) -> Result<Vec<u32>> {
        let count = self.sb.id_count as usize;
        let blocks = count.div_ceil(2048);
        let mut idx = vec![0u8; blocks * 8];
        self.read_at(&mut idx, self.sb.id_table_start)?;
        let mut ids = Vec::with_capacity(count);
        for b in 0..blocks {
            let abs = u64_at(&idx, b * 8);
            let (data, _) = self.meta_block(abs, self.sb.flags & FLAG_UNCOMP_IDS != 0)?;
            let n = (count - ids.len()).min(2048);
            for i in 0..n {
                ids.push(u32_at(&data, i * 4));
            }
        }
        Ok(ids)
    }

    fn read_frag_table(&self) -> Result<Vec<(u64, u32)>> {
        let count = self.sb.frag_count as usize;
        if count == 0 {
            return Ok(Vec::new());
        }
        let blocks = count.div_ceil(512);
        let mut idx = vec![0u8; blocks * 8];
        self.read_at(&mut idx, self.sb.frag_table_start)?;
        let mut frags = Vec::with_capacity(count);
        for b in 0..blocks {
            let abs = u64_at(&idx, b * 8);
            let (data, _) = self.meta_block(abs, self.sb.flags & FLAG_UNCOMP_FRAGS != 0)?;
            let n = (count - frags.len()).min(512);
            for i in 0..n {
                frags.push((u64_at(&data, i * 16), u32_at(&data, i * 16 + 8)));
            }
        }
        Ok(frags)
    }

    pub fn root_ref(&self) -> InodeRef {
        InodeRef {
            block: self.sb.inode_table_start + (self.sb.root_inode >> 16),
            offset: (self.sb.root_inode & 0xFFFF) as u16,
        }
    }

    fn id(&self, idx: u16) -> u32 {
        self.ids.get(idx as usize).copied().unwrap_or(0)
    }

    pub fn read_inode(&self, r: InodeRef) -> Result<Inode> {
        let mut c = self.cursor(r.block, r.offset, Table::Inode);
        let h = c.take(16)?;
        let itype = u16_at(&h, 0);
        let perm = u16_at(&h, 2);
        let uid = self.id(u16_at(&h, 4));
        let gid = self.id(u16_at(&h, 6));
        let mtime = u32_at(&h, 8);
        let number = u32_at(&h, 12);
        let bs = self.sb.block_size as u64;
        let (nlink, kind) = match itype {
            1 => {
                let b = c.take(16)?;
                let block = self.sb.dir_table_start + u32_at(&b, 0) as u64;
                let nlink = u32_at(&b, 4);
                let size = u16_at(&b, 8) as u32;
                let offset = u16_at(&b, 10);
                let parent = u32_at(&b, 12);
                (
                    nlink,
                    InodeKind::Dir {
                        block,
                        offset,
                        size: size.saturating_sub(3),
                        parent,
                    },
                )
            }
            8 => {
                let b = c.take(24)?;
                let nlink = u32_at(&b, 0);
                let size = u32_at(&b, 4);
                let block = self.sb.dir_table_start + u32_at(&b, 8) as u64;
                let parent = u32_at(&b, 12);
                let offset = u16_at(&b, 18);
                (
                    nlink,
                    InodeKind::Dir {
                        block,
                        offset,
                        size: size.saturating_sub(3),
                        parent,
                    },
                )
            }
            2 => {
                let b = c.take(16)?;
                let blocks_start = u32_at(&b, 0) as u64;
                let frag_index = u32_at(&b, 4);
                let frag_offset = u32_at(&b, 8);
                let size = u32_at(&b, 12) as u64;
                let kind =
                    self.file_kind(&mut c, blocks_start, size, frag_index, frag_offset, bs)?;
                (1, kind)
            }
            9 => {
                let b = c.take(40)?;
                let blocks_start = u64_at(&b, 0);
                let size = u64_at(&b, 8);
                let nlink = u32_at(&b, 24);
                let frag_index = u32_at(&b, 28);
                let frag_offset = u32_at(&b, 32);
                let kind =
                    self.file_kind(&mut c, blocks_start, size, frag_index, frag_offset, bs)?;
                (nlink, kind)
            }
            3 | 10 => {
                let b = c.take(8)?;
                let nlink = u32_at(&b, 0);
                let tlen = u32_at(&b, 4) as usize;
                let target = c.take(tlen)?;
                (nlink, InodeKind::Symlink(target))
            }
            4 | 5 => {
                let b = c.take(8)?;
                let nlink = u32_at(&b, 0);
                let dev = u32_at(&b, 4);
                let kind = if itype == 4 {
                    InodeKind::BlockDev(dev)
                } else {
                    InodeKind::CharDev(dev)
                };
                (nlink, kind)
            }
            11 | 12 => {
                let b = c.take(12)?;
                let nlink = u32_at(&b, 0);
                let dev = u32_at(&b, 4);
                let kind = if itype == 11 {
                    InodeKind::BlockDev(dev)
                } else {
                    InodeKind::CharDev(dev)
                };
                (nlink, kind)
            }
            6 | 7 => {
                let b = c.take(4)?;
                let nlink = u32_at(&b, 0);
                (
                    nlink,
                    if itype == 6 {
                        InodeKind::Fifo
                    } else {
                        InodeKind::Socket
                    },
                )
            }
            13 | 14 => {
                let b = c.take(8)?;
                let nlink = u32_at(&b, 0);
                (
                    nlink,
                    if itype == 13 {
                        InodeKind::Fifo
                    } else {
                        InodeKind::Socket
                    },
                )
            }
            other => bail!("unknown inode type {other}"),
        };
        Ok(Inode {
            number,
            perm,
            uid,
            gid,
            mtime,
            nlink,
            kind,
        })
    }

    fn file_kind(
        &self,
        c: &mut MetaCursor<'_>,
        blocks_start: u64,
        size: u64,
        frag_index: u32,
        frag_offset: u32,
        bs: u64,
    ) -> Result<InodeKind> {
        let nblocks = if frag_index == FRAG_NONE {
            size.div_ceil(bs)
        } else {
            size / bs
        } as usize;
        let raw = c.take(nblocks * 4)?;
        let mut block_sizes = Vec::with_capacity(nblocks);
        let mut block_offsets = Vec::with_capacity(nblocks);
        let mut off = blocks_start;
        for i in 0..nblocks {
            let w = u32_at(&raw, i * 4);
            block_sizes.push(w);
            block_offsets.push(off);
            off += (w & 0x00FF_FFFF) as u64;
        }
        Ok(InodeKind::File {
            blocks_start,
            size,
            frag_index,
            frag_offset,
            block_sizes,
            block_offsets,
        })
    }

    /// Read a directory listing.
    pub fn read_dir(&self, inode: &Inode) -> Result<Vec<DirEntry>> {
        let InodeKind::Dir {
            block,
            offset,
            size,
            ..
        } = inode.kind
        else {
            bail!("not a directory");
        };
        let mut c = self.cursor(block, offset, Table::Dir);
        let mut out = Vec::new();
        let mut consumed = 0usize;
        let total = size as usize;
        while consumed < total {
            let h = c.take(12)?;
            consumed += 12;
            let count = u32_at(&h, 0) as usize + 1;
            let start = u32_at(&h, 4) as u64;
            let base_num = u32_at(&h, 8) as i64;
            for _ in 0..count {
                let e = c.take(8)?;
                consumed += 8;
                let eoff = u16_at(&e, 0);
                let delta = i16::from_le_bytes([e[2], e[3]]) as i64;
                let kind = u16_at(&e, 4);
                let nlen = u16_at(&e, 6) as usize + 1;
                let name = c.take(nlen)?;
                consumed += nlen;
                out.push(DirEntry {
                    name,
                    inode_ref: InodeRef {
                        block: self.sb.inode_table_start + start,
                        offset: eoff,
                    },
                    inode_number: (base_num + delta) as u32,
                    kind,
                });
            }
        }
        Ok(out)
    }

    fn data_block(
        &self,
        abs: u64,
        word: u32,
        uncompressed_flag: bool,
        cap: usize,
    ) -> Result<Arc<Vec<u8>>> {
        if let Some(hit) = self.data.lock().unwrap().get(abs) {
            return Ok(hit);
        }
        let size = (word & 0x00FF_FFFF) as usize;
        let raw = word & 0x0100_0000 != 0 || uncompressed_flag;
        let mut buf = vec![0u8; size];
        self.read_at(&mut buf, abs)?;
        let data = if raw {
            buf
        } else {
            self.decompress(&buf, cap)?
        };
        let data = Arc::new(data);
        self.data.lock().unwrap().put(abs, data.clone());
        Ok(data)
    }

    /// Read `len` bytes of a regular file starting at `offset`.
    pub fn read_file(&self, inode: &Inode, offset: u64, len: usize) -> Result<Vec<u8>> {
        let InodeKind::File {
            size,
            frag_index,
            frag_offset,
            ref block_sizes,
            ref block_offsets,
            ..
        } = inode.kind
        else {
            bail!("not a regular file");
        };
        if offset >= size {
            return Ok(Vec::new());
        }
        let bs = self.sb.block_size as u64;
        let end = (offset + len as u64).min(size);
        let mut out = Vec::with_capacity((end - offset) as usize);
        let mut pos = offset;
        let uncomp_data = self.sb.flags & FLAG_UNCOMP_DATA != 0;
        let uncomp_frag = self.sb.flags & FLAG_UNCOMP_FRAGS != 0;
        while pos < end {
            let bi = (pos / bs) as usize;
            let within = (pos % bs) as usize;
            let (chunk, chunk_len): (Option<Arc<Vec<u8>>>, usize) = if bi < block_sizes.len() {
                let w = block_sizes[bi];
                let this_len = ((size - bi as u64 * bs).min(bs)) as usize;
                if w & 0x00FF_FFFF == 0 {
                    (None, this_len)
                } else {
                    (
                        Some(self.data_block(block_offsets[bi], w, uncomp_data, bs as usize)?),
                        this_len,
                    )
                }
            } else {
                // Tail fragment.
                if frag_index == FRAG_NONE {
                    bail!("read past the last block");
                }
                let (fstart, fword) = *self
                    .frags
                    .get(frag_index as usize)
                    .context("fragment index out of range")?;
                let frag = self.data_block(fstart, fword, uncomp_frag, bs as usize)?;
                let tail_len = (size - bi as u64 * bs) as usize;
                let fo = frag_offset as usize;
                let slice = frag
                    .get(fo..fo + tail_len)
                    .context("fragment too short")?
                    .to_vec();
                (Some(Arc::new(slice)), tail_len)
            };
            let take = ((end - pos) as usize).min(chunk_len - within);
            match chunk {
                Some(c) => out.extend_from_slice(&c[within..within + take]),
                None => out.resize(out.len() + take, 0),
            }
            pos += take as u64;
        }
        Ok(out)
    }

    /// Walk the whole tree once and return `inode_number -> InodeRef`.
    pub fn index(&self) -> Result<Vec<Option<InodeRef>>> {
        let mut refs: Vec<Option<InodeRef>> = vec![None; self.sb.inode_count as usize + 1];
        let root = self.read_inode(self.root_ref())?;
        if let Some(slot) = refs.get_mut(root.number as usize) {
            *slot = Some(self.root_ref());
        }
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for e in self.read_dir(&dir)? {
                if let Some(slot) = refs.get_mut(e.inode_number as usize) {
                    if slot.is_none() {
                        *slot = Some(e.inode_ref);
                        if e.kind == 1 {
                            stack.push(self.read_inode(e.inode_ref)?);
                        }
                    }
                }
            }
        }
        Ok(refs)
    }
}

#[derive(Clone, Copy)]
enum Table {
    Inode,
    Dir,
}

/// Sequential reader over the metadata block chain.
struct MetaCursor<'a> {
    fs: &'a Squashfs,
    block: u64,
    off: usize,
    buf: Option<Arc<Vec<u8>>>,
    next: u64,
    raw: bool,
}

impl<'a> MetaCursor<'a> {
    fn fill(&mut self) -> Result<()> {
        if self.buf.is_none() {
            let (data, next) = self.fs.meta_block(self.block, self.raw)?;
            self.buf = Some(data);
            self.next = next;
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            self.fill()?;
            let buf = self.buf.as_ref().unwrap();
            if self.off >= buf.len() {
                self.block = self.next;
                self.off = 0;
                self.buf = None;
                continue;
            }
            let avail = buf.len() - self.off;
            let want = (n - out.len()).min(avail);
            out.extend_from_slice(&buf[self.off..self.off + want]);
            self.off += want;
        }
        Ok(out)
    }
}
