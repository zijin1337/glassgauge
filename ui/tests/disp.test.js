// 位移场测试：中性区、边带方向、alpha。
import { test } from "node:test";
import assert from "node:assert/strict";
import { dispField } from "../disp.js";

// 玻璃 100×60，外扩 24，圆角 20，边带 16 → 画布 148×108
const M = 24, GW = 100, GH = 60;
const f = dispField(GW + 2 * M, GH + 2 * M, M, 20, 16);
const px = (x, y) => {
  const i = (y * f.width + x) * 4;
  return [f.data[i], f.data[i + 1], f.data[i + 2], f.data[i + 3]];
};

test("外扩区（玻璃外）全部中性 128/128", () => {
  for (const [x, y] of [[0, 0], [f.width - 1, f.height - 1], [12, 54], [f.width - 5, 3]]) {
    const [r, g] = px(x, y);
    assert.equal(r, 128, `(${x},${y}) R`);
    assert.equal(g, 128, `(${x},${y}) G`);
  }
});

test("玻璃深处（离边 > band）中性", () => {
  const [r, g] = px(M + GW / 2, M + GH / 2);
  assert.equal(r, 128);
  assert.equal(g, 128);
});

test("边带方向：左边带 R<128、右边带 R>128、下边带 G>128、上边带 G<128", () => {
  const midY = M + GH / 2, midX = M + GW / 2;
  assert.ok(px(M + 1, midY)[0] < 128, "左");
  assert.ok(px(M + GW - 2, midY)[0] > 128, "右");
  assert.ok(px(midX, M + GH - 2)[1] > 128, "下");
  assert.ok(px(midX, M + 1)[1] < 128, "上");
});

test("圆角区：位移沿对角（右下角 R>128 且 G>128）", () => {
  // 圆角 20：(GW-8, GH-8) 在弧内侧（signed distance ≈ -3），(GW-4, GH-4) 已在弧外被裁
  const [r, g] = px(M + GW - 8, M + GH - 8);
  assert.ok(r > 128 && g > 128);
  const [ro, go] = px(M + GW - 4, M + GH - 4);
  assert.equal(ro, 128); // 弧外 → 中性
  assert.equal(go, 128);
});

test("alpha 通道恒为 255（feImage 不能有透明像素）", () => {
  for (let i = 3; i < f.data.length; i += 4) {
    if (f.data[i] !== 255) assert.fail(`alpha at ${(i - 3) / 4}`);
  }
});
