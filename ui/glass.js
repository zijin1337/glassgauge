// L2 真玻璃（spec §5/§6）：把真实壁纸按窗口在虚拟桌面上的物理位置裁出来，
// 经 SVG 位移滤镜折射。几何在 crop.js、位移场在 disp.js，本文件管 DOM 与 Tauri。
//
// 滤镜挂在 #wallpaper-glass（窗口尺寸 + 四周外扩 MARGIN 的普通元素）上，
// 不用 backdrop-filter:url()（Chromium 不支持）；外层 #wallpaper-layer 负责圆角裁剪。

import { pickRect, coverInto, cssPlacement } from "./crop.js";
import { dispField } from "./disp.js";

const MARGIN = 24; // 向外扩的像素：给边缘模糊留真实采样，防止吸到透明出现亮暗晕

const { invoke } = window.__TAURI__.core;
const appWindow = window.__TAURI__.window.getCurrentWindow();

let st = null; // { cfg, imgW, imgH, monitors, style, offset }

export async function initGlass(cfg) {
  const el = document.getElementById("wallpaper-glass");
  if (!el) return;
  const wp = await invoke("get_wallpaper"); // { dataUrl, style, path }
  const size = await imageSize(wp.dataUrl);
  const monitors = (await window.__TAURI__.window.availableMonitors()).map((m) => ({
    x: m.position.x,
    y: m.position.y,
    w: m.size.width,
    h: m.size.height,
  }));
  st = {
    cfg,
    imgW: size.w,
    imgH: size.h,
    monitors,
    style: wp.style,
    offset: cfg.wallpaperOffset ?? [0, 0],
  };
  buildFilter(cfg);
  const w = window.innerWidth, h = window.innerHeight;
  el.style.left = `${-MARGIN}px`;
  el.style.top = `${-MARGIN}px`;
  el.style.width = `${w + MARGIN * 2}px`;
  el.style.height = `${h + MARGIN * 2}px`;
  el.style.backgroundImage = `url("${wp.dataUrl}")`;
  el.style.filter = "url(#gg-glass)";
  const pos = await appWindow.outerPosition();
  recropTo(pos.x, pos.y);
}

/** 拖动/移动时调用：按窗口新物理位置重摆背景。纯样式写，帧内完成。 */
export function recropTo(px, py) {
  if (!st) return;
  const el = document.getElementById("wallpaper-glass");
  const dpr = window.devicePixelRatio || 1;
  const center = {
    x: px + (window.innerWidth * dpr) / 2,
    y: py + (window.innerHeight * dpr) / 2,
  };
  const rect = pickRect(st.style, st.monitors, center);
  const map = coverInto(rect, st.imgW, st.imgH);
  const p = cssPlacement(map, st.imgW, st.imgH, { x: px, y: py }, dpr, st.offset);
  // 元素自身向外挪了 MARGIN，背景坐标补回来
  el.style.backgroundSize = `${p.w}px ${p.h}px`;
  el.style.backgroundPosition = `${p.tx + MARGIN}px ${p.ty + MARGIN}px`;
}

/** 壁纸文件变了（wallpaper-changed 事件 / 托盘刷新）：重读重裁。 */
export async function reloadWallpaper() {
  if (!st) return;
  const wp = await invoke("get_wallpaper");
  const size = await imageSize(wp.dataUrl);
  st.imgW = size.w;
  st.imgH = size.h;
  st.style = wp.style;
  st.monitors = (await window.__TAURI__.window.availableMonitors()).map((m) => ({
    x: m.position.x,
    y: m.position.y,
    w: m.size.width,
    h: m.size.height,
  }));
  document.getElementById("wallpaper-glass").style.backgroundImage = `url("${wp.dataUrl}")`;
  const pos = await appWindow.outerPosition();
  recropTo(pos.x, pos.y);
}

/* ---------- 滤镜链（spec §6.1，约定 σ = blur/2、displacement 为 scale 原值） ---------- */
function buildFilter(cfg) {
  const g = cfg.glass ?? {};
  const w = window.innerWidth, h = window.innerHeight;
  const W = w + MARGIN * 2, H = h + MARGIN * 2;
  const map = fieldToDataUrl(dispField(W, H, MARGIN, g.radiusCollapsed ?? 20, g.band ?? 16));
  const sigma = (g.blur ?? 14) / 2;
  // sRGB 必须显式声明：SVG 滤镜默认 linearRGB，会让 128 不再是位移中性值
  document.getElementById("filter-defs").innerHTML = `
    <filter id="gg-glass" x="0" y="0" width="${W}" height="${H}"
            filterUnits="userSpaceOnUse" color-interpolation-filters="sRGB">
      <feGaussianBlur in="SourceGraphic" stdDeviation="${sigma}" result="frost"/>
      <feImage href="${map}" x="0" y="0" width="${W}" height="${H}" result="map"/>
      <feDisplacementMap in="frost" in2="map" scale="${g.displacement ?? 24}"
                         xChannelSelector="R" yChannelSelector="G" result="bent"/>
      <feColorMatrix in="bent" type="saturate" values="${g.saturate ?? 1.12}"/>
    </filter>`;
}

function fieldToDataUrl(field) {
  const c = document.createElement("canvas");
  c.width = field.width;
  c.height = field.height;
  c.getContext("2d").putImageData(new ImageData(field.data, field.width, field.height), 0, 0);
  return c.toDataURL();
}

function imageSize(src) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve({ w: img.naturalWidth, h: img.naturalHeight });
    img.onerror = () => reject(new Error("wallpaper decode failed"));
    img.src = src;
  });
}
