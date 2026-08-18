// 渲染与数据环。派生计算全部来自 derive.js；本文件只做取数节奏和 DOM。
import { deriveAll } from "./derive.js";
import { initGlass, recropTo, reloadWallpaper, teardownGlass } from "./glass.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

let config = null;
let lastGood = null; // 最后一次成功的 {json, at}
let timer = null;
let backoffMs = 0; // 0 = 正常节奏；失败后 5s→30s

// 生效玻璃模式（spec §4）：refract=原生折射 | wallpaper=壁纸折射兜底 | live=DWM 亚克力。
// 启动时问 get_glass_mode，之后由 glass-mode 事件驱动（引擎降级/恢复）。
let glassMode = "refract";

function applyRadius() {
  const g = config?.glass ?? {};
  // live 模式 DWM 只能裁 ~8px 圆角，CSS 必须一致；refract/wallpaper 都是 20px 药丸
  document.documentElement.style.setProperty(
    "--radius-collapsed",
    glassMode === "live" ? "8px" : (g.radiusCollapsed ?? 20) + "px",
  );
}

async function loadConfig() {
  config = JSON.parse(await invoke("get_config"));
  const g = config.glass ?? {};
  const root = document.documentElement.style;
  if (g.alpha != null) root.setProperty("--alpha", g.alpha);
  if (g.radiusCard != null) root.setProperty("--radius-card", g.radiusCard + "px");
  applyRadius();
}

/* ---------- 取数节奏：正常 refreshSeconds 轮询；失败 5s→30s 退避 ---------- */
async function tick() {
  clearTimeout(timer);
  let ok = false;
  try {
    const res = await invoke("fetch_limits");
    lastGood = { json: JSON.parse(res.json), at: Date.now() };
    ok = true;
  } catch {
    /* relay-not-found 或网络失败 → 降级渲染 */
  }
  render(ok);
  if (ok) {
    backoffMs = 0;
    timer = setTimeout(tick, (config?.refreshSeconds ?? 60) * 1000);
  } else {
    backoffMs = backoffMs ? Math.min(backoffMs * 2, 30000) : 5000;
    timer = setTimeout(tick, backoffMs);
  }
}

/* ---------- 渲染 ---------- */
function render(connected) {
  const app = document.getElementById("app");
  if (!lastGood) {
    app.innerHTML = shellHtml({
      dot: "grey",
      who: connected ? "加载中…" : "未找到 mirasim",
      pct: "–",
      fill: 0,
      tickAt: 0,
      stale: !connected,
    });
    markDragRegion();
    return;
  }
  const all = deriveAll(lastGood.json, Date.now() / 1000);
  const t = all.tight;
  const abnormal = all.status.kind !== "ok";
  app.innerHTML = shellHtml({
    dot: connected ? all.status.dot : "grey",
    who: abnormal
      ? all.status.text
      : connected
        ? `最紧窗口 · ${t ? shortName(t.name) : "–"}`
        : "连接丢失 · 显示最后数据",
    pct: t ? t.usedPct + "%" : "–",
    fill: t ? t.usedPct : 0,
    tickAt: t ? t.pacePct : 0,
    stale: !connected,
  });
  markDragRegion();
}

function shellHtml({ dot, who, pct, fill, tickAt, stale }) {
  const dotCls = dot === "accent" ? "dot" : `dot ${dot}`;
  return `
    <div class="shell${stale ? " stale" : ""}">
      <div class="top">
        <span class="${dotCls}"></span>
        <span class="who">${who}</span>
        <span class="pct">${pct}</span>
      </div>
      <div class="bar">
        <div class="fill" style="width:${fill}%"></div>
        <div class="tick" style="left:${tickAt}%"></div>
      </div>
    </div>`;
}

function shortName(n) {
  return { "5h": "5 小时", "7d": "7 天", "30d": "30 天" }[n] ?? n;
}

/* ---------- 拖动与位置 ---------- */
function markDragRegion() {
  for (const el of document.querySelectorAll("body, #app, #app *")) {
    if (!el.closest("[data-gg-interactive]")) {
      el.setAttribute("data-tauri-drag-region", "");
    }
  }
}

let moveTimer = null;
appWindow.onMoved(({ payload }) => {
  recropTo(payload.x, payload.y); // 玻璃跟手：拖动中每次事件都重摆背景
  clearTimeout(moveTimer);
  moveTimer = setTimeout(() => {
    invoke("save_state", { x: payload.x, y: payload.y });
  }, 500);
});

/* ---------- 启动 ---------- */
(async () => {
  await loadConfig();
  glassMode = await invoke("get_glass_mode").catch(() => "wallpaper");
  applyRadius();
  render(true); // 先画"加载中"
  // 壁纸折射层只在 wallpaper 模式（含引擎降级）启用；失败只降级为素壳，不挡数据
  if (glassMode === "wallpaper") initGlass(config).catch((e) => console.error("glass init:", e));
  await listen("glass-mode", ({ payload }) => {
    glassMode = payload;
    applyRadius();
    if (payload === "wallpaper") initGlass(config).catch(() => {});
    else teardownGlass();
  });
  await listen("manual-refresh", () => {
    // 托盘刷新 = 配置热载 + 重读壁纸 + 立即拉数
    loadConfig().then(tick);
    if (glassMode === "wallpaper") reloadWallpaper().catch(() => {});
  });
  await listen("wallpaper-changed", () => {
    if (glassMode === "wallpaper") reloadWallpaper().catch(() => {});
  });
  tick();
})();
