// Prevent an extra console window on Windows. Harmless on macOS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    lj_hub_lib::run()
}
