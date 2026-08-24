// 渲染与数据环。派生计算全部来自 derive.js；本文件只做取数节奏和 DOM。
import { deriveAll } from "./derive.js";
import { initGlass, recropTo, reloadWallpaper, teardownGlass } from "./glass.js";
import { applyWallpaperTheme } from "./theme.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

let config = null;
let lastGood = null; // 最后一次成功的 {json, at}
let timer = null;
let backoffMs = 0; // 0 = 正常节奏；失败后 5s→30s
let lastConnected = true;
let expanded = false; // 悬停展开态（spec 形态 C）

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
  lastConnected = ok;
  render(ok);
  // expand:"always"（默认）= 常驻展开：拿到首批数据就展开定形
  if (ok && (config?.expand ?? "always") === "always" && !expanded) {
    setExpanded(true);
  }
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
  if (expanded) {
    app.innerHTML = expandedHtml(all, connected);
    markDragRegion();
    return;
  }
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

/* ---------- 展开态（spec 形态 C：悬停 304 宽三窗口卡） ---------- */
function expandedHtml(all, connected) {
  const dot = connected ? all.status.dot : "grey";
  const dotCls = dot === "accent" ? "dot" : `dot ${dot}`;
  const cards = all.windows
    .map(
      (w) => `
      <div class="card">
        <div class="r1">
          <span class="win">${w.label}</span>
          <span class="rem">剩余 ${w.remPct}%</span>
          <span class="pct2">${w.usedPct}%</span>
        </div>
        <div class="bar">
          <div class="fill" style="width:${w.usedPct}%"></div>
          <div class="tick" style="left:${w.pacePct}%"></div>
        </div>
        <div class="l3"><span>${w.resetText}</span><span class="d">${w.deltaText}</span></div>
      </div>`,
    )
    .join("");
  return `
    <div class="shell expanded${connected ? "" : " stale"}">
      <div class="head">
        <span class="${dotCls}"></span>
        <span class="title">Mirasim 用量</span>
        <span class="badge">${config?.planLabel ?? "MAX"}</span>
        <span class="exp">套餐到期 ${config?.validUntil ?? "–"}</span>
      </div>
      ${cards}
    </div>`;
}

const COLLAPSED_SIZE = [244, 62];
const EXPANDED_W = 304;

async function setExpanded(v) {
  if (expanded === v) return;
  if (v && !lastGood) return; // 没数据没什么可展开的
  expanded = v;
  render(lastConnected);
  const { LogicalSize } = window.__TAURI__.dpi;
  if (v) {
    // 展开壳定宽 302，先渲染后量自然高，窗口跟内容走
    const shell = document.querySelector(".shell.expanded");
    const h = (shell?.offsetHeight ?? 300) + 2;
    await appWindow.setSize(new LogicalSize(EXPANDED_W, h));
  } else {
    await appWindow.setSize(new LogicalSize(COLLAPSED_SIZE[0], COLLAPSED_SIZE[1]));
  }
  // 壁纸兜底模式的滤镜/裁剪是按窗口尺寸建的，尺寸变了要重建
  if (glassMode === "wallpaper") {
    setTimeout(() => initGlass(config).catch(() => {}), 120);
  }
}

// expand:"hover" 才启用悬停展开/收起；"always" 常驻展开不理会指针
let hoverTimer = null;
document.addEventListener("pointerenter", () => {
  if ((config?.expand ?? "always") === "always") return;
  clearTimeout(hoverTimer);
  hoverTimer = setTimeout(() => setExpanded(true), 120);
});
document.addEventListener("pointerleave", () => {
  if ((config?.expand ?? "always") === "always") return;
  clearTimeout(hoverTimer);
  hoverTimer = setTimeout(() => setExpanded(false), 280);
});

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
  return { "5h": "5 小时", "7d": "7 天", "7d_fable": "Fable 7 天", "30d": "30 天" }[n] ?? n;
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
  applyWallpaperTheme(config); // 主色/明暗跟壁纸走（与玻璃模式无关）
  // 壁纸折射层只在 wallpaper 模式（含引擎降级）启用；失败只降级为素壳，不挡数据
  if (glassMode === "wallpaper") initGlass(config).catch((e) => console.error("glass init:", e));
  await listen("glass-mode", ({ payload }) => {
    glassMode = payload;
    applyRadius();
    if (payload === "wallpaper") initGlass(config).catch(() => {});
    else teardownGlass();
  });
  await listen("manual-refresh", () => {
    // 托盘刷新 = 配置热载 + 重取色 + 重读壁纸 + 立即拉数
    loadConfig().then(() => {
      applyWallpaperTheme(config);
      tick();
    });
    if (glassMode === "wallpaper") reloadWallpaper().catch(() => {});
  });
  await listen("wallpaper-changed", () => {
    applyWallpaperTheme(config);
    if (glassMode === "wallpaper") reloadWallpaper().catch(() => {});
  });
  tick();
})();
