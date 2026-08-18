// 主题应用层：读壁纸 → 96×60 降采样 → theme-core 取色 → 写 CSS 变量。
// 与玻璃模式无关（三种模式都取壁纸定色）；失败保持默认蓝，不挡功能。

import { analyzePixels, themeVars } from "./theme-core.js";

const { invoke } = window.__TAURI__.core;

export async function applyWallpaperTheme(config) {
  try {
    const wp = await invoke("get_wallpaper");
    const img = await loadImage(wp.dataUrl);
    const c = document.createElement("canvas");
    c.width = 96;
    c.height = 60;
    const ctx = c.getContext("2d", { willReadFrequently: true });
    ctx.drawImage(img, 0, 0, 96, 60);
    const sample = analyzePixels(ctx.getImageData(0, 0, 96, 60).data);
    const v = themeVars(sample, config?.accent ?? "auto");
    const root = document.documentElement.style;
    root.setProperty("--ink", v.ink);
    root.setProperty("--accent", v.accent);
    root.setProperty("--track", v.track);
    root.setProperty("--tick", v.tick);
  } catch (e) {
    console.error("theme:", e); // 默认蓝兜底
  }
}

function loadImage(src) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("wallpaper decode failed"));
    img.src = src;
  });
}
