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
//! | 77 | 壁纸被新 `DefView` 整块盖住（屏幕只剩一片单色） | 退回系统壁纸，**不要自动重试**（重试还是被盖住） |
//!
//! 生命周期：
//!
//! 1. `heal_desktop()` + `restore_leaked_windows()` 收拾上次崩溃的残局；
//! 2. 枚举显示器 → 创建宿主窗口 → `attach` 桌面；
//! 3. 消息循环 + 约 2 s 一次的 z 序看门狗；
//! 4. 退出前先 `detach`（把 `DefView` 还给 `Progman`）再销毁窗口。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
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
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, PostMessageW, TranslateMessage, EVENT_OBJECT_REORDER,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, MSG,
    PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PM_REMOVE, WINEVENT_OUTOFCONTEXT,
    WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_POWERBROADCAST,
    WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

use wp_core::PhysicalRect;
use wp_platform::{
    create_host_window, destroy_host_window, enumerate_monitors, is_minimized, paint_solid,
    restore_window, BandState, DcompSurface, DesktopHost, HostWindowSpec, MonitorInfo, Strategy,
    TEST_COLOR, TEST_RGB,
};

/// 需要重启本进程（Explorer 换了桌面壳层，进程内无法自愈）。
const EXIT_RESTART: i32 = 75;
/// 目标显示器已不存在，停止该屏渲染。
const EXIT_MONITOR_GONE: i32 = 76;
/// 壁纸在屏幕上完全看不到（被不透明的 `DefView` 盖住），已退回系统壁纸。
const EXIT_COVERED: i32 = 77;

/// 看门狗周期。与 `wpctl probe` 保持一致：单次修正失败可能是过渡窗口，5 次连续失败才报 `Lost`。
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);

/// 暂停判定周期。全屏应用切进切出比 z 序漂移快，1 s 足够且开销可忽略。
const PAUSE_INTERVAL: Duration = Duration::from_secs(1);

/// 覆盖自检：挂上去之后先等画面稳定，再做第一次采样。
const VISIBLE_FIRST_CHECK: Duration = Duration::from_millis(1500);
/// 覆盖自检周期。
const VISIBLE_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// 已经怀疑被盖住时的复查间隔。
const COVERED_RECHECK: Duration = Duration::from_millis(700);
/// 连续多少次判定「看不到壁纸」才放弃。中间一次采样可能撞上重绘，不算数。
const COVERED_STRIKES: u32 = 2;

/// 事件触发的 z 序核对最小间隔。
///
/// `EVENT_OBJECT_REORDER` 在拖窗口、Alt+Tab 时会连续爆发，而每次核对都要遍历整条
/// z 链；限流到 5 次/秒既能把「最小化后壁纸短暂盖住任务栏」缩到肉眼一瞬，
/// 也不会在快速切换窗口时把 CPU 打上去。
const EVENT_RECHECK_INTERVAL: Duration = Duration::from_millis(200);

/// z 序事件标志。事件回调只置位，真正的核对留给事件循环——回调里不能做重活。
static Z_EVENT: AtomicBool = AtomicBool::new(false);

/// 订阅「可能改 z 序」的 WinEvent。
///
/// `WINEVENT_OUTOFCONTEXT` 的回调在我们的消息循环里执行，不需要单独线程。
/// 菜单、焦点、名称变更之类的事件也会走到回调，所以先按事件号过滤。
struct ZEventHooks {
    hooks: [HWINEVENTHOOK; 2],
}

impl ZEventHooks {
    fn install() -> Self {
        let proc = Some(win_event_proc as _);
        let hooks = unsafe {
            [
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_MINIMIZEEND,
                    None,
                    proc,
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                ),
                SetWinEventHook(
                    EVENT_OBJECT_REORDER,
                    EVENT_OBJECT_REORDER,
                    None,
                    proc,
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                ),
            ]
        };
        Self { hooks }
    }
}

impl Drop for ZEventHooks {
    fn drop(&mut self) {
        for hook in self.hooks {
            if !hook.is_invalid() {
                unsafe {
                    let _ = UnhookWinEvent(hook);
                }
            }
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if matches!(
        event,
        EVENT_SYSTEM_FOREGROUND
            | EVENT_SYSTEM_MINIMIZESTART
            | EVENT_SYSTEM_MINIMIZEEND
            | EVENT_OBJECT_REORDER
    ) {
        Z_EVENT.store(true, Ordering::Relaxed);
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "wp-host",
    version,
    about = "wallpaper 渲染宿主进程（每显示器一个）"
)]
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
    /// 图片壁纸路径（PNG / JPEG / BMP）。给定后自动使用 DComp 管线，忽略 `--content`
    #[arg(long)]
    image: Option<PathBuf>,
    /// 渲染帧率上限（纯色测试也走同一条节流路径）
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// 运行秒数，0 表示一直运行到收到退出信号
    #[arg(long, default_value_t = 0)]
    duration: u64,
    /// 关闭暂停判定（全屏应用 / 最大化遮挡 / 电池 / 远程会话）
    #[arg(long)]
    no_pause: bool,
    /// 跳过启动时的桌面自愈（仅用于验证自愈逻辑本身）
    #[arg(long)]
    no_heal: bool,
    /// 不监听命名管道（调试用；`wpctl attach` 就无法热换图了）
    #[arg(long)]
    no_ipc: bool,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
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
    // 图片只能在 DComp 表面里画（窗口带 NOREDIRECTIONBITMAP 时 GDI 画不进去），
    // 所以给了 `--image` 就锁定 d3d，不让 `--content gdi` 把它悄悄降级成纯色。
    let content = if args.image.is_some() {
        Content::D3d
    } else {
        Content::parse(&args.content)?
    };
    let fps = args.fps.clamp(1, 240);
    let duration = (args.duration > 0).then(|| Duration::from_secs(args.duration));

    let (monitor_index, monitor) = resolve_monitor(args.device.as_deref(), args.monitor)?;
    tracing::info!(
        device = %monitor.device,
        rect = ?monitor.rect,
        strategy = strategy.as_str(),
        content = content.as_str(),
        image = args.image.as_ref().map(|p| p.display().to_string()),
        fps,
        "wp-host 启动"
    );

    let mut session = Session::open(strategy, content, &monitor, args.image.as_deref())?;
    tracing::info!(window = ?session.hwnd, effective_content = session.content.as_str(), "已挂到桌面");

    // 锁屏/解锁、休眠/唤醒都靠这两类通知；失败只影响暂停策略，不影响壁纸。
    install_session_notifications(session.hwnd);
    install_console_handler();
    HOST_HWND.store(session.hwnd, Ordering::Relaxed);

    // IPC 线程只负责「收命令、回事件」，真正改 Session 的活留给主循环：
    // 会话里有窗口、D3D 表面和桌面挂载，跨线程共享只会引入新的竞态。
    let (ipc_tx, ipc_rx) = mpsc::channel();
    if args.no_ipc {
        tracing::info!("已关闭 IPC（--no-ipc）");
    } else {
        let name = wp_ipc::pipe_name(wp_platform::current_session_id(), monitor_index as u32);
        spawn_ipc(name, ipc_tx);
    }

    let result = event_loop(&mut session, &ipc_rx, fps, duration, !args.no_pause);

    HOST_HWND.store(0, Ordering::Relaxed);
    // 顺序很重要：先放掉 DComp 表面，再把 DefView 还给 Progman，最后销毁窗口。
    session.teardown();
    tracing::info!(?result, "wp-host 退出");

    result
}

fn event_loop(
    session: &mut Session,
    ipc_rx: &Receiver<IpcRequest>,
    fps: u32,
    duration: Option<Duration>,
    pause_detection: bool,
) -> Result<i32> {
    let started = Instant::now();
    let frame_interval = frame_interval(session.content, fps);

    // z 序事件只用来「提前叫醒」看门狗；装不上也不影响 2 s 定时核对。
    let _z_hooks = ZEventHooks::install();
    Z_EVENT.store(false, Ordering::Relaxed);

    let mut events = Events::default();
    let mut next_check = Instant::now();
    let mut last_event_check = Instant::now();
    let mut event_checks = 0u64;
    let mut next_frame = Instant::now();
    let mut next_pause_check = Instant::now();
    let mut next_visible_check = Instant::now() + VISIBLE_FIRST_CHECK;
    let mut covered_strikes = 0u32;
    let mut pause_reasons: Vec<wp_platform::PauseReason> = Vec::new();
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
                tracing::info!(
                    seconds = limit.as_secs(),
                    corrections,
                    event_checks,
                    "运行时间到，正常退出"
                );
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

        // IPC 换图：命令来自 wpctl / supervisor。放在暂停早退之前，
        // 这样锁屏/休眠时也能及时回应客户端；是否立刻重绘再按暂停状态判断。
        match ipc_rx.try_recv() {
            Ok(IpcRequest::LoadFile { path, reply }) => {
                let result = session.load_file(&path);
                match &result {
                    Ok((width, height)) => {
                        tracing::info!(path = %path.display(), width = *width, height = *height, "通过 IPC 换图");
                        if !events.locked && !events.suspended && pause_reasons.is_empty() {
                            if let Err(e) = session.paint() {
                                tracing::warn!("换图后绘制失败: {e}");
                            }
                        }
                    }
                    Err(e) => tracing::warn!(path = %path.display(), "通过 IPC 换图失败: {e}"),
                }
                // `{:#}` 打出整条错误链：客户端看到的不只是「加载壁纸失败」，
                // 还包含底层的「无法识别图片格式」之类真正有用的原因。
                let _ = reply.send(result.map_err(|e| format!("{e:#}")));
            }
            Ok(IpcRequest::Shutdown) => {
                tracing::info!("收到 IPC 退出请求，准备还原桌面");
                return Ok(0);
            }
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {}
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

        // 暂停判定：全屏游戏 / 最大化窗口 / 电池 / 远程桌面 / 锁屏 / 休眠。
        // 锁屏与休眠在上面已经走 `continue`，这里负责其余条件。
        if pause_detection && now >= next_pause_check {
            let report = wp_platform::pause::probe(
                Some(session.hwnd),
                session.rect,
                wp_platform::PauseFlags {
                    locked: events.locked,
                    suspended: events.suspended,
                    user_paused: false,
                },
            );
            if report.reasons != pause_reasons {
                if report.paused {
                    tracing::info!(reasons = %report.summary(), "暂停渲染");
                } else {
                    tracing::info!("暂停条件消失，恢复渲染");
                }
                pause_reasons = report.reasons;
            }
            next_pause_check = now + PAUSE_INTERVAL;
        }

        // 前台切换 / 最小化 / 窗口重排会立刻改 z 序：不等 2 s 定时器，尽快核对一次。
        if Z_EVENT.swap(false, Ordering::Relaxed)
            && now.duration_since(last_event_check) >= EVENT_RECHECK_INTERVAL
        {
            last_event_check = now;
            event_checks += 1;
            next_check = now;
        }

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
            if pause_reasons.is_empty() {
                if let Err(e) = session.paint() {
                    tracing::warn!("绘制失败: {e}");
                }
            }
            next_frame = now + frame_interval;
        }

        // 覆盖自检：Explorer 重启后新的图标层会自己画一层不透明背景，把壁纸整块盖住。
        // 那一刻桌面是全黑的，用户没有任何入口知道「壁纸其实还在画」。
        // 暂停状态（全屏应用、锁屏）下屏幕本来就该被别的窗口盖住，不检查。
        if now >= next_visible_check && pause_reasons.is_empty() {
            match session.visible_on_screen() {
                Ok(true) => {
                    covered_strikes = 0;
                }
                Ok(false) => {
                    covered_strikes += 1;
                    tracing::warn!(strikes = covered_strikes, "屏幕上看不到壁纸");
                    if covered_strikes >= COVERED_STRIKES {
                        // 兜底：宁可退回系统壁纸，也绝不停在黑屏上（硬规则 9）。
                        tracing::error!(
                            "壁纸被桌面图标层整块盖住（26H2 Explorer 重启后的已知问题），退回系统壁纸"
                        );
                        return Ok(EXIT_COVERED);
                    }
                }
                Err(e) => tracing::debug!("可见性自检失败: {e}"),
            }
            // 已经怀疑被盖住就快一点复查：黑屏每一秒都是用户可见的缺陷。
            next_visible_check = now
                + if covered_strikes > 0 {
                    COVERED_RECHECK
                } else {
                    VISIBLE_CHECK_INTERVAL
                };
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

fn resolve_monitor(device: Option<&str>, index: usize) -> Result<(usize, MonitorInfo)> {
    let monitors = enumerate_monitors();
    let total = monitors.len();
    if let Some(device) = device {
        let position = monitors
            .iter()
            .position(|m| m.device.eq_ignore_ascii_case(device))
            .with_context(|| format!("找不到显示器 {device}（当前共 {total} 台）"))?;
        return Ok((position, monitors[position].clone()));
    }
    let monitor = monitors
        .into_iter()
        .nth(index)
        .with_context(|| format!("显示器索引 {index} 不存在（当前共 {total} 台）"))?;
    Ok((index, monitor))
}

/// 显示器几何变化：按设备名找同一块屏；没变就什么都不做。
fn refresh_geometry(
    session: &mut Session,
) -> std::result::Result<Option<PhysicalRect>, GeometryError> {
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
    session.restart(rect).map_err(GeometryError::Failed)?;
    Ok(Some(rect))
}

enum GeometryError {
    MonitorGone,
    Failed(anyhow::Error),
}

/// IPC 请求在主线程里最多等多久。图片解码是同步的 CPU 工作，
/// 4K 图 + 机械盘也可控；超过这个时间说明主循环卡住，必须让客户端知道。
const IPC_TIMEOUT: Duration = Duration::from_secs(20);

/// 来自 IPC 线程的请求。
///
/// 只有「必须访问 [`Session`]」的命令走这条通道：管道线程永远不碰
/// 窗口、D3D 表面和桌面挂载，避免把竞态引进最难调试的那一层。
enum IpcRequest {
    LoadFile {
        path: PathBuf,
        reply: Sender<std::result::Result<(u32, u32), String>>,
    },
    Shutdown,
}

/// 起一个专用线程做管道 server。
///
/// 线程永远阻塞在 `accept` / `read_frame` 上，进程退出时随之结束：
/// 不需要 join，也不会拖住优雅退出（主循环返回后 `main` 直接 `process::exit`）。
fn spawn_ipc(name: String, tx: Sender<IpcRequest>) {
    let spawned = std::thread::Builder::new()
        .name("wp-ipc".into())
        .spawn(move || {
            let server = wp_ipc::pipe::PipeServer::bind(&name);
            tracing::info!(pipe = %name, "IPC 已监听");
            loop {
                let mut conn = match server.accept() {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!("IPC 接受连接失败: {e}");
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                };
                tracing::debug!("IPC 客户端已连接");
                if let Err(e) = serve_connection(&mut conn, &tx) {
                    // 客户端正常断开也会走到这里（读不到更多帧），日志留 debug 即可。
                    tracing::debug!("IPC 连接结束: {e}");
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("IPC 线程启动失败，热换图不可用: {e}");
    }
}

/// 一次连接的生命周期：读一条命令，回一条事件，直到客户端断开。
///
/// 现在只做「一问一答」：`wpctl attach` 发完 `LoadFile` 就读回应。
/// 长时间的统计推送（`Stats`）留给 supervisor，那时会需要读超时或异步 IO。
fn serve_connection(
    conn: &mut std::fs::File,
    tx: &Sender<IpcRequest>,
) -> std::result::Result<(), wp_ipc::FrameError> {
    loop {
        let command: wp_ipc::Command = wp_ipc::read_frame(conn)?;
        match command {
            wp_ipc::Command::LoadFile { path } => {
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx
                    .send(IpcRequest::LoadFile {
                        path: path.clone(),
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    let _ = wp_ipc::write_frame(
                        conn,
                        &wp_ipc::Event::Error {
                            code: wp_ipc::ErrorCode::Internal,
                            message: "宿主主循环已退出".into(),
                        },
                    );
                    return Ok(());
                }

                let event = match reply_rx.recv_timeout(IPC_TIMEOUT) {
                    Ok(Ok((width, height))) => wp_ipc::Event::Loaded {
                        path: path.display().to_string(),
                        width,
                        height,
                    },
                    Ok(Err(message)) => wp_ipc::Event::Error {
                        code: wp_ipc::ErrorCode::Decode,
                        message,
                    },
                    Err(_) => wp_ipc::Event::Error {
                        code: wp_ipc::ErrorCode::Internal,
                        message: format!("宿主 {} 秒内没有响应", IPC_TIMEOUT.as_secs()),
                    },
                };
                wp_ipc::write_frame(conn, &event)?;
            }
            wp_ipc::Command::Shutdown => {
                let _ = wp_ipc::write_frame(
                    conn,
                    &wp_ipc::Event::Log {
                        level: 1,
                        message: "正在退出".into(),
                    },
                );
                let _ = tx.send(IpcRequest::Shutdown);
                return Ok(());
            }
            other => {
                tracing::warn!(?other, "IPC 收到暂不支持的命令");
                wp_ipc::write_frame(
                    conn,
                    &wp_ipc::Event::Error {
                        code: wp_ipc::ErrorCode::Unsupported,
                        message: format!("{other:?} 暂不支持"),
                    },
                )?;
            }
        }
    }
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

/// 自检采样点（相对宿主窗口的像素坐标）。
///
/// 避开左侧图标列和底部任务栏：那两处永远不是壁纸，拿来比较只会误导。
fn visible_probe_points(rect: PhysicalRect) -> Vec<(u32, u32)> {
    const XS: [f32; 5] = [0.30, 0.45, 0.60, 0.75, 0.90];
    const YS: [f32; 4] = [0.20, 0.40, 0.60, 0.80];
    let mut points = Vec::with_capacity(XS.len() * YS.len());
    for y in YS {
        for x in XS {
            points.push((
                (rect.width as f32 * x) as u32,
                (rect.height as f32 * y) as u32,
            ));
        }
    }
    points
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
    /// 已缩放到屏幕尺寸的壁纸；`None` 表示画 M0 的测试纯色。
    image: Option<wp_render::BgraImage>,
    /// 图片路径。`restart` 要重新解码，所以留着原路径。
    image_path: Option<PathBuf>,
    /// 图片是否需要重画。静态壁纸只在加载后提交一帧。
    dirty: bool,
}

impl Session {
    fn open(
        strategy: Strategy,
        content: Content,
        monitor: &MonitorInfo,
        image_path: Option<&Path>,
    ) -> Result<Self> {
        // 先解码再建窗口：图片坏了要立刻退出，绝不能先闪一个空窗口——
        // 更别说桌面挂载一旦发生，`wpctl attach` 那边就会看到「窗口出现 = 成功」。
        let image = match (content, image_path) {
            (Content::D3d, Some(path)) => Some(
                wp_render::decode_file(
                    path,
                    wp_render::PhysicalSize {
                        width: monitor.rect.width,
                        height: monitor.rect.height,
                    },
                )
                .map_err(|e| anyhow::anyhow!(e))
                .with_context(|| format!("加载壁纸 {} 失败", path.display()))?,
            ),
            _ => None,
        };

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
            match DcompSurface::new(hwnd, monitor.rect.width, monitor.rect.height, topmost) {
                Ok(surface) => Some(surface),
                Err(e) => {
                    // 绝不能停在黑屏：拆掉 noredirection 窗口，用 GDI 纯色重来一次。
                    // 图片没法在 GDI 窗口里画，顺便说清楚降级了。
                    if image_path.is_some() {
                        tracing::warn!("D3D/DComp 不可用，图片壁纸降级为纯色（{e}）");
                    } else {
                        tracing::warn!("D3D/DComp 初始化失败（{e}），退回 GDI 纯色");
                    }
                    let _ = host.detach(hwnd);
                    destroy_host_window(hwnd);
                    return Self::open(strategy, Content::Gdi, monitor, None);
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
            image,
            image_path: image_path.map(Path::to_path_buf),
            dirty: true,
        })
    }

    /// 内容在窗口内 (x, y) 处应该是的颜色。原点在窗口左上角。
    fn expected_rgb(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if let Some(image) = self.image.as_ref() {
            if x >= image.width || y >= image.height {
                return None;
            }
            let offset = (y as usize * image.width as usize + x as usize) * 4;
            let px = image.pixels.get(offset..offset + 4)?;
            // BGRA
            return Some([px[2], px[1], px[0]]);
        }
        Some(TEST_RGB)
    }

    /// 壁纸是否真的出现在屏幕上。
    ///
    /// 判定分三步，条件刻意收得很紧——别的窗口挡住一部分、用户开着深色全屏应用
    /// 都不该被误判（宁可漏报，也不能错误地把壁纸撤掉）：
    ///
    /// 1. 采样点上能对上我们该画的像素 → 可见；
    /// 2. 一个都对不上，但屏幕本身「有内容」（采样点颜色不一致）→ 是别的窗口盖着，也算可见；
    /// 3. 屏幕整块单色、又没有一处是我们的像素 → 被 `DefView` 自己画的背景盖住了。
    ///
    /// 第 3 条正是 Explorer 重启后的已知现象：新 `SHELLDLL_DefView` 会自己画一层不透明
    /// 背景（黑屏，或者干脆是系统壁纸），把我们的 DComp 内容整块盖住。
    /// 实测：把 `DefView` 缩小到 400×300，壁纸立刻露出来。
    fn visible_on_screen(&self) -> Result<bool> {
        let screen = wp_platform::capture(self.rect).map_err(|e| anyhow::anyhow!(e))?;
        let mut expected_pixels = Vec::new();
        let mut actual_pixels = Vec::new();
        for (x, y) in visible_probe_points(self.rect) {
            let Some(expected) = self.expected_rgb(x, y) else {
                continue;
            };
            let Some(actual) = screen.pixel(x, y) else {
                continue;
            };
            expected_pixels.push(expected);
            actual_pixels.push(actual);
        }
        // 一个点都采不到（窗口太小之类）：不下结论。
        if expected_pixels.is_empty() {
            return Ok(true);
        }

        let close = |want: &[u8; 3], got: &[u8; 3]| {
            want.iter()
                .zip(got.iter())
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(255)
                <= 16
        };
        if expected_pixels
            .iter()
            .zip(actual_pixels.iter())
            .any(|(want, got)| close(want, got))
        {
            return Ok(true);
        }

        // 屏幕上本来就五颜六色：多半是应用窗口盖在上面，不是这个 bug。
        let spread = |channel: usize| {
            let min = actual_pixels.iter().map(|p| p[channel]).min().unwrap_or(0);
            let max = actual_pixels.iter().map(|p| p[channel]).max().unwrap_or(0);
            max - min
        };
        let uniform = (0..3).all(|c| spread(c) <= 20);
        Ok(!uniform)
    }

    fn paint(&mut self) -> Result<()> {
        if let Some(surface) = self.surface.as_mut() {
            if let Some(image) = self.image.as_ref() {
                // 静态壁纸：只有加载完 / 换屏之后需要提交一帧。
                if !self.dirty {
                    return Ok(());
                }
                surface
                    .draw_bgra(image.width, image.height, &image.pixels)
                    .map_err(|e| anyhow::anyhow!(e))?;
                self.dirty = false;
                return Ok(());
            }
            return surface.clear(TEST_RGB).map_err(|e| anyhow::anyhow!(e));
        }
        match self.content {
            Content::Gdi => paint_solid(self.hwnd, TEST_COLOR).map_err(|e| anyhow::anyhow!(e)),
            Content::D3d => Ok(()),
        }
    }

    /// 换一张图：解码到当前屏幕尺寸并标记重绘。
    ///
    /// 解码留在主线程而不是 IPC 线程：显示器变化后 `self.rect` 才是最新的，
    /// 而且这里能顺带确认表面还在（GDI 降级模式下没有地方画图片）。
    fn load_file(&mut self, path: &Path) -> Result<(u32, u32)> {
        if self.surface.is_none() {
            bail!("当前是 GDI 降级模式，无法加载图片");
        }
        let image = wp_render::decode_file(
            path,
            wp_render::PhysicalSize {
                width: self.rect.width,
                height: self.rect.height,
            },
        )
        .map_err(|e| anyhow::anyhow!(e))
        .with_context(|| format!("加载壁纸 {} 失败", path.display()))?;
        let size = (image.width, image.height);
        self.image = Some(image);
        self.image_path = Some(path.to_path_buf());
        self.dirty = true;
        Ok(size)
    }

    /// 先完整还原桌面，再按新矩形重建整个会话。
    fn restart(&mut self, rect: PhysicalRect) -> Result<()> {
        let (strategy, content, device) = (self.strategy, self.content, self.device.clone());
        let image_path = self.image_path.clone();
        self.teardown();
        // 先移动旧窗口已经没意义了（teardown 已销毁），这里创建新窗口时直接用新矩形。
        let monitor = MonitorInfo {
            key: wp_core::MonitorKey(device.clone()),
            device,
            rect,
            primary: false,
        };
        *self = Self::open(strategy, content, &monitor, image_path.as_deref())?;
        Ok(())
    }

    fn teardown(&mut self) {
        // DComp 表面先于窗口释放：它持有 hwnd 的 target。
        self.surface = None;
        self.image = None;
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
                    PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0))
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
        if let Err(e) =
            WTSRegisterSessionNotification(HWND(hwnd as *mut _), NOTIFY_FOR_THIS_SESSION)
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
