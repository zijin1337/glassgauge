#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod discovery;
mod relay;
mod window;

fn main() {
    tauri::Builder::default()
        .manage(relay::RelayState::default())
        .invoke_handler(tauri::generate_handler![
            window::get_config,
            window::save_state,
            relay::fetch_limits,
        ])
        .setup(|app| {
            window::setup(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running glassgauge");
}
