// 壁纸 → 窗口的几何映射（spec §5.2）。纯函数，node 可测。
// 输入坐标一律是虚拟桌面的物理像素；输出是 WebView 的 CSS 像素。

/** 所有显示器物理矩形的包围盒（= WallpaperStyle 22 跨屏模式铺的区域）。 */
export function virtualDesktop(monitors) {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const m of monitors) {
    x0 = Math.min(x0, m.x);
    y0 = Math.min(y0, m.y);
    x1 = Math.max(x1, m.x + m.w);
    y1 = Math.max(y1, m.y + m.h);
  }
  if (!isFinite(x0)) return { x: 0, y: 0, w: 0, h: 0 };
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

/**
 * 壁纸实际铺在哪个矩形上：
 * style "22"（跨屏）→ 整个虚拟桌面；其余按逐屏 fill 近似 → 窗口中心所在的显示器。
 * fit/stretch/tile 也按 fill 近似（差异只在边缘几像素，spec §5.2 的取舍）。
 */
export function pickRect(style, monitors, center) {
  if (String(style) === "22" || monitors.length === 0) return virtualDesktop(monitors);
  const hit = monitors.find(
    (m) => center.x >= m.x && center.x < m.x + m.w && center.y >= m.y && center.y < m.y + m.h,
  );
  return hit ? { x: hit.x, y: hit.y, w: hit.w, h: hit.h } : virtualDesktop(monitors);
}

/** cover 铺法：等比放大到盖满 rect，居中。返回缩放和图片左上角落点（物理坐标）。 */
export function coverInto(rect, imgW, imgH) {
  const s = Math.max(rect.w / imgW, rect.h / imgH) || 1;
  return {
    s,
    originX: rect.x + (rect.w - imgW * s) / 2,
    originY: rect.y + (rect.h - imgH * s) / 2,
  };
}

/**
 * 窗口物理位置 → 背景图相对窗口左上角的 CSS 摆放。
 * offset 是配置里的 wallpaperOffset 校准量（CSS 像素）。
 */
export function cssPlacement(map, imgW, imgH, winPos, dpr, offset = [0, 0]) {
  return {
    tx: (map.originX - winPos.x) / dpr + (offset[0] ?? 0),
    ty: (map.originY - winPos.y) / dpr + (offset[1] ?? 0),
    w: (imgW * map.s) / dpr,
    h: (imgH * map.s) / dpr,
  };
}
