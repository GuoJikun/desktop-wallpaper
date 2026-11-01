//! 监管进程：每块屏拉起一个 `wp-host`，按退出码自动恢复，并对外提供一个控制管道。
//!
//! 为什么必须是独立进程：`wp-host` 崩溃或 Explorer 更换桌面壳层时，
//! 宿主进程自己无法恢复（见 AGENTS.md 硬规则 3 第四条），必须有人把它重新拉起来。
//! 这个「有人」就是本进程。
//!
//! 退出码约定（与 `wp-host` 文档一致）：
//!
//! | 退出码 | 含义 | 本进程动作 |
//! |---|---|---|
//! | 0 | 正常停止 | 该屏不再拉起 |
//! | 1 / 其它 | 启动失败或运行时错误 | 退避重启（0.5 s → 30 s） |
//! | 75 | Explorer 换了桌面壳层 | 约 300 ms 后立即重启 |
//! | 76 | 显示器已移除 | 停止该屏 |
//!
//! 停止一律走优雅路径：IPC `Shutdown` → `WM_CLOSE` → 等待。
//! **绝不** `TerminateProcess`：`DefView` 是宿主的子窗口，硬杀会连桌面图标层一起销毁。

use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Proc, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use windows::core::BOOL;
use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};

use wp_core::PhysicalRect;
use wp_ipc::supervisor::{Command as SupCommand, Event as SupEvent};
use wp_ipc::ErrorCode;
use wp_platform::{MonitorInfo, Strategy};

/// 宿主请求「换进程重启」：Explorer 更换了桌面壳层。
const EXIT_RESTART: i32 = 75;
/// 宿主的目标显示器已不存在。
const EXIT_MONITOR_GONE: i32 = 76;
/// 宿主看不到自己的画面（被 Explorer 新建的不透明图标层整块盖住），已退回系统壁纸。
const EXIT_COVERED: i32 = 77;

/// 运行满这么久之后再崩溃，退避计数从头算——避免「偶发崩溃」把下次延迟拖到 30 s。
const STABLE_AFTER: Duration = Duration::from_secs(30);

/// Explorer 重启后的快速重启延迟：新壳层已经就绪，慢一步桌面就是黑的。
const EXPLORER_RESTART_DELAY: Duration = Duration::from_millis(300);

/// 等新桌面壳层建好的上限。Explorer 一般 1–2 s 内就有图标列表，超过这个时间
/// 说明它自己卡住了，再等也没意义，直接拉起宿主（至少图标层会由我们接管）。
const SHELL_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// 主循环轮询间隔。只在等子进程退出和重试时间点，25 ms 足够且几乎不占 CPU。
const TICK: Duration = Duration::from_millis(25);

/// 等宿主管道可用的上限：解码大图 + 创建窗口 + 挂载桌面，20 s 已经很宽松。
const HOST_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// 控制连接的处理上限要大于上面的 20 s，否则客户端会先看到「超时」。
const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(60);

/// Ctrl+C / 关闭控制台：只置位，真正收尾留给主循环（与 `wp-host` 同一套路）。
static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Parser, Debug)]
#[command(
    name = "wp-supervisor",
    version,
    about = "wallpaper 监管进程：按显示器拉起宿主并在崩溃后恢复"
)]
struct Args {
    /// 初始壁纸（PNG / JPEG / BMP）。不给则宿主先画占位纯色，等控制管道换图
    #[arg(long)]
    image: Option<PathBuf>,
    /// 要接管的显示器索引（可重复，0 起）。默认接管全部
    #[arg(long = "monitor")]
    monitors: Vec<usize>,
    /// 桌面宿主策略
    #[arg(long, default_value = "defview_adopt")]
    strategy: String,
    /// 关闭暂停判定（全屏应用 / 最大化遮挡 / 电池 / 远程会话）
    #[arg(long)]
    no_pause: bool,
    /// 渲染帧率上限
    #[arg(long, default_value_t = 60)]
    fps: u32,
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
            tracing::error!("wp-supervisor 失败: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let args = Args::parse();
    let session = wp_platform::current_session_id();

    // 单实例：`attach` 可能被连点两下，两个 supervisor 同时拉起会打架。
    let mutex_name = format!(r"Local\wallpaper-supervisor-{session}");
    let _guard = match wp_platform::SingleInstance::acquire(&mutex_name) {
        Some(guard) => guard,
        None => {
            tracing::info!("已有 supervisor 在运行，本进程退出");
            return Ok(0);
        }
    };

    // 策略名提前校验，别等每块屏的宿主起来才报错。
    args.strategy
        .parse::<Strategy>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(image) = &args.image {
        if !image.is_file() {
            bail!("初始壁纸不存在: {}", image.display());
        }
    }

    let host_exe = host_exe()?;
    let mut supervisor = Supervisor {
        args,
        session,
        host_exe,
        workers: Vec::new(),
    };

    // 先绑控制管道再拉宿主：客户端在这一瞬间连上来也能被接住，
    // 命令会等主循环把 worker 建好再处理。
    let (tx, rx) = mpsc::channel();
    let pipe = wp_ipc::supervisor::pipe_name(session);
    spawn_control(pipe.clone(), tx);
    install_console_handler();
    tracing::info!(pipe = %pipe, session, "supervisor 已启动");

    let monitors = targets(&supervisor.args)?;
    let image = supervisor.args.image.clone();
    for monitor in monitors {
        if let Err(e) = supervisor.spawn_worker(monitor, image.clone()) {
            // 单块屏起不来不该拖垮整台机器：记下来，等控制命令再试。
            tracing::error!(monitor, "拉起宿主失败: {e:#}");
        }
    }

    let code = supervisor.event_loop(&rx);
    // 控制线程写完 `Stopped` 才轮到对端读到；给它一点时间落盘再退出。
    std::thread::sleep(Duration::from_millis(200));
    Ok(code)
}

/// 启动时要接管的显示器：显式 `--monitor` 优先，否则全部。
fn targets(args: &Args) -> Result<Vec<usize>> {
    let monitors = wp_platform::enumerate_monitors();
    if monitors.is_empty() {
        bail!("没有找到任何显示器");
    }
    if args.monitors.is_empty() {
        return Ok((0..monitors.len()).collect());
    }
    for &index in &args.monitors {
        if index >= monitors.len() {
            bail!("显示器索引 {index} 不存在（当前共 {} 台）", monitors.len());
        }
    }
    let mut list = args.monitors.clone();
    list.sort_unstable();
    list.dedup();
    Ok(list)
}

fn host_exe() -> Result<PathBuf> {
    let me = std::env::current_exe().context("无法定位 wp-supervisor 自身路径")?;
    let dir = me.parent().context("无法定位 wp-supervisor 所在目录")?;
    let exe = dir.join("wp-host.exe");
    if !exe.is_file() {
        bail!("找不到 {}，先运行 `cargo build -p wp-host`", exe.display());
    }
    Ok(exe)
}

/// 每个显示器一份日志，每次拉起覆盖（崩溃历史由 supervisor 自己的日志记录）。
fn log_path(monitor: usize) -> PathBuf {
    std::env::temp_dir().join(format!("wallpaper-host-{monitor}.log"))
}

/// 取日志最后几行，只在宿主启动失败时解释原因。
fn log_tail(monitor: usize, lines: usize) -> String {
    let path = log_path(monitor);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return "(日志读不出来)".into();
    };
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = all.len().saturating_sub(lines);
    if start == all.len() {
        return "(日志为空)".into();
    }
    all[start..].join("\n")
}

fn spawn_host(args: &Args, exe: &Path, monitor: usize, image: Option<&Path>) -> Result<Child> {
    let log = log_path(monitor);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0x7)
        .open(&log)
        .with_context(|| format!("创建日志 {} 失败", log.display()))?;
    let err = file.try_clone()?;

    let mut cmd = Proc::new(exe);
    cmd.arg("--monitor")
        .arg(monitor.to_string())
        .arg("--strategy")
        .arg(&args.strategy)
        .arg("--fps")
        .arg(args.fps.to_string());
    if let Some(image) = image {
        cmd.arg("--image").arg(image);
    }
    if args.no_pause {
        cmd.arg("--no-pause");
    }
    // 绝不能用 `--no-ipc`：supervisor 换图全靠宿主管道。
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err))
        .spawn()
        .with_context(|| format!("启动 {} 失败", exe.display()))
}

/// 一块屏一个 worker。`child` 为 `None` 表示正在等重启，`restart_at` 给出时间点。
struct Worker {
    monitor: usize,
    device: String,
    rect: PhysicalRect,
    /// 最近一次设置的壁纸。崩溃重启时要靠它恢复画面。
    image: Option<PathBuf>,
    child: Option<Child>,
    started: Option<Instant>,
    restart_at: Option<Instant>,
    failures: u32,
    /// 等新的桌面壳层建好再拉起（只走 Explorer 重启路径）。
    ///
    /// 新 shell 的 `SHELLDLL_DefView` 在 `SysListView32` 建好之前是不透明的，
    /// 抢在这之前接管，壁纸会被整块盖住且进程内无法补救。
    wait_shell: bool,
    /// 等壳层的截止时间，超过就照常拉起，别让桌面一直空白。
    shell_deadline: Option<Instant>,
}

impl Worker {
    fn ran_for(&self, now: Instant) -> Duration {
        self.started
            .map(|t| now.duration_since(t))
            .unwrap_or_default()
    }
}

struct Supervisor {
    args: Args,
    session: u32,
    host_exe: PathBuf,
    workers: Vec<Worker>,
}

impl Supervisor {
    fn event_loop(&mut self, rx: &Receiver<ControlRequest>) -> i32 {
        loop {
            if STOP.swap(false, Ordering::Relaxed) {
                tracing::info!("收到 Ctrl+C，优雅停止所有宿主");
                self.shutdown_all();
                return 0;
            }

            while let Ok(request) = rx.try_recv() {
                match request.command {
                    SupCommand::LoadFile { monitor, path } => {
                        let event = self.load_file(monitor as usize, path);
                        let _ = request.reply.send(event);
                    }
                    SupCommand::Shutdown => {
                        let stopped = self.shutdown_all();
                        let _ = request.reply.send(SupEvent::Stopped);
                        tracing::info!(stopped, "已按控制命令退出");
                        return 0;
                    }
                }
            }

            self.tick(Instant::now());
            std::thread::sleep(TICK);
        }
    }

    /// 收尸 + 到点重启。每轮都跑，事件驱动之外留一条兜底路径。
    fn tick(&mut self, now: Instant) {
        for worker in &mut self.workers {
            if worker.child.is_none() {
                continue;
            }
            // 先把借用收窄到一个块里，之后才能改 worker 的其它字段。
            let ran = worker.ran_for(now);
            let Some((code, pid)) = (|| {
                let child = worker.child.as_mut()?;
                let status = match child.try_wait() {
                    Ok(status) => status,
                    Err(e) => {
                        tracing::warn!(monitor = worker.monitor, "查询宿主状态失败: {e}");
                        return None;
                    }
                };
                Some((status?.code().unwrap_or(-1), child.id()))
            })() else {
                continue;
            };

            worker.child = None;
            worker.started = None;

            match code {
                0 => {
                    tracing::info!(monitor = worker.monitor, pid, "wp-host 正常退出，不再拉起");
                }
                EXIT_MONITOR_GONE => {
                    tracing::warn!(monitor = worker.monitor, pid, "显示器已移除，停止该屏渲染");
                }
                EXIT_COVERED => {
                    // 自动重试只会再黑一次：这个状态要等 Explorer 自己恢复。
                    worker.failures = 0;
                    worker.restart_at = None;
                    tracing::warn!(
                        monitor = worker.monitor,
                        pid,
                        "壁纸被桌面图标层盖住，已退回系统壁纸；下次换图时再试"
                    );
                }
                EXIT_RESTART => {
                    worker.failures = 0;
                    worker.wait_shell = true;
                    worker.shell_deadline = Some(now + SHELL_READY_TIMEOUT);
                    worker.restart_at = Some(now + EXPLORER_RESTART_DELAY);
                    tracing::warn!(
                        monitor = worker.monitor,
                        pid,
                        delay_ms = EXPLORER_RESTART_DELAY.as_millis(),
                        timeout_s = SHELL_READY_TIMEOUT.as_secs(),
                        "桌面壳层已更换，等新壳层建好后重启宿主"
                    );
                }
                _ => {
                    if ran >= STABLE_AFTER {
                        worker.failures = 0;
                    }
                    worker.failures += 1;
                    let delay = backoff(worker.failures);
                    worker.restart_at = Some(now + delay);
                    tracing::warn!(
                        monitor = worker.monitor,
                        pid,
                        code,
                        secs = ran.as_secs(),
                        failures = worker.failures,
                        delay_ms = delay.as_millis(),
                        "wp-host 异常退出，退避重启"
                    );
                }
            }
        }

        // 正常退出 / 显示器移除的 worker 直接丢掉；保留了重启计划的留下。
        self.workers
            .retain(|w| w.child.is_some() || w.restart_at.is_some() || w.started.is_some());

        let due: Vec<usize> = self
            .workers
            .iter()
            .filter(|w| w.child.is_none() && w.restart_at.is_some_and(|t| t <= now))
            // Explorer 重启路径要等新壳层建好；超时则不再等。
            .filter(|w| {
                !w.wait_shell
                    || w.shell_deadline.is_some_and(|d| now >= d)
                    || wp_platform::shell_ready()
            })
            .map(|w| w.monitor)
            .collect();
        for monitor in due {
            if let Err(e) = self.restart_worker(monitor) {
                tracing::error!(monitor, "重启宿主失败: {e:#}");
                // 拉起失败也要退避，否则 25 ms 一次死循环刷日志。
                if let Some(worker) = self.worker_mut(monitor) {
                    worker.failures += 1;
                    worker.restart_at = Some(now + backoff(worker.failures));
                }
            }
        }
    }

    fn worker_mut(&mut self, monitor: usize) -> Option<&mut Worker> {
        self.workers.iter_mut().find(|w| w.monitor == monitor)
    }

    fn spawn_worker(&mut self, monitor: usize, image: Option<PathBuf>) -> Result<()> {
        if self.workers.iter().any(|w| w.monitor == monitor) {
            if let Some(worker) = self.worker_mut(monitor) {
                if image.is_some() {
                    worker.image = image;
                }
            }
            return Ok(());
        }

        let monitors = wp_platform::enumerate_monitors();
        let info = monitors.get(monitor).with_context(|| {
            format!(
                "显示器索引 {monitor} 不存在（当前共 {} 台）",
                monitors.len()
            )
        })?;
        self.start(monitor, info, image)
    }

    fn start(&mut self, monitor: usize, info: &MonitorInfo, image: Option<PathBuf>) -> Result<()> {
        let child = spawn_host(&self.args, &self.host_exe, monitor, image.as_deref())?;
        let pid = child.id();
        tracing::info!(
            monitor,
            pid,
            device = %info.device,
            image = image.as_ref().map(|p| p.display().to_string()),
            "已拉起 wp-host"
        );
        self.workers.push(Worker {
            monitor,
            device: info.device.clone(),
            rect: info.rect,
            image,
            child: Some(child),
            started: Some(Instant::now()),
            restart_at: None,
            failures: 0,
            wait_shell: false,
            shell_deadline: None,
        });
        Ok(())
    }

    fn restart_worker(&mut self, monitor: usize) -> Result<()> {
        let Some(worker) = self.worker_mut(monitor) else {
            return Ok(());
        };
        let image = worker.image.clone();
        let child = spawn_host(&self.args, &self.host_exe, monitor, image.as_deref())?;
        let pid = child.id();
        if let Some(worker) = self.worker_mut(monitor) {
            worker.child = Some(child);
            worker.started = Some(Instant::now());
            worker.restart_at = None;
            worker.wait_shell = false;
            worker.shell_deadline = None;
        }
        tracing::info!(monitor, pid, "wp-host 已重启");
        Ok(())
    }

    /// 控制命令：把指定显示器换成这张图。
    fn load_file(&mut self, monitor: usize, path: PathBuf) -> SupEvent {
        let monitors = wp_platform::enumerate_monitors();
        let Some(info) = monitors.get(monitor).cloned() else {
            return SupEvent::Error {
                monitor: Some(monitor as u32),
                code: ErrorCode::NotFound,
                message: format!(
                    "显示器索引 {monitor} 不存在（当前共 {} 台）",
                    monitors.len()
                ),
            };
        };

        // 只有当前没有宿主时才把新路径写进 worker：运行中的宿主换图失败要保留旧图，
        // 否则一次坏图会让之后每次崩溃重启都去加载那个坏文件。
        let exists = self.workers.iter().any(|w| w.monitor == monitor);
        if exists {
            let worker = self.worker_mut(monitor).expect("刚确认存在");
            // 热插拔后几何可能变了，顺手更新，停止时按新矩形找窗口。
            worker.rect = info.rect;
            worker.device = info.device.clone();
            // 正在等重启的（例如 Explorer 刚重启过）现在就拉起来，别让客户端干等。
            // 但新壳层还没建好时不抢跑：那时候接管会得到不透明的图标层。
            if worker.child.is_none() {
                if worker.wait_shell {
                    let ready = worker.shell_deadline.is_some_and(|d| Instant::now() >= d)
                        || wp_platform::shell_ready();
                    if !ready {
                        return SupEvent::Error {
                            monitor: Some(monitor as u32),
                            code: ErrorCode::Internal,
                            message: "桌面壳层正在重建，稍后重试".into(),
                        };
                    }
                }
                worker.image = Some(path.clone());
                worker.restart_at = None;
                if let Err(e) = self.restart_worker(monitor) {
                    return SupEvent::Error {
                        monitor: Some(monitor as u32),
                        code: ErrorCode::Internal,
                        message: format!("重启宿主失败: {e:#}"),
                    };
                }
            }
        } else if let Err(e) = self.spawn_worker(monitor, Some(path.clone())) {
            return SupEvent::Error {
                monitor: Some(monitor as u32),
                code: ErrorCode::NotFound,
                message: format!("拉起宿主失败: {e:#}"),
            };
        }

        let pipe = wp_ipc::pipe_name(self.session, monitor as u32);
        let deadline = Instant::now() + HOST_READY_TIMEOUT;
        let mut delay = Duration::from_millis(20);

        loop {
            // 宿主可能在启动期就退出了（图片坏了、D3D 不可用）。
            if let Some((code, ran)) = self.take_exit(monitor) {
                let tail = log_tail(monitor, 15);
                tracing::warn!(monitor, code, secs = ran.as_secs(), "宿主在换图前退出");
                match code {
                    EXIT_RESTART => {
                        // 壳层刚换过，重启计划已经排好；客户端重试一次即可。
                        return SupEvent::Error {
                            monitor: Some(monitor as u32),
                            code: ErrorCode::Internal,
                            message: "桌面壳层刚更换，宿主正在自动重启，请重试".into(),
                        };
                    }
                    EXIT_COVERED => {
                        self.drop_worker(monitor);
                        return SupEvent::Error {
                            monitor: Some(monitor as u32),
                            code: ErrorCode::Internal,
                            message:
                                "壁纸被桌面图标层盖住（Explorer 重启后的已知问题），已退回系统壁纸"
                                    .into(),
                        };
                    }
                    _ => {
                        // 明确由本次命令引发的失败：不自动重试坏图，免得无限重启。
                        self.drop_worker(monitor);
                        return SupEvent::Error {
                            monitor: Some(monitor as u32),
                            code: ErrorCode::Decode,
                            message: format!("wp-host 启动失败（退出码 {code}）：\n{tail}"),
                        };
                    }
                }
            }

            match wp_ipc::pipe::connect(&pipe, Duration::from_millis(250)) {
                Ok(mut conn) => {
                    return match forward_load(&mut conn, &path) {
                        Ok((width, height)) => {
                            // 成功才更新「重启时用哪张图」。
                            if let Some(worker) = self.worker_mut(monitor) {
                                worker.image = Some(path.clone());
                            }
                            SupEvent::Loaded {
                                monitor: monitor as u32,
                                path: path.display().to_string(),
                                width,
                                height,
                            }
                        }
                        Err(e) => SupEvent::Error {
                            monitor: Some(monitor as u32),
                            code: ErrorCode::Internal,
                            message: format!("{e:#}"),
                        },
                    };
                }
                Err(wp_ipc::pipe::PipeError::NotRunning) => {
                    if Instant::now() >= deadline {
                        self.drop_worker(monitor);
                        return SupEvent::Error {
                            monitor: Some(monitor as u32),
                            code: ErrorCode::Internal,
                            message: format!(
                                "{} 秒内没有等到宿主管道 {pipe}；日志：\n{}",
                                HOST_READY_TIMEOUT.as_secs(),
                                log_tail(monitor, 10)
                            ),
                        };
                    }
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_millis(200));
                }
                Err(e) => {
                    return SupEvent::Error {
                        monitor: Some(monitor as u32),
                        code: ErrorCode::Internal,
                        message: format!("连接宿主管道 {pipe} 失败: {e}"),
                    };
                }
            }
        }
    }

    /// 如果该屏的宿主已经退出，取走退出码并更新状态；没退出返回 `None`。
    fn take_exit(&mut self, monitor: usize) -> Option<(i32, Duration)> {
        let worker = self.worker_mut(monitor)?;
        let (code, ran) = {
            let child = worker.child.as_mut()?;
            let status = child.try_wait().ok()??;
            (status.code().unwrap_or(-1), worker.ran_for(Instant::now()))
        };
        worker.child = None;
        worker.started = None;
        // 命令路径上的退出不安排自动重启：坏图重试只会白烧 CPU。
        worker.restart_at = None;
        worker.wait_shell = false;
        worker.shell_deadline = None;
        if code == EXIT_RESTART {
            // 壳层刚换过，重启计划已经排好，但要等新壳层建好再拉起。
            worker.wait_shell = true;
            worker.shell_deadline = Some(Instant::now() + SHELL_READY_TIMEOUT);
            worker.restart_at = Some(Instant::now() + EXPLORER_RESTART_DELAY);
        }
        Some((code, ran))
    }

    fn drop_worker(&mut self, monitor: usize) {
        self.workers.retain(|w| w.monitor != monitor);
    }

    /// 优雅停止所有宿主，返回成功停掉的块数。
    fn shutdown_all(&mut self) -> usize {
        let mut stopped = 0;
        for worker in &mut self.workers {
            let Some(mut child) = worker.child.take() else {
                continue;
            };
            let pid = child.id();
            // 第一步：管道请求退出。失败不影响下一步。
            let pipe = wp_ipc::pipe_name(self.session, worker.monitor as u32);
            if let Ok(mut conn) = wp_ipc::pipe::connect(&pipe, Duration::from_secs(1)) {
                let _ = wp_ipc::write_frame(&mut conn, &wp_ipc::Command::Shutdown);
            } else {
                tracing::warn!(monitor = worker.monitor, "宿主管道不可用，改用 WM_CLOSE");
            }

            if wait_exit(&mut child, Duration::from_secs(6)) {
                stopped += 1;
                tracing::info!(monitor = worker.monitor, pid, "wp-host 已优雅退出");
                continue;
            }

            // 第二步：发 WM_CLOSE。宿主会走 detach → 销毁窗口的同一条路。
            let count = wp_platform::close_host_windows_on(worker.rect);
            tracing::warn!(
                monitor = worker.monitor,
                pid,
                windows = count,
                "宿主未响应管道退出，已发送 WM_CLOSE"
            );
            if wait_exit(&mut child, Duration::from_secs(4)) {
                stopped += 1;
                tracing::info!(
                    monitor = worker.monitor,
                    pid,
                    "wp-host 收到 WM_CLOSE 后退出"
                );
            } else {
                // 硬杀会毁掉桌面图标层，只能留着并让用户处理。
                tracing::error!(
                    monitor = worker.monitor,
                    pid,
                    "wp-host 10 秒内没有退出；未强制结束（会破坏桌面），请查看日志"
                );
                worker.child = Some(child);
            }
        }
        self.workers.retain(|w| w.child.is_some());
        stopped
    }
}

fn wait_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                tracing::warn!("等待宿主退出失败: {e}");
                return false;
            }
        }
    }
    false
}

fn forward_load(conn: &mut std::fs::File, path: &Path) -> Result<(u32, u32)> {
    wp_ipc::write_frame(
        conn,
        &wp_ipc::Command::LoadFile {
            path: path.to_path_buf(),
        },
    )
    .context("发送换图命令失败")?;
    let event: wp_ipc::Event = wp_ipc::read_frame(conn).context("读取宿主回应失败")?;
    match event {
        wp_ipc::Event::Loaded { width, height, .. } => Ok((width, height)),
        wp_ipc::Event::Error { code, message } => bail!("宿主拒绝换图（{code:?}）: {message}"),
        other => bail!("宿主返回了意料之外的事件: {other:?}"),
    }
}

/// 1 次失败 0.5 s，之后翻倍，封顶 30 s。
fn backoff(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(6);
    let millis = 500u64.saturating_mul(1u64 << shift).min(30_000);
    Duration::from_millis(millis)
}

struct ControlRequest {
    command: SupCommand,
    reply: Sender<SupEvent>,
}

/// 控制管道线程：只做收发，命令交给主循环——worker 状态只允许一个线程改。
fn spawn_control(name: String, tx: Sender<ControlRequest>) {
    let spawned = std::thread::Builder::new()
        .name("wp-supervisor-ipc".into())
        .spawn(move || {
            let server = wp_ipc::pipe::PipeServer::bind(&name);
            loop {
                let mut conn = match server.accept() {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!("控制管道接受连接失败: {e}");
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                };
                let event = match wp_ipc::read_frame::<_, SupCommand>(&mut conn) {
                    Ok(command) => {
                        let (reply, rx) = mpsc::channel();
                        if tx.send(ControlRequest { command, reply }).is_err() {
                            return;
                        }
                        match rx.recv_timeout(CONTROL_REPLY_TIMEOUT) {
                            Ok(event) => event,
                            Err(_) => SupEvent::Error {
                                monitor: None,
                                code: ErrorCode::Internal,
                                message: "supervisor 处理命令超时".into(),
                            },
                        }
                    }
                    Err(e) => SupEvent::Error {
                        monitor: None,
                        code: ErrorCode::Internal,
                        message: format!("读取控制命令失败: {e}"),
                    },
                };
                if let Err(e) = wp_ipc::write_frame(&mut conn, &event) {
                    tracing::debug!("控制连接回写失败: {e}");
                }
            }
        });
    if let Err(e) = spawned {
        tracing::error!("控制管道线程启动失败: {e}");
    }
}

unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            STOP.store(true, Ordering::Relaxed);
            BOOL(1)
        }
        _ => BOOL(0),
    }
}

fn install_console_handler() {
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_millis(1000));
        assert_eq!(backoff(3), Duration::from_millis(2000));
        assert_eq!(backoff(7), Duration::from_millis(30_000));
        assert_eq!(backoff(100), Duration::from_millis(30_000));
    }

    #[test]
    fn exit_codes_are_distinct() {
        assert_eq!(EXIT_RESTART, 75);
        assert_eq!(EXIT_MONITOR_GONE, 76);
        assert_eq!(EXIT_COVERED, 77);
        assert_ne!(EXIT_RESTART, EXIT_MONITOR_GONE);
        assert_ne!(EXIT_COVERED, EXIT_RESTART);
    }
}
