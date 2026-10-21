//! The Cell launcher. It heads every `.cell` file: it reads the trailer and
//! manifest from its own executable, makes the layers visible and runs the
//! application. The Linux build sandboxes with namespaces and FUSE; the
//! Windows build serves the layers through WinFsp, or extracts them when
//! WinFsp is not installed.
//!
//! Control commands come through `CELL_CMD` so they never collide with the
//! application's own arguments:
//!
//! - `CELL_CMD=info`    print host probe, grants and layers
//! - `CELL_CMD=shell`   open a shell inside the sandbox
//! - `CELL_CMD=verify`  check layer hashes
//! - `CELL_CMD=install` / `uninstall`  desktop entry, icon, URL schemes (Linux)
//! - `CELL_CMD=steam-add` / `steam-remove` / `steam-list`  Steam shortcut and art (Linux)
//! - `CELL_FILE=<path>` read the payload from another file (noexec mounts)

#[cfg(windows)]
mod appcontainer;
#[cfg(target_os = "linux")]
mod desktop;
#[cfg(target_os = "linux")]
mod grants;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod registry;
#[cfg(windows)]
mod setup;
#[cfg(target_os = "linux")]
mod steam;
#[cfg(windows)]
mod windows;

mod common;

fn main() {
    #[cfg(target_os = "linux")]
    let result = linux::real_main();
    #[cfg(windows)]
    let result = windows::real_main();
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("cell: {e:#}");
            std::process::exit(125);
        }
    }
}
