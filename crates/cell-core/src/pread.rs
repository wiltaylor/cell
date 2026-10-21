//! Positional reads that work on Linux and Windows.
//!
//! The squashfs reader and the trailer reader need `pread`-style access to
//! a file that several threads share. Unix has `FileExt::read_exact_at`;
//! Windows has `seek_read`, which moves the file pointer, so it is wrapped
//! in a loop here and callers never rely on the pointer.

use std::fs::File;
use std::io;

#[cfg(unix)]
pub fn read_exact_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, off)
}

#[cfg(windows)]
pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, off) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ))
            }
            Ok(n) => {
                buf = &mut buf[n..];
                off += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
