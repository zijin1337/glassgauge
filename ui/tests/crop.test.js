// 几何映射测试：用本机真实的双屏布局做基准。
// 主屏 2560×1440 @ (0,0)，副屏竖 1280×2048 @ (-1280,0)，壁纸 4160×2560，style 22（跨屏）。
import { test } from "node:test";
import assert from "node:assert/strict";
import { virtualDesktop, pickRect, coverInto, cssPlacement } from "../crop.js";

const MONITORS = [
  { x: 0, y: 0, w: 2560, h: 1440 },
  { x: -1280, y: 0, w: 1280, h: 2048 },
];
const IMG = { w: 4160, h: 2560 };

test("虚拟桌面 = 两屏物理矩形的包围盒", () => {
  assert.deepEqual(virtualDesktop(MONITORS), { x: -1280, y: 0, w: 3840, h: 2048 });
  assert.deepEqual(virtualDesktop([]), { x: 0, y: 0, w: 0, h: 0 });
});

test("跨屏 cover：缩放取 max(宽比,高比)，居中落点", () => {
  const map = coverInto(virtualDesktop(MONITORS), IMG.w, IMG.h);
  assert.ok(Math.abs(map.s - 3840 / 4160) < 1e-9); // 宽比 0.923 > 高比 0.8
  assert.equal(map.originX, -1280); // 宽度正好盖满 → 不偏
  assert.ok(Math.abs(map.originY - (2048 - 2560 * (3840 / 4160)) / 2) < 1e-9); // ≈ -157.54
});

test("窗口在主屏 (0,83) dpr 1.25 → 背景 CSS 摆放", () => {
  const map = coverInto(virtualDesktop(MONITORS), IMG.w, IMG.h);
  const p = cssPlacement(map, IMG.w, IMG.h, { x: 0, y: 83 }, 1.25);
  assert.equal(p.tx, -1024); // (-1280-0)/1.25
  assert.ok(Math.abs(p.ty - (map.originY - 83) / 1.25) < 1e-9);
  assert.ok(Math.abs(p.w - (4160 * map.s) / 1.25) < 1e-9); // 3072 CSS px
});

test("wallpaperOffset 校准量直接加在 CSS 坐标上", () => {
  const map = coverInto(virtualDesktop(MONITORS), IMG.w, IMG.h);
  const base = cssPlacement(map, IMG.w, IMG.h, { x: 100, y: 200 }, 1.25);
  const nudged = cssPlacement(map, IMG.w, IMG.h, { x: 100, y: 200 }, 1.25, [3, -5]);
  assert.equal(nudged.tx - base.tx, 3);
  assert.equal(nudged.ty - base.ty, -5);
});

test("style 22 → 虚拟桌面；其他 style → 窗口中心所在屏", () => {
  assert.deepEqual(pickRect("22", MONITORS, { x: 500, y: 500 }), {
    x: -1280, y: 0, w: 3840, h: 2048,
  });
  // fill 模式：窗口在副屏 → 用副屏矩形
  assert.deepEqual(pickRect("10", MONITORS, { x: -600, y: 900 }), {
    x: -1280, y: 0, w: 1280, h: 2048,
  });
  // 中心不在任何屏上（拖到缝隙）→ 退回虚拟桌面
  assert.deepEqual(pickRect("10", MONITORS, { x: 9999, y: 9999 }), {
    x: -1280, y: 0, w: 3840, h: 2048,
  });
});

test("单屏 fill：宽比 0.615 > 高比 0.5625 → 宽度盖满，上下裁", () => {
  const map = coverInto({ x: 0, y: 0, w: 2560, h: 1440 }, IMG.w, IMG.h);
  assert.ok(Math.abs(map.s - 2560 / 4160) < 1e-9);
  assert.equal(map.originX, 0);
  assert.ok(Math.abs(map.originY - (1440 - 2560 * (2560 / 4160)) / 2) < 1e-9); // ≈ -67.7
});
