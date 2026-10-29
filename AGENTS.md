# AGENTS.md — wallpaper

> 目标：在 Windows 上复刻 Wallpaper Engine 的全部功能。
> 技术栈：Rust + Tauri 2.x。平台：仅 Windows。
> 当前验证环境：Windows 11 26H2（build 26300.9818），单屏 1920×1080，GT 720 / 2 GB。
> 完整实验记录：`.opencode/plans/wallpaper-probe-26h2.md`

---

## 0. 硬规则（违反即回退）

1. **不要用 `WorkerW` 作为壁纸宿主。** 26H2 上 `0x052C`（wParam=0 与 0xD）都返回 0，
   不产生宿主窗口，已实测证否。
2. **26H2 的壁纸唯一可行路线是「反向接管 `SHELLDLL_DefView`」**：把图标层挂成宿主窗口的子窗口。
   壁纸画父窗口（D3D11 / DirectComposition），图标是子窗口，天然在上。
3. **策略 E 的三条铁律**：① DComp `CreateTargetForHwnd` 的 `topmost` 必须为 `false`；
   ② 销毁宿主窗口前必须先把 DefView 还给 `Progman`；③ 进程启动先跑 `heal_desktop()`。
   违反任一条 → 图标消失、盖住图标或被 `DestroyWindow` 一起带走。
4. **不要把内容画到 `Progman` 子窗口的 GDI 表面上。** 实测黑屏，且会被 DefView 盖住。
5. **不要只调用一次 `SetWindowPos` 就认为层级正确。** z 带会漂移，必须有看门狗。
6. **不要让壁纸窗口吃掉桌面输入。** 除 `WS_EX_TRANSPARENT` 外，必须实现 `WM_NCHITTEST → HTTRANSPARENT`。
7. **M1 之前不写任何编辑器代码。** 范围蔓延的最大来源。
8. **壁纸渲染不能在主进程里做。** 每显示器一个独立 `wp-host` 进程。
9. **挂不上桌面时退回静态壁纸并提示，绝不黑屏。**
10. **完成一个小功能就立刻提交一次，不要攒一批改动。** 提交信息必须遵循 Angular 规范，
   且**全部使用中文**：
   `<type>(<scope>): <中文描述>`，type 取
   `feat | fix | docs | style | refactor | perf | test | build | ci | chore | revert`。
   scope 用 crate 名去掉 `wp-` 前缀（`core`、`ipc`、`platform`、`render`、`host`、`cli`），
   跨模块改动可省略。描述用祈使句、不加句号，长度不超过 40 个汉字。
   破坏性变更用 `!` 标记并在页脚写 `BREAKING CHANGE:`。
   示例：`feat(platform): 添加顶层置底宿主策略与 z 序自愈`、
   `fix(cli): 修正 probe 输出编码`。

---

## 1. 项目定位

复刻对象是 Wallpaper Engine 的功能集合（动态壁纸、网页壁纸、场景壁纸、多屏、性能策略、壁纸包、编辑器）。

同时明确**不做**：
- Steam 创意工坊对接（无公开 API，只能自建商店）
- Android 投屏 / Steam 集成
- 注入式 overlay（会被反作弊判定）

---

## 2. 当前状态与最高风险

### 已完成
- 26H2 桌面挂载层攻坚：**策略 E「反向接管 `SHELLDLL_DefView`」实测通过**
  （壁纸铺满、图标正常、右键/单击/双击正常、150 秒零漂移）。
- 策略 A / B / D 全部证否并记录原因（见 `.opencode/plans/wallpaper-probe-26h2.md` §6–§7）。

### 最高风险（M0 收尾要解决）
挂载机制已确定，剩下的是**边界条件**而不是可行性：

| 场景 | 现状 |
|---|---|
| 多显示器 / 混合 DPI | 单屏环境，未验证 |
| Explorer 重启、`Win+D`、任务视图 | 代码已实现重挂；`Win+D` 人工试过无异常 |
| 锁屏解锁、RDP 重连、休眠唤醒 | 未验证 |
| 崩溃后桌面自愈 | `heal_desktop()` 已实现，未做故障注入测试 |
| 全屏独占游戏暂停判定 | 未验证 |
| 长时间（≥ 30 分钟）稳定性 / GPU 占用 | 只跑到 150 秒 |

**M0 尚未收尾，仍不要开始写业务代码。**

---

## 3. 架构

### 3.1 进程模型

```
wallpaper.exe            Tauri 应用：UI / 库管理 / 配置 / 调度 / IPC 中枢
  └─ wp-supervisor       拉起、监控、重启渲染进程
       ├─ wp-host #monitor1    独立进程：桌面宿主窗口 + 渲染后端
       ├─ wp-host #monitor2
       └─ ...
```

- IPC：命名管道 `\\.\pipe\wallpaper-host-{id}`，长度前缀 + serde 消息
- 主进程不渲染壁纸；Tauri WebView 只做 UI 与预览缩略图

### 3.2 目录约定

```
crates/
  wp-core/       领域模型、壁纸包格式、属性 schema、配置
  wp-platform/   Win32 桌面宿主、显示器枚举、DPI、电源、全屏检测、锁屏监听
  wp-render/     渲染后端：image / video(mpv) / web(webview2) / scene(wgpu)
  wp-host/       渲染进程 bin（每显示器一个）
  wp-ipc/        管道协议与客户端
apps/
  desktop/       Tauri 2.x 应用
  cli/           wpctl：无 UI 也能 attach/pause/switch
```

`wp-platform` 必须能脱离 UI 单独测试。

### 3.3 桌面宿主模块（最高优先级）

```
wp-platform::desktop_host
  ├─ probe()            运行时探测可用策略（启动时 + Explorer 重启后）
  ├─ DesktopHost trait  attach / detach / ensure_band / surface
  ├─ StrategyA          Progman 子窗口 + DirectComposition
  ├─ StrategyB          顶层窗口置底
  ├─ StrategyC          精确插入 DefView 相邻位
  ├─ StrategyD          WorkerW（Win10 / 22H2–23H2 回退）
  └─ BandWatchdog       z 带看门狗 + 自愈
```

看门狗必须覆盖：

- `SetWinEventHook`：`EVENT_OBJECT_REORDER`、`EVENT_SYSTEM_FOREGROUND`
- `TaskbarCreated`（Explorer 重启）
- `WM_DISPLAYCHANGE`、`WM_DPICHANGED`、`WM_SETTINGCHANGE`
- `WTS_SESSION_*`（锁屏/解锁）、`WM_POWERBROADCAST`
- 约 2 s 一次的 `GetWindow(hwnd, GW_HWNDPREV)` 链条核对

### 3.4 渲染后端

| 类型 | 方案 | 备注 |
|---|---|---|
| 图片/幻灯片 | `image` + D3D11 纹理 | |
| 视频 | libmpv 嵌入 `--wid` | GPL 传染，闭源需换 FFmpeg(LGPL) 或 Media Foundation |
| 网页 | WebView2 直接嵌入宿主窗口 | 不用 Tauri 窗口；`webview2-com` |
| 场景/3D | `wgpu` + 自研 scene 格式 | M3 起；MVP 可先用 three.js 网页壁纸 |

**许可证决策必须在 M0 定稿**，否则整条视频管线要重写。

---

## 4. 里程碑

| 阶段 | 内容 | 周期 | 通过标准 |
|---|---|---|---|
| M0 | 桌面宿主攻坚 + `wpctl probe` | 1–2 周 | 三条策略矩阵测完，选定一条并记录边界条件 |
| M1 | 视频/图片壁纸、多屏分配、托盘、自启、暂停规则、SQLite 配置 | 2–3 周 | 非技术用户能设一张视频壁纸并稳定一周 |
| M2 | 网页壁纸、JS 属性桥、音频响应、跨屏、播放列表、`.wpkg` 包 | 3–4 周 | 跑通自写的带属性面板 + 音频响应网页壁纸 |
| M3 | 鼠标交互、外部 exe 嵌入、GPU/显存预算、HDR 初步 | 3–4 周 | 交互壁纸可用，桌面输入不丢 |
| M4 | 场景运行时（wgpu）、glTF、Web 编辑器 | 6–8 周 | 可做出并导入一个场景壁纸 |
| M5 | 自建商店、自动更新、崩溃收集 | 4–6 周（可选） | — |

M0 验收细则：

1. 画面可见、满屏、无撕裂
2. 桌面图标、右键菜单、任务栏正常
3. 10 分钟 + 最小化/还原 20 次后层级仍正确
4. `Win+D`、任务视图、锁屏解锁后 3 s 内自恢复
5. Explorer 重启后 5 s 内自恢复
6. 1080p H.264：CPU < 10%，GPU < 30%
7. 多屏（如有）：独立挂载，混合 DPI 不错位

---

## 5. 环境约束

- **Shell 一律用 pwsh 7**（`D:\software\PowerShell\7\pwsh.EXE`）。禁止调用 `powershell.exe`（5.1），
  它的管道编码与语法差异会让输出乱码、脚本失效。涉及中文输出时先设置
  `[Console]::OutputEncoding = [Text.Encoding]::UTF8`。
- **GT 720 / 2 GB**：仅 H.264 硬解，无 VP9/AV1/HEVC 硬解，无 NVENC → 首版按 1080p H.264 定标，导入时做能力检测。
- WebView2 156.x 已安装，可直接用。
- 现有可复用脚手架：`E:\private\Rust\aweak`（托盘 / 自启 / 配置 / 崩溃处理 / 单实例 / 电源事件），可抽成公共 crate，省约一周。

---

## 6. 常用命令（约定）

```bash
cargo run -p wpctl -- probe --strategy all --duration 60
cargo run -p wpctl -- probe --strategy toplevel_bottom --minimize --shot out\shot.bmp
cargo run -p wpctl -- attach --monitor 0 --source <file|url>
cargo run -p wpctl -- pause
cargo tauri dev   # 在 apps/desktop 下
```

---

## 7. 参考文档

- `docs/design.md` — 技术设计（架构、模块边界、接口、ADR），**改动核心机制前必读**
- `docs/implementation.md` — 实现方案（工程结构、任务拆解、规范、测试与发布）
- `.opencode/plans/wallpaper-plan.md` — 完整实现规划（功能分级、风险、里程碑）
- `.opencode/plans/wallpaper-probe-26h2.md` — 26H2 挂载层实验记录与证据
