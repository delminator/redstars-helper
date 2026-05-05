// Binary entry — delegates to the lib so the app code is reusable for mobile.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    redstars_helper_lib::run()
}
