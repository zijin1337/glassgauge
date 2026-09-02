// 用量派生计算（spec §4.3）。纯函数，node --test 可测，不碰 DOM。

export const WINDOW_LEN = { "5h": 18000, "7d": 604800, "7d_fable": 604800, "30d": 2592000 };
export const WINDOW_LABEL = {
  "5h": "5 小时窗口",
  "7d": "7 天窗口",
  "7d_fable": "7 天 · Fable 专属",
  "30d": "30 天窗口",
};

/** 单个窗口 -> 显示值。now 为 Unix 秒。 */
export function deriveWindow(w, now) {
  const len = WINDOW_LEN[w.name];
  if (!len || !(w.budget > 0)) return null;
  const usedPct = (w.used / w.budget) * 100;
  const remaining = Math.max(0, w.reset_at - now);
  const pacePct = Math.min(100, Math.max(0, ((len - remaining) / len) * 100));
  const delta = usedPct - pacePct;
  return {
    name: w.name,
    label: WINDOW_LABEL[w.name] ?? w.name,
    usedPct: round1(usedPct),
    remPct: Math.max(0, Math.round(100 - usedPct)),
    pacePct: round1(pacePct),
    delta: round1(delta),
    deltaText: `匀速线 ${round1(pacePct)}% · ${delta >= 0 ? "超前" : "落后"} ${Math.abs(round1(delta))}%`,
    resetText: resetText(remaining),
    // 原始额度单位（美元折算兜底用；换算率在渲染层乘 centsPerUnit）
    budgetUnits: w.budget,
    usedUnits: w.used,
    remainUnits: Math.max(0, w.budget - w.used),
    // 后端派生的美元（已花÷已用% 法，学 mirasim-telemetry）；缺失时渲染层退回 units×cents
    usedUsd: w.usedUsd,
    budgetUsd: w.budgetUsd,
    remainUsd: w.remainUsd,
    usdEstimated: w.usdEstimated,
  };
}

/** 全响应 -> {status, windows[], tight}。窗口按 5h/7d/30d 固定排序。 */
export function deriveAll(limits, now) {
  const order = ["5h", "7d", "7d_fable", "30d"];
  const windows = (limits.windows ?? [])
    .map((w) => deriveWindow(w, now))
    .filter(Boolean)
    .sort((a, b) => order.indexOf(a.name) - order.indexOf(b.name));
  return {
    status: deriveStatus(limits),
    windows,
    tight: tightest(windows),
  };
}

/** 最紧窗口 = 已用百分比最大者；空数组返回 null。 */
export function tightest(derivedWindows) {
  return derivedWindows.reduce((a, b) => (a == null || b.usedPct > a.usedPct ? b : a), null);
}

/** suspended/degraded/unmetered -> 状态点（spec §7）。优先级：红 > 黄 > 蓝 > 正常。 */
export function deriveStatus(limits) {
  if (limits.suspended) return { kind: "suspended", dot: "red", text: "账号已暂停" };
  if (limits.degraded) return { kind: "degraded", dot: "amber", text: "服务降级中" };
  if (limits.unmetered) return { kind: "unmetered", dot: "blue", text: "不计量模式" };
  return { kind: "ok", dot: "accent", text: null };
}

/** 剩余秒 -> 倒计时文案。>=1 天给 天+小时；>=1 小时给 小时+分；否则只给分。 */
export function resetText(sec) {
  const s = Math.max(0, Math.floor(sec));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d} 天 ${h} 小时后重置`;
  if (h > 0) return `${h} 小时 ${m} 分后重置`;
  return `${m} 分后重置`;
}

function round1(x) {
  return Math.round(x * 10) / 10;
}
