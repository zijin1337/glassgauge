import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { deriveWindow, deriveAll, deriveStatus, resetText, tightest } from "../derive.js";

const fixture = JSON.parse(
  new TextDecoder().decode(readFileSync(new URL("./fixtures/limits.json", import.meta.url))),
);

test("用户截图数值复现：5h 卡（匀速线 23%，落后 20%）", () => {
  // 截图：已用 3%，"3 小时后重置"（实际 3h51m 余量 → 已过 1h09m）
  const now = 1_000_000;
  const w = { name: "5h", used: 3, budget: 100, reset_at: now + 3 * 3600 + 51 * 60 };
  const d = deriveWindow(w, now);
  assert.equal(d.usedPct, 3);
  assert.equal(d.pacePct, 23);
  assert.equal(d.delta, -20);
  assert.match(d.deltaText, /落后 20%/);
});

test("匀速线夹在 [0,100]，reset_at 已过期不产生负数", () => {
  const now = 2_000_000;
  const d = deriveWindow({ name: "5h", used: 50, budget: 100, reset_at: now - 10 }, now);
  assert.equal(d.pacePct, 100);
  const d2 = deriveWindow({ name: "5h", used: 0, budget: 100, reset_at: now + 18000 + 999 }, now);
  assert.equal(d2.pacePct, 0); // 剩余超过窗口长度也不为负
});

test("超前/落后符号与文案", () => {
  const now = 0;
  // 用得比匀速快 → 超前
  const ahead = deriveWindow({ name: "7d", used: 50, budget: 100, reset_at: now + 604800 * 0.9 }, now);
  assert.ok(ahead.delta > 0);
  assert.match(ahead.deltaText, /超前/);
  // delta = 0 边界归入"超前"
  const even = deriveWindow({ name: "7d", used: 10, budget: 100, reset_at: now + 604800 * 0.9 }, now);
  assert.equal(even.delta, 0);
  assert.match(even.deltaText, /超前 0%/);
});

test("倒计时文案三档边界", () => {
  assert.equal(resetText(29 * 60), "29 分后重置");
  assert.equal(resetText(3600), "1 小时 0 分后重置");
  assert.equal(resetText(86400 - 1), "23 小时 59 分后重置");
  assert.equal(resetText(86400), "1 天 0 小时后重置");
  assert.equal(resetText(6 * 86400 + 3600 * 5), "6 天 5 小时后重置");
  assert.equal(resetText(-5), "0 分后重置");
});

test("真实夹具：三窗口齐、排序固定、最紧窗口正确", () => {
  const now = Math.min(...fixture.windows.map((w) => w.reset_at)) - 60;
  const all = deriveAll(fixture, now);
  assert.equal(all.windows.length, 3);
  assert.deepEqual(all.windows.map((w) => w.name), ["5h", "7d", "30d"]);
  const maxUsed = Math.max(...all.windows.map((w) => w.usedPct));
  assert.equal(all.tight.usedPct, maxUsed);
  assert.equal(all.status.kind, "ok");
});

test("状态优先级：suspended > degraded > unmetered", () => {
  assert.equal(deriveStatus({ suspended: true, degraded: true, unmetered: true }).dot, "red");
  assert.equal(deriveStatus({ degraded: true, unmetered: true }).dot, "amber");
  assert.equal(deriveStatus({ unmetered: true }).dot, "blue");
  assert.equal(deriveStatus({}).kind, "ok");
});

test("坏数据不炸：budget<=0 / 未知窗口名被丢弃", () => {
  const all = deriveAll(
    { windows: [{ name: "5h", used: 1, budget: 0, reset_at: 10 }, { name: "1y", used: 1, budget: 5, reset_at: 10 }] },
    0,
  );
  assert.equal(all.windows.length, 0);
  assert.equal(all.tight, null);
});
