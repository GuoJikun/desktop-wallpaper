# 桌面挂载层实验记录（Windows 11 26H2）

> 实验日期：2026-10-29
> 目标：在写业务代码之前，确定「动态壁纸如何挂到 Windows 桌面上」这个机制是否成立。
> 结论：**经典 WorkerW 方案在 26H2 上已失效，替代方案尚未稳定，必须单独立项攻坚。**

---

## 1. 实验环境

| 项 | 值 |
|---|---|
| 系统版本 | Windows 11 **26H2**，DisplayVersion=`26H2`，CurrentBuild=`26300`，UBR=`9818` |
| `ProductName` | 注册表仍为 `Windows 10 Pro`（老字段，非实际系统） |
| 机器 | Acer Veriton B450 |
| 显示器 | **1 台**，1920×1080（多屏路径本次无法验证） |
| GPU | **NVIDIA GeForce GT 720，2 GB 显存，驱动 30.0.14.7429** |
| WebView2 Runtime | 156.0.4314.50 ✅ |
| 开发依赖 | Rust/cargo ✅、mpv ✅、ffmpeg ✅、Node/Volta ✅、.NET 10 SDK ✅ |

### GPU 能力推论（GT 720 / Kepler）

- 硬件解码：仅 H.264（及 MPEG-2 等老格式）
- **不支持** VP9 / AV1 硬解；HEVC 支持有限
- 无 NVENC
- 2 GB 显存 → 4K 视频壁纸基本不可用

**结论：第一版按「1080p + H.264」定标，壁纸导入时做能力检测与转码提示。**

---

## 2. 实验方法

全部通过 PowerShell + 内联 C#（P/Invoke）完成，不依赖第三方工具：

1. 枚举顶层窗口与 `Progman` / `WorkerW` 子窗口树
2. 发送 `WM_SPAWN_WORKER`（`0x052C`）并前后对比窗口树
3. 原生 `RegisterClassEx` + `CreateWindowEx` 创建测试窗口（纯色背景），分别尝试三种宿主方式
4. 临时最小化全部应用窗口 → `CopyFromScreen` 截屏 → 逐像素比对 → 恢复窗口
5. `WindowFromPoint` 验证输入命中
6. `DwmGetWindowAttribute(DWMWA_CLOAKED)` 验证 DWM 遮蔽状态

所有测试窗口在结束时 `DestroyWindow`，桌面状态已恢复。

---

## 3. 实验结果

### 3.1 桌面窗口树（26H2）

```
Progman 0x10130 (explorer, vis=True)
  ├─ SHELLDLL_DefView 0x1013A  (0,0)-(1920,1080)
  │    ├─ SysListView32 0x1013C (0,0)-(1920,1080)
  │    └─ SysHeader32 0x10140
```

- `SHELLDLL_DefView` **直接挂在 Progman 下**（24H2 之前的布局是挂在 WorkerW 下）
- `Progman` 样式 `0x96000000` = `WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN`

### 3.2 `0x052C` 握手：返回成功，但无效果

```
SendMessageTimeout(Progman, 0x052C, wParam=0xD, lParam=0) -> ret=1
前后窗口树完全一致
```

系统中存在 14 个 `WorkerW` 顶层窗口，全部：

- `IsWindowVisible = False`
- 无子窗口
- 分属不同进程（explorer 及其它应用）

→ 没有可用的壁纸宿主窗口。**网上基于 WorkerW 的通用方案在此版本 100% 无效。**

### 3.3 方案 A：`SetParent(hwnd, Progman)`（24H2 社区路线）

| 检查项 | 结果 |
|---|---|
| `SetParent` 返回值 | 成功 |
| `GetParent` | `0x10130`（Progman） |
| `IsWindowVisible` | `True` |
| 窗口矩形 | `(0,0)-(1920,1080)` |
| `DWMWA_CLOAKED` | `0`（未被遮蔽） |
| **实际画面** | **不可见，桌面仍为原壁纸** |

进一步测试：把该子窗口放到 `DefView` **上方**后：

- 整个桌面变成纯色 **RGB(20,20,20) / #141414**
- 不是窗口应有的绘制颜色（红色 RGB(200,0,0)）
- 任务栏仍正常

**推断：窗口占位正确，但 GDI 绘制内容没有被 DWM 合成；26H2 的桌面宿主很可能要求 DirectComposition / DXGI 交换链内容（`WS_EX_NOREDIRECTIONBITMAP` + DComp）。**

### 3.4 方案 B：顶层窗口置底（`HWND_BOTTOM`）

窗口属性：`WS_POPUP | WS_VISIBLE`，`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`，全屏 `1920×1080`。

多次运行结果**不一致**：

| 运行 | 创建时机 | z 序（ours / Progman / total） | 画面 | 桌面图标 |
|---|---|---|---|---|
| A | 先建窗，后最小化其他窗口 | 258 / 259 / 347 | **不显示**（原壁纸） | 正常 |
| B | 先最小化，后建窗 | 260 / 261 / — | 显示 | **被盖住** |
| C | 先挂 Progman 再脱离，再置底 | — | 显示 | **正常在上**（唯一理想结果） |
| D | 插到 Progman 正下方（`hWndInsertAfter = Progman`） | 258 / 259 / 347 | 显示 | **被盖住** |
| E | 同 D，1.5 s 后重申一次 | 259 / 260 / 349 | 显示 | 被盖住 |

- `DWMWA_CLOAKED` 始终为 `0`，`IsWindowVisible` 始终为 `True`
- 说明**不是窗口被隐藏，而是 z 带相对「图标层 / 壁纸合成层」发生漂移**

### 3.5 输入命中测试

| 场景 | `WindowFromPoint(960,540)` | 判断 |
|---|---|---|
| 窗口在图标层之下 | `SysListView32` | 桌面输入正常，不被遮挡 |
| 窗口在图标层之上 | 测试窗口自身 | **会吃掉桌面点击** |
| 窗口在图标层之上 + `WS_EX_TRANSPARENT` | 仍返回测试窗口自身 | **仅加扩展样式不够**，需在 `WM_NCHITTEST` 返回 `HTTRANSPARENT` |

---

## 4. 结论

1. **26H2 桌面已改为 DWM 合成**：壁纸不再由 `Progman` 绘制，`WorkerW` 宿主窗口不再产生。
2. **Progman 子窗口 + GDI 内容 = 黑屏/不可见**；是否为「必须用 D3D 内容」尚未验证，但这是最合理的解释。
3. **顶层置底可以出画面**，但与图标层的相对 z 序不稳定，依赖窗口创建时机，**不能一次 `SetWindowPos` 了事**。
4. 输入的坑：窗口一旦落到图标层之上，必须用 `WM_NCHITTEST → HTTRANSPARENT` 放行，否则桌面右键与图标点击全部失效。

---

## 5. 未验证项（M0 必须补上）

- [ ] Progman 子窗口 + **DirectComposition / D3D11 交换链** 内容是否可见
- [ ] 顶层置底 + D3D11 交换链 的 z 带行为是否比 GDI 稳定
- [ ] 多显示器（本机仅 1 屏，需扩展屏或虚拟机验证）
- [ ] 混合 DPI 场景
- [ ] `Win+D`（显示桌面）、任务视图、`Alt+Tab` 切换后的层级恢复
- [ ] Explorer 重启（`TaskbarCreated`）后的重建
- [ ] 锁屏 / 解锁、RDP 连接断开重连
- [ ] 全屏独占游戏 / `SHQueryUserNotificationState` 的暂停判定
- [ ] 长时间（≥ 30 分钟）稳定性与 CPU/GPU 占用

---

## 6. Rust 复核（2026-10-29，`wpctl` 实测）

用 `crates/wp-platform` + `wpctl probe --minimize --shot` 把上面的结论重新跑了一遍，
这次判定依据是截图和像素统计，不再依赖肉眼与 PowerShell 脚本。

| 策略 | 内容 | attach | 层级自愈 | 截图结果 | 结论 |
|---|---|---|---|---|---|
| B 顶层置底 | GDI 纯色 | ok | 6 s 内修正 1 次 | 测试色 **95.6%**，图标区 400×300 异色像素 **0** | 画面可见，但**盖住桌面图标** |
| A Progman 子窗口 | GDI 纯色 | ok | 无需修正 | 测试色 **0%**，桌面原样（原壁纸 + 图标都在） | 内容**完全不参与合成**，与 PowerShell 结论一致 |

补充事实：

- `SetWindowPos(HWND_BOTTOM)` 之后，窗口在顶层 z 链中的索引恰好是 `Progman 索引 - 1`：
  **`HWND_BOTTOM` 只能压到「紧贴 Progman 之上」，压不到 Progman 之下**。
  因此「层级正确」的判据改成「紧贴桌面上方」，见 `desktop/toplevel.rs`。
- `WindowFromPoint` 会跳过 `WS_EX_TRANSPARENT` 窗口，所以命中 `SysListView32`
  **不能**用来证明图标在上层；判断可见性必须截图。
- 策略 B 的 6 秒里出现过 1 次 z 序漂移并被 `ensure_band` 修正，证明看门狗是必要且有效的。

下一步（M0 剩余关键格）：

1. 策略 A + `WS_EX_NOREDIRECTIONBITMAP` + DirectComposition visual —— 这是让 Progman 子窗口
   内容进入 DWM 合成的标准做法，最有可能同时满足「不盖图标」和「可见」。
2. 策略 B + D3D11 交换链，比较 z 带漂移频率是否比 GDI 更低。

---

## 7. M0 结论：DefView 接管（策略 E）

### 7.1 判别实验（为什么 A / B / D 都不行）

| 实验 | 内容 | 测试色占比 | 图标区异色像素 | 结论 |
|---|---|---|---|---|
| A 子窗口在 DefView **之下** | D3D11 + DComp | 0.0% | 120000 | 完全被 DefView 盖住 |
| A 子窗口在 DefView **之上** | D3D11 + DComp | **93.6%** | 0 | 合成本身没问题，是 DefView 挡住我们 |
| D WorkerW（wParam=0） | — | — | — | `0x052C` 返回 0，不产生宿主窗口，与 `0xD` 一致 |

由此确定：**26H2 上「壁纸可见」与「图标可见」的分界线就是 `SHELLDLL_DefView` 这一层**。
`Progman` 的子窗口永远处在分界线的某一侧，所以策略 A 在结构上不可能同时满足两者。

### 7.2 策略 E 做法

把自己的窗口（顶层、置底、`WS_EX_TRANSPARENT` + `WM_NCHITTEST → HTTRANSPARENT`）
当作容器，把 `SHELLDLL_DefView` **反向** 挂进来：

```
我们的宿主窗口（壁纸：GDI / D3D11+DComp）
  └─ SHELLDLL_DefView（图标，Explorer 自己的窗口）
       └─ SysListView32
```

- 壁纸画在父窗口 → 铺满屏；
- 图标是子窗口 → 天然在壁纸之上，颜色、选中、双击全部走 Explorer 原逻辑；
- 退出时 `SetParent(DefView → Progman)` 还原，`Drop` 兜底，启动时 `heal_desktop()` 自愈。

### 7.3 实测结果

| 项目 | GDI 纯色 | D3D11 + DComp |
|---|---|---|
| 测试色占比 | 98.9% | **99.1%** |
| 图标区（400×300）异色像素 | 7855 | 5603 |
| 右键菜单 / 单击图标 / 双击图标 | — | **全部正常（人工确认）** |
| 150 秒稳定性 | — | 检查 75 次，修正 0，丢失 0 |

关键细节：DComp 的 `CreateTargetForHwnd(hwnd, topmost)` 必须传 **`false`**。
`topmost = true` 时 visual 会盖住子窗口，实测图标区异色像素变成 0（图标全被盖住）。

### 7.4 仍待验证

- [ ] Explorer 重启（`TaskbarCreated`）后重挂 —— 代码已实现，未实测
- [ ] 多显示器 / 混合 DPI
- [ ] 锁屏解锁、RDP 重连
- [ ] 全屏独占游戏时的暂停判定
- [ ] 长时间（≥ 30 分钟）稳定性与 GPU 占用


