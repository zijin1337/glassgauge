//! 原生实时折射引擎（spec 2026-08-18-glassgauge-native-refraction-engine）。
//! 引擎独占一个线程，拥有全部图形对象；主线程通过 mpsc 投递命令，
//! 引擎通过 `glass-mode` 事件 + GlassMode 状态汇报生效模式（refract/wallpaper）。
//! 状态机（spec §8）：Refract ⇄ Degraded（指数退避 0.5s→30s 不放弃）；
//! 启动即失败也走同一条退避路——远程桌面等场景等价于永久 Degraded。

pub mod capture;
pub mod dispmap;
pub mod geometry;
pub mod render;
pub mod spike;

use geometry::Rect;
use serde_json::Value;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// 玻璃参数（CSS 像素语义，spec §9：三种模式共用一套）。
#[derive(Clone, Copy, Debug)]
pub struct GlassCfg {
    pub blur: f32,
    pub displacement: f32,
    pub band: f32,
    pub radius: f32,
    pub margin: f32,
    pub saturate: f32,
}

impl Default for GlassCfg {
    fn default() -> Self {
        Self {
            blur: 14.0,
            displacement: 24.0,
            band: 16.0,
            radius: 20.0,
            margin: 24.0,
            saturate: 1.12,
        }
    }
}

impl GlassCfg {
    pub fn from_config(cfg: &Value) -> Self {
        let d = Self::default();
        let g = cfg.get("glass");
        let f = |k: &str, dv: f32| {
            g.and_then(|g| g.get(k))
                .and_then(Value::as_f64)
                .map(|v| v as f32)
                .unwrap_or(dv)
        };
        Self {
            blur: f("blur", d.blur),
            displacement: f("displacement", d.displacement),
            band: f("band", d.band),
            radius: f("radiusCollapsed", d.radius),
            margin: d.margin,
            saturate: f("saturate", d.saturate),
        }
    }
}

fn params_for(cfg: GlassCfg, dpr: f64) -> render::GlassParams {
    let d = dpr as f32;
    render::GlassParams {
        sigma: cfg.blur / 2.0 * d, // 约定：σ = blur/2
        displacement: cfg.displacement * d,
        band: cfg.band * d,
        radius: cfg.radius * d,
        margin: (cfg.margin * d).round() as i32,
        saturate: cfg.saturate,
    }
}

pub enum Cmd {
    /// 窗口几何变化（桌面物理坐标 + 该窗口的 DPI 缩放）
    Geometry { win: Rect, dpr: f64 },
    /// 强制重渲染（托盘刷新）
    Refresh,
    /// debug：把当前玻璃帧落 %APPDATA%/glassgauge/glass-dump.png
    Dump,
    Stop,
}

/// 主线程持有的引擎句柄（tauri 状态）。
pub struct EngineHandle(pub Mutex<Sender<Cmd>>);

/// 生效玻璃模式（tauri 状态，前端 get_glass_mode 读）。
pub struct GlassMode(pub Mutex<String>);

fn set_mode(app: &AppHandle, mode: &str) {
    if let Some(st) = app.try_state::<GlassMode>() {
        let mut m = st.0.lock().unwrap();
        if *m == mode {
            return; // 不重复广播
        }
        *m = mode.to_string();
    }
    let _ = app.emit("glass-mode", mode.to_string());
}

/// 挂件对屏幕捕获隐形（spec §8：refract 模式进程期常开）。
pub fn exclude_from_capture(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    unsafe {
        let _ = SetWindowDisplayAffinity(HWND(hwnd as *mut _), WDA_EXCLUDEFROMCAPTURE);
    }
}

pub fn start(app: AppHandle, hwnd: isize, cfg: GlassCfg) -> EngineHandle {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || run(app, hwnd, cfg, rx));
    EngineHandle(Mutex::new(tx))
}

struct Stack {
    ch: capture::Channel,
    rend: render::Renderer,
    dpr: f64,
    win: Rect,
}

fn run(app: AppHandle, hwnd: isize, cfg: GlassCfg, rx: Receiver<Cmd>) {
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut geo: Option<(Rect, f64)> = None;
    let mut backoff = Duration::from_millis(500);

    'outer: loop {
        // 没有几何就干等命令
        while geo.is_none() {
            match rx.recv() {
                Ok(Cmd::Geometry { win, dpr }) => geo = Some((win, dpr)),
                Ok(Cmd::Stop) | Err(_) => return,
                Ok(_) => {}
            }
        }
        let (win, dpr) = geo.unwrap();
        match build(hwnd, cfg, win, dpr) {
            Err(err) => {
                eprintln!("engine: build failed, degraded: {err}");
                set_mode(&app, "wallpaper");
                // 退避期间继续消化命令
                let deadline = Instant::now() + backoff;
                loop {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    match rx.recv_timeout(deadline - now) {
                        Ok(Cmd::Geometry { win, dpr }) => geo = Some((win, dpr)),
                        Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                        Ok(_) => {}
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue 'outer;
            }
            Ok(mut stack) => {
                set_mode(&app, "refract");
                backoff = Duration::from_millis(500);
                let mut force = true;
                // 验收辅助：启动 2.5s 后的渲染自动落一张玻璃帧（等位置/内容稳定；
                // 截图照不到挂件，只能靠这个）
                let mut dump_once = std::env::var("GG_DUMP_ONCE").is_ok();
                let started = Instant::now();
                let dump_delay = Duration::from_millis(2500);
                loop {
                    // 先清空命令队列
                    loop {
                        match rx.try_recv() {
                            Ok(Cmd::Geometry { win, dpr }) => {
                                geo = Some((win, dpr));
                                match apply_geometry(&mut stack, cfg, win, dpr) {
                                    Ok(changed) => force |= changed,
                                    Err(GeoOutcome::OutputChanged) => {
                                        // 换屏：整栈重建（先 drop，DComp 目标每窗唯一）
                                        drop(stack);
                                        backoff = Duration::from_millis(500);
                                        continue 'outer;
                                    }
                                    Err(GeoOutcome::Fatal(e)) => {
                                        eprintln!("engine: geometry failed: {e}");
                                        set_mode(&app, "wallpaper");
                                        drop(stack);
                                        continue 'outer;
                                    }
                                }
                            }
                            Ok(Cmd::Refresh) => force = true,
                            Ok(Cmd::Dump) => dump(&mut stack),
                            Ok(Cmd::Stop) => return,
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                        }
                    }
                    if dump_once && started.elapsed() >= dump_delay {
                        force = true; // 静止画面也逼一次渲染，好让 dump 有得拍
                    }
                    let polled = stack.ch.poll(100, force);
                    let render_now = match &polled {
                        capture::Poll::Updated => true,
                        capture::Poll::NoChange => force, // 拖动后底下没变：用常驻纹理重渲染
                        capture::Poll::Lost(e) => {
                            eprintln!("engine: capture lost: {e}");
                            set_mode(&app, "wallpaper");
                            drop(stack);
                            continue 'outer;
                        }
                    };
                    if render_now {
                        if let Some(tex) = stack.ch.region_texture() {
                            let tex = tex.clone();
                            if let Err(e) = stack.rend.render(&tex, stack.ch.generation()) {
                                eprintln!("engine: render failed: {e}");
                                set_mode(&app, "wallpaper");
                                drop(stack);
                                continue 'outer;
                            }
                            force = false;
                            if dump_once && started.elapsed() >= dump_delay {
                                dump_once = false;
                                dump(&mut stack);
                            }
                        }
                        // 还没有内容（首帧未到）：保持 force，下轮再试
                    }
                }
            }
        }
    }
}

fn build(hwnd: isize, cfg: GlassCfg, win: Rect, dpr: f64) -> Result<Stack, String> {
    let outputs = capture::list_outputs()?;
    let rects: Vec<_> = outputs.iter().map(|o| o.rect).collect();
    let cx = win.left + win.width() / 2;
    let cy = win.top + win.height() / 2;
    let idx = geometry::pick_output(&rects, cx, cy).ok_or("no outputs")?;
    let mut ch = capture::Channel::new(outputs[idx])?;
    let p = params_for(cfg, dpr);
    let o = ch.output_rect;
    let win_local = Rect::new(
        win.left - o.left,
        win.top - o.top,
        win.right - o.left,
        win.bottom - o.top,
    );
    ch.set_geometry(win_local, p.margin)?;
    let mut rend = render::Renderer::new(ch.device(), hwnd)?;
    rend.set_geometry(win.width() as u32, win.height() as u32, p)?;
    Ok(Stack { ch, rend, dpr, win })
}

enum GeoOutcome {
    OutputChanged,
    Fatal(String),
}

/// 同输出内的几何更新。返回 Ok(true) = 需要重渲染。
fn apply_geometry(
    stack: &mut Stack,
    cfg: GlassCfg,
    win: Rect,
    dpr: f64,
) -> Result<bool, GeoOutcome> {
    let cx = win.left + win.width() / 2;
    let cy = win.top + win.height() / 2;
    if !stack.ch.output_rect.contains(cx, cy) {
        return Err(GeoOutcome::OutputChanged);
    }
    if win == stack.win && dpr == stack.dpr {
        return Ok(false);
    }
    let p = params_for(cfg, dpr);
    let size_changed = win.width() != stack.win.width()
        || win.height() != stack.win.height()
        || dpr != stack.dpr;
    let o = stack.ch.output_rect;
    let win_local = Rect::new(
        win.left - o.left,
        win.top - o.top,
        win.right - o.left,
        win.bottom - o.top,
    );
    stack
        .ch
        .set_geometry(win_local, p.margin)
        .map_err(GeoOutcome::Fatal)?;
    if size_changed {
        stack
            .rend
            .set_geometry(win.width() as u32, win.height() as u32, p)
            .map_err(GeoOutcome::Fatal)?;
    }
    stack.win = win;
    stack.dpr = dpr;
    Ok(true)
}

fn dump(stack: &mut Stack) {
    let Some(tex) = stack.ch.region_texture() else {
        eprintln!("engine: dump skipped, no content yet");
        return;
    };
    let tex = tex.clone();
    match stack.rend.render_to_cpu(&tex, stack.ch.generation()) {
        Ok((w, h, bytes)) => {
            let path = crate::window::appdata_dir().join("glass-dump.png");
            match spike::write_png_bgra(&path, w, h, &bytes) {
                Ok(()) => eprintln!("engine: dumped {}", path.display()),
                Err(e) => eprintln!("engine: dump write failed: {e}"),
            }
        }
        Err(e) => eprintln!("engine: dump render failed: {e}"),
    }
}
