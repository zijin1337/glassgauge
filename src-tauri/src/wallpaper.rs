//! L2 真玻璃的数据源（spec §5）：读当前壁纸文件、监听壁纸变化。
//! Wallpaper Engine 在跑时，注册表 WallPaper 指向它写在 Themes 目录的静态快照
//! （WallpaperEngineOverride_*.jpg），所以注册表优先、TranscodedWallpaper 兜底。

use base64::Engine;
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperInfo {
    pub data_url: String,
    /// 注册表 WallpaperStyle 原文："22" = 跨屏(span)，"10" = 逐屏填充(fill)…
    pub style: String,
    pub path: String,
}

fn themes_dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into()))
        .join("Microsoft")
        .join("Windows")
        .join("Themes")
}

fn desktop_key() -> Option<RegKey> {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Control Panel\\Desktop")
        .ok()
}

fn registry_wallpaper() -> Option<PathBuf> {
    let p: String = desktop_key()?.get_value("WallPaper").ok()?;
    let p = p.trim();
    (!p.is_empty()).then(|| PathBuf::from(p))
}

fn wallpaper_style() -> String {
    desktop_key()
        .and_then(|k| k.get_value::<String, _>("WallpaperStyle").ok())
        .unwrap_or_else(|| "10".into())
}

/// 按文件头识别图片类型；识别不出按 jpeg（TranscodedWallpaper 没有扩展名）。
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, ..] => "image/jpeg",
        [b'B', b'M', ..] => "image/bmp",
        _ => "image/jpeg",
    }
}

#[tauri::command]
pub fn get_wallpaper() -> Result<WallpaperInfo, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = registry_wallpaper() {
        candidates.push(p);
    }
    candidates.push(themes_dir().join("TranscodedWallpaper"));

    for p in candidates {
        let Ok(bytes) = fs::read(&p) else { continue };
        if bytes.is_empty() {
            continue;
        }
        let mime = sniff_mime(&bytes);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(WallpaperInfo {
            data_url: format!("data:{mime};base64,{b64}"),
            style: wallpaper_style(),
            path: p.display().to_string(),
        });
    }
    Err("wallpaper-not-found".into())
}

/// 监听 Themes 目录：壁纸换了（含 WE 重写快照）→ 2 秒静默后发 `wallpaper-changed`。
/// 监听失败只损失热更新，不影响主功能。
pub fn start_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(mut watcher) =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if res.is_ok() {
                    let _ = tx.send(());
                }
            })
        else {
            return;
        };
        if watcher.watch(&themes_dir(), RecursiveMode::NonRecursive).is_err() {
            return;
        }
        while rx.recv().is_ok() {
            // 吸收连续写入（WE 换场景会写好几个文件）
            while rx.recv_timeout(Duration::from_secs(2)).is_ok() {}
            let _ = app.emit("wallpaper-changed", ());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_sniffing() {
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(sniff_mime(&[0x89, b'P', b'N', b'G', 0x0D]), "image/png");
        assert_eq!(sniff_mime(&[b'B', b'M', 0x36]), "image/bmp");
        // 无扩展名的 TranscodedWallpaper 大概率是 jpeg，认不出时按 jpeg
        assert_eq!(sniff_mime(&[0x00, 0x01]), "image/jpeg");
    }
}
