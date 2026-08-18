use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{App, AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

pub const DEFAULT_CONFIG: &str = include_str!("../../config.json");

pub(crate) fn appdata_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base).join("glassgauge");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// 返回用户配置；文件缺失时落一份默认值，文件损坏时返回默认值但不覆盖用户的文件。
#[tauri::command]
pub fn get_config() -> String {
    let path = appdata_dir().join("config.json");
    match fs::read_to_string(&path) {
        Ok(s) => {
            if serde_json::from_str::<Value>(&s).is_ok() {
                s
            } else {
                DEFAULT_CONFIG.to_string()
            }
        }
        Err(_) => {
            let _ = fs::write(&path, DEFAULT_CONFIG);
            DEFAULT_CONFIG.to_string()
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct WinState {
    pub x: i32,
    pub y: i32,
}

#[tauri::command]
pub fn save_state(x: i32, y: i32) {
    let _ = fs::write(
        appdata_dir().join("state.json"),
        serde_json::to_string(&WinState { x, y }).unwrap(),
    );
}

fn load_state() -> Option<WinState> {
    serde_json::from_str(&fs::read_to_string(appdata_dir().join("state.json")).ok()?).ok()
}

fn monitor_contains(win: &WebviewWindow, x: i32, y: i32) -> bool {
    if let Ok(monitors) = win.available_monitors() {
        for m in monitors {
            let p = m.position();
            let s = m.size();
            if x >= p.x && x < p.x + s.width as i32 && y >= p.y && y < p.y + s.height as i32 {
                return true;
            }
        }
    }
    false
}

fn snap_primary_topright(win: &WebviewWindow) {
    let (Ok(Some(m)), Ok(size)) = (win.primary_monitor(), win.outer_size()) else {
        return;
    };
    let p = m.position();
    let s = m.size();
    let x = p.x + s.width as i32 - size.width as i32 - 16;
    let y = p.y + 16;
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

pub fn setup(app: &mut App) -> tauri::Result<()> {
    let win = app.get_webview_window("main").expect("main window missing");

    let cfg: Value = serde_json::from_str(&get_config()).unwrap_or(Value::Null);
    let acrylic = cfg.get("acrylic").and_then(Value::as_bool).unwrap_or(true);
    let on_top = cfg
        .get("alwaysOnTop")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let _ = win.set_always_on_top(on_top);

    if acrylic {
        // 失败（旧系统/远程桌面等）只降级为纯透明，不报错弹窗
        if let Err(e) = window_vibrancy::apply_acrylic(&win, Some((255, 255, 255, 10))) {
            eprintln!("acrylic unavailable, transparent fallback: {e}");
        }
    }

    match load_state() {
        Some(st) if monitor_contains(&win, st.x, st.y) => {
            let _ = win.set_position(PhysicalPosition::new(st.x, st.y));
        }
        _ => snap_primary_topright(&win),
    }
    let _ = win.show();

    build_tray(app.handle(), on_top)?;
    Ok(())
}

fn build_tray(app: &AppHandle, initial_on_top: bool) -> tauri::Result<()> {
    let refresh = MenuItem::with_id(app, "refresh", "立即刷新", true, None::<&str>)?;
    let pin = CheckMenuItem::with_id(app, "pin", "置顶", true, initial_on_top, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&refresh, &pin, &quit])?;

    let pinned = Arc::new(AtomicBool::new(initial_on_top));

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().expect("bundled icon").clone())
        .tooltip("Mirasim 用量")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, ev| match ev.id().as_ref() {
            "refresh" => {
                let _ = app.emit("manual-refresh", ());
            }
            "pin" => {
                if let Some(w) = app.get_webview_window("main") {
                    let now = !pinned.load(Ordering::Relaxed);
                    pinned.store(now, Ordering::Relaxed);
                    let _ = w.set_always_on_top(now);
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}
