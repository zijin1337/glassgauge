// 壁纸取色纯计算（spec §主题）。node 可测，不碰 DOM/canvas。
// 亮度决定明暗字色；色相直方图（36 桶、饱和度²加权、灰像素弃权）决定主色；
// 绿色带 70°–165° 按用户约束绕开（偏黄绿→42° 琥珀，偏青绿→190° 青）。

/** RGBA 字节数组 → { lum, hue, rawHue, dodged, saturated } */
export function analyzePixels(d) {
  let lum = 0;
  let n = 0;
  const buckets = new Array(36).fill(0);
  for (let i = 0; i < d.length; i += 4) {
    const r = d[i] / 255, g = d[i + 1] / 255, b = d[i + 2] / 255;
    lum += 0.2126 * r + 0.7152 * g + 0.0722 * b;
    n++;
    const mx = Math.max(r, g, b), mn = Math.min(r, g, b), dl = mx - mn;
    if (dl < 0.12) continue; // 灰的不参与色相投票
    let h;
    if (mx === r) h = ((g - b) / dl) % 6;
    else if (mx === g) h = (b - r) / dl + 2;
    else h = (r - g) / dl + 4;
    h = (h * 60 + 360) % 360;
    buckets[Math.floor(h / 10)] += dl * dl; // 越鲜艳票越重
  }
  lum /= Math.max(1, n);
  let best = 0;
  for (let k = 1; k < 36; k++) if (buckets[k] > buckets[best]) best = k;
  const saturated = buckets[best] > 0;
  let hue = best * 10 + 5;
  const rawHue = hue;
  let dodged = false;
  if (saturated && hue >= 70 && hue <= 165) {
    hue = hue < 118 ? 42 : 190;
    dodged = true;
  }
  return { lum, hue, rawHue, dodged, saturated };
}

/**
 * 采样 + accent 配置（auto | blue | amber | ink | #hex）→ CSS 变量值。
 * 壁纸全灰（无饱和像素）时 auto 退回蓝，避免直方图空转出红。
 */
export function themeVars(sample, mode = "auto") {
  const dark = sample.lum < 0.5; // 壁纸偏暗 → 亮字
  const ink = dark ? "#ffffff" : "#171a22";
  const blue = `hsl(212 82% ${dark ? 66 : 46}%)`;
  let accent;
  if (mode === "ink") accent = ink;
  else if (mode === "blue") accent = blue;
  else if (mode === "amber") accent = `hsl(36 88% ${dark ? 62 : 44}%)`;
  else if (typeof mode === "string" && mode.startsWith("#")) accent = mode;
  else accent = sample.saturated ? `hsl(${sample.hue} 74% ${dark ? 66 : 44}%)` : blue;
  return {
    dark,
    ink,
    accent,
    track: dark ? "rgba(255,255,255,0.22)" : "rgba(0,0,0,0.12)",
    tick: dark ? "rgba(255,255,255,0.8)" : "rgba(0,0,0,0.45)",
  };
}
