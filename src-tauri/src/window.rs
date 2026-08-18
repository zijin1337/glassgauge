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

/// 生效模式（spec §9）：`mode` 键优先；旧 `acrylic` 布尔兼容映射；都没有 → refract。
fn config_mode(cfg: &Value) -> String {
    if let Some(m) = cfg.get("mode").and_then(Value::as_str) {
        return m.to_string();
    }
    match cfg.get("acrylic").and_then(Value::as_bool) {
        Some(true) => "live".into(),
        Some(false) => "wallpaper".into(),
        None => "refract".into(),
    }
}

fn send_engine_geometry(win: &WebviewWindow, handle: &crate::engine::EngineHandle) {
    let (Ok(pos), Ok(size), Ok(scale)) = (
        win.outer_position(),
        win.outer_size(),
        win.scale_factor(),
    ) else {
        return;
    };
    let rect = crate::engine::geometry::Rect::new(
        pos.x,
        pos.y,
        pos.x + size.width as i32,
        pos.y + size.height as i32,
    );
    let _ = handle
        .0
        .lock()
        .unwrap()
        .send(crate::engine::Cmd::Geometry {
            win: rect,
            dpr: scale,
        });
}

pub fn setup(app: &mut App) -> tauri::Result<()> {
    let win = app.get_webview_window("main").expect("main window missing");

    let cfg: Value = serde_json::from_str(&get_config()).unwrap_or(Value::Null);
    // mode（spec §9）：refract=原生实时折射（默认）| live=DWM 亚克力（8px 圆角）
    // | wallpaper=壁纸折射。refract 失败时引擎自己降级到 wallpaper 并广播。
    let mode = config_mode(&cfg);
    app.manage(crate::engine::GlassMode(std::sync::Mutex::new(mode.clone())));
    let on_top = cfg
        .get("alwaysOnTop")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let _ = win.set_always_on_top(on_top);

    // 所有模式都裁 DWM 原生圆角：live 模式靠它裁亚克力四角；
    // refract/wallpaper 的 20px CSS/原生圆角是 8px 的子集，不受影响。
    dwm_round_corners(&win);

    let spiking = std::env::var("GG_SPIKE").is_ok();
    match mode.as_str() {
        "live" => {
            // 失败（旧系统/远程桌面等）只降级为纯透明，不报错弹窗
            if let Err(e) = window_vibrancy::apply_acrylic(&win, Some((255, 255, 255, 5))) {
                eprintln!("acrylic unavailable, transparent fallback: {e}");
            }
        }
        "refract" if !spiking => {
            let hwnd = win.hwnd().map(|h| h.0 as isize).unwrap_or(0);
            // 截图隐形：进程期常开（spec §8），避免切换闪烁
            crate::engine::exclude_from_capture(hwnd);
            // 初始几何在位置恢复后（setup 末尾）再发，这里只建线程和事件桥
            let eng = crate::engine::start(
                app.handle().clone(),
                hwnd,
                crate::engine::GlassCfg::from_config(&cfg),
            );
            app.manage(eng);
            let app2 = app.handle().clone();
            let win2 = win.clone();
            win.on_window_event(move |ev| {
                use tauri::WindowEvent::{Moved, Resized, ScaleFactorChanged};
                if matches!(ev, Moved(_) | Resized(_) | ScaleFactorChanged { .. }) {
                    if let Some(h) = app2.try_state::<crate::engine::EngineHandle>() {
                        send_engine_geometry(&win2, &h);
                    }
                }
            });
        }
        _ => {} // wallpaper（或 spike 占用窗口时）：前端壁纸层处理
    }

    match load_state() {
        Some(st) if monitor_contains(&win, st.x, st.y) => {
            let _ = win.set_position(PhysicalPosition::new(st.x, st.y));
        }
        _ => snap_primary_topright(&win),
    }
    let _ = win.show();

    // 位置已恢复：现在发初始几何，引擎第一帧就在正确位置
    if let Some(h) = app.try_state::<crate::engine::EngineHandle>() {
        send_engine_geometry(&win, &h);
    }

    // Phase 0-3 技术验证入口（GG_SPIKE=b|a|cap|pipe），Phase 5 清理
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

/// 前端启动时查询生效模式（之后靠 glass-mode 事件跟进）。
#[tauri::command]
pub fn get_glass_mode(state: tauri::State<crate::engine::GlassMode>) -> String {
    state.0.lock().unwrap().clone()
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
    // 截图照不到挂件（refract 剔除），debug 构建给一条落盘出口做验收
    let dump = if cfg!(debug_assertions) {
        Some(MenuItem::with_id(app, "dump", "导出玻璃帧", true, None::<&str>)?)
    } else {
        None
    };
    let menu = match &dump {
        Some(d) => Menu::with_items(app, &[&refresh, d, &pin, &quit])?,
        None => Menu::with_items(app, &[&refresh, &pin, &quit])?,
    };

    let pinned = Arc::new(AtomicBool::new(initial_on_top));

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().expect("bundled icon").clone())
        .tooltip("Mirasim 用量")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, ev| match ev.id().as_ref() {
            "refresh" => {
                let _ = app.emit("manual-refresh", ());
                if let Some(h) = app.try_state::<crate::engine::EngineHandle>() {
                    let _ = h.0.lock().unwrap().send(crate::engine::Cmd::Refresh);
                }
            }
            "dump" => {
                if let Some(h) = app.try_state::<crate::engine::EngineHandle>() {
                    let _ = h.0.lock().unwrap().send(crate::engine::Cmd::Dump);
                }
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
