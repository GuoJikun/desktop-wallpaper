# 实现方案

> 本文是 **执行层面的落地方案**：工程结构、任务拆解、规范、验证与发布。
> 技术设计见 [`design.md`](./design.md)；项目规则见 `AGENTS.md`。

---

## 1. 工程结构

```
E:\wallpaper\
├─ AGENTS.md
├─ Cargo.toml                # workspace 根
├─ rust-toolchain.toml       # 固定工具链
├─ crates/
│  ├─ wp-core/               # 领域模型、包格式、属性 schema
│  ├─ wp-platform/           # Win32：桌面宿主、显示器、DPI、电源、会话
│  ├─ wp-render/             # 后端：image / video / web / scene
│  ├─ wp-ipc/                # 管道协议（crate 同时被 host 与 supervisor 依赖）
│  └─ wp-host/               # bin：每显示器一个渲染进程
├─ apps/
│  ├─ desktop/               # Tauri 应用（src-tauri + ui）
│  └─ cli/                   # wpctl：调试与管理
├─ docs/
└─ .opencode/plans/
```

### 1.1 workspace 根 `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = ["crates/*", "apps/cli", "apps/desktop/src-tauri"]

[workspace.package]
edition = "2021"
rust-version = "1.83"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rmp-serde = "1"
thiserror = "1"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
rusqlite = { version = "0.32", features = ["bundled"] }
uuid = { version = "1", features = ["v4", "serde"] }
windows = "0.58"
```

版本在 M0 落地时以 `cargo add` 的实际解析结果为准，不手抄。

### 1.2 依赖边界

| crate | 允许依赖 |
|---|---|
| `wp-core` | serde / uuid / thiserror（**不依赖 windows**） |
| `wp-platform` | windows、tracing、wp-core |
| `wp-render` | windows、wgpu/mpv/webview2、wp-core |
| `wp-ipc` | serde、rmp-serde、wp-core |
| `wp-host` | 上述全部 + tokio（可用 std 线程替代） |
| `apps/desktop` | tauri、wp-* |

`wp-core` 不依赖 `windows` 是为了能在非 Windows 上跑单元测试。

---

## 2. 开发环境

| 项 | 要求 |
|---|---|
| Rust | stable ≥ 1.83，`x86_64-pc-windows-msvc` |
| VS Build Tools | MSVC + Windows SDK 10.0.22621+ |
| Node | ≥ 20（Tauri 前端） |
| WebView2 | 已安装（本机 156.x） |
| mpv | 已安装（阶段一用外部进程） |

验证脚本：`cargo xtask doctor` 检查以上依赖并打印版本（M0 附带）。

---

## 3. M0：桌面宿主攻坚（1–2 周）

### 3.1 目标

用真实 Rust 代码替代 PowerShell 实验，把 A/B/C/D 四条策略 × 两种内容（GDI / D3D11 交换链）的矩阵测完，产出**策略决策记录**。

### 3.2 文件级任务

```
crates/wp-platform/src/
  desktop/
    mod.rs          DesktopHost trait、Strategy 枚举、现状与建议顺序
    probe.rs        运行时探测：Progman/DefView/WorkerW 存在性、0x052C 效果
    toplevel.rs     策略 B：顶层窗口置底（已实现，含 z 序自愈）  ✅
    progman.rs      策略 A：Progman 子窗口 + DComp visual（待实现）
    anchored.rs     策略 C：精确插入 DefView 相邻位（待实现）
    watchdog.rs     SetWinEventHook + 定时校验（M0 第 4 步）
  window.rs         创建宿主窗口（窗口类、DPI awareness、WM_NCHITTEST、GDI 测试绘制）
  monitor.rs        EnumDisplayMonitors / 每屏矩形 / 设备名
apps/cli/src/
  main.rs           clap：probe（含矩阵执行器）/ attach / pause / stats
```

策略 D（WorkerW）在 26H2 上已被证否，不单独建文件：`Strategy::WorkerW` 的 `open()`
直接返回 `Unsupported`，仅保留给旧系统分支使用。

### 3.3 probe 的行为

```
wpctl probe --strategy all --content gdi,d3d --duration 30 --report out/report.json
```

每个组合执行：

1. 创建宿主窗口（全屏、纯色 / D3D11 清屏为已知颜色）
2. attach
3. 断言：`WindowFromPoint` 命中图标层而非自身；z 序邻居符合预期
4. 截图采样：中心点颜色是否为预期色
5. 干扰：最小化/还原全部窗口 ×5、`Win+D`、切换前台
6. 每步记录 `BandState` 与恢复耗时
7. 输出 JSON：`{strategy, content, visible, icons_on_top, input_ok, recover_ms, verdict}`

**当前已落地（步骤 1/2/6）**：`wpctl probe --strategy all --content gdi --duration 30`
会为每条策略真实建窗、`attach`、每 2 s 跑一次 `ensure_band` 并统计修正/丢失次数，
最后输出 `{strategy, attached, checks, corrected, lost, final_band, handle_alive}`。
步骤 3/4/5（自动判定可见性与输入穿透、自动施加干扰）仍需人工观察屏幕配合，
自动化版本在 M0 第 3 步补上；`--content d3d` 在 D3D11 后端接入后可用。

### 3.4 验收

| # | 标准 |
|---|---|
| 1 | 画面可见、满屏、无撕裂 |
| 2 | 图标、右键菜单、任务栏正常 |
| 3 | 10 分钟 + 20 次最小化/还原后层级仍正确 |
| 4 | `Win+D`、任务视图、锁屏解锁 3 s 内自恢复 |
| 5 | Explorer 重启 5 s 内自恢复 |
| 6 | 1080p H.264：CPU < 10%，GPU < 30% |
| 7 | 多屏（如有）：独立挂载，混合 DPI 不错位 |

### 3.5 出口条件

- 至少一条策略通过全部 7 项 → 写入 ADR，进入 M1
- 全部失败 → 立即停下来复议架构（考虑 DirectComposition 全屏层 / 系统壁纸 API 降级），不硬推到 M1

---

## 4. M1：MVP（2–3 周）

### 4.1 范围

| 模块 | 内容 |
|---|---|
| 库管理 | 导入 mp4/webm/gif/jpg/png，生成缩略图，列表 |
| 分配 | 按显示器设置壁纸（1 屏先跑通） |
| 宿主 | 单显示器 attach/detach，暂停/恢复 |
| 托盘 | 显示、暂停、退出、打开主界面 |
| 自启 | 注册表 Run 键（参考 aweak 的实现） |
| 配置 | SQLite 落库 + 启动恢复 |
| 守护 | 单实例、host 崩溃自动重启、降级静态壁纸 |

### 4.2 任务顺序

1. `wp-core` 领域模型 + SQLite schema
2. `wp-ipc` 协议与管道实现（含超时、重连）
3. `wp-host` 骨架：窗口 + 图片后端（最小闭环）
4. `wpctl attach` 打通：能手动挂一张图
5. 视频后端：外部 mpv 进程
6. Tauri UI：库 + 预览 + 托盘
7. 暂停规则（全屏/锁屏/电池）
8. 自启、单实例、崩溃重启

> 顺序原则：**先打通"进程 → 窗口 → 像素"，再做 UI**。UI 是最后一块。

### 4.3 通过标准

非技术用户能把一个视频设为壁纸，连续一周不需人工干预；Explorer 重启、锁屏、睡眠唤醒后自动恢复。

---

## 5. M2：网页壁纸 + 多屏（3–4 周）

| 内容 | 要点 |
|---|---|
| WebView2 后端 | 嵌入宿主 HWND，独立 user data 目录 |
| 属性桥 | `postMessage` 双向，白名单方法 |
| 音频响应 | WASAPI loopback → 频段数组 30 Hz 推送 |
| 多屏 | 每屏一个 host，`MonitorKey` 用设备名 |
| span 模式 | 单个跨屏窗口或按屏切分一致画面 |
| 播放列表 | 定时切换、随机、按屏独立 |
| 热键 | 暂停/下一张/静音 |
| `.wpkg` | 打包、导入、校验、缩略图 |

通过标准：能跑通自写的、带属性面板和音频响应的网页壁纸，且双屏下互不干扰。

---

## 6. M3：交互与外部程序（3–4 周）

- 鼠标坐标转发（`GetCursorPos` 轮询 → IPC → 渲染端）
- 区域命中（壁纸内 UI 元素）
- 任意外部 exe 嵌入（`--wid` 等价能力）
- 全屏应用忽略列表 + 反作弊友好策略（检测到独占全屏彻底停渲染）
- GPU 适配器选择、显存预算降级
- HDR 初步（`DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`）

---

## 7. M4：场景系统（6–8 周）

| 子项 | 说明 |
|---|---|
| 场景格式 | 节点树 JSON：camera / mesh / light / particle / material |
| 运行时 | wgpu 渲染，PBR 基础光照 |
| 导入 | glTF 2.0 静态模型 |
| 粒子 | CPU 模拟（M4），后期评估 GPU 粒子 |
| 编辑器 | Web 端，复用 Tauri WebView，属性面板 + 场景树 |
| 预览 | 编辑器内实时渲染（同一运行时） |

---

## 8. 编码规范

1. **错误处理**：库代码 `thiserror`，应用层 `anyhow`；禁止 `unwrap()`（测试除外）
2. **unsafe**：每个 `unsafe` 块必须有 `// SAFETY:` 注释说明不变量
3. **平台隔离**：所有 Win32 调用集中在 `wp-platform`，上层不直接依赖 `windows`
4. **日志**：`tracing`，字段化（`info!(monitor = %key, strategy = ?s, "attached")`）
5. **配置**：不新增配置文件格式，统一进 SQLite
6. **提交**：Conventional Commits；一个 PR 一个主题
7. **feature flag**：后端按 `--features video,web,scene` 可选，便于定位崩溃源

---

## 9. 测试策略

| 层级 | 内容 | 工具 |
|---|---|---|
| 单元 | `wp-core` 模型、包解析、路径校验 | `cargo test`（跨平台） |
| 集成 | IPC 往返、host 启动握手、超时重启 | `cargo test --test ipc` |
| 平台 | 桌面宿主矩阵 | `wpctl probe` + 报告断言 |
| E2E | 导入 → 设置 → 暂停 → 恢复 → 退出 | 手工脚本 + 截图比对 |
| 兼容 | Win10 22H2 / Win11 23H2 / 24H2 / 26H2 | 虚拟机快照 |
| 性能 | 1080p 视频、网页壁纸的 CPU/GPU/内存 | 自带 `stats` + 任务管理器采样 |

**兼容性矩阵**（M0 起每周跑一次）：

| 系统 | 单屏 | 双屏 | 150% DPI |
|---|---|---|---|
| Win10 22H2 | ✓ | ✓ | ✓ |
| Win11 23H2 | ✓ | ✓ | ✓ |
| Win11 24H2 | ✓ | ✓ | ✓ |
| Win11 26H2 | ✓ | — | ✓ |

---

## 10. 构建与发布

| 阶段 | 做法 |
|---|---|
| 开发 | `cargo run -p wpctl -- probe`，`cargo tauri dev` |
| CI | GitHub Actions：`cargo fmt --check`、`clippy -D warnings`、`cargo test`（windows-latest） |
| 打包 | `cargo tauri build` → NSIS/MSI |
| 签名 | 代码签名证书（上架前必备，否则 SmartScreen 拦截） |
| 更新 | `tauri-plugin-updater` + 自建更新源（M5） |
| 崩溃 | `minidump-writer` 捕获 host 崩溃，落本地供用户导出 |

**产物**：`wallpaper.exe`、`wp-host.exe`、`wpctl.exe`（可选）。

---

## 11. 风险登记册

| ID | 风险 | 概率 | 影响 | 应对 | 触发点 |
|---|---|---|---|---|---|
| R1 | 26H2 宿主策略全部失败 | 中 | 致命 | M0 设出口条件；备选 DComp 全屏层 | M0 结束 |
| R2 | z 序反复漂移，用户体验差 | 高 | 高 | 看门狗 + 降级 | M0 验收 3/4 |
| R3 | libmpv GPL 影响闭源 | 中 | 高 | 外部进程方案 / 自研 FFmpeg | M0 决策 |
| R4 | 网页壁纸安全问题 | 中 | 高 | 独立进程 + 白名单 API | M2 评审 |
| R5 | 弱 GPU 上性能不达标 | 高 | 中 | 能力检测 + 分辨率降级 | M1 基准测试 |
| R6 | 多屏 DPI 兼容 | 中 | 中 | MonitorKey + DPI 感知统一 | M2 |
| R7 | 范围蔓延（编辑器） | 高 | 高 | 硬规则：M1 前不写 | 每次评审 |
| R8 | 反作弊误判 | 低 | 高 | 不注入、不 overlay | M3 |

---

## 12. 时间表（假设单人全职）

| 周 | 阶段 | 交付 |
|---|---|---|
| 1–2 | M0 | probe 报告 + ADR |
| 3–5 | M1 | 可用的视频壁纸 MVP |
| 6–9 | M2 | 网页壁纸 + 多屏 + 包格式 |
| 10–13 | M3 | 交互 + 外部程序 + 性能策略 |
| 14–21 | M4 | 场景运行时 + Web 编辑器 |
| 22–27 | M5（可选） | 商店 / 更新 / 崩溃上报 |

合计约 **6–7 个月**到"功能基本对齐"，不含商店与打磨。

---

## 13. 下一步动作

1. 建 workspace 与空 crate，跑通 `cargo check`
2. 实现 `wp-platform::window` + `strategy_b`（26H2 已验证可显示，先拿到能跑的基线）
3. 加 D3D11 清屏内容，重复实验观察是否比 GDI 稳定
4. 实现 `watchdog::ensure_band`，验证能否把漂移收敛
5. 补 A/C 策略，跑完矩阵，写 ADR
