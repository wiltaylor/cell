//! NVIDIA driver discovery. Mesa needs nothing from the host; the
//! proprietary NVIDIA userspace must match the loaded kernel module. The
//! libraries come from the host (`:host`) or from the cache that
//! [`crate::nvidia_fetch`] fills (`:fetch`). Both give [`DriverDir`]s.

use std::path::{Path, PathBuf};

/// Library name prefixes that belong to the NVIDIA userspace driver.
const NVIDIA_PREFIXES: &[&str] = &[
    "libnvidia-",
    "libcuda.",
    "libcudadebugger.",
    "libnvcuvid.",
    "libnvoptix.",
    "libEGL_nvidia.",
    "libGLX_nvidia.",
    "libGLESv1_CM_nvidia.",
    "libGLESv2_nvidia.",
    "libnvidia-egl-",
    "libnvidia-vulkan-producer.",
];

/// Whether `name` is a library file of the NVIDIA userspace driver.
pub fn is_nvidia_lib(name: &str) -> bool {
    NVIDIA_PREFIXES.iter().any(|p| name.starts_with(p)) && name.contains(".so")
}

/// A directory of NVIDIA libraries, on the host or in the fetch cache.
#[derive(Debug, Clone)]
pub struct DriverDir {
    /// Directory that holds the libraries.
    pub dir: PathBuf,
    /// Library file names found in it.
    pub libs: Vec<String>,
    /// Whether this is a 32-bit directory.
    pub lib32: bool,
}

/// The NVIDIA library names in `dir`, sorted.
pub fn scan_driver_dir(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if is_nvidia_lib(&name) {
            out.push(name);
        }
    }
    out.sort();
    out
}

/// The `lib/` and `lib32/` directories of a fetch cache that hold
/// libraries.
pub fn fetched_driver_dirs(cache: &Path) -> Vec<DriverDir> {
    let mut out = Vec::new();
    for (sub, lib32) in [("lib", false), ("lib32", true)] {
        let dir = cache.join(sub);
        let libs = scan_driver_dir(&dir);
        if !libs.is_empty() {
            out.push(DriverDir { dir, libs, lib32 });
        }
    }
    out
}

/// Find host NVIDIA library directories. NixOS first, then the FHS
/// directories of the common distributions.
pub fn find_host_driver_dirs() -> Vec<DriverDir> {
    let candidates64 = [
        "/run/opengl-driver/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/usr/lib/nvidia",
    ];
    let candidates32 = [
        "/run/opengl-driver-32/lib",
        "/usr/lib/i386-linux-gnu",
        "/usr/lib32",
        "/usr/lib/nvidia32",
    ];
    let mut out = Vec::new();
    for (list, lib32) in [(candidates64.as_slice(), false), (candidates32.as_slice(), true)] {
        for c in list {
            let p = Path::new(c);
            let libs = scan_driver_dir(p);
            if !libs.is_empty() {
                out.push(DriverDir {
                    dir: p.to_path_buf(),
                    libs,
                    lib32,
                });
                break;
            }
        }
    }
    out
}

/// Extra host files the NVIDIA stack needs beside the libraries.
pub fn host_extra_files() -> Vec<(PathBuf, &'static str)> {
    let mut v = Vec::new();
    for (p, rel) in [
        ("/usr/lib/gbm/nvidia-drm_gbm.so", "gbm/nvidia-drm_gbm.so"),
        ("/usr/lib/x86_64-linux-gnu/gbm/nvidia-drm_gbm.so", "gbm/nvidia-drm_gbm.so"),
        ("/run/opengl-driver/lib/gbm/nvidia-drm_gbm.so", "gbm/nvidia-drm_gbm.so"),
        ("/usr/lib/vdpau/libvdpau_nvidia.so.1", "vdpau/libvdpau_nvidia.so.1"),
        ("/usr/lib/x86_64-linux-gnu/vdpau/libvdpau_nvidia.so.1", "vdpau/libvdpau_nvidia.so.1"),
    ] {
        if Path::new(p).exists() {
            v.push((PathBuf::from(p), rel));
        }
    }
    v
}

/// The glvnd EGL vendor file for the NVIDIA driver.
pub fn egl_vendor_json(lib_dir: &str) -> String {
    format!(
        "{{\n  \"file_format_version\": \"1.0.0\",\n  \"ICD\": {{\n    \"library_path\": \"{lib_dir}/libEGL_nvidia.so.0\"\n  }}\n}}\n"
    )
}

/// The Vulkan ICD file for the NVIDIA driver.
pub fn vulkan_icd_json(lib_dir: &str, version: &str) -> String {
    format!(
        "{{\n  \"file_format_version\": \"1.0.1\",\n  \"ICD\": {{\n    \"library_path\": \"{lib_dir}/libGLX_nvidia.so.0\",\n    \"api_version\": \"1.4.0\"\n  }},\n  \"driver_version\": \"{version}\"\n}}\n"
    )
}

/// The EGL external platform file for Wayland on NVIDIA.
pub fn egl_external_platform_json(lib_dir: &str) -> String {
    format!(
        "{{\n  \"file_format_version\": \"1.0.0\",\n  \"ICD\": {{\n    \"library_path\": \"{lib_dir}/libnvidia-egl-wayland.so.1\"\n  }}\n}}\n"
    )
}
