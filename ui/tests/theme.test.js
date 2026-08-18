// 壁纸取色测试：直方图/绿带绕行/明暗判定/accent 覆写。
import { test } from "node:test";
import assert from "node:assert/strict";
import { analyzePixels, themeVars } from "../theme-core.js";

/** 生成 n 个同色 RGBA 像素 */
function pixels(r, g, b, n = 100) {
  const d = new Uint8ClampedArray(n * 4);
  for (let i = 0; i < n * 4; i += 4) {
    d[i] = r; d[i + 1] = g; d[i + 2] = b; d[i + 3] = 255;
  }
  return d;
}

test("深蓝壁纸：暗→亮字，色相落蓝桶", () => {
  const s = analyzePixels(pixels(0, 80, 255));
  assert.ok(s.lum < 0.5);
  assert.equal(s.hue, 225); // 221° → 22 号桶中心 225
  assert.ok(!s.dodged);
  const v = themeVars(s, "auto");
  assert.equal(v.ink, "#ffffff");
  assert.equal(v.accent, "hsl(225 74% 66%)");
});

test("青绿壁纸（135°）绕到 190° 青", () => {
  const s = analyzePixels(pixels(40, 200, 80));
  assert.equal(s.rawHue, 135);
  assert.ok(s.dodged);
  assert.equal(s.hue, 190);
});

test("黄绿壁纸（75°）绕到 42° 琥珀", () => {
  const s = analyzePixels(pixels(150, 200, 40));
  assert.equal(s.rawHue, 75);
  assert.ok(s.dodged);
  assert.equal(s.hue, 42);
});

test("纯灰壁纸：无饱和像素 → auto 退回蓝", () => {
  const s = analyzePixels(pixels(128, 128, 128));
  assert.ok(!s.saturated);
  const v = themeVars(s, "auto");
  assert.ok(v.accent.startsWith("hsl(212"));
});

test("亮壁纸 → 暗字暗刻度", () => {
  const s = analyzePixels(pixels(240, 238, 235));
  assert.ok(s.lum >= 0.5);
  const v = themeVars(s, "auto");
  assert.equal(v.ink, "#171a22");
  assert.equal(v.tick, "rgba(0,0,0,0.45)");
});

test("accent 覆写：#hex 直通、ink 用字色、amber 固定琥珀", () => {
  const s = analyzePixels(pixels(0, 80, 255)); // dark
  assert.equal(themeVars(s, "#ff8800").accent, "#ff8800");
  assert.equal(themeVars(s, "ink").accent, "#ffffff");
  assert.ok(themeVars(s, "amber").accent.startsWith("hsl(36"));
});
