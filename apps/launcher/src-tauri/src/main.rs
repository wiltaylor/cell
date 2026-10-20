// Prevents an extra console window on Windows; harmless elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // webkit2gtk's DMABUF renderer shows a blank white window on some
    // NVIDIA and virtual-GPU setups. Off by default; set the variable to
    // any value yourself to override.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    cell_launcher_app_lib::run()
}
