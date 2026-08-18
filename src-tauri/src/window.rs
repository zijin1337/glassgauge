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
    // acrylic=true（默认）= 实时模式：DWM 亚克力实时模糊窗口后面的真实内容，
    //   四角用 DWM 原生圆角裁（这是唯一能裁掉亚克力材质的办法，CSS 裁不到它），
    //   前端把圆角同步成 8px 并停用壁纸折射层。
    // acrylic=false = 壁纸折射模式：窗口全透明，20px 药丸圆角，玻璃只折射壁纸。
    let acrylic = cfg.get("acrylic").and_then(Value::as_bool).unwrap_or(true);
    let on_top = cfg
        .get("alwaysOnTop")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let _ = win.set_always_on_top(on_top);

    if acrylic {
        // 失败（旧系统/远程桌面等）只降级为纯透明，不报错弹窗
        if let Err(e) = window_vibrancy::apply_acrylic(&win, Some((255, 255, 255, 5))) {
            eprintln!("acrylic unavailable, transparent fallback: {e}");
        }
    }
    // 两种模式都裁：8px 的 DWM 圆角是 20px CSS 圆角的超集，壁纸模式下不碰可见内容
    dwm_round_corners(&win);

    match load_state() {
        Some(st) if monitor_contains(&win, st.x, st.y) => {
            let _ = win.set_position(PhysicalPosition::new(st.x, st.y));
        }
        _ => snap_primary_topright(&win),
    }
    let _ = win.show();

    // Phase 0 技术验证入口（GG_SPIKE=b|a），验证完删除
    if let Ok(which) = std::env::var("GG_SPIKE") {
        let hwnd = win.hwnd().map(|h| h.0 as isize).unwrap_or(0);
        let pos = win
            .outer_position()
            .unwrap_or(tauri::PhysicalPosition { x: 0, y: 0 });
        let size = win.outer_size().unwrap_or(tauri::PhysicalSize {
            width: 244,
            height: 62,
        });
        crate::engine::spike::run(&which, hwnd, size.width, size.height, pos.x, pos.y);
    }

    crate::wallpaper::start_watcher(app.handle().clone());
    build_tray(app.handle(), on_top)?;
    Ok(())
}

/// Win11 原生圆角 + 去掉 DWM 描边。DWM 在合成层裁整个窗口面（含亚克力材质），
/// 半径固定 ~8px（随 DPI 缩放），前端 CSS 圆角必须与之一致。
fn dwm_round_corners(win: &WebviewWindow) {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attr: u32,
            val: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    let Ok(hwnd) = win.hwnd() else { return };
    let hwnd = hwnd.0 as isize;
    unsafe {
        let round: u32 = 2; // DWMWA_WINDOW_CORNER_PREFERENCE(33) = DWMWCP_ROUND
        DwmSetWindowAttribute(hwnd, 33, &round as *const u32 as _, 4);
        let none: u32 = 0xFFFF_FFFE; // DWMWA_BORDER_COLOR(34) = DWMWA_COLOR_NONE
        DwmSetWindowAttribute(hwnd, 34, &none as *const u32 as _, 4);
    }
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
