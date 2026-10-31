//! 暂停判定：全屏应用、最大化遮挡、电池、远程会话、锁屏、休眠。
//!
//! 对应 `docs/design.md` §7.1 的 [`PauseReason`]。设计目标有两个：
//!
//! 1. **纯函数做决策**——Win32 只负责采样，[`decide`] 不碰系统 API，
//!    这样所有规则都能在测试里覆盖；
//! 2. **判定永远偏保守**——拿不到信息就当作「没暂停」。宁可多渲染一帧，
//!    也不要因为一次查询失败把壁纸冻住。
//!
//! 采样口径（26H2 实测）：
//!
//! | 判据 | 来源 |
//! |---|---|
//! | 全屏应用 | 前台窗口矩形 ⊇ 显示器矩形 |
//! | 最大化窗口盖住本屏 | 前台窗口 `IsZoomed` + 与本屏同一 `HMONITOR` |
//! | 电池供电且电量低 | `GetSystemPowerStatus` |
//! | 远程桌面会话 | `SM_REMOTESESSION` |
//! | 锁屏 / 休眠 | `WM_WTSSESSION_CHANGE` / `WM_POWERBROADCAST`（由调用方传入） |
//!
//! **不要用 `SHQueryUserNotificationState` 判定全屏。** 它看起来最权威，实际会把
//! 我们自己那个全屏尺寸的宿主窗口也算成「有全屏应用」，返回 `QUNS_BUSY(2)`，
//! 于是壁纸一挂上就永久暂停（实测：没有宿主时状态 5，宿主一起就变成 2）。
//! 该值只写进报告供诊断。

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    MonitorFromPoint, MonitorFromWindow, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::Shell::SHQueryUserNotificationState;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, IsZoomed, SM_REMOTESESSION,
};

use wp_core::PhysicalRect;

use crate::window::WINDOW_CLASS;

/// 电池电量低于该百分比且未接电源时暂停渲染。
pub const LOW_BATTERY_PERCENT: u8 = 20;

/// 暂停原因。与 `docs/design.md` §7.1 一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// 全屏独占应用（游戏、视频播放器、全屏演示）。
    FullscreenApp,
    /// 本屏被一个最大化 / 全屏的普通窗口盖住。
    MaximizedCover,
    /// 电池供电且电量低。
    BatteryLow,
    /// 会话已锁定。
    SessionLocked,
    /// 显示器已关闭 / 系统休眠。
    DisplayOff,
    /// 远程桌面会话。
    RemoteSession,
    /// 用户手动暂停。
    UserManual,
}

impl PauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullscreenApp => "fullscreen_app",
            Self::MaximizedCover => "maximized_cover",
            Self::BatteryLow => "battery_low",
            Self::SessionLocked => "session_locked",
            Self::DisplayOff => "display_off",
            Self::RemoteSession => "remote_session",
            Self::UserManual => "user_manual",
        }
    }

    /// 给用户看的一句话说明。
    pub fn describe(self) -> &'static str {
        match self {
            Self::FullscreenApp => "有全屏应用在运行",
            Self::MaximizedCover => "桌面被最大化窗口挡住",
            Self::BatteryLow => "电池电量过低",
            Self::SessionLocked => "会话已锁定",
            Self::DisplayOff => "显示器已关闭或系统休眠",
            Self::RemoteSession => "远程桌面会话",
            Self::UserManual => "用户手动暂停",
        }
    }
}

/// 前台窗口快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundWindow {
    pub hwnd: isize,
    pub class: String,
    pub pid: u32,
    pub rect: PhysicalRect,
    /// 是否是最大化窗口（`IsZoomed`）。
    pub maximized: bool,
    /// 是否完全盖住目标显示器。
    ///
    /// 包含两种情况：矩形覆盖整屏（无边框全屏），或者最大化且落在同一块屏上。
    /// 注意**最大化窗口不等于整屏**——Win11 上最大化窗口止于任务栏，
    /// 实测 1920×1080 屏上 Chrome 最大化后是 `(-8,-8,1936,1048)`，
    /// 只按矩形判断会漏掉所有普通最大化窗口。
    pub covers_monitor: bool,
}

/// 决策输入。Win32 采样结果全部先落到这里，再交给 [`decide`]。
#[derive(Debug, Clone)]
pub struct PauseInputs {
    /// `SHQueryUserNotificationState` 的原始值（拿不到时为 0）。
    ///
    /// **仅供参考，不参与判定**：我们自己的宿主窗口会把它顶成 `QUNS_BUSY`。
    pub notification_state: u32,
    pub foreground: Option<ForegroundWindow>,
    /// 我们自己的宿主窗口，前台是它时不算遮挡。
    pub host_hwnd: Option<isize>,
    /// 目标显示器矩形。
    pub monitor: PhysicalRect,
    pub remote_session: bool,
    pub on_battery: bool,
    pub battery_percent: Option<u8>,
    pub locked: bool,
    pub suspended: bool,
    pub user_paused: bool,
}

impl PauseInputs {
    /// 以「没有任何暂停条件」为基线，调用方按需覆盖。
    pub fn new(monitor: PhysicalRect) -> Self {
        Self {
            notification_state: 0,
            foreground: None,
            host_hwnd: None,
            monitor,
            remote_session: false,
            on_battery: false,
            battery_percent: None,
            locked: false,
            suspended: false,
            user_paused: false,
        }
    }

    fn battery_low(&self) -> bool {
        self.on_battery
            && self
                .battery_percent
                .is_some_and(|p| p <= LOW_BATTERY_PERCENT)
    }
}

/// 采样结果 + 判定结论。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseReport {
    pub paused: bool,
    pub reasons: Vec<PauseReason>,
    /// `SHQueryUserNotificationState` 的原始值，便于对照微软文档。
    /// 宿主窗口存在时通常是 2（`QUNS_BUSY`），**不能作为暂停依据**。
    pub notification_state: u32,
    pub notification_state_name: &'static str,
    pub foreground: Option<ForegroundWindow>,
    pub remote_session: bool,
    pub on_battery: bool,
    pub battery_percent: Option<u8>,
}

impl PauseReport {
    /// 日志用的一行摘要。
    pub fn summary(&self) -> String {
        if !self.paused {
            return "运行中".into();
        }
        self.reasons
            .iter()
            .map(|r| format!("{}（{}）", r.as_str(), r.describe()))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

/// 纯决策函数：命中任意一条即暂停。
///
/// 注意 `notification_state` **不参与**判定，只写进报告用于诊断：
/// 我们自己那个铺满屏幕的宿主窗口会把 `SHQueryUserNotificationState`
/// 顶成 `QUNS_BUSY(2)`，实测见 §7.9。按它暂停等于「壁纸永远暂停」。
pub fn decide(input: &PauseInputs) -> Vec<PauseReason> {
    let mut reasons = Vec::new();
    if input.user_paused {
        reasons.push(PauseReason::UserManual);
    }
    if input.locked {
        reasons.push(PauseReason::SessionLocked);
    }
    if input.suspended {
        reasons.push(PauseReason::DisplayOff);
    }
    if input.remote_session {
        reasons.push(PauseReason::RemoteSession);
    }
    if input.battery_low() {
        reasons.push(PauseReason::BatteryLow);
    }
    if let Some(fg) = &input.foreground {
        // 桌面壳层和自己的宿主窗口永远不算遮挡：宿主本身就是全屏尺寸的。
        let is_shell = matches!(
            fg.class.as_str(),
            "Progman" | "WorkerW" | "SHELLDLL_DefView"
        ) || fg.class == WINDOW_CLASS;
        let is_host = input.host_hwnd == Some(fg.hwnd);
        // `covers_monitor` 在采样时已经合并了「矩形覆盖整屏」和「最大化 + 同屏」两种情况。
        if fg.covers_monitor && !is_shell && !is_host {
            // 矩形真的盖住整屏 = 全屏应用（独占或无边窗口化）；
            // 只是最大化、底部留着任务栏的，算普通遮挡。
            reasons.push(if covers(fg.rect, input.monitor) {
                PauseReason::FullscreenApp
            } else {
                PauseReason::MaximizedCover
            });
        }
    }
    reasons
}

/// 采样当前状态并给出判定。
///
/// `host` 传宿主窗口句柄时，前台是它自己不算遮挡；`flags` 是必须由消息循环
/// 提供的状态（锁屏、休眠、用户手动暂停）。
pub fn probe(host: Option<isize>, monitor: PhysicalRect, flags: PauseFlags) -> PauseReport {
    let mut input = sample(host, monitor);
    input.locked = flags.locked;
    input.suspended = flags.suspended;
    input.user_paused = flags.user_paused;
    report(input)
}

/// 消息循环维护的状态位。
#[derive(Debug, Clone, Copy, Default)]
pub struct PauseFlags {
    pub locked: bool,
    pub suspended: bool,
    pub user_paused: bool,
}

fn report(input: PauseInputs) -> PauseReport {
    let reasons = decide(&input);
    PauseReport {
        paused: !reasons.is_empty(),
        reasons,
        notification_state: input.notification_state,
        notification_state_name: notification_state_name(input.notification_state),
        foreground: input.foreground,
        remote_session: input.remote_session,
        on_battery: input.on_battery,
        battery_percent: input.battery_percent,
    }
}

/// 从 Win32 采样（不含消息循环状态）。
pub fn sample(host: Option<isize>, monitor: PhysicalRect) -> PauseInputs {
    let notification_state = unsafe { SHQueryUserNotificationState() }
        .map(|s| s.0 as u32)
        .unwrap_or(0);

    let foreground = foreground_window().map(|mut fg| {
        fg.covers_monitor = hides_monitor(
            fg.maximized,
            on_same_monitor(fg.hwnd, monitor),
            fg.rect,
            monitor,
        );
        fg
    });

    let (on_battery, battery_percent) = battery();
    let remote_session = unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0;

    PauseInputs {
        notification_state,
        foreground,
        host_hwnd: host,
        monitor,
        remote_session,
        on_battery,
        battery_percent,
        locked: false,
        suspended: false,
        user_paused: false,
    }
}

fn foreground_window() -> Option<ForegroundWindow> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return None;
    }
    // 最小化 / 隐藏的前台窗口（切换过程中会出现）不算遮挡。
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
        return None;
    }
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };

    Some(ForegroundWindow {
        hwnd: hwnd.0 as isize,
        class: class_name(hwnd),
        pid,
        rect: PhysicalRect::new(
            rect.left,
            rect.top,
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ),
        maximized: unsafe { IsZoomed(hwnd) }.as_bool(),
        covers_monitor: false,
    })
}

/// 窗口是否落在 `monitor` 这块屏上（按显示器中心点定位，避免用索引）。
fn on_same_monitor(hwnd: isize, monitor: PhysicalRect) -> bool {
    let center = POINT {
        x: monitor.x + monitor.width as i32 / 2,
        y: monitor.y + monitor.height as i32 / 2,
    };
    unsafe {
        let target = MonitorFromPoint(center, MONITOR_DEFAULTTOPRIMARY);
        let window = MonitorFromWindow(crate::window::raw(hwnd), MONITOR_DEFAULTTONEAREST);
        target == window
    }
}

/// 前台窗口是否会遮住整块 `monitor`。
///
/// 两个条件任一成立：
/// - 矩形完全覆盖整屏（无边框全屏 / 游戏）；
/// - 最大化窗口且就在这块屏上（Win11 最大化会留出任务栏，矩形并不覆盖整屏）。
pub(crate) fn hides_monitor(
    maximized: bool,
    same_monitor: bool,
    rect: PhysicalRect,
    monitor: PhysicalRect,
) -> bool {
    covers(rect, monitor) || (maximized && same_monitor)
}

/// `outer` 是否完全覆盖 `target`（容许 1 px 误差，窗口边框偶尔差一格）。
fn covers(outer: PhysicalRect, target: PhysicalRect) -> bool {
    outer.x <= target.x + 1
        && outer.y <= target.y + 1
        && outer.x + outer.width as i32 >= target.x + target.width as i32 - 1
        && outer.y + outer.height as i32 >= target.y + target.height as i32 - 1
}

fn battery() -> (bool, Option<u8>) {
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return (false, None);
    }
    let on_battery = status.ACLineStatus == 0;
    // 255 = 未知；台式机没有电池时就是这个值。
    let percent = (status.BatteryLifePercent != 255).then_some(status.BatteryLifePercent);
    (on_battery, percent)
}

fn notification_state_name(state: u32) -> &'static str {
    match state {
        1 => "not_present",
        2 => "busy",
        3 => "running_d3d_full_screen",
        4 => "presentation_mode",
        5 => "accepts_notifications",
        6 => "quiet_time",
        7 => "app",
        0 => "unavailable",
        _ => "unknown",
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: PhysicalRect = PhysicalRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    fn fg(class: &str, rect: PhysicalRect) -> ForegroundWindow {
        ForegroundWindow {
            hwnd: 42,
            class: class.into(),
            pid: 100,
            rect,
            maximized: false,
            covers_monitor: covers(rect, SCREEN),
        }
    }

    fn full_screen() -> PhysicalRect {
        PhysicalRect::new(0, 0, 1920, 1080)
    }

    #[test]
    fn baseline_is_running() {
        let input = PauseInputs::new(SCREEN);
        assert!(decide(&input).is_empty());
    }

    #[test]
    fn covering_foreground_window_is_fullscreen_app() {
        let mut input = PauseInputs::new(SCREEN);
        input.foreground = Some(fg("GameWindowClass", full_screen()));
        assert_eq!(decide(&input), vec![PauseReason::FullscreenApp]);
    }

    /// 回归：`SHQueryUserNotificationState` 会被我们自己的全屏宿主窗口顶成
    /// `QUNS_BUSY(2)`。如果拿它当判据，壁纸一挂上就永久暂停。
    #[test]
    fn notification_state_alone_never_pauses() {
        for state in [2, 3, 4] {
            let mut input = PauseInputs::new(SCREEN);
            input.notification_state = state;
            assert!(decide(&input).is_empty(), "状态 {state} 不应单独触发暂停");
        }
    }

    #[test]
    fn maximized_window_that_stops_at_taskbar_pauses_as_cover() {
        let mut window = fg("Chrome_WidgetWin_1", PhysicalRect::new(-8, -8, 1936, 1048));
        window.maximized = true;
        // 采样时 `hides_monitor` 会把这个组合算成「盖住本屏」。
        window.covers_monitor = true;

        let mut input = PauseInputs::new(SCREEN);
        input.foreground = Some(window);
        assert_eq!(decide(&input), vec![PauseReason::MaximizedCover]);
    }

    #[test]
    fn window_on_another_monitor_does_not_pause() {
        let mut input = PauseInputs::new(SCREEN);
        // 第二块屏在右边：本屏完全没被盖住。
        input.foreground = Some(fg(
            "Chrome_WidgetWin_1",
            PhysicalRect::new(1920, 0, 1920, 1080),
        ));
        assert!(decide(&input).is_empty());
    }

    #[test]
    fn small_window_does_not_pause() {
        let mut input = PauseInputs::new(SCREEN);
        input.foreground = Some(fg("Notepad", PhysicalRect::new(100, 100, 800, 600)));
        assert!(decide(&input).is_empty());
    }

    #[test]
    fn desktop_shell_and_host_are_not_covers() {
        for class in ["Progman", "WorkerW", "SHELLDLL_DefView", WINDOW_CLASS] {
            let mut input = PauseInputs::new(SCREEN);
            input.foreground = Some(fg(class, full_screen()));
            assert!(decide(&input).is_empty(), "{class} 不该算遮挡");
        }

        let mut input = PauseInputs::new(SCREEN);
        let mut window = fg("Whatever", full_screen());
        window.hwnd = 7;
        input.foreground = Some(window);
        input.host_hwnd = Some(7);
        assert!(decide(&input).is_empty(), "自己的宿主窗口不该算遮挡");
    }

    #[test]
    fn explorer_window_counts_as_cover() {
        // 文件资源管理器最大化确实挡住了壁纸，不能按进程放行。
        let mut window = fg("CabinetWClass", PhysicalRect::new(-8, -8, 1936, 1048));
        window.maximized = true;
        window.covers_monitor = true;
        let mut input = PauseInputs::new(SCREEN);
        input.foreground = Some(window);
        assert_eq!(decide(&input), vec![PauseReason::MaximizedCover]);
    }

    #[test]
    fn battery_only_pauses_when_low_and_unplugged() {
        let mut input = PauseInputs::new(SCREEN);
        input.battery_percent = Some(5);
        assert!(decide(&input).is_empty(), "插着电源不该暂停");

        input.on_battery = true;
        assert_eq!(decide(&input), vec![PauseReason::BatteryLow]);

        input.battery_percent = Some(80);
        assert!(decide(&input).is_empty());

        input.battery_percent = None;
        assert!(decide(&input).is_empty(), "未知电量不该暂停");
    }

    #[test]
    fn flags_collect_all_reasons() {
        let mut input = PauseInputs::new(SCREEN);
        input.user_paused = true;
        input.locked = true;
        input.suspended = true;
        input.remote_session = true;
        input.on_battery = true;
        input.battery_percent = Some(10);
        input.foreground = Some(fg("GameWindowClass", full_screen()));
        assert_eq!(
            decide(&input),
            vec![
                PauseReason::UserManual,
                PauseReason::SessionLocked,
                PauseReason::DisplayOff,
                PauseReason::RemoteSession,
                PauseReason::BatteryLow,
                PauseReason::FullscreenApp,
            ]
        );
    }

    #[test]
    fn covers_tolerates_one_pixel_border() {
        assert!(covers(PhysicalRect::new(-1, -1, 1922, 1082), SCREEN));
        assert!(!covers(PhysicalRect::new(0, 0, 1900, 1080), SCREEN));
        assert!(!covers(PhysicalRect::new(0, 0, 1920, 1000), SCREEN));
    }

    /// 实测回归：Win11 上最大化窗口是 `(-8,-8,1936,1048)`，比 1080 矮一条任务栏。
    /// 只按矩形判断会漏掉所有普通最大化窗口，壁纸在最大化应用后面还继续跑。
    #[test]
    fn maximized_window_that_stops_at_taskbar_still_hides_monitor() {
        let maximized = PhysicalRect::new(-8, -8, 1936, 1048);
        assert!(!covers(maximized, SCREEN), "最大化的矩形本来就不覆盖整屏");
        assert!(hides_monitor(true, true, maximized, SCREEN));
        // 最大化在另一块屏上不算遮挡本屏。
        assert!(!hides_monitor(true, false, maximized, SCREEN));
        // 没最大化、又没覆盖整屏的窗口不算。
        assert!(!hides_monitor(false, true, maximized, SCREEN));
    }

    #[test]
    fn report_summary_mentions_reason() {
        let mut input = PauseInputs::new(SCREEN);
        input.notification_state = 5;
        input.foreground = Some(fg("GameWindowClass", full_screen()));
        let report = report(input);
        assert!(report.paused);
        assert_eq!(report.reasons, vec![PauseReason::FullscreenApp]);
        assert!(report.summary().contains("fullscreen_app"));
        assert_eq!(report.notification_state_name, "accepts_notifications");
    }

    /// 真实采样一次，确认系统 API 调用没有拼错。不能断言具体值：
    /// 取决于运行环境（有没有电池、是不是远程会话）。
    #[test]
    fn sample_returns_sane_values() {
        let input = sample(None, SCREEN);
        assert!(input.notification_state <= 7);
        if let Some(fg) = &input.foreground {
            assert!(!fg.class.is_empty() || fg.pid != 0);
        }
    }
}
