# Mirasim 用量液态玻璃挂件 — 设计规格

日期：2026-08-18
状态：已与用户逐节确认（材质 → 形态 → 玻璃管线 → 行为/错误/测试）
代号：**glassgauge**

## 1. 目标

一个常驻 Windows 桌面、可随意拖动的小挂件，显示 mirasim 中转（relay）的
5 小时 / 7 天 / 30 天用量窗口，视觉上是 iOS 26 风格的**真实液态玻璃**：
挂件弯折、模糊的是它身后真实的桌面内容（用户的实际壁纸），不是自带的假背景。

只显示百分比与派生指标，**不显示金额**（用户明确要求）。

### 非目标

- 屏幕实时采集（WGC/desktopCapturer）驱动折射——CPU 常驻开销不可接受。
  动态壁纸的折射对象是 Wallpaper Engine 的静态快照（见 §5），用户已接受此取舍。
- 调用 mirasim 云端账户接口取套餐信息——需要复刻 token/refresh 逻辑且随
  mirasim 版本漂移。套餐徽章与到期日走 `config.json` 手填。
- macOS / Linux 支持。仅 Windows 11（acrylic 依赖它）。
- 开机自启（后续想要再加，一行注册表/快捷方式的事）。

## 2. 结论速记（评审时先看这里）

| 决策点 | 结论 |
|---|---|
| 外壳 | Tauri v2（Rust 后端 + WebView2 前端），成品 ~6–10MB。Electron 被否：mirasim 自带那份已 340MB，再来一份不成比例 |
| 形态 | 折叠条 **244×62** 常驻，悬停 250ms 展开三卡（304 宽），移开收回（用户在可视化对比里点选 C） |
| 材质 | 雪白磨砂（低折射 12–24px 位移 + 高磨砂 + 内缘亮边），用户在三种材质对比里点选 B |
| 透明度 | 白色浓度 **16%**（三档里最透；用户嫌之前的不够透） |
| 主色 | **壁纸自动取色**，色相落在绿色带（70°–165°）时自动绕开（用户明确排除绿色） |
| 玻璃的"真" | 三层管线：OS acrylic（真模糊）+ 壁纸映射折射层（真弯折）+ 内容层，见 §5 |
| 数据源 | relay `GET /v1/limits`，免鉴权，端口自动发现，见 §4 |
| 套餐徽章 | `config.json` 手填 `planLabel` / `validUntil`（当前值 MAX / 2027-08-11），留空不渲染 |
| 状态徽章 | `suspended / unmetered / degraded` 驱动的彩点，异常才变色 |

## 3. 架构（三层，职责划死）

```
src-tauri/（Rust）                 前端（纯静态，无框架无构建器）
├─ discovery.rs  端口发现          ├─ derive.js   窗口数据 → 显示值（纯函数）
├─ relay.rs      HTTP 取数         ├─ glass.js    位移图生成 + SVG 滤镜注入
├─ wallpaper.rs  壁纸定位/监听     ├─ crop.js     窗口矩形 → 壁纸裁切（纯函数）
├─ window.rs     acrylic/拖动/托盘 ├─ theme.js    壁纸采样 → 明暗 + 主色
└─ main.rs       组装 + IPC 命令   └─ panel.js    渲染折叠/展开两态
config.json（用户可改的一切参数）
```

边界规则：

- Rust 只负责"拿到原始事实"（limits JSON、壁纸文件路径、窗口/显示器几何、
  文件变更事件），**不做任何百分比/文案计算**。
- 所有派生计算在 `derive.js`，纯函数、可单测；`glass.js` 不知道用量长什么样；
  `derive.js` / `crop.js` 不碰 DOM。
- HTTP 一律走 Rust 侧（`reqwest`），绕开 WebView 的 CORS——relay 不发 CORS 头。

## 4. 数据层

### 4.1 relay 端口发现（discovery.rs）

relay 端口由 mirasim 每次启动动态分配（本会话内就从 61083 → 51678 变过一次），
磁盘无记录，环境变量只有 mirasim 自己的子进程能继承。发现算法：

1. 读缓存 `%APPDATA%/glassgauge/endpoint.json`，直接探一次 `/v1/limits`，通了就用。
2. 失败则 `netstat -ano` 列出 `127.0.0.1` 上所有 LISTENING 端口，并发
   `GET http://127.0.0.1:{port}/v1/limits`（超时 800ms，并发≤32）。
3. **认领特征**：HTTP 200 + JSON 含 `windows` 数组且元素有
   `name/used/budget/reset_at` 四字段。特征不符即拒绝（不能只看 200——
   别的本地服务也可能有 `/v1/limits` 路径）。
4. 命中后写回缓存。全扫失败 → 进入"未连接"态，30s 后重试。

### 4.2 取数（relay.rs）

- `GET /v1/limits`，**无需任何鉴权头**（已实测）。响应：
  `{subject, suspended, unmetered, degraded, windows:[{name:"5h"|"7d"|"30d", used, budget, reset_at}]}`，
  `used/budget` 单位为美分，`reset_at` 为 Unix 秒。
- 轮询 60s（`config.refreshSeconds`）；悬停展开时立即补一次；请求打本地回环，成本≈0。
- IPC：前端 `invoke("fetch_limits")` → Rust 返回原始 JSON 字符串 + 本次端点。

### 4.3 派生计算（derive.js，全部可单测）

窗口长度映射：`5h=18000s`、`7d=604800s`、`30d=2592000s`。对每个窗口：

- `usedPct = used / budget`
- `pacePct = (len − (reset_at − now)) / len`（匀速消耗参考线）
- `delta = usedPct − pacePct`，≥0 显示"超前"，<0 显示"落后"
- 倒计时文案：`X 天 Y 小时后重置` / `X 小时 Y 分后重置` / `X 分后重置`
- 折叠态"最紧窗口" = `usedPct` 最大的那个

以上公式已对着用户截图验证（5h 卡：匀速线 23%、落后 20% 全部吻合）。

## 5. 玻璃管线（本设计的核心）

三层，自底向上：

**L1 acrylic（真实模糊）** — Tauri `window-vibrancy` 对整窗
`apply_acrylic`；窗口 `transparent: true`、`decorations: false`。
桌面上真实存在的一切（壁纸、图标、其他窗口）被 OS 合成为模糊底。

**L2 壁纸折射层（真实弯折）** — WebView 里铺一张**用户真实壁纸**的 `<img>`
裁切，其上元素用 `backdrop-filter: url(#liquid)` 跑 SVG 滤镜链：

```
feGaussianBlur(σ≈7) → feImage(位移图) → feDisplacementMap(scale≈24, R→x G→y)
→ feColorMatrix(saturate 1.12)
```

位移图在 Canvas 运行时按元素尺寸生成：圆角矩形有符号距离场 → 边缘法向 ×
smoothstep 落差带（band 16px），R/G 编码 dx/dy，128 为中性。此技术与
archisvaze/liquid-glass 同源（该仓库无 license，**实现全部自写，不搬文件**）。

**对齐是效果成立的前提**：玻璃内外的壁纸必须严丝合缝。
`crop.js` 的映射（纯函数）：

- 输入：窗口物理矩形（Tauri `outer_position/size`，物理像素）、各显示器
  物理几何 + scale factor、壁纸模式。
- 用户环境实测：主屏 2560×1440@125%（虚拟坐标 0,0），副屏竖 1280×2048@120%
  （逻辑 -1280,0），壁纸 **style 22 跨屏**——一张图铺满虚拟桌面外接矩形。
- 输出：`background-position/size` 的负偏移。窗口 `moved` 事件里实时更新
  （纯 CSS 属性写，无重采样，拖动不掉帧）。
- 兜底：`config.wallpaperOffset: [dx, dy]` 手动校准。

**壁纸来源与监听（wallpaper.rs）**：

- 读注册表 `HKCU\Control Panel\Desktop\WallPaper` 得当前壁纸路径
  （用户环境为 Wallpaper Engine 写的 `WallpaperEngineOverride_*.jpg` 静态快照），
  回退 `Themes\TranscodedWallpaper`。
- `notify` 监听 Themes 目录：文件变更 → 防抖 2s → 通知前端重载纹理并重跑
  §6 的采样。WE 正在写文件导致读失败 → 沿用旧纹理，2s 重试。
- 动态壁纸的折射对象是静态快照（用户已接受）；acrylic 层里动画仍真实可见。

## 6. 主题（theme.js：壁纸采样 → 明暗 + 主色）

把壁纸缩到 96×60 采样：

- **明暗**：平均相对亮度 <0.5 → 白玻璃亮字；否则暗字。决定墨色、描边、轨道色。
- **主色**：色相直方图（36 桶 ×10°，灰色像素不投票，票权 = 饱和度²），
  取最重桶；**落在绿色带 70°–165° 时绕开**——<118° 推到琥珀 42°，
  否则推到青 190°（用户排除绿色）。
- `config.accent` 可强制覆盖为 `"auto" | "blue" | "amber" | "ink" | "#RRGGBB"`。

材质参数（全部在 `config.glass`，默认值即用户选定档）：

```json
{ "alpha": 0.16, "blur": 14, "displacement": 24, "band": 16,
  "radiusCollapsed": 20, "radiusCard": 14, "saturate": 1.12 }
```

约定：`blur` 为直观直径，滤镜里 `feGaussianBlur` 的 `stdDeviation = blur / 2`；
`displacement` 即 `feDisplacementMap` 的 `scale` 原值，不再二次换算。

## 7. UI 两态

**折叠 244×62**：状态点 ·「最紧窗口 · 7 天」· 大号百分比 ｜ 进度条 + 匀速线刻度。

**展开 304×自适应**（悬停 250ms 触发，移开收回；展开向不越出屏幕的方向）：
标题行（状态点 + "Mirasim 用量" + 套餐徽章 + 有效期）+ 三张玻璃卡，每张：
窗口名 / 剩余% / 大号已用% ｜ 进度条 + 匀速线刻度 ｜ 倒计时 + 「匀速线 X% · 超前/落后 Y%」。

状态点颜色：正常=主色；`degraded`=琥珀；`suspended`=红；`unmetered`=蓝；
未连接=灰。异常时折叠态的"最紧窗口"文案替换为状态说明。

## 8. 窗口行为

- 无边框、透明、默认置顶（`config.alwaysOnTop`）、整条按住即拖
  （`data-tauri-drag-region`，展开态排除可能加的交互件）。
- 位置持久化到 `%APPDATA%/glassgauge/state.json`（含所在显示器），
  启动时若该位置已不在任何屏内则回主屏右上。
- 托盘图标菜单：立即刷新 / 置顶开关 / 退出。折叠条上不放任何按钮。
- 显示器增减、DPI 变化事件 → 重算裁切与位置合法性。
- 不进任务栏（`skipTaskbar: true`）。

## 9. config.json（用户可改的一切）

```json
{
  "planLabel": "MAX",
  "validUntil": "2027-08-11",
  "refreshSeconds": 60,
  "alwaysOnTop": true,
  "accent": "auto",
  "wallpaperOffset": [0, 0],
  "glass": { "alpha": 0.16, "blur": 14, "displacement": 24, "band": 16,
             "radiusCollapsed": 20, "radiusCard": 14, "saturate": 1.12 }
}
```

`planLabel`/`validUntil` 留空则不渲染对应元素。改文件 → 托盘"立即刷新"即热载
（配置读取在前端，重读文件即可，不必重启）。

## 10. 错误处理（全部降级显示，绝不弹窗）

| 场景 | 行为 |
|---|---|
| 缓存端口失效 | 全量重扫（§4.1）；期间显示旧数据 + 灰点 |
| 全扫失败（mirasim 未开） | "未连接"态：灰点 + 最后数据置灰 + 30s 重试 |
| `/v1/limits` 5xx/超时 | 保留旧数据，指数退避 5s→30s |
| `suspended/degraded/unmetered` | 状态点变色（§7），不打断显示 |
| 壁纸读失败（WE 写一半） | 旧纹理 + 2s 重试 |
| 跨屏裁切错位 | `wallpaperOffset` 手动校准兜底 |
| config.json 解析失败 | 用内置默认值 + 托盘图标加感叹角标 |

## 11. 测试

**单测（node:test，纯函数）**

- `derive.js`：三窗口换算、匀速线/超前落后符号、倒计时文案边界（<1h、<1d、跨天）、
  最紧窗口选择；用录制的真实 `/v1/limits` 响应做夹具。
- `crop.js`：双屏几何（2560×1440@125% + 竖 1280×2048@120%，style 22）下，
  窗口在主屏/副屏/跨界三个位置的偏移断言。
- `theme.js`：构造纯绿图断言绕带；构造暗图/亮图断言明暗切换。

**集成（Rust `#[cfg(test)]`）**

- 起一个假 relay（随机端口，返回夹具 JSON）→ 断言发现算法认领；
  起一个返回相似但缺字段的假服务 → 断言拒绝。

**手动核对清单（视觉，无法自动化）**

- [ ] 玻璃内外壁纸对齐：静止 / 拖动中 / 拖过屏幕交界，均无可见错缝
- [ ] WE 换壁纸后 ≤5s：折射纹理、明暗、主色全部跟上
- [ ] 重启 mirasim（端口变化）后 ≤30s 自愈
- [ ] 悬停展开/收回不闪烁；展开态不越出屏幕
- [ ] 双屏各自 DPI 下文字不糊（devicePixelRatio 渲染正确）

## 12. 实现顺序（给 implementation plan 的骨架）

1. Tauri 脚手架 + acrylic 透明无边框窗 + 拖动 + 托盘（先有个能拖的玻璃壳）
2. discovery + relay 轮询 + derive + 折叠态渲染（先有真数据）
3. 壁纸定位/监听 + crop 对齐 + 位移滤镜（玻璃变"真"）
4. theme 采样 + 展开态 + 状态徽章 + config 热载
5. 单测补齐 + 手动清单过一遍 + `tauri build` 出 exe
