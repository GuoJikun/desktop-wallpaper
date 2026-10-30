//! Win32 桌面宿主：策略抽象 + 运行时探测。
//!
//! 26H2 实测结论见 `.opencode/plans/wallpaper-probe-26h2.md`。

pub mod adopt;
pub mod defview_child;
pub mod probe;
pub mod progman;
pub mod toplevel;
pub mod workerw;

use wp_core::MonitorKey;

pub use adopt::{defview_handle, defview_state, heal_desktop, resize_defview};
pub use defview_child::DefViewChild;
pub use probe::{probe, ProbeReport, WorkerWInfo};
pub use toplevel::{z_chain, TopLevelBottom};

/// 桌面宿主策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Progman 子窗口 + DirectComposition。
    ProgmanChild,
    /// 判别实验：Progman 子窗口，但放在 `SHELLDLL_DefView` **之上**。
    ProgmanChildTop,
    /// 顶层窗口置于全局 z 序最底层。
    TopLevelBottom,
    /// 顶层窗口精确插入 Progman / DefView 相邻位。
    TopLevelAnchored,
    /// 经典 WorkerW 子窗口（Win10 / 22H2–23H2）。
    WorkerW,
    /// 反向接管：把 `SHELLDLL_DefView` 挂到自己窗口下。
    DefViewAdopt,
    /// 反向挂载：自己挂到 `SHELLDLL_DefView` 下，压在图标之下。
    DefViewChild,
}

impl Strategy {
    /// M0 探测矩阵要跑的全部策略，含 WorkerW——即使 26H2 上预期失败，
    /// 也要实测出「失败在握手还是失败在合成」。
    pub const ALL: [Self; 4] = [
        Self::ProgmanChild,
        Self::TopLevelBottom,
        Self::TopLevelAnchored,
        Self::WorkerW,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProgmanChild => "progman_child",
            Self::ProgmanChildTop => "progman_child_top",
            Self::TopLevelBottom => "toplevel_bottom",
            Self::TopLevelAnchored => "toplevel_anchored",
            Self::WorkerW => "workerw",
            Self::DefViewAdopt => "defview_adopt",
            Self::DefViewChild => "defview_child",
        }
    }

    /// 按策略创建挂载适配器。
    ///
    /// 未实现的策略显式返回 `Unsupported`——M0 的探测矩阵需要知道
    /// 「这条策略没跑」和「这条策略跑了但失败」的区别。
    pub fn open(self) -> Result<Box<dyn DesktopHost>> {
        match self {
            Self::TopLevelBottom => Ok(Box::new(TopLevelBottom::new()?)),
            Self::ProgmanChild => Ok(Box::new(progman::ProgmanChild::new()?)),
            Self::ProgmanChildTop => Ok(Box::new(progman::ProgmanChild::above_defview()?)),
            Self::TopLevelAnchored => Err(PlatformError::Unsupported(
                "精确插入 DefView 相邻位待实现（M0 第 3 步）",
            )),
            Self::WorkerW => Ok(Box::new(workerw::WorkerWHost::new()?)),
            Self::DefViewAdopt => Ok(Box::new(adopt::DefViewAdopt::new()?)),
            Self::DefViewChild => Ok(Box::new(defview_child::DefViewChild::new()?)),
        }
    }
}

impl std::str::FromStr for Strategy {
    type Err = PlatformError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "progman_child" => Ok(Self::ProgmanChild),
            "progman_child_top" => Ok(Self::ProgmanChildTop),
            "toplevel_bottom" => Ok(Self::TopLevelBottom),
            "toplevel_anchored" => Ok(Self::TopLevelAnchored),
            "workerw" => Ok(Self::WorkerW),
            "defview_adopt" => Ok(Self::DefViewAdopt),
            "defview_child" => Ok(Self::DefViewChild),
            other => Err(PlatformError::Unsupported(match other {
                "all" => "all 不是单条策略名",
                _ => "未知策略名（progman_child / progman_child_top / toplevel_bottom / toplevel_anchored / workerw / defview_adopt / defview_child）",
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

/// 顶层窗口 z 序位置（0 最靠上）。诊断用。
pub fn z_index_of(hwnd: isize) -> Option<u32> {
    toplevel::z_index(crate::window::raw(hwnd))
}

/// 桌面窗口当前的 z 序位置。诊断用。
pub fn progman_z() -> Option<u32> {
    toplevel::z_index(probe::progman()?)
}
