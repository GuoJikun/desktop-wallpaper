//! Windows 平台层：桌面宿主、显示器枚举、窗口创建。
//!
//! 所有 Win32 调用集中在这里，上层 crate 不直接依赖 `windows`。

pub mod capture;
pub mod dcomp;
pub mod desktop;
pub mod monitor;
pub mod window;

pub use capture::{capture, save_bmp, ScreenImage};
pub use dcomp::DcompSurface;
pub use desktop::{
    BandState, DesktopHost, PlatformError, ProbeReport, Strategy, TopLevelBottom, WorkerWInfo,
};
pub use monitor::{enumerate_monitors, MonitorInfo};
pub use window::{
    class_at_point, create_host_window, destroy_host_window, is_host_window_alive,
    minimize_other_windows, paint_solid, pump_messages, restore_windows, HostWindowSpec,
    TEST_COLOR, TEST_RGB, WINDOW_CLASS,
};

/// 桌面宿主窗口的推荐扩展样式。
pub const HOST_EX_STYLE: u32 = 0x0800_0000 // WS_EX_NOACTIVATE
    | 0x0000_0080                            // WS_EX_TOOLWINDOW
    | 0x0000_0020; // WS_EX_TRANSPARENT

/// `WM_NCHITTEST` 的返回码，必须让壁纸窗口放行鼠标。
pub const HTTRANSPARENT: isize = -1;
