// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if aiterm_lib::run_browser_link_host_if_requested() {
        return;
    }
    aiterm_lib::run()
}
