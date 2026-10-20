// Prevents an extra console window on Windows; harmless elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    cell_launcher_app_lib::run()
}
