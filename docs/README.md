# 文档索引

| 文档 | 内容 | 适合谁看 |
|---|---|---|
| [`design.md`](./design.md) | 技术设计：架构、进程模型、桌面宿主、渲染后端、数据模型、IPC、安全模型、ADR | 参与实现的人 |
| [`implementation.md`](./implementation.md) | 实现方案：工程结构、M0–M5 任务拆解、编码规范、测试矩阵、发布流程、时间表 | 排期与执行 |
| [`../AGENTS.md`](../AGENTS.md) | 项目硬规则与约束（WorkerW 禁用、D3D 内容、看门狗、输入穿透等） | 所有人 / agent |
| [`../.opencode/plans/wallpaper-plan.md`](../.opencode/plans/wallpaper-plan.md) | 规划总览：功能分级、里程碑、风险 | 快速了解全貌 |
| [`../.opencode/plans/wallpaper-probe-26h2.md`](../.opencode/plans/wallpaper-probe-26h2.md) | 26H2 挂载层实验记录与原始证据 | 怀疑"为什么要这么设计"时 |

## 阅读顺序

1. `AGENTS.md` —— 先看硬规则，避免走回头路
2. `design.md` 第 3 章 —— 桌面宿主是当前最大风险，先理解它
3. `implementation.md` 第 3 章 —— M0 要做什么、怎么验收
4. 其余按需

## 关键结论速查

- 26H2 上 `WorkerW` 已不可用，`0x052C` 无效
- `Progman` 子窗口 + GDI 内容 = 黑屏，桌面内容必须 D3D11 / DirectComposition
- 顶层置底窗口能出画面，但 z 序会漂移，必须有看门狗
- 壁纸窗口要放行输入，必须 `WM_NCHITTEST → HTTRANSPARENT`
