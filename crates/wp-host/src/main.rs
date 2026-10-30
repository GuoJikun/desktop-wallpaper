//! 渲染宿主进程：每台显示器一个。
//!
//! 进程边界就是故障边界——Explorer 重启、D3D 设备丢失，以及后续的视频/网页崩溃，
//! 都只影响这一个进程。上层 supervisor 按退出码决定后续动作：
//!
//! | 退出码 | 含义 | supervisor 该做什么 |
//! |---|---|---|
//! | 0 | 正常退出（收到 `WM_CLOSE` / 到时） | 不重启 |
//! | 1 | 启动失败或运行时错误 | 退避后重启 |
//! | 75 | `ensure_band() == Lost`（Explorer 换了桌面壳层） | **必须重启本进程**，进程内无解 |
//! | 76 | 目标显示器已移除 | 停止该显示器的渲染，等热插拔事件 |
//!
//! 生命周期：
//!
//! 1. `heal_desktop()` + `restore_leaked_windows()` 收拾上次崩溃的残局；
//! 2. 枚举显示器 → 创建宿主窗口 → `attach` 桌面；
//! 3. 消息循环 + 约 2 s 一次的 z 序看门狗；
//! 4. 退出前先 `detach`（把 `DefView` 还给 `Progman`）再销毁窗口。

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, PostMessageW, TranslateMessage, MSG, PBT_APMRESUMEAUTOMATIC,
    PM_REMOVE, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_ENDSESSION, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

use wp_core::PhysicalRect;
use wp_platform::{
    create_host_window, destroy_host_window, enumerate_monitors, is_minimized, paint_solid,
    restore_window, BandState, DesktopHost, DcompSurface, HostWindowSpec, MonitorInfo, Strategy,
    TEST_COLOR, TEST_RGB,
};

/// 需要重启本进程（Explorer 换了桌面壳层，进程内无法自愈）。
const EXIT_RESTART: i32 = 75;
/// 目标显示器已不存在，停止该屏渲染。
const EXIT_MONITOR_GONE: i32 = 76;

/// 看门狗周期。与 `wpctl probe` 保持一致：单次修正失败可能是过渡窗口，5 次连续失败才报 `Lost`。
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Parser, Debug)]
#[command(name = "wp-host", version, about = "wallpaper 渲染宿主进程（每显示器一个）")]
struct Args {
    /// 显示器索引（0 起）。热插拔频繁时建议改用 `--device`
    #[arg(long, default_value_t = 0)]
    monitor: usize,
    /// 显示器设备名（如 `\\.\DISPLAY1`），显示器顺序变化后仍能认回同一块屏
    #[arg(long)]
    device: Option<String>,
    /// 桌面宿主策略
    #[arg(long, default_value = "defview_adopt")]
    strategy: String,
    /// 测试内容：`d3d`（DComp 纯色，正式管线）或 `gdi`（纯色对照）
    #[arg(long, default_value = "d3d", value_parser = ["d3d", "gdi"])]
    content: String,
    /// 渲染帧率上限（纯色测试也走同一条节流路径）
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// 运行秒数，0 表示一直运行到收到退出信号
    #[arg(long, default_value_t = 0)]
    duration: u64,
    /// 跳过启动时的桌面自愈（仅用于验证自愈逻辑本身）
    #[arg(long)]
    no_heal: bool,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            tracing::error!("wp-host 失败: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let args = Args::parse();

    if !args.no_heal {
        if let Some(message) = wp_platform::heal_desktop() {
            tracing::warn!(target: "self_heal", "{message}");
        }
        if let Some(message) = wp_platform::restore_leaked_windows() {
            tracing::warn!(target: "self_heal", "{message}");
        }
    }

    let strategy = args
        .strategy
        .parse::<Strategy>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let content = Content::parse(&args.content)?;
    let fps = args.fps.clamp(1, 240);
    let duration = (args.duration > 0).then(|| Duration::from_secs(args.duration));

    let monitor = resolve_monitor(args.device.as_deref(), args.monitor)?;
    tracing::info!(
        device = %monitor.device,
        rect = ?monitor.rect,
        strategy = strategy.as_str(),
        content = content.as_str(),
        fps,
        "wp-host 启动"
    );

    let mut session = Session::open(strategy, content, &monitor)?;
    tracing::info!(window = ?session.hwnd, effective_content = session.content.as_str(), "已挂到桌面");

    // 锁屏/解锁、休眠/唤醒都靠这两类通知；失败只影响暂停策略，不影响壁纸。
    install_session_notifications(session.hwnd);
    install_console_handler();
    HOST_HWND.store(session.hwnd, Ordering::Relaxed);

    let result = event_loop(&mut session, fps, duration);

    HOST_HWND.store(0, Ordering::Relaxed);
    // 顺序很重要：先放掉 DComp 表面，再把 DefView 还给 Progman，最后销毁窗口。
    session.teardown();
    tracing::info!(?result, "wp-host 退出");

    result
}

fn event_loop(
    session: &mut Session,
    fps: u32,
    duration: Option<Duration>,
) -> Result<i32> {
    let started = Instant::now();
    let frame_interval = frame_interval(session.content, fps);

    let mut events = Events::default();
    let mut next_check = Instant::now();
    let mut next_frame = Instant::now();
    let mut was_paused = false;
    let mut corrections = 0u64;

    loop {
        pump_messages(&mut events);

        if events.stop {
            tracing::info!("收到退出请求，准备还原桌面");
            return Ok(0);
        }
        if let Some(limit) = duration {
            if started.elapsed() >= limit {
                tracing::info!(seconds = limit.as_secs(), "运行时间到，正常退出");
                return Ok(0);
            }
        }

        if events.display_changed {
            events.display_changed = false;
            match refresh_geometry(session) {
                Ok(Some(rect)) => {
                    tracing::info!(?rect, "显示器几何变化，已重新挂载");
                    // 会话重建换了窗口句柄：控制台退出与锁屏通知都要重新绑定。
                    HOST_HWND.store(session.hwnd, Ordering::Relaxed);
                    install_session_notifications(session.hwnd);
                    next_check = Instant::now();
                }
                Ok(None) => {}
                Err(GeometryError::MonitorGone) => {
                    tracing::warn!(device = %session.device, "目标显示器已移除");
                    return Ok(EXIT_MONITOR_GONE);
                }
                Err(GeometryError::Failed(e)) => return Err(e),
            }
        }

        let paused = events.locked || events.suspended;
        if paused {
            was_paused = true;
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if was_paused {
            was_paused = false;
            tracing::info!("恢复渲染");
            // 锁屏期间 `Win+D`、显示器休眠都可能把窗口收起来，先还原再交给看门狗。
            if is_minimized(session.hwnd) {
                restore_window(session.hwnd);
            }
            next_check = Instant::now();
        }

        let now = Instant::now();
        if now >= next_check {
            match session.host.ensure_band(session.hwnd) {
                Ok(BandState::Correct) => {}
                Ok(BandState::Corrected) => {
                    corrections += 1;
                    tracing::info!(corrections, "z 序有漂移，已修正");
                }
                Ok(BandState::Lost) => {
                    // 契约：新 DefView 不透明，进程内重建窗口实测仍黑屏，必须换进程。
                    tracing::error!(
                        "桌面壳层已更换（Explorer 重启），本进程无法自愈，请求 supervisor 重启"
                    );
                    return Ok(EXIT_RESTART);
                }
                Err(e) => tracing::warn!("看门狗检查失败: {e}"),
            }
            next_check = now + WATCHDOG_INTERVAL;
        }

        if now >= next_frame {
            if let Err(e) = session.paint() {
                tracing::warn!("绘制失败: {e}");
            }
            next_frame = now + frame_interval;
        }

        // 空转会烧 CPU；16 ms 一帧时这个睡眠就是「等到下一帧」的粗略实现。
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn frame_interval(content: Content, fps: u32) -> Duration {
    match content {
        // GDI 纯色只有验证价值，没必要 60 fps 重画。
        Content::Gdi => Duration::from_millis(500),
        Content::D3d => Duration::from_millis(1000 / fps.max(1) as u64),
    }
}

fn resolve_monitor(device: Option<&str>, index: usize) -> Result<MonitorInfo> {
    let monitors = enumerate_monitors();
    let total = monitors.len();
    if let Some(device) = device {
        return monitors
            .into_iter()
            .find(|m| m.device.eq_ignore_ascii_case(device))
            .with_context(|| format!("找不到显示器 {device}（当前共 {total} 台）"));
    }
    monitors
        .into_iter()
        .nth(index)
        .with_context(|| format!("显示器索引 {index} 不存在（当前共 {total} 台）"))
}

/// 显示器几何变化：按设备名找同一块屏；没变就什么都不做。
fn refresh_geometry(session: &mut Session) -> std::result::Result<Option<PhysicalRect>, GeometryError> {
    let monitors = enumerate_monitors();
    let Some(monitor) = monitors
        .iter()
        .find(|m| m.device.eq_ignore_ascii_case(&session.device))
    else {
        return Err(GeometryError::MonitorGone);
    };
    if monitor.rect == session.rect {
        return Ok(None);
    }
    let rect = monitor.rect;
    session
        .restart(rect)
        .map_err(GeometryError::Failed)?;
    Ok(Some(rect))
}

enum GeometryError {
    MonitorGone,
    Failed(anyhow::Error),
}

/// 本次消息泵里收集到的、需要主循环处理的系统事件。
#[derive(Default)]
struct Events {
    stop: bool,
    display_changed: bool,
    locked: bool,
    suspended: bool,
}

/// 抽干线程消息队列。
///
/// 自己处理关心的事件，其余交给窗口过程——尤其 `WM_NCHITTEST`（输入穿透）
/// 必须走到 `wp-platform` 的 `host_wnd_proc`。
fn pump_messages(events: &mut Events) {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            match msg.message {
                // 默认窗口过程会直接销毁窗口——那会连 DefView 一起带走，
                // 必须先回到主循环走 detach 流程。
                WM_CLOSE => events.stop = true,
                WM_ENDSESSION => {
                    if msg.wParam.0 != 0 {
                        events.stop = true;
                    }
                }
                WM_DISPLAYCHANGE => events.display_changed = true,
                // 混合 DPI 下换屏 / 改缩放会先给 WM_DPICHANGED，几何同样要重算。
                WM_DPICHANGED => events.display_changed = true,
                WM_WTSSESSION_CHANGE => match msg.wParam.0 as u32 {
                    WTS_SESSION_LOCK => {
                        events.locked = true;
                        tracing::info!("会话已锁定，暂停渲染");
                    }
                    WTS_SESSION_UNLOCK => {
                        events.locked = false;
                        tracing::info!("会话已解锁");
                    }
                    _ => {}
                },
                WM_POWERBROADCAST => match msg.wParam.0 as u32 {
                    PBT_APMSUSPEND => {
                        events.suspended = true;
                        tracing::info!("系统休眠，暂停渲染");
                    }
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                        events.suspended = false;
                        tracing::info!("系统唤醒");
                    }
                    _ => {}
                },
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// 一次「窗口 + 挂载 + 表面」的完整会话。
///
/// 显示器几何变化或 D3D 降级时整段重建：窗口样式在创建时确定
/// （`WS_EX_NOREDIRECTIONBITMAP` 决定了 GDI 还能不能画），换内容类型只能重建窗口。
struct Session {
    hwnd: isize,
    host: Box<dyn DesktopHost>,
    surface: Option<DcompSurface>,
    content: Content,
    strategy: Strategy,
    device: String,
    rect: PhysicalRect,
}

impl Session {
    fn open(strategy: Strategy, content: Content, monitor: &MonitorInfo) -> Result<Self> {
        let spec = HostWindowSpec::for_monitor(monitor.rect)
            .with_noredirection_bitmap(content == Content::D3d);
        let hwnd = create_host_window(&spec).context("创建宿主窗口失败")?;

        let mut host = strategy
            .open()
            .with_context(|| format!("初始化策略 {} 失败", strategy.as_str()))?;
        if let Err(e) = host.attach(hwnd, monitor.rect) {
            destroy_host_window(hwnd);
            return Err(anyhow::anyhow!(e)).context("挂载桌面失败");
        }

        let surface = if content == Content::D3d {
            // 策略 E/F 让 DefView 成为子窗口，visual 必须在子窗口之下（topmost=false），
            // 否则图标会被 DComp 表面整块盖住（实测图标区异色像素归零）。
            let topmost = !matches!(strategy, Strategy::DefViewAdopt | Strategy::DefViewChild);
            match DcompSurface::new(
                hwnd,
                monitor.rect.width,
                monitor.rect.height,
                topmost,
            ) {
                Ok(surface) => Some(surface),
                Err(e) => {
                    // 绝不能停在黑屏：拆掉 noredirection 窗口，用 GDI 纯色重来一次。
                    tracing::warn!("D3D/DComp 初始化失败（{e}），退回 GDI 纯色");
                    let _ = host.detach(hwnd);
                    destroy_host_window(hwnd);
                    return Self::open(strategy, Content::Gdi, monitor);
                }
            }
        } else {
            None
        };

        Ok(Self {
            hwnd,
            host,
            surface,
            content,
            strategy,
            device: monitor.device.clone(),
            rect: monitor.rect,
        })
    }

    fn paint(&mut self) -> Result<()> {
        match (&mut self.surface, self.content) {
            (Some(surface), _) => surface
                .clear(TEST_RGB)
                .map_err(|e| anyhow::anyhow!(e)),
            (None, Content::Gdi) => paint_solid(self.hwnd, TEST_COLOR).map_err(|e| anyhow::anyhow!(e)),
            (None, Content::D3d) => Ok(()),
        }
    }

    /// 先完整还原桌面，再按新矩形重建整个会话。
    fn restart(&mut self, rect: PhysicalRect) -> Result<()> {
        let (strategy, content, device) = (self.strategy, self.content, self.device.clone());
        self.teardown();
        // 先移动旧窗口已经没意义了（teardown 已销毁），这里创建新窗口时直接用新矩形。
        let monitor = MonitorInfo {
            key: wp_core::MonitorKey(device.clone()),
            device,
            rect,
            primary: false,
        };
        *self = Self::open(strategy, content, &monitor)?;
        Ok(())
    }

    fn teardown(&mut self) {
        // DComp 表面先于窗口释放：它持有 hwnd 的 target。
        self.surface = None;
        if self.hwnd == 0 {
            return;
        }
        if let Err(e) = self.host.detach(self.hwnd) {
            tracing::warn!("detach 失败（图标层可能已失效）: {e}");
        }
        destroy_host_window(self.hwnd);
        self.hwnd = 0;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    D3d,
    Gdi,
}

impl Content {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "d3d" => Ok(Self::D3d),
            "gdi" => Ok(Self::Gdi),
            other => bail!("未知内容类型 {other}（d3d / gdi）"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::D3d => "d3d",
            Self::Gdi => "gdi",
        }
    }
}

/// 窗口过程在线程里，控制台处理函数在另一个线程：用一个原子句柄传退出请求。
static HOST_HWND: AtomicIsize = AtomicIsize::new(0);

/// Ctrl+C / 关闭控制台时走正常退出路径（`WM_CLOSE`），而不是被直接杀掉。
///
/// 这不是可选项：策略 E 下 `DefView` 是我们的子窗口，硬杀会把桌面图标层一起销毁。
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> windows::core::BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            let hwnd = HOST_HWND.load(Ordering::Relaxed);
            if hwnd != 0 {
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(hwnd as *mut _)),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
            windows::core::BOOL(1)
        }
        _ => windows::core::BOOL(0),
    }
}

fn install_console_handler() {
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);
    }
}

fn install_session_notifications(hwnd: isize) {
    unsafe {
        if let Err(e) = WTSRegisterSessionNotification(HWND(hwnd as *mut _), NOTIFY_FOR_THIS_SESSION)
        {
            tracing::warn!("会话通知注册失败（锁屏时不会自动暂停）: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_roundtrip() {
        assert_eq!(Content::parse("d3d").unwrap().as_str(), "d3d");
        assert_eq!(Content::parse("gdi").unwrap().as_str(), "gdi");
        assert!(Content::parse("mpv").is_err());
    }

    #[test]
    fn frame_interval_respects_fps_and_floors_at_one() {
        assert_eq!(frame_interval(Content::D3d, 60), Duration::from_millis(16));
        assert_eq!(frame_interval(Content::D3d, 0), Duration::from_millis(1000));
        assert_eq!(frame_interval(Content::Gdi, 60), Duration::from_millis(500));
    }

    #[test]
    fn restart_exit_code_is_distinct() {
        // 75 是 sysexits 的 EX_TEMPFAIL，supervisor 用它区分「可重启」与「普通失败」。
        assert_eq!(EXIT_RESTART, 75);
        assert_ne!(EXIT_MONITOR_GONE, EXIT_RESTART);
    }
}
