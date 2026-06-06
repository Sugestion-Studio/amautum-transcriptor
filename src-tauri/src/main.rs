// Tauri 2.0: la lógica vive en lib.rs para que el mismo crate sirva como
// binary y como librería (mobile_entry_point). El main solo delega.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    amautum_agent_lib::run()
}
