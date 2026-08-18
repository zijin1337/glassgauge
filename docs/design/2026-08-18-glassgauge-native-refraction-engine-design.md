# glassgauge 原生实时折射引擎（方案 B）设计

日期：2026-08-18
状态：已批准（对话中逐节确认）
前置：`2026-08-18-mirasim-usage-glass-widget-design.md`（本文件修订其 §5/§6 的玻璃层实现）

## 1. 目标与范围

挂件的玻璃必须同时满足两件事：**透出窗口后面实时的真实内容**（不是壁纸贴图），
且带 **iOS 26 液态玻璃的边缘折射**（内容在边带处被弯曲）。已验证的结论：

- 浏览器层（backdrop-filter / SVG filter）拿不到窗口后面的像素 → 折射只能作用于壁纸贴图；
- DWM 亚克力是实时的，但只有模糊没有折射，且圆角被锁在 ~8px。

因此上原生渲染引擎：实时抓取挂件正后方那一小块屏幕，在 GPU 上做
模糊 + 位移折射，画在透明 WebView 内容底下。20px 药丸圆角回归。

**用户已明确接受的代价**：技术上必须把挂件自身从屏幕捕获中剔除
（否则玻璃照到自己形成回环），因此挂件在用户的截图/录屏中隐形。

## 2. 与现有架构的关系

| 层 | 旧（Phase 2 现状） | 新 |
| --- | --- | --- |
| 玻璃内容源 | L2 壁纸贴图折射（JS/SVG） | 原生引擎实时折射（默认）；壁纸折射降级兜底 |
| L1 亚克力 | live 模式（默认开） | 保留为手动 `mode:"live"`，不再是默认 |
| 白纱/棱边光/文字 | CSS | 不变，原样叠在引擎输出上 |
| 拖动重裁 | JS onMoved → recropTo | refract 模式在 Rust 引擎内完成；壁纸兜底时仍走 JS |

现有 `glass.js/crop.js/disp.js` 全部保留——它们就是 Degraded/Dead 态的兜底实现。

## 3. 技术路线（已选：桌面复制）

**DXGI Desktop Duplication + Direct2D 特效 + DirectComposition 合成。**

- 桌面复制**明文承诺**剔除设置了 `WDA_EXCLUDEFROMCAPTURE` 的窗口（Win10 2004+）；
- 帧事件驱动：屏幕不变不给帧，静止时功耗≈0；帧带脏区域元数据可再过滤；
- 帧是 GPU 纹理，特效链全程不落 CPU；
- 备选 Windows.Graphics.Capture（WinRT 脚手架更重、无视觉收益）与 GDI 轮询
  （剔除自身无文档保证、持续烧 CPU）均已排除。

## 4. 模块与边界

新增 `src-tauri/src/engine/`：

```
engine/mod.rs      状态机 + 对外接口（start/stop/事件投递/当前模式查询）
engine/capture.rs  DXGI 复制通道：按显示器建、AcquireNextFrame、常驻纹理
engine/render.rs   D2D 特效链 + DComp 目标/表面/提交
engine/dispmap.rs  SDF 位移图（disp.js 的 Rust 镜像，同数学同单测）
engine/geometry.rs 纯函数：选屏、物理区域、clamp（可单测）
```

- 引擎独占一个线程，**拥有全部图形对象**（D3D11 设备、复制通道、D2D、DComp），
  主线程与它只通过 mpsc 通道通信；
- 主线程职责：启停引擎；把 tauri 窗口事件（Moved/Resized/ScaleFactorChanged）投递给
  引擎线程；把引擎状态变化转发给前端（`glass-mode` 事件）；
- 前端契约：
  - 启动时 `invoke("get_glass_mode")` 取生效模式（避免与事件竞态）；
  - 监听 `glass-mode` 事件 `{ mode: "refract" | "wallpaper" | "live" }`：
    `wallpaper` → 启用现有壁纸层（initGlass，幂等）；`refract` → 清空壁纸层；
    `live` 只出现在手动配置时（8px 圆角），引擎路径永远 20px。

## 5. 渲染管线

数据流（全物理像素）：

```
DWM 合成桌面（已剔除本窗口）
→ AcquireNextFrame（GPU 纹理）
→ 脏区域 ∩ (窗口物理 rect ± MARGIN) 为空且无强制渲染 → 丢帧
→ CopyResource 到自有常驻纹理，立即 ReleaseFrame
→ D2D 特效链：Crop(窗口区±MARGIN)
   → GaussianBlur(σ = blur/2 × dpr)
   → DisplacementMap(scale = displacement × dpr，位移输入 = SDF 图，R→x G→y，128 中性)
   → Saturation(saturate)
→ 以 radiusCollapsed × dpr 的圆角几何（抗锯齿）画进 DComp 表面
→ Commit → DWM 垫在 WebView 子窗口后面（CreateTargetForHwnd topmost=FALSE）
```

- **常驻纹理是关键**：拖动时底下内容常无变化（复制接口不给新帧），
  用最近画面即时重裁重渲染，玻璃完全跟手；
- MARGIN = 24 CSS px × dpr，作用同 JS 版（给边缘模糊留真实采样）；
- D2D 的 DisplacementMap 语义与 SVG feDisplacementMap 一致，参数直接沿用
  既有约定（σ = blur/2；displacement 为 scale 原值），CSS 像素语义、引擎内乘 dpr；
- SDF 位移图在窗口尺寸/DPI 变化时重建，其余时间常驻。

## 6. 帧循环

```
loop:
  drain 窗口事件通道 → 有 Moved/Resized/换屏 → 更新几何（必要时重建通道/SDF）→ 置强制渲染
  AcquireNextFrame(timeout 100ms)
    timeout 且无强制渲染 → continue
    ACCESS_LOST / DEVICE_REMOVED → 进 Degraded（§8）
    有帧 → 脏区过滤 → 常驻纹理更新
  需要渲染 → 跑管线 → Commit
```

不加额外帧率节流：复制接口天然按合成节奏给帧，渲染区域 ~370×140 物理像素
（(244+48)×(62+48) CSS px × 1.25），管线耗时 sub-ms。静止时零帧零渲染。

## 7. 几何与多屏

- 一切计算用物理像素；`geometry.rs` 纯函数化以便单测；
- **源显示器 = 窗口中心所在显示器**；中心换屏 → 重建该屏的复制通道；
- 窗口骑屏缝：掉出源屏的部分 clamp 到纹理边缘（边缘像素延伸，接受轻微拉伸）;
- DPI 变化（Resized/ScaleFactorChanged）→ 重建 SDF 图与表面尺寸；
- 本机基准（写进单测）：主屏 2560×1440 @ (0,0) 125%，副屏竖 1280×2048 @ (-1280,0) 120%。

## 8. 自我剔除与降级状态机

**剔除**：refract 模式启动时 `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`，
**进程生命周期内保持**（含 Degraded 期间）——反复切换会引起窗口重合成闪烁。
手动 `mode:"live"` / `"wallpaper"` 不设置（挂件在截图中可见）。

**状态机**：

| 态 | 进入条件 | 行为 | 退出 |
| --- | --- | --- | --- |
| Refract | 引擎初始化成功 | 正常渲染 | 抓取断流 → Degraded |
| Degraded | ACCESS_LOST/DEVICE_REMOVED（锁屏、UAC、独占全屏、显卡重置） | 发 `glass-mode:"wallpaper"`；指数退避重建（0.5s 起 ×2，封顶 30s，**不放弃**——锁屏可长可短） | 重建成功 → 发 `glass-mode:"refract"` 回 Refract |
| Dead | 启动时引擎起不来（远程桌面、老显卡、Win10 2004 以下） | 本次运行固定壁纸折射 | 无（重启进程再试） |

壁纸兜底与 refract 同为 20px 药丸、同折射数学——**切换无形状跳变**，只有内容源变化。

已知盲区（接受，不修）：鼠标指针不映在玻璃里；DRM 视频区域在捕获中为黑；
DWM 动画瞬间（窗口最小化动画等）与玻璃内容存在一帧级不同步。

## 9. 配置

`acrylic` 布尔升级为模式串：

```json
{ "mode": "refract" }   // 默认。可选 "live"（DWM 亚克力，8px 圆角）、"wallpaper"
```

- 兼容旧键：无 `mode` 时，`acrylic:true → live`，`acrylic:false → wallpaper`，
  两者皆无 → `refract`；
- `glass` 参数（alpha/blur/displacement/band/radiusCollapsed/radiusCard/saturate）
  三种模式共用一套，语义不变；
- 仓库内嵌默认与 `%APPDATA%/glassgauge/config.json` 同步改为 `"mode":"refract"`。

## 10. 测试与验收

**先行技术验证（半天级，两条都过才铺全量）**：
1. 设置剔除标记后，复制帧里确实没有挂件自己（把原始捕获帧落盘人查）；
2. DComp 目标（topmost=FALSE）确实垫在 WebView 子窗口后面且 WebView 透明区透出它。
   失败备胎：挂件正下方贴同形状原生兄弟窗口承载渲染（已知可行，作为 plan 的分支步骤）。

**Rust 单测**：
- `dispmap.rs` 逐像素镜像 `ui/tests/disp.test.js` 全部 5 组用例；
- `geometry.rs`：选屏/clamp/跨屏用例，用 §7 的本机双屏基准。

**自动化验收**：debug 构建限定的托盘项"导出玻璃帧"——把管线最终输出写
`%APPDATA%/glassgauge/glass-dump.png`。截图隐形后这是唯一的自动化自检路径。

**手工清单**：拖动跟手；底下开亮窗玻璃变亮、关掉变暗；锁屏→解锁自动恢复
（Degraded→Refract）；双屏往返；静止时任务管理器 GPU 占用≈0；
截图确认挂件隐形（这是特性不是 bug）。

## 11. 依赖与出包

`windows` crate（features: Win32_Graphics_Direct3D11 / Dxgi / Direct2D /
DirectComposition / Win32_System_Com / Win32_UI_WindowsAndMessaging 等）。
全部链接系统 DLL，产物体积增量可忽略；不引入任何运行时再分发。

## 12. 非目标

- 指针/DRM 内容出现在玻璃里（§8 盲区）；
- Win10 2004 以下支持（Dead → 壁纸兜底即为该场景的产品行为）；
- 抓取范围超出挂件区域的任何用途（引擎只裁窗口±MARGIN，不存不传）。
