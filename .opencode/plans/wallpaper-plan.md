# Wallpaper 复刻实现规划

> 基准：**复刻 Wallpaper Engine 的全量功能**
> 平台：**仅 Windows**（当前验证环境 Windows 11 26H2 / build 26300）
> 技术栈：**Rust + Tauri 2.x**
> 节奏：**先做可用 MVP，再逐段扩展**
> 实验记录见同目录 `wallpaper-probe-26h2.md`

---

## 一、核心判断

真正难的不是"渲染壁纸"，而是三件事：

1. **桌面挂载层的稳定性**——26H2 已证实经典 `WorkerW` 方案失效，替代方案 z 序会漂移。这是当前第一风险。
2. **多进程隔离与崩溃恢复**——WE 是 `ui32.exe` + 每张壁纸一个宿主进程；Tauri 单进程做不到。
3. **场景（Scene）系统与编辑器**——自研粒子/模型格式 + 可视化编辑器，工作量超过其余所有功能之和。

规划原则：**先把平台层和进程模型钉死，渲染后端按「视频 → 网页 → 场景」逐段接**，不要先做 UI。

---

## 二、功能域分级

| 功能域 | 说明 | 档期 |
|---|---|---|
| 静态图 / 幻灯片 / 过渡 | 无状态，最简单 | MVP |
| 视频壁纸 | HW 解码、循环、静音、倍速、多屏对齐 | MVP |
| 性能策略 | FPS 上限、全屏/最大化/电池/锁屏暂停 | MVP |
| 多显示器 / span / 混合 DPI | 每屏独立或整幅跨屏 | MVP |
| 托盘、自启、播放列表、热键 | 系统集成 | MVP |
| 网页壁纸 + 属性桥 + 音频响应 | WebView2 + JS API + WASAPI loopback | M2 |
| 壁纸包格式 / 导入导出 / 缩略图 | 自研 `.wpkg`（zip + manifest） | M2 |
| 鼠标交互（穿透 + 坐标转发） | 桌面不丢焦点，壁纸能收到坐标 | M2 |
| 应用程序壁纸（任意 exe） | 外部窗口嵌入桌面层 | M3 |
| 场景壁纸（粒子/模型/光照） | 自研格式 + 运行时 | M3–M4 |
| 可视化编辑器 | 独立"软件"量级 | M4 |
| 创意工坊 | Steam 无公开 API，只能自建商店 | M5（可选） |
| 录制/GIF 导出、Steam 集成、安卓投屏 | 边缘功能 | 不做/最后 |

**预期：不含商店与编辑器约 6–9 个月全职；含则翻倍。**

---

## 三、目标架构

### 3.1 进程模型

```
wallpaper.exe            Tauri 应用：UI / 库管理 / 配置 / 调度 / IPC 中枢
  └─ wp-supervisor       拉起、监控、重启渲染进程
       ├─ wp-host #monitor1    独立进程：桌面宿主窗口 + 渲染后端
       ├─ wp-host #monitor2
       └─ ...
```

- **每个显示器一个渲染进程**：GPU 驱动崩溃、网页死循环、解码失败都不拖垮主程序
- **IPC 用命名管道**：`\\.\pipe\wallpaper-host-{id}`，长度前缀 + serde 消息
- **主进程不渲染壁纸**；Tauri WebView 只负责 UI 与预览缩略图

### 3.2 Crate 结构

```
crates/
  wp-core/       领域模型、壁纸包格式、属性 schema、配置
  wp-platform/   Win32 桌面宿主、显示器枚举、DPI、电源、全屏检测、锁屏监听
  wp-render/     渲染后端：image / video(mpv) / web(webview2) / scene(wgpu)
  wp-host/       渲染进程 bin（每显示器一个）
  wp-ipc/        管道协议与客户端
apps/
  desktop/       Tauri 2.x 应用
  cli/           wpctl：无 UI 也能 attach/pause/switch，调试利器
```

`wp-platform` 必须能脱离 UI 单独测试。

### 3.3 桌面宿主模块（新增，最高优先级）

```
wp-platform::desktop_host
  ├─ probe()           运行时探测本机可用策略（启动时 + Explorer 重启后）
  ├─ DesktopHost trait
  │    attach(strategy, hwnd) / detach() / ensure_band() / surface() 
  ├─ StrategyA   Progman 子窗口 + DirectComposition
  ├─ StrategyB   顶层窗口置底（HWND_BOTTOM）
  ├─ StrategyC   顶层窗口精确插入 DefView 相邻位
  ├─ StrategyD   WorkerW（旧版本回退，Win10 / 22H2–23H2）
  └─ BandWatchdog  z 带看门狗 + 自愈
```

**看门狗必须覆盖的事件：**

- `SetWinEventHook`：`EVENT_OBJECT_REORDER`、`EVENT_SYSTEM_FOREGROUND`
- `TaskbarCreated`（Explorer 重启）
- `WM_DISPLAYCHANGE`、`WM_DPICHANGED`、`WM_SETTINGCHANGE`
- `WTS_SESSION_*`（锁屏/解锁）、`WM_POWERBROADCAST`
- 低频定时核对（约 2 s）：检查 `GetWindow(hwnd, GW_HWNDPREV)` 链条是否仍在预期位置
- **兜底**：挂不上时退回静态壁纸 + 明确提示，绝不黑屏

**输入穿透：**

- `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT`
- **必须实现 `WM_NCHITTEST → HTTRANSPARENT`**（实测仅加扩展样式不够）
- 鼠标坐标用 `GetCursorPos` 轮询（30–60 Hz）经 IPC 转发，实现"能感应、不抢焦点"

### 3.4 渲染后端选型

| 类型 | 方案 | 理由 |
|---|---|---|
| 图片/幻灯片 | `image` + D3D11 纹理 / shader 过渡 | 轻量可控 |
| 视频 | **libmpv 嵌入 `--wid`** | HW 解码、循环、倍速、HDR 现成，省 2 个月 |
| 网页 | **WebView2 直接嵌入宿主窗口**（`webview2-com`，不要用 Tauri 窗口） | 需精细控制窗口样式、透明、无焦点 |
| 场景/3D | `wgpu` + 自研 scene 格式 | M3 之后；MVP 可用 three.js 网页壁纸替代 |

> ⚠️ **许可证**：libmpv 是 GPLv2+，动态链接同样传导 GPL。若要闭源/上架，视频后端改为 **FFmpeg(LGPL) + 自写 D3D11 渲染** 或 **Media Foundation**。**必须在 M0 就定**，换后端要重写整条视频管线。

---

## 四、M0：桌面宿主攻坚（1–2 周，先做）

用 Rust + `windows` crate 写 `wpctl probe`（单 bin，≤ 500 行），跑 **3×2 矩阵**：

| 宿主策略 | GDI 内容 | D3D11 / DComp 内容 |
|---|---|---|
| A. Progman 子窗口 | 已证否（黑屏） | **待测** |
| B. 顶层窗口置底 | 能显示，z 带漂移 | **待测** |
| C. 精确插入 DefView 相邻位 | 能显示，盖图标 | **待测** |

补充对照：D. WorkerW（应失败，用于确认探测逻辑正确）。

### 4.1 验收标准（脚本化自动判定）

1. 画面可见、满屏、无撕裂
2. 桌面图标、右键菜单、任务栏全部正常
3. 连续 10 分钟 + 最小化/还原其他窗口 20 次后仍在正确层级
4. `Win+D`、任务栏"显示桌面"、任务视图、锁屏解锁后 3 秒内自恢复
5. Explorer 重启后 5 秒内自恢复
6. 1080p H.264 循环：CPU < 10%，GPU < 30%
7. 多显示器（若可接第二屏）：每屏独立挂载，混合 DPI 不错位

### 4.2 M0 产出物

- `wp-platform` 骨架：`DesktopHost` trait + 策略探测 + 看门狗
- 一份**策略决策记录**（在本机哪条策略可用、边界条件）
- 若三条策略全部失败：触发架构复议（考虑 DirectComposition 全屏层 / 系统壁纸 API 降级方案）

---

## 五、里程碑

### M1 · MVP（2–3 周）

导入 mp4/webm/gif/jpg/png、按显示器分配、托盘 + 开机自启、暂停规则、配置持久化（SQLite）、单实例守护、崩溃自动重启。

*通过标准：非技术用户能把一个视频设成壁纸并保持一周不干预。*

### M2 · 网页壁纸 + 多屏（3–4 周）

WebView2 后端、JS 属性桥（对齐 WE 的 `wallpaperPropertyListener`）、WASAPI loopback 音频数据、跨屏 span、混合 DPI、播放列表与定时切换、热键、`.wpkg` 打包/导入/缩略图。

*通过标准：能跑通一个自写的、带属性面板和音频响应的网页壁纸。*

### M3 · 交互与外部程序壁纸（3–4 周）

鼠标/键盘事件转发、区域命中、任意 exe 嵌入、"忽略全屏应用"白名单、GPU 选择与显存预算、HDR 初步。

### M4 · 场景系统（6–8 周）

`wgpu` scene 运行时、粒子/材质/相机、glTF 导入、Web 端可视化编辑器（比原生编辑器省一半工作量）。

### M5 · 分发（可选，4–6 周）

自建商店/订阅、`tauri-plugin-updater` 更新、minidump 崩溃收集。

---

## 六、风险清单

| 风险 | 等级 | 对策 |
|---|---|---|
| 桌面宿主机制在 26H2 未稳定 | **极高** | M0 专攻；多策略探测 + 看门狗 + 静态壁纸兜底 |
| Windows 版本行为漂移（24H2/25H2/26H2） | 高 | 策略链 + 运行时探测；VM 快照回归（Win10 22H2 / Win11 23H2 / 24H2 / 26H2） |
| 网页壁纸是任意代码 | 高 | 独立进程 + 独立 WebView2 用户数据目录 + 禁本地文件与下载 + 资源限额 |
| 多屏多进程吃显存 | 中 | 同源视频共享解码；FPS 上限 + 暂停策略 |
| 反作弊/独占全屏游戏 | 中 | 检测到独占全屏**彻底停渲染**，不做 overlay |
| HEVC/H.265 授权 | 中 | 优先走系统 Media Foundation 的 HEVC 扩展 |
| 本机 GPU 过弱（GT 720 / 2 GB） | 中 | 首版按 1080p H.264 定标；导入时能力检测 |
| 范围蔓延（编辑器） | 高 | M1 前不写任何编辑器代码 |

---

## 七、可复用的已有代码

`E:\private\Rust\aweak`（PowerToys Awake 的 Rust 实现，v1.4.0）已具备：

- `tray.rs` 托盘、`autostart.rs` 自启、`config.rs` 配置、`crash.rs` 崩溃处理
- `process.rs` 单实例/子进程管理、`power.rs` 电源事件、`notify.rs` 通知、`cli.rs` 参数解析

与 wallpaper 的 supervisor/Tauri 侧高度重合，**建议抽成公共 crate 复用，预计省约 1 周**。

---

## 八、下一步

1. 切到执行模式，创建 `E:\wallpaper` 的 Cargo workspace
2. 实现 `wpctl probe`（M0 矩阵测试）
3. 根据结果确定宿主策略，写策略决策记录
4. 再进入 M1
