//! Win32 桌面宿主：策略抽象 + 运行时探测。
//!
//! 26H2 实测结论见 `.opencode/plans/wallpaper-probe-26h2.md`。

pub mod probe;
pub mod toplevel;

use wp_core::MonitorKey;

pub use probe::{probe, ProbeReport, WorkerWInfo};
pub use toplevel::TopLevelBottom;

/// 桌面宿主策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Progman 子窗口 + DirectComposition。
    ProgmanChild,
    /// 顶层窗口置于全局 z 序最底层。
    TopLevelBottom,
    /// 顶层窗口精确插入 Progman / DefView 相邻位。
    TopLevelAnchored,
    /// 经典 WorkerW 子窗口（Win10 / 22H2–23H2）。
    WorkerW,
}

impl Strategy {
    /// M0 探测矩阵要跑的全部策略（WorkerW 仅作旧系统回退，不参与）。
    pub const ALL: [Self; 3] = [
        Self::ProgmanChild,
        Self::TopLevelBottom,
        Self::TopLevelAnchored,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProgmanChild => "progman_child",
            Self::TopLevelBottom => "toplevel_bottom",
            Self::TopLevelAnchored => "toplevel_anchored",
            Self::WorkerW => "workerw",
        }
    }

    /// 按策略创建挂载适配器。
    ///
    /// 未实现的策略显式返回 `Unsupported`——M0 的探测矩阵需要知道
    /// 「这条策略没跑」和「这条策略跑了但失败」的区别。
    pub fn open(self) -> Result<Box<dyn DesktopHost>> {
        match self {
            Self::TopLevelBottom => Ok(Box::new(TopLevelBottom::new()?)),
            Self::ProgmanChild => Err(PlatformError::Unsupported(
                "Progman 子窗口 + DirectComposition 待实现（M0 第 3 步）",
            )),
            Self::TopLevelAnchored => Err(PlatformError::Unsupported(
                "精确插入 DefView 相邻位待实现（M0 第 3 步）",
            )),
            Self::WorkerW => Err(PlatformError::Unsupported(
                "26H2 上不存在 WorkerW 宿主，策略 D 仅作旧系统回退",
            )),
        }
    }
}

impl std::str::FromStr for Strategy {
    type Err = PlatformError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "progman_child" => Ok(Self::ProgmanChild),
            "toplevel_bottom" => Ok(Self::TopLevelBottom),
            "toplevel_anchored" => Ok(Self::TopLevelAnchored),
            "workerw" => Ok(Self::WorkerW),
            other => Err(PlatformError::Unsupported(match other {
                "all" => "all 不是单条策略名",
                _ => "未知策略名（可选 progman_child / toplevel_bottom / toplevel_anchored / workerw）",
            })),
        }
    }
}

/// 看门狗对层级状态的判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandState {
    /// 层级正确，无需处理。
    Correct,
    /// 刚被修正回正确层级。
    Corrected,
    /// 无法恢复，需要重建窗口或降级。
    Lost,
}

/// 桌面宿主窗口的挂载接口。
///
/// 实现必须保证 `ensure_band` 幂等——看门狗会反复调用。
pub trait DesktopHost: Send {
    fn strategy(&self) -> Strategy;
    fn attach(&mut self, hwnd: isize, rect: wp_core::PhysicalRect) -> Result<()>;
    fn ensure_band(&mut self, hwnd: isize) -> Result<BandState>;
    fn detach(&mut self, hwnd: isize) -> Result<()>;
}

pub type Result<T> = std::result::Result<T, PlatformError>;

#[derive(Debug)]
pub enum PlatformError {
    NoDesktopHost,
    Win32 { context: &'static str, code: u32 },
    Unsupported(&'static str),
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDesktopHost => write!(f, "没有可用的桌面宿主窗口"),
            Self::Win32 { context, code } => write!(f, "{context} 失败，Win32 错误码 {code}"),
            Self::Unsupported(s) => write!(f, "不支持: {s}"),
        }
    }
}

impl std::error::Error for PlatformError {}

/// 显示器标识包装，便于在日志中区分。
pub fn monitor_label(key: &MonitorKey) -> &str {
    key.as_str()
}
