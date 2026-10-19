//! Shared pieces of the Cell format.
//!
//! - [`format`]: the trailer at the end of a `.cell` file and the manifest.
//! - [`squashfs`]: a read-only squashfs reader with random access.
//! - [`fusefs`]: a FUSE server that exposes one squashfs layer.
//! - [`sandbox`]: user namespaces, mounts, grants and the final `exec`.
//! - [`host`]: what the host offers (kernel, display, GPU).
//! - [`nvidia_fetch`]: the NVIDIA driver cache for `:fetch`.

pub mod format;
pub mod fusefs;
pub mod gpu;
pub mod host;
pub mod nvidia_fetch;
pub mod sandbox;
pub mod squashfs;
pub mod xauth;
