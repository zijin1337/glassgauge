#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod discovery;
mod engine;
mod relay;
mod wallpaper;
mod webui;
mod window;

fn main() {
    tauri::Builder::default()
        .manage(relay::RelayState::default())
        .invoke_handler(tauri::generate_handler![
            window::get_config,
            window::get_glass_mode,
            window::save_state,
            relay::fetch_limits,
            wallpaper::get_wallpaper,
        ])
        .setup(|app| {
            window::setup(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running glassgauge");
}
