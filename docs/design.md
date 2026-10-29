# 设计方案

> 本文是 **技术设计**（架构、模块边界、接口与关键机制）。
> 项目规则见 `AGENTS.md`；排期与任务拆解见 [`implementation.md`](./implementation.md)；实验证据见 `.opencode/plans/wallpaper-probe-26h2.md`。

---

## 1. 目标与边界

在 Windows 上实现动态壁纸引擎，功能对标 Wallpaper Engine：视频、网页、场景、图片壁纸，多显示器，性能策略，壁纸包，编辑器。

**不做**：Steam 创意工坊对接、Android 投屏、注入式 overlay。

**当前平台约束**（实测，见实验记录）：

- 26H2（build 26300）上 `WorkerW` 宿主窗口不再产生
- `Progman` 子窗口 + GDI 内容 = 不渲染 / 黑屏
- 顶层置底窗口能出画面，但与图标层的 z 序会漂移
- 本机 GT 720：仅 H.264 硬解

---

## 2. 总体架构

### 2.1 进程拓扑

```
┌──────────────────────────────────────────────────────────┐
│ wallpaper.exe  (Tauri)                                   │
│  ├─ UI WebView（React/Svelte）                            │
│  ├─ Library / Config / Scheduler（tokio 任务）            │
│  └─ Supervisor：拉起 wp-host、健康检查、重启、退化          │
└───────────────┬──────────────────────────────────────────┘
                │ 命名管道（长度前缀 + MessagePack）
   ┌────────────┼─────────────┬─────────────────┐
   ▼            ▼             ▼                 ▼
wp-host#0    wp-host#1     wp-host#2        wpctl（调试）
（显示器 0）  （显示器 1）   （显示器 2）
 HWND         HWND          HWND
 +后端        +后端         +后端
```

**为什么每屏一个进程**：GPU 驱动崩溃、网页死循环、解码器异常都只影响单屏；主程序不受影响，且可单独重启。

**为什么主进程不渲染**：Tauri 的 WebView 用于 UI，混入壁纸渲染会导致显存与崩溃域耦合。

### 2.2 wp-host 内部线程

| 线程 | 职责 | 说明 |
|---|---|---|
| main | Win32 消息循环，持有 HWND | 所有窗口 API 必须在此线程 |
| render | 驱动 D3D11/DComp 或托管 mpv | 与 main 通过 channel 通信 |
| audio | WASAPI loopback 采集（频谱用） | 仅在网页壁纸声明需要时启动 |
| ipc | 命名管道读写 | 单线程 async 或独立线程 |

HWND 的创建、`SetWindowPos`、`SetParent` 一律在 main 线程；渲染线程只操作 swapchain / DComp visual。

### 2.3 技术选型

| 领域 | 选择 | 理由 |
|---|---|---|
| UI 壳 | Tauri 2.x | 体积、Rust 集成、系统托盘 |
| 窗口/系统 | `windows` crate（0.6x） | 官方绑定，覆盖 DComp/DXGI |
| 异步 | tokio（仅主进程） | Tauri 已内置 |
| 序列化 | serde + rmp-serde | IPC 紧凑 |
| 存储 | SQLite（rusqlite bundled） | 单文件、事务、易迁移 |
| 日志 | tracing + tracing-subscriber | 结构化、可落文件 |
| 视频 | libmpv（阶段一可先用 mpv.exe + `--wid`） | 见 §5.2 |
| 网页 | WebView2（`webview2-com`） | 系统自带，支持 WebGL |
| 场景 | wgpu | 后期，跨后端 |

---

## 3. 桌面宿主子系统（核心难点）

### 3.1 设计目标

1. 壁纸内容显示在桌面图标**之下**、系统壁纸**之上**
2. 桌面右键、图标框选、任务栏交互完全不受影响
3. 任何系统状态变化后能在 3 秒内自愈
4. 挂不上时**降级**为静态壁纸，不黑屏

### 3.2 策略抽象

```rust
pub trait DesktopHost: Send {
    fn attach(&mut self, hwnd: HWND, rect: Rect) -> Result<()>;
    /// 把窗口放回预期层级；幂等，可被看门狗反复调用
    fn ensure_band(&mut self, hwnd: HWND) -> Result<BandState>;
    fn detach(&mut self, hwnd: HWND) -> Result<()>;
    fn strategy(&self) -> Strategy;
}
```

`BandState` 三态：`Correct` / `Corrected` / `Lost`。看门狗据此决定是否重建。

### 3.3 策略清单

| ID | 策略 | 适用 | 状态 |
|---|---|---|---|
| **E** | **反向接管 `SHELLDLL_DefView`（把图标层挂成自己的子窗口）** | **26H2 主力方案** | **实测通过：壁纸 99.1% 铺满 + 图标正常 + 右键/单击/双击全部正常** |
| A | `Progman` 子窗口（DefView 之下） | 24H2+ 理论路线 | 已证否：GDI 与 DComp 内容都被 DefView 完全盖住（测试色 0%） |
| A′ | `Progman` 子窗口（DefView 之上） | 诊断用 | 可见（93.6%）但盖住所有图标，仅用于判别实验 |
| B | 顶层窗口置底（`HWND_BOTTOM`） | 26H2 可显示 | 画面可见，但盖住桌面图标；z 序会漂移，需看门狗 |
| C | 顶层窗口精确插入 `DefView` 相邻位 | 26H2 备选 | 未实现（分界在 DefView，结构性不可行，优先级下调） |
| D | `WorkerW` 子窗口 | Win10 / 22H2–23H2 | 26H2 上 `0x052C`（wParam=0 与 0xD）均返回 0，不产生宿主窗口 |

**关键结论**：26H2 上「壁纸可见」与「图标可见」的分界线就是 `SHELLDLL_DefView`。
`Progman` 的子窗口只能落在分界线的某一侧，因此策略 A 在结构上无法同时满足两者；
唯一能同时满足的是「把 DefView 反向挂到自己的窗口下」（策略 E）。

**实测数据来源**：`.opencode/plans/wallpaper-probe-26h2.md` §6–§7（`wpctl probe --minimize --shot` 截图判定 + 人工点击验证）。

**选择顺序**：E → B（降级）→ A′（仅诊断）。判据不是"能 attach"，而是"画面可见 + 图标在上 + 输入正常"三项同时成立。

**策略 E 实现约束**（违反任一条都会退化为黑屏或盖图标）：

1. DComp `CreateTargetForHwnd(hwnd, topmost)` 必须传 `false`，否则 visual 盖住子窗口里的图标。
2. 宿主窗销毁前必须先 `SetParent(DefView → Progman)`；`DestroyWindow` 会带走子窗口。
3. 进程启动时调用 `heal_desktop()`，修复上次异常退出遗留的窗口树；`Drop` 只能兜住正常路径。

### 3.4 状态机

```
        ┌──────────┐  probe ok   ┌──────────┐
        │ Detecting├────────────►│ Attached │
        └────┬─────┘             └────┬─────┘
             │ 全部失败                │ band lost
             ▼                        ▼
        ┌──────────┐  retry×N     ┌──────────┐
        │ Failed   │◄─────────────┤ Recovering│
        └────┬─────┘              └────┬─────┘
             │ 降级                     │ 成功
             ▼                          └──► Attached
        ┌──────────┐
        │ Degraded │（静态壁纸 + 提示）
        └──────────┘
```

恢复策略：指数退避（0.5s → 1s → 2s → 4s，上限 5 次/分钟）；连续失败则降级并上报 UI。

### 3.5 看门狗

触发源：

| 事件 | API |
|---|---|
| z 序变化 | `SetWinEventHook(EVENT_OBJECT_REORDER)` |
| 前台切换 | `EVENT_SYSTEM_FOREGROUND` |
| Explorer 重启 | `RegisterWindowMessage("TaskbarCreated")` |
| 分辨率/显示器变化 | `WM_DISPLAYCHANGE` |
| DPI 变化 | `WM_DPICHANGED` |
| 锁屏/解锁 | `WTSRegisterSessionNotification` |
| 电源状态 | `WM_POWERBROADCAST` |
| 兜底轮询 | 2 s 定时器 |

判定方法（避免误判）：检查 `GetWindow(hwnd, GW_HWNDPREV)` 链条中，宿主层级标志窗口（`Progman` / `SHELLDLL_DefView` / 指定 anchor）与自身的前后关系是否与策略预期一致。

> 不要用"像素采样"做健康检查：代价高且会被遮挡误判。

### 3.6 窗口样式

```
WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS
WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT
```

- `WS_EX_NOACTIVATE`：点击不抢焦点
- `WS_EX_TOOLWINDOW`：不出现在 Alt+Tab / 任务栏
- `WS_EX_TRANSPARENT`：尽量让命中测试穿透

**必须**在 `WndProc` 中处理：

```rust
WM_NCHITTEST => return HTTRANSPARENT as LRESULT
```

实测：仅加 `WS_EX_TRANSPARENT`，`WindowFromPoint` 仍返回自身，桌面点击会被吃掉。

### 3.7 鼠标/键盘交互

壁纸窗口不接受输入，但壁纸可能想响应鼠标（视差、粒子跟随）。

- 用 `GetCursorPos`（30–60 Hz）读取全局坐标，换算成壁纸本地坐标，经 IPC 推给渲染进程
- 事件类型：`CursorMove { x, y, inside }`、`MouseDown/Up`（可选，需要额外钩子）
- 键盘交互留到 M3，且必须"显式开启"，默认关闭（避免误触）

---

## 4. 渲染子系统

### 4.1 两种渲染形态

| 形态 | 谁驱动 | 例子 |
|---|---|---|
| **Renderer** | 我们每帧调用 `render()` | 图片、幻灯片、wgpu 场景 |
| **Presenter** | 外部组件自渲染，我们只控制 | libmpv（`--wid`）、WebView2 |

接口拆成两个 trait，避免用一个 `render()` 硬套：

```rust
pub trait Renderer: Send {
    fn resize(&mut self, size: PhysicalSize) -> Result<()>;
    fn render(&mut self, ctx: &FrameCtx) -> Result<()>;
    fn set_visible(&mut self, visible: bool);
    fn stats(&self) -> Stats;
}

pub trait Presenter: Send {
    fn load(&mut self, spec: &WallpaperSpec) -> Result<()>;
    fn resize(&mut self, size: PhysicalSize) -> Result<()>;
    fn play(&mut self) -> Result<()>;
    fn pause(&mut self) -> Result<()>;
    fn set_volume(&mut self, v: f32) -> Result<()>;
    fn stats(&self) -> Stats;
}
```

### 4.2 视频后端

阶段一（M1）：**外部 `mpv.exe --wid=<hwnd>`** + `--input-ipc-server` 控制。

- 优点：不需要链接 libmpv，规避 GPL 传染；崩溃隔离天然成立
- 缺点：进程数 +1，启动约 200 ms

阶段二（产品化前决策）：二选一

1. 继续外部进程（GPL 只约束 mpv 自身分发）
2. 自研 FFmpeg(LGPL) + D3D11 视频渲染管线（约 2 个月）

**这个决策必须在 M0 结束前定稿**，写入 ADR。

### 4.3 网页后端

WebView2 直接嵌入宿主 HWND（不用 Tauri 窗口）：

- 每张网页壁纸独立 `user data folder`：`%LOCALAPPDATA%\wallpaper\webview-profiles\{item_id}`
- 关闭下载、开发者工具（release）、外部导航（白名单）
- 属性桥：`window.wallpaperPropertyListener` 风格 API，通过 `postMessage` 双向通信
- 音频响应：宿主进程采集 WASAPI loopback → 计算频段 → 以 30 Hz 推送数组

### 4.4 场景后端（M3+）

- `wgpu` + 自研场景格式（节点树 + 材质 + 粒子系统）
- 先支持：静态模型（glTF）、相机、光照、粒子发射器
- 编辑器放 Web 端（复用 Tauri WebView），场景文件与运行时共用 schema

---

## 5. 数据模型

### 5.1 核心类型

```rust
pub struct WallpaperId(pub Uuid);

pub enum WallpaperKind { Image, Video, Web, Scene, Application }

pub struct LibraryItem {
    pub id: WallpaperId,
    pub kind: WallpaperKind,
    pub title: String,
    pub source: PathBuf,          // 包目录或单文件
    pub preview: Option<PathBuf>,
    pub tags: Vec<String>,
    pub properties: PropertySchema,
    pub added_at: i64,
}

pub struct Assignment {
    pub monitor: MonitorKey,      // 设备名，避免索引漂移
    pub item: WallpaperId,
    pub overrides: PropertyValues,
}

pub struct MonitorKey(pub String); // \\.\DISPLAY1
```

### 5.2 属性系统

对齐 WE 的属性模型，便于导入其风格的项目：

```rust
pub enum PropertyType {
    Bool { default: bool },
    Slider { min: f32, max: f32, step: f32, default: f32 },
    Color { default: [u8; 3] },
    Combo { options: Vec<String>, default: usize },
    Text { default: String },
    File { filter: String },
}
```

每个属性带 `key` 与 `label`，网页壁纸通过 `postMessage` 订阅变更。

### 5.3 壁纸包 `.wpkg`

本质是 zip：

```
manifest.json
preview.jpg
content/            # 视频文件 / 网页根目录 / 场景文件
properties.json     # 可选，作者声明的可调参数
LICENSE.txt
```

```json
{
  "schema": 1,
  "id": "8f1c...",
  "type": "web",
  "title": "Neon Rain",
  "entry": "content/index.html",
  "version": "1.0.0",
  "author": "...",
  "minEngine": "0.1.0"
}
```

导入时做校验：zip 炸弹防护（解压大小上限）、路径穿越校验、条目类型与文件存在性检查。

---

## 6. IPC 协议

### 6.1 传输

命名管道：`\\.\pipe\wallpaper-host-{session_id}-{monitor_index}`

帧格式：`u32 LE 长度` + msgpack 负载，单帧上限 8 MB（防止异常输入撑爆内存）。

### 6.2 消息

```rust
pub enum Command {
    Load { item: WallpaperId, spec: WallpaperSpec, rect: Rect },
    Unload,
    Pause { reason: PauseReason },
    Resume,
    SetProperties { values: PropertyValues },
    SetRect { rect: Rect },
    SetCursor { x: i32, y: i32, inside: bool },
    SetMuted { muted: bool },
    GetStats,
    Shutdown,
}

pub enum Event {
    Ready { backend: String },
    Stats { fps: f32, cpu: f32, gpu_mem_mb: u32 },
    Error { code: ErrorCode, message: String },
    Log { level: u8, message: String },
}
```

### 6.3 生命周期

1. Supervisor spawn `wp-host --monitor N --token <random>`
2. host 创建 HWND、attach 桌面、连接管道
3. host 发 `Ready`；超时 5 s 未就绪则杀掉重启
4. 运行期心跳：host 每 2 s 发 `Stats`；30 s 无消息视为失联

---

## 7. 调度与性能策略

### 7.1 暂停判定

```rust
pub enum PauseReason {
    FullscreenApp,      // SHQueryUserNotificationState == QUNS_RUNNING_D3D_FULL_SCREEN
    MaximizedCover,     // 同屏存在最大化窗口且未在忽略列表
    BatteryLow,
    SessionLocked,
    DisplayOff,
    RemoteSession,
    UserManual,
}
```

任一命中即暂停（避免多屏时一屏播放一屏看视频时 CPU 抢占）。

### 7.2 帧率与资源

| 类型 | 默认 | 可配 |
|---|---|---|
| 视频 | 跟随片源 | 倍速、循环、静音 |
| 网页 | 30 FPS | 5–60 |
| 场景 | 60 FPS | 5–144 |
| 图片 | 事件驱动 | — |

额外：显存预算（`IDXGIAdapter3::QueryVideoMemoryInfo`），超限降级分辨率或停用部分壁纸。

---

## 8. 持久化与目录布局

```
%APPDATA%\wallpaper\
  library.db          SQLite（items / assignments / playlists / settings）
  library\            <item_id>\ 解包后的壁纸
  cache\previews\     缩略图
  logs\

%LOCALAPPDATA%\wallpaper\
  webview-profiles\<item_id>\
```

### 8.1 表结构（初版）

| 表 | 关键列 |
|---|---|
| `items` | id, kind, title, source, preview, tags, added_at |
| `properties` | item_id, key, type, spec_json, value_json |
| `assignments` | monitor_key, item_id, overrides_json |
| `playlists` | id, name, interval_sec, shuffle |
| `playlist_items` | playlist_id, item_id, ord |
| `settings` | key, value |

SQLite 开 WAL；迁移用版本号 + 顺序脚本（`PRAGMA user_version`）。

---

## 9. 安全模型

网页壁纸 = 任意代码，按不可信处理：

1. 独立进程 + 独立 WebView2 用户数据目录
2. 禁用下载、外部导航、开发工具（release）
3. 宿主对象只暴露白名单方法（无文件系统、无剪贴板）
4. 包导入限制解压总大小、路径穿越、可执行文件
5. 渲染进程降权：不用管理员权限运行（`app.manifest` 声明 `asInvoker`）

---

## 10. 关键决策记录（ADR 摘要）

| ID | 决策 | 理由 | 影响 |
|---|---|---|---|
| ADR-001 | 放弃 `WorkerW` 宿主 | 26H2 实测不再产生（wParam=0 / 0xD 均无效） | 需多策略探测 |
| ADR-002 | 桌面内容必须走 D3D/DComp | GDI 子窗口实测黑屏 | 后端不允许 GDI 绘制 |
| ADR-003 | 每显示器独立进程 | 崩溃/显存隔离 | IPC 复杂度上升 |
| ADR-004 | 视频先用外部 mpv.exe | 规避 GPL 链接风险 | 多一个进程，M0 后复核 |
| ADR-005 | 网页壁纸独立 WebView2 配置目录 | 安全隔离 | 磁盘占用增加 |
| ADR-006 | 26H2 采用「反向接管 `SHELLDLL_DefView`」 | 分界线在 DefView，Progman 子窗口只能落一侧 | 必须 detach + 启动自愈，且 DComp visual 不得 topmost |

---

## 11. 待验证假设

这些假设会在 M0 被证实或推翻，设计需保持可切换：

- [x] ~~A 策略（Progman 子窗口 + DirectComposition）在 26H2 可行~~ → **已推翻**：内容永远落在 DefView 的一侧
- [x] ~~`WM_NCHITTEST → HTTRANSPARENT` 足以让桌面完全可用~~ → **已验证**（策略 E，人工确认右键/单击/双击）
- [x] ~~B/C 策略的 z 序漂移可通过 `ensure_band` 完全收敛~~ → **部分**：漂移可修正，但 B 会盖住图标，不能作为最终方案
- [ ] mpv `--wid` 嵌入在置底窗口上能正常输出（D3D 交换链 vs 窗口 surface）
- [ ] 多屏 + 混合 DPI 下窗口矩形计算无需特殊处理
- [ ] Explorer 重启后策略 E 能自动重挂（代码已实现，未实测）
