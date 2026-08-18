# glassgauge 原生折射引擎 实施计划

日期：2026-08-18
依据：`docs/superpowers/specs/2026-08-18-glassgauge-native-refraction-engine-design.md`（已批准）

## Phase 0 — 技术验证（gate：两条都过才继续）

顺序有讲究：**先验合成层序（B），再验自我剔除（A）**——一旦设了剔除标记，
我的自动截屏就看不见挂件了，层序必须趁看得见时验。

1. `Cargo.toml` 加 `windows` crate（D3D11/DXGI/D2D/DComp/WIC/WindowsAndMessaging features）。
2. **Spike B（层序）**：`engine/spike.rs`，环境变量 `GG_SPIKE=b` 时在 setup 里执行：
   DComp 设备 → `CreateTargetForHwnd(hwnd, FALSE)` → 半透明纯色表面 → Commit。
   验收：截屏可见纯色垫在白纱之下、内容之上不受影响。
   失败分支：改为"同形状原生兄弟窗口"承载渲染（spec §10），后续阶段接口不变。
3. **Spike A（剔除）**：`GG_SPIKE=a`：设 `WDA_EXCLUDEFROMCAPTURE` → 建复制通道 →
   取一帧 → 用 WIC 把挂件区域落成 PNG。验收：PNG 里是挂件**后面**的内容，没有挂件。

## Phase 1 — 几何与位移图（纯函数层）

- `engine/geometry.rs`：DXGI 输出枚举 → 显示器物理矩形表；中心选屏；
  crop 区（窗口±24×dpr，clamp 到输出边界）。单测用本机双屏基准。
- `engine/dispmap.rs`：`disp.js` 的 Rust 镜像（BGRA 字节序，R→x G→y）。
  单测逐像素镜像 `ui/tests/disp.test.js` 全部 5 组用例。
- 验收：`cargo test` 新增用例全绿。

## Phase 2 — 抓取通道

- `engine/capture.rs`：按输出建 D3D11 设备（BGRA flag）+ `DuplicateOutput`；
  `AcquireNextFrame(100ms)`；脏区 ∩ crop 区过滤；`CopySubresourceRegion`
  裁剪进**常驻小纹理**后立即 `ReleaseFrame`；`ACCESS_LOST/DEVICE_REMOVED`
  上报状态机。
- 验收：`GG_SPIKE=cap` 连续落 3 帧 PNG，内容随后面窗口变化而变化。

## Phase 3 — D2D 管线 + DComp 输出

- `engine/render.rs`：D2D 设备上下文挂 DComp 表面；特效链
  Crop→GaussianBlur(σ=blur/2×dpr)→DisplacementMap(scale=displacement×dpr,
  位移输入=dispmap 位图)→Saturation；圆角几何 PushLayer（radius×dpr，AA）；
  绘制偏移 −margin，使窗口区对齐表面原点。
- 帧循环（spec §6）：事件通道 drain → 取帧/过滤 → 强制渲染标志 → 渲染 → Commit。
  移动时用常驻纹理即时重渲染（跟手）。
- 验收：`GG_SPIKE=pipe` 落管线输出 PNG，肉眼可见模糊+边缘折射+圆角。

## Phase 4 — 模式接线

- 配置：`mode` 键 + 旧 `acrylic` 兼容映射（spec §9）；两份 config 改 `"mode":"refract"`。
- `get_glass_mode` 命令 + `glass-mode` 事件；`panel.js`：radius 逻辑改三模式
  （refract/wallpaper=20px，live=8px）；壁纸层由模式/事件驱动（refract→清空，
  wallpaper→initGlass）。
- 状态机：Refract/Degraded(0.5s×2 退避封顶 30s 不放弃)/Dead；WDA 只在 refract
  进程期常开；窗口事件（Moved/Resized/ScaleFactorChanged）投递引擎线程。
- 验收：正常=引擎渲染；锁屏→解锁自动兜底与恢复；`mode:"live"`/`"wallpaper"`
  手动可切且外观符合各自定义。

## Phase 5 — 验收与收尾

- debug 构建托盘项"导出玻璃帧"（复用 spike 的 WIC 落盘）。
- 全量测试：`cargo test` + `node --test ui/tests/*.test.js` 全绿。
- 手工清单（spec §10）：跟手/亮暗响应/锁屏恢复/双屏/静止 GPU≈0/截图隐形确认。
- 删除 spike 环境变量入口中不再复用的部分；提交。

## 风险与预案

| 风险 | 预案 |
| --- | --- |
| DComp 垫不到 WebView 后面 | Phase 0 失败分支：同形状兄弟窗口（接口不变，只换输出宿主） |
| D2D 位移贴图通道序（BGRA/RGBA）弄反 | dispmap 单测 + `GG_SPIKE=pipe` 落盘肉眼比对折射方向 |
| 混合显卡（核显+独显）设备与输出不同适配器 | 设备按**输出所属适配器**创建，不用默认适配器 |
| 拖动时 Moved 事件频率过高 | 渲染 sub-ms，先不节流；若掉帧再合并事件（每帧最多渲一次） |
| WDA 设置后我的自动截屏失明 | 验收全部改走引擎内 PNG 落盘（dump 命令） |
