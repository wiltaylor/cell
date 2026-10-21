//! Shared pieces of the Cell format.
//!
//! - [`format`]: the trailer at the end of a `.cell` file and the manifest.
//! - [`squashfs`]: a read-only squashfs reader with random access.
//! - [`fusefs`]: a FUSE server that exposes one squashfs layer.
//! - [`sandbox`]: user namespaces, mounts, grants and the final `exec`.
//! - [`host`]: what the host offers (kernel, display, GPU).
//! - [`nvidia_fetch`]: the NVIDIA driver cache for `:fetch`.
//! - [`extract`]: unpack a squashfs layer into a directory (the no-mount tier).
//! - [`winfsp`], [`unionfs`]: on Windows, the WinFsp binding and the union
//!   file system that merges the layers with a writable upper directory.

pub mod extract;
pub mod format;
#[cfg(target_os = "linux")]
pub mod fusefs;
#[cfg(target_os = "linux")]
pub mod gpu;
pub mod host;
#[cfg(target_os = "linux")]
pub mod nvidia_fetch;
pub mod pread;
pub mod registry;
#[cfg(target_os = "linux")]
pub mod sandbox;
pub mod squashfs;
#[cfg(windows)]
pub mod unionfs;
#[cfg(windows)]
pub mod winfsp;
#[cfg(target_os = "linux")]
pub mod xauth;
