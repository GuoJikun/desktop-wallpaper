//! wpctl：不开 UI 也能探测桌面结构、跑挂载矩阵、截图验证、查看状态。

use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use wp_core::PhysicalRect;
use wp_platform::{BandState, Strategy};

#[derive(Parser, Debug)]
#[command(name = "wpctl", version, about = "wallpaper 调试与管理工具")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 探测桌面窗口结构，可选跑挂载策略矩阵
    Probe {
        /// 以 JSON 输出（便于脚本断言）
        #[arg(long)]
        json: bool,
        /// 同时写入报告文件
        #[arg(long)]
        report: Option<PathBuf>,
        /// 额外跑挂载测试：`all` 或单条策略名
        /// （progman_child / toplevel_bottom / toplevel_anchored）
        #[arg(long)]
        strategy: Option<String>,
        /// 测试内容：`gdi` 画纯色，`none` 只看层级
        #[arg(long, default_value = "gdi")]
        content: String,
        /// 每条策略运行秒数
        #[arg(long, default_value_t = 30)]
        duration: u64,
        /// 挂到第几台显示器（0 起）
        #[arg(long, default_value_t = 0)]
        monitor: usize,
        /// 运行中截屏到 BMP，并统计像素用于判断可见性
        #[arg(long)]
        shot: Option<PathBuf>,
        /// 测试前最小化其它窗口，测试后恢复
        #[arg(long)]
        minimize: bool,
        /// 截图后模拟桌面右键，验证壁纸没吃掉桌面输入
        #[arg(long)]
        input_test: bool,
        /// 每次看门狗检查都打印窗口状态，用于定位层级问题
        #[arg(long)]
        verbose: bool,
        /// 诊断：截图后再把图标层还给 Progman 截一张，区分「没渲染」和「被盖住」
        #[arg(long)]
        detach_shot: bool,
        /// 诊断：截图后把图标层缩到左上角 400×300 再截一张
        #[arg(long)]
        shrink_shot: bool,
        /// 截图后按 Win+D，等 3 秒再截一张，验证桌面切换后壁纸是否还在
        #[arg(long)]
        win_d_shot: bool,
    },
    /// 截取整屏到 BMP，不做其它事
    Shot {
        /// 输出文件
        #[arg(long, default_value = "out/shot.bmp")]
        out: PathBuf,
        /// 显示器索引
        #[arg(long, default_value_t = 0)]
        monitor: usize,
        /// 截图前最小化其它窗口，截完恢复（把桌面亮出来）
        #[arg(long)]
        minimize: bool,
    },
    /// 把图片壁纸挂到指定显示器（拉起一个 wp-host 子进程）
    Attach {
        /// 显示器索引（0 起）
        #[arg(long, default_value_t = 0)]
        monitor: usize,
        /// 媒体文件路径（M1 只支持 PNG / JPEG / BMP）
        #[arg(long)]
        source: PathBuf,
        /// 桌面宿主策略
        #[arg(long, default_value = "defview_adopt")]
        strategy: String,
        /// 运行秒数，0 表示一直跑到 `wpctl stop`
        #[arg(long, default_value_t = 0)]
        duration: u64,
        /// 调试用：不因前台全屏 / 最大化窗口暂停渲染
        #[arg(long)]
        no_pause: bool,
        /// 拉起后立即返回，不确认宿主窗口是否出来
        #[arg(long)]
        no_wait: bool,
    },
    /// 优雅停止所有壁纸渲染进程（发 WM_CLOSE，不硬杀）
    Stop,
    /// 查看暂停判定结果（全屏应用 / 最大化遮挡 / 电池 / 远程会话）
    Pause {
        /// 以 JSON 输出
        #[arg(long)]
        json: bool,
        /// 显示器索引
        #[arg(long, default_value_t = 0)]
        monitor: usize,
        /// 连续采样 N 秒（每秒一行），0 表示只采一次
        #[arg(long, default_value_t = 0)]
        watch: u64,
        /// 自测：临时创建一个铺满屏幕的前台窗口，验证遮挡判定
        #[arg(long)]
        self_test: bool,
        /// 自测窗口停留毫秒数（给 wp-host 的 1 s 采样留出观察窗口）
        #[arg(long, default_value_t = 600)]
        hold: u64,
        /// 自测窗口尺寸 `宽x高`，默认铺满显示器；小于屏幕时可验证「不遮挡」分支
        #[arg(long)]
        size: Option<String>,
    },
    /// 最小化/还原循环压测：反复切换前台后核对层级（M0 验收第 3 条）
    Cycles {
        /// 循环次数
        #[arg(long, default_value_t = 20)]
        count: u32,
        /// 挂载策略，默认 26H2 主力策略
        #[arg(long, default_value = "defview_adopt")]
        strategy: String,
        /// 测试内容：`d3d` 画 DirectComposition，`gdi` 画纯色
        #[arg(long, default_value = "d3d")]
        content: String,
        /// 显示器索引
        #[arg(long, default_value_t = 0)]
        monitor: usize,
        /// 每次最小化 / 还原后的等待毫秒数
        #[arg(long, default_value_t = 800)]
        settle: u64,
        /// 最后一轮之后截图（可选），用于人工确认画面仍可见
        #[arg(long)]
        shot: Option<PathBuf>,
        /// 以 JSON 输出
        #[arg(long)]
        json: bool,
    },
    /// 查看渲染进程状态
    Stats,
    /// 在桌面上模拟右键，验证桌面输入是否正常（基线对照用）
    InputTest {
        /// 测试前最小化其它窗口，测试后恢复
        #[arg(long)]
        minimize: bool,
        /// 显示器索引
        #[arg(long, default_value_t = 0)]
        monitor: usize,
    },
}

fn main() -> Result<()> {
    // 上一次异常退出可能把图标层留在了别的窗口下，先修桌面再干别的。
    if let Some(healed) = wp_platform::heal_desktop() {
        eprintln!("[桌面自愈] {healed}");
    }
    // 上一次被强杀时收起的窗口也还回去（只认日志里对得上的句柄）。
    if let Some(restored) = wp_platform::restore_leaked_windows() {
        eprintln!("[窗口自愈] {restored}");
    }

    match Cli::parse().command {
        Cmd::Probe {
            json,
            report,
            strategy,
            content,
            duration,
            monitor,
            shot,
            minimize,
            input_test,
            verbose,
            detach_shot,
            shrink_shot,
            win_d_shot,
        } => probe(
            json,
            report,
            strategy,
            RunOpts {
                content: &content,
                duration,
                shot: shot.as_deref(),
                minimize,
                input_test,
                verbose,
                detach_shot,
                shrink_shot,
                win_d_shot,
            },
            monitor,
        ),
        Cmd::Shot {
            out,
            monitor,
            minimize,
        } => shot_once(&out, monitor, minimize),
        Cmd::InputTest { minimize, monitor } => {
            let monitors = wp_platform::enumerate_monitors();
            let rect = monitors
                .get(monitor)
                .with_context(|| {
                    format!("显示器索引 {monitor} 不存在（共 {} 台）", monitors.len())
                })?
                .rect;
            let result = test_input(rect, minimize);
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Cmd::Attach {
            monitor,
            source,
            strategy,
            duration,
            no_pause,
            no_wait,
        } => attach(monitor, &source, &strategy, duration, no_pause, no_wait),
        Cmd::Stop => stop_hosts(),
        Cmd::Pause {
            json,
            monitor,
            watch,
            self_test,
            hold,
            size,
        } => pause_cmd(json, monitor, watch, self_test, hold, size.as_deref()),
        Cmd::Cycles {
            count,
            strategy,
            content,
            monitor,
            settle,
            shot,
            json,
        } => run_cycles(
            count,
            &strategy,
            &content,
            monitor,
            settle,
            shot.as_deref(),
            json,
        ),
        Cmd::Stats => bail!("stats 尚未实现（M1）"),
    }
}

/// 一次挂载测试的参数。
struct RunOpts<'a> {
    content: &'a str,
    duration: u64,
    shot: Option<&'a Path>,
    minimize: bool,
    input_test: bool,
    verbose: bool,
    detach_shot: bool,
    shrink_shot: bool,
    win_d_shot: bool,
}

fn probe(
    json_out: bool,
    report_path: Option<PathBuf>,
    strategy: Option<String>,
    opts: RunOpts<'_>,
    monitor: usize,
) -> Result<()> {
    let monitors = wp_platform::enumerate_monitors();
    let desktop = wp_platform::desktop::probe();

    let matrix = match &strategy {
        Some(selection) => run_matrix(selection, &opts, monitor)?,
        None => Vec::new(),
    };

    let combined = json!({
        "desktop": desktop,
        "monitors": monitors.iter().map(|m| json!({
            "device": m.device,
            "rect": m.rect,
            "primary": m.primary,
        })).collect::<Vec<_>>(),
        "matrix": matrix,
    });

    if let Some(path) = &report_path {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, serde_json::to_string_pretty(&combined)?)
            .with_context(|| format!("写入 {} 失败", path.display()))?;
        eprintln!("报告已写入 {}", path.display());
    }

    if json_out {
        println!("{}", serde_json::to_string_pretty(&combined)?);
        return Ok(());
    }

    print_desktop(&desktop, &monitors);
    if !matrix.is_empty() {
        print_matrix(&matrix);
    }
    Ok(())
}

/// M1 支持的图片扩展名。URL / 壁纸包留到 M2。
const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "bmp"];

/// `wpctl attach`：拉起一个 `wp-host` 子进程把图片挂到指定显示器。
///
/// 选择「子进程」而不是进程内挂载，是因为渲染必须与 UI / 调试工具隔离：
/// 每块屏一个 `wp-host`，崩溃只影响那一块屏（AGENTS.md 硬规则 8）。
fn attach(
    monitor_idx: usize,
    source: &Path,
    strategy: &str,
    duration: u64,
    no_pause: bool,
    no_wait: bool,
) -> Result<()> {
    let monitors = wp_platform::enumerate_monitors();
    let monitor = monitors
        .get(monitor_idx)
        .with_context(|| {
            format!(
                "显示器索引 {monitor_idx} 不存在（共 {} 台）",
                monitors.len()
            )
        })?
        .clone();

    // 规范化成绝对路径：wp-host 的工作目录不一定和 wpctl 一样。
    let source = source
        .canonicalize()
        .with_context(|| format!("壁纸文件不存在: {}", source.display()))?;
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        bail!(
            "M1 只支持图片壁纸（{}），收到 .{ext}",
            IMAGE_EXTENSIONS.join(" / ")
        );
    }

    let exe = host_exe()?;
    let before = wp_platform::host_windows().len();
    if before > 0 {
        eprintln!("[attach] 已经有 {before} 个宿主窗口在运行，先挂的会被新窗口盖住");
    }

    // 日志落文件而不是继承控制台：wp-host 是常驻进程，继承了控制台的管道会让
    // 调用方一直等 EOF 才能返回；文件还能在出问题时回看。
    let log = host_log_path(monitor_idx);
    // 共享读写：宿主在跑的时候也能 tail 日志。
    let log_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0x7)
        .open(&log)
        .with_context(|| format!("创建日志 {} 失败", log.display()))?;
    let log_err = log_file.try_clone()?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--monitor")
        .arg(monitor_idx.to_string())
        .arg("--strategy")
        .arg(strategy)
        .arg("--image")
        .arg(&source)
        .arg("--duration")
        .arg(duration.to_string());
    if no_pause {
        cmd.arg("--no-pause");
    }

    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("启动 {} 失败", exe.display()))?;
    let pid = child.id();

    if no_wait {
        println!("已拉起 wp-host（pid {pid}），日志 {}", log.display());
        return Ok(());
    }

    // 等宿主窗口真的出现：wp-host 启动失败（图片损坏、D3D 不可用）时
    // 会立刻退出，这里要把退出码和日志尾部报给用户，而不是假装成功。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            let tail = log_tail(&log, 12);
            bail!(
                "wp-host 启动后立即退出（{status}），日志 {}：\n{tail}",
                log.display()
            );
        }
        if wp_platform::host_windows().len() > before {
            // 窗口出现只是第一步：解码失败或表面初始化失败会让进程在窗口出来之后
            // 立刻退出。稍等一拍再确认它还活着，避免报「成功」之后图标层没人管。
            std::thread::sleep(Duration::from_millis(400));
            if let Some(status) = child.try_wait()? {
                let tail = log_tail(&log, 12);
                bail!(
                    "wp-host 起来后又退出（{status}），日志 {}：\n{tail}",
                    log.display()
                );
            }
            println!(
                "已挂载 {} → 显示器 [{monitor_idx}] {} {}x{}，wp-host pid {pid}",
                source.display(),
                monitor.device,
                monitor.rect.width,
                monitor.rect.height
            );
            println!("日志 {}；用 `wpctl stop` 优雅停止", log.display());
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "5 秒内没看到宿主窗口；wp-host（pid {pid}）仍在运行，日志 {}",
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 每个显示器一份日志，重复 attach 会覆盖（只留最近一次）。
fn host_log_path(monitor_idx: usize) -> PathBuf {
    std::env::temp_dir().join(format!("wallpaper-host-{monitor_idx}.log"))
}

/// 取日志最后几行，只在启动失败时用来解释原因。
fn log_tail(path: &Path, lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return "(日志读不出来)".into();
    };
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = all.len().saturating_sub(lines);
    if start == all.len() {
        return "(日志为空)".into();
    }
    all[start..].join("\n")
}

/// `wpctl stop`：给所有宿主窗口发 `WM_CLOSE`，等它们自己把桌面还回去。
fn stop_hosts() -> Result<()> {
    let count = wp_platform::close_host_windows();
    if count == 0 {
        println!("没有正在运行的壁纸宿主");
        return Ok(());
    }
    println!("已向 {count} 个宿主窗口发送 WM_CLOSE，等待退出…");

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let left = wp_platform::host_windows().len();
        if left == 0 {
            println!("全部退出，桌面已还原");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let left = wp_platform::host_windows().len();
    bail!("{left} 个宿主窗口 5 秒内没有退出；请查看 wp-host 日志，不要用 Stop-Process 硬杀");
}

/// `wp-host.exe` 与本工具同目录（都是 cargo 产物）。找不到时给出可操作的提示。
fn host_exe() -> Result<PathBuf> {
    let me = std::env::current_exe().context("无法定位 wpctl 自身路径")?;
    let dir = me.parent().context("无法定位 wpctl 所在目录")?;
    let exe = dir.join("wp-host.exe");
    if !exe.is_file() {
        bail!("找不到 {}，先运行 `cargo build -p wp-host`", exe.display());
    }
    Ok(exe)
}

fn shot_once(out: &Path, monitor_idx: usize, minimize: bool) -> Result<()> {
    let monitors = wp_platform::enumerate_monitors();
    let monitor = monitors.get(monitor_idx).with_context(|| {
        format!(
            "显示器索引 {monitor_idx} 不存在（共 {} 台）",
            monitors.len()
        )
    })?;

    // 只最小化，不隐藏：隐藏的窗口会从任务栏 / Alt+Tab 消失，进程一死就回不来。
    let guard = RestoreGuard::new(if minimize {
        let handles = wp_platform::minimize_other_windows();
        std::thread::sleep(Duration::from_millis(500));
        handles
    } else {
        Vec::new()
    });

    let image = wp_platform::capture(monitor.rect)?;
    wp_platform::save_bmp(out, &image).with_context(|| format!("保存 {} 失败", out.display()))?;

    let ratio = image.ratio_of(wp_platform::TEST_RGB);
    let mean = image.mean_rgb();
    let non_black = image.non_black_ratio();
    guard.release();

    println!(
        "已截屏 {}（{}x{}），测试色占比 {:.2}%，非黑占比 {:.1}%，平均 RGB({}, {}, {})",
        out.display(),
        image.width,
        image.height,
        ratio * 100.0,
        non_black * 100.0,
        mean[0],
        mean[1],
        mean[2]
    );
    Ok(())
}

fn print_desktop(desktop: &wp_platform::ProbeReport, monitors: &[wp_platform::MonitorInfo]) {
    println!("== 显示器 ==");
    for (i, m) in monitors.iter().enumerate() {
        println!(
            "  [{i}] {:<16} {}x{}{}  ({},{})",
            m.device,
            m.rect.width,
            m.rect.height,
            if m.primary { " (主)" } else { "" },
            m.rect.x,
            m.rect.y
        );
    }

    println!("\n== 桌面窗口 ==");
    println!(
        "  Progman             : {}",
        desktop.progman.as_deref().unwrap_or("未找到")
    );
    if let Some(t) = &desktop.progman_title {
        println!("  Progman 标题        : {t}");
    }
    println!(
        "  DefView 在 Progman 下: {}",
        if desktop.defview_under_progman {
            "是"
        } else {
            "否"
        }
    );
    println!("  WorkerW 数量        : {}", desktop.workerw.len());
    for w in &desktop.workerw {
        println!(
            "    - {} visible={} children={} defview={}",
            w.hwnd, w.visible, w.child_count, w.has_defview
        );
    }
    println!(
        "  含 DefView 的 WorkerW: {}",
        desktop.workerw_with_defview.as_deref().unwrap_or("无")
    );
    match desktop.spawn_worker_result {
        Some(code) => println!("  0x052C (wParam=0)   : {code}"),
        None => println!("  0x052C (wParam=0)   : 未发送（无 Progman）"),
    }
    if let Some(code) = desktop.spawn_worker_result_alt {
        println!("  0x052C (wParam=0xD) : {code}");
    }
    println!(
        "  发送后出现宿主       : {}",
        desktop.host_after_spawn.as_deref().unwrap_or("无")
    );

    println!("\n== 推荐策略顺序 ==");
    for (i, s) in desktop.recommended.iter().enumerate() {
        println!("  {}. {s}", i + 1);
    }
}

fn print_matrix(matrix: &[Value]) {
    println!("\n== 挂载矩阵 ==");
    for run in matrix {
        let name = run["strategy"].as_str().unwrap_or("?");
        if let Some(skip) = run["skipped"].as_str() {
            println!("  {name:<18} 跳过: {skip}");
            continue;
        }
        println!(
            "  {name:<18} attach={} 检查={} 修正={} 丢失={} 终态={}",
            if run["attached"].as_bool().unwrap_or(false) {
                "ok"
            } else {
                "fail"
            },
            run["checks"].as_u64().unwrap_or(0),
            run["corrected"].as_u64().unwrap_or(0),
            run["lost"].as_u64().unwrap_or(0),
            run["final_band"].as_str().unwrap_or("-"),
        );
        if let Some(err) = run["attach_error"].as_str() {
            println!("  {:<18} attach 错误: {err}", "");
        }
        if run["shot"].is_object() {
            let shot = &run["shot"];
            println!(
                "  {:<18} 截图: {} 测试色占比={:.1}% 图标区异色像素={} 点(60,60)命中={} 屏幕中心命中={}",
                "",
                shot["path"].as_str().unwrap_or("-"),
                shot["wallpaper_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                shot["icons_non_matching"].as_u64().unwrap_or(0),
                shot["point_60_class"].as_str().unwrap_or("-"),
                shot["center_class"].as_str().unwrap_or("-"),
            );
        }
        if run["win_d_shot"].is_object() {
            let shot = &run["win_d_shot"];
            println!(
                "  {:<18} Win+D 后截图: {} 测试色占比={:.1}% 图标区异色像素={} 屏幕中心命中={}",
                "",
                shot["path"].as_str().unwrap_or("-"),
                shot["wallpaper_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                shot["icons_non_matching"].as_u64().unwrap_or(0),
                shot["center_class"].as_str().unwrap_or("-"),
            );
        }
    }
}

fn run_matrix(selection: &str, opts: &RunOpts<'_>, monitor_idx: usize) -> Result<Vec<Value>> {
    let monitors = wp_platform::enumerate_monitors();
    let monitor = monitors
        .get(monitor_idx)
        .with_context(|| {
            format!(
                "显示器索引 {monitor_idx} 不存在（共 {} 台）",
                monitors.len()
            )
        })?
        .clone();

    let strategies: Vec<Strategy> = if selection == "all" {
        Strategy::ALL.to_vec()
    } else {
        vec![selection
            .parse::<Strategy>()
            .map_err(|e| anyhow::anyhow!("{e}"))?]
    };

    let mut out = Vec::new();
    for strategy in strategies {
        out.push(run_one(strategy, &monitor, opts)?);
    }
    Ok(out)
}

fn run_one(
    strategy: Strategy,
    monitor: &wp_platform::MonitorInfo,
    opts: &RunOpts<'_>,
) -> Result<Value> {
    let name = strategy.as_str();
    let content = opts.content;
    let duration = opts.duration;

    let mut host = match strategy.open() {
        Ok(h) => h,
        Err(e) => {
            return Ok(json!({
                "strategy": name,
                "skipped": e.to_string(),
            }))
        }
    };

    // 先把桌面露出来，避免截图被别的窗口糊住。只最小化，绝不隐藏（见 restore 相关说明）。
    let minimize_guard = RestoreGuard::new(if opts.minimize {
        let handles = wp_platform::minimize_other_windows();
        std::thread::sleep(Duration::from_millis(400));
        handles
    } else {
        Vec::new()
    });

    let spec = match strategy {
        Strategy::DefViewChild => {
            let parent =
                wp_platform::defview_handle().context("找不到图标层，策略 F 无法创建子窗口")?;
            wp_platform::HostWindowSpec::child_of(parent, monitor.rect)
        }
        _ => wp_platform::HostWindowSpec::for_monitor(monitor.rect),
    }
    .with_noredirection_bitmap(content == "d3d");
    let hwnd = wp_platform::create_host_window(&spec)
        .with_context(|| format!("创建宿主窗口失败（{name}）"))?;

    eprintln!("[{name}] 窗口已创建，开始 {duration}s 挂载测试（内容={content}）");

    let attach_error = host.attach(hwnd, monitor.rect).err().map(|e| e.to_string());
    let attached = attach_error.is_none();
    if let Some(err) = &attach_error {
        eprintln!("[{name}] attach 失败: {err}");
    } else {
        eprintln!(
            "[{name}] attach 后窗口状态: {}",
            wp_platform::describe(hwnd)
        );
    }

    // D3D 内容必须在 attach 之后创建：策略 A 会重新 SetParent，
    // 提前建的 DComp target 会挂在旧父窗口上。
    let topmost = !matches!(strategy, Strategy::DefViewAdopt | Strategy::DefViewChild);
    let mut surface = if content == "d3d" && attached {
        // 策略 E 里 DefView 是我们的子窗口，visual 必须在子窗口之下。
        make_surface(name, hwnd, monitor.rect, topmost)
    } else {
        None
    };

    let start = Instant::now();
    let mut next_check = start;
    let mut next_paint = start;
    let mut shot_at = opts
        .shot
        .map(|_| start + Duration::from_secs(duration.max(2) / 2));
    let mut shot_result: Option<Value> = None;
    let mut input_result: Option<Value> = None;
    let mut detach_shot_result: Option<Value> = None;
    let mut shrink_shot_result: Option<Value> = None;
    let mut win_d_shot_result: Option<Value> = None;
    let mut checks = 0u64;
    let mut corrected = 0u64;
    let mut lost = 0u64;
    let mut final_band: Option<BandState> = None;
    let mut needs_restart = false;

    while start.elapsed() < Duration::from_secs(duration) {
        wp_platform::pump_messages();
        let now = Instant::now();

        if attached && now >= next_check {
            match host.ensure_band(hwnd) {
                Ok(state) => {
                    checks += 1;
                    match state {
                        BandState::Correct => {}
                        BandState::Corrected => {
                            corrected += 1;
                            eprintln!("[{name}] z 序漂移，已修正");
                        }
                        BandState::Lost => {
                            lost += 1;
                            eprintln!("[{name}] z 序无法恢复（Lost）");
                        }
                    }
                    final_band = Some(state);
                    // Explorer 重启后新的图标层是不透明的，沿用当前进程无论重建窗口还是等多久都黑屏
                    // （实测 0%；杀掉进程再起一次立刻恢复 94.6%）。
                    // 所以在进程内没有可行解法——请求上层重启 wp-host，这里是唯一正确的出口。
                    if state == BandState::Lost {
                        eprintln!("[{name}] 桌面壳层已更换，本进程无法自愈，需要重启渲染进程");
                        needs_restart = true;
                        break;
                    }
                    if opts.verbose {
                        eprintln!(
                            "[{name}] #{checks} state={} host_z={:?} progman_z={:?} {}\n           {}",
                            band_name(state),
                            wp_platform::z_index_of(hwnd),
                            wp_platform::progman_z(),
                            wp_platform::describe(hwnd),
                            wp_platform::defview_state()
                        );
                    }
                }
                Err(e) => {
                    lost += 1;
                    eprintln!("[{name}] ensure_band 出错: {e}");
                }
            }
            next_check = now + Duration::from_secs(2);
        }

        if content == "gdi" && now >= next_paint {
            if let Err(e) = wp_platform::paint_solid(hwnd, wp_platform::TEST_COLOR) {
                eprintln!("[{name}] 绘制失败: {e}");
            }
            next_paint = now + Duration::from_millis(500);
        }

        if let Some(surface) = surface.as_mut() {
            if now >= next_paint {
                if let Err(e) = surface.clear(wp_platform::TEST_RGB) {
                    eprintln!("[{name}] DComp 绘制失败: {e}");
                }
                next_paint = now + Duration::from_millis(16);
            }
        }

        if let (Some(path), Some(at)) = (opts.shot, shot_at) {
            if now >= at {
                shot_result = Some(take_shot(name, path, monitor.rect, opts.minimize)?);
                shot_at = None;
            }
        }

        if opts.input_test && shot_result.is_some() && input_result.is_none() {
            input_result = Some(test_input(monitor.rect, opts.minimize));
        }

        // 诊断：把图标层还给 Progman 后再截一张，用来区分
        // 「我们的窗口没渲染」和「图标层盖住了我们的画面」。
        if opts.detach_shot && shot_result.is_some() && detach_shot_result.is_none() {
            let _ = host.detach(hwnd);
            std::thread::sleep(Duration::from_millis(600));
            let path = PathBuf::from(format!("{}.detached.bmp", opts.shot.unwrap().display()));
            detach_shot_result = Some(take_shot(name, &path, monitor.rect, opts.minimize)?);
            let _ = host.attach(hwnd, monitor.rect);
        }

        // 诊断：把图标层缩到角落。若屏幕其余部分露出壁纸，说明是图标层不透明盖住了我们；
        // 若整屏仍然没有壁纸，说明宿主窗口本身没合成。
        if opts.shrink_shot && shot_result.is_some() && shrink_shot_result.is_none() {
            if wp_platform::resize_defview(0, 0, 400, 300) {
                std::thread::sleep(Duration::from_millis(600));
                let path = PathBuf::from(format!("{}.shrunk.bmp", opts.shot.unwrap().display()));
                shrink_shot_result = Some(take_shot(name, &path, monitor.rect, opts.minimize)?);
                wp_platform::resize_defview(
                    0,
                    0,
                    monitor.rect.width as i32,
                    monitor.rect.height as i32,
                );
            } else {
                eprintln!("[{name}] 缩窗诊断失败：没找到图标层");
            }
        }

        // 诊断：按 Win+D 显示桌面，验证「桌面切换后 3 秒内壁纸仍在、看门狗无需重启」。
        // 等待期间保持消息泵和看门狗运行，否则测的是「没人管」的状态。
        if opts.win_d_shot && shot_result.is_some() && win_d_shot_result.is_none() {
            let explorer_before = wp_platform::desktop_pid();
            wp_platform::press_win_d();
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                wp_platform::pump_messages();
                if attached {
                    let _ = host.ensure_band(hwnd);
                }
                std::thread::sleep(Duration::from_millis(16));
            }
            let path = PathBuf::from(format!("{}.win_d.bmp", opts.shot.unwrap().display()));
            if opts.verbose {
                eprintln!(
                    "[{name}] Win+D 后：minimized={} host={} explorer={:?}→{:?}\n           {}\n           {}",
                    wp_platform::is_minimized(hwnd),
                    wp_platform::describe(hwnd),
                    explorer_before,
                    wp_platform::desktop_pid(),
                    wp_platform::defview_state(),
                    z_band_report(hwnd)
                );
            }
            win_d_shot_result = Some(take_shot(name, &path, monitor.rect, opts.minimize)?);
            if opts.verbose {
                eprintln!("[{name}] Win+D 截图后 z 链：{}", z_band_report(hwnd));
            }
            // 再按一次恢复窗口，避免后续检查和收尾跑在「显示桌面」状态里。
            wp_platform::press_win_d();
            std::thread::sleep(Duration::from_millis(500));
            next_check = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(16));
    }

    let handle_alive = wp_platform::is_host_window_alive(hwnd);
    drop(surface);
    // 一定要先 detach 再销毁窗口：策略 E 会把 DefView 挂成子窗口，
    // 直接 DestroyWindow 会把图标层一起带走。
    if attached {
        if let Err(e) = host.detach(hwnd) {
            eprintln!("[{name}] detach 失败: {e}");
        }
    }
    wp_platform::destroy_host_window(hwnd);
    minimize_guard.release();

    eprintln!(
        "[{name}] 结束：检查 {checks} 次，修正 {corrected} 次，丢失 {lost} 次，终态 {}",
        final_band.map(band_name).unwrap_or("-")
    );

    Ok(json!({
        "strategy": name,
        "content": content,
        "duration_s": duration,
        "attached": attached,
        "attach_error": attach_error,
        "checks": checks,
        "corrected": corrected,
        "lost": lost,
        "final_band": final_band.map(band_name),
        "needs_restart": needs_restart,
        "handle_alive": handle_alive,
        "shot": shot_result,
        "detach_shot": detach_shot_result,
        "shrink_shot": shrink_shot_result,
        "win_d_shot": win_d_shot_result,
        "input": input_result,
    }))
}

/// 每次 `ensure_band` 的累计统计。
#[derive(Default)]
struct CycleStats {
    checks: u64,
    corrected: u64,
    lost: u64,
    needs_restart: bool,
    worst: Option<BandState>,
}

impl CycleStats {
    fn record(&mut self, state: BandState) {
        self.checks += 1;
        match state {
            BandState::Correct => {}
            BandState::Corrected => self.corrected += 1,
            BandState::Lost => {
                self.lost += 1;
                // 与 wp-host 的退出码 75 同义：进程内无解，只能重启渲染进程。
                self.needs_restart = true;
            }
        }
        self.worst = Some(match (self.worst, state) {
            (_, BandState::Lost) | (Some(BandState::Lost), _) => BandState::Lost,
            (_, BandState::Corrected) | (Some(BandState::Corrected), _) => BandState::Corrected,
            _ => BandState::Correct,
        });
    }
}

/// 等待一段时间，期间保持消息泵与 DComp 绘制运行。
fn pump_for(surface: &mut Option<wp_platform::DcompSurface>, settle: Duration) {
    let deadline = Instant::now() + settle;
    while Instant::now() < deadline {
        wp_platform::pump_messages();
        if let Some(s) = surface.as_mut() {
            let _ = s.clear(wp_platform::TEST_RGB);
        }
        std::thread::sleep(Duration::from_millis(16));
    }
}

/// 核对一次层级并累计统计。
fn check_band(
    host: &mut dyn wp_platform::DesktopHost,
    hwnd: isize,
    stats: &mut CycleStats,
) -> Result<BandState> {
    let state = host.ensure_band(hwnd).map_err(|e| anyhow::anyhow!("{e}"))?;
    stats.record(state);
    Ok(state)
}

/// `wpctl cycles`：反复「最小化所有窗口 → 还原」，每次切换后核对层级。
///
/// M0 验收第 3 条要求「10 分钟 + 最小化/还原 20 次后层级仍正确」。最小化会让桌面
/// 露出来，`Progman` / `DefView` 的激活状态随之变化，是最容易把 z 序改乱的操作之一，
/// 因此单独压测：每次切换后都调用 `ensure_band`，统计修正与丢失次数。
fn run_cycles(
    count: u32,
    strategy_name: &str,
    content: &str,
    monitor_idx: usize,
    settle_ms: u64,
    shot: Option<&Path>,
    as_json: bool,
) -> Result<()> {
    let monitors = wp_platform::enumerate_monitors();
    let monitor = monitors
        .get(monitor_idx)
        .with_context(|| {
            format!(
                "显示器索引 {monitor_idx} 不存在（共 {} 台）",
                monitors.len()
            )
        })?
        .clone();

    let strategy = strategy_name
        .parse::<Strategy>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut host = strategy.open()?;

    let spec = match strategy {
        Strategy::DefViewChild => {
            let parent =
                wp_platform::defview_handle().context("找不到图标层，策略 F 无法创建子窗口")?;
            wp_platform::HostWindowSpec::child_of(parent, monitor.rect)
        }
        _ => wp_platform::HostWindowSpec::for_monitor(monitor.rect),
    }
    .with_noredirection_bitmap(content == "d3d");
    let hwnd = wp_platform::create_host_window(&spec).context("创建宿主窗口失败")?;

    if let Err(e) = host.attach(hwnd, monitor.rect) {
        wp_platform::destroy_host_window(hwnd);
        return Err(anyhow::anyhow!("attach 失败: {e}"));
    }

    let topmost = !matches!(strategy, Strategy::DefViewAdopt | Strategy::DefViewChild);
    let mut surface = if content == "d3d" {
        make_surface(strategy_name, hwnd, monitor.rect, topmost)
    } else {
        None
    };

    let settle = Duration::from_millis(settle_ms.max(100));
    let mut stats = CycleStats::default();
    let mut rounds = Vec::new();

    for round in 0..count {
        // 1) 最小化所有普通窗口，把桌面亮出来。
        let handles = wp_platform::minimize_other_windows();
        pump_for(&mut surface, settle);
        // 记录「修正前」的图标层状态：最小化会让 Explorer 自己动窗口树，
        // 这里要分清是父窗口被抢走，还是只是被隐藏。
        let state_after_minimize = wp_platform::defview_state();
        let z_after_minimize = z_band_report(hwnd);
        let hidden = check_band(&mut *host, hwnd, &mut stats)?;

        // 2) 还原，模拟用户切回应用。
        wp_platform::restore_windows(&handles);
        pump_for(&mut surface, settle);
        let state_after_restore = wp_platform::defview_state();
        let shown = check_band(&mut *host, hwnd, &mut stats)?;

        rounds.push(json!({
            "round": round + 1,
            "hidden_windows": handles.len(),
            "band_after_minimize": band_name(hidden),
            "band_after_restore": band_name(shown),
            "defview_after_minimize": state_after_minimize,
            "defview_after_restore": state_after_restore,
            "z_after_minimize": z_after_minimize,
        }));

        if stats.needs_restart {
            eprintln!("[cycles] 第 {} 轮层级丢失（Lost），停止压测", round + 1);
            break;
        }
    }

    // 截图前先最小化、再核对层级：这样截到的是「看门狗修正之后」的真实画面，
    // 而不是过渡状态。同时把修正前后的 z 链一起记下来，方便定位是谁盖住了壁纸。
    let mut shot_result = None;
    if let Some(path) = shot {
        let handles = wp_platform::minimize_other_windows();
        pump_for(&mut surface, settle);
        let z_before = z_band_report(hwnd);
        // 先量一次「修正前」的可见性，再修正、再量一次：用来判断层级漂移到底
        // 会不会让壁纸被盖住，还是只是无害的 z 序错位。
        let before_shot = wp_platform::capture(monitor.rect)?;
        let ratio_before = before_shot.ratio_of(wp_platform::TEST_RGB);
        let state = check_band(&mut *host, hwnd, &mut stats)?;
        pump_for(&mut surface, Duration::from_millis(200));
        let image = wp_platform::capture(monitor.rect)?;
        wp_platform::save_bmp(path, &image)
            .with_context(|| format!("保存 {} 失败", path.display()))?;
        let ratio = image.ratio_of(wp_platform::TEST_RGB);
        let z_after = z_band_report(hwnd);
        wp_platform::restore_windows(&handles);
        eprintln!(
            "截图 {}：修正前 {:.1}% → 修正后 {:.1}%（层级 {}）",
            path.display(),
            ratio_before * 100.0,
            ratio * 100.0,
            band_name(state)
        );
        shot_result = Some(json!({
            "path": path,
            "band_before_shot": band_name(state),
            "wallpaper_ratio_before_fix": ratio_before,
            "wallpaper_ratio": ratio,
            "z_before": z_before,
            "z_after": z_after,
        }));
    }

    drop(surface);
    let _ = host.detach(hwnd);
    wp_platform::destroy_host_window(hwnd);

    let passed = stats.lost == 0 && !stats.needs_restart;
    let summary = json!({
        "strategy": strategy_name,
        "content": content,
        "requested": count,
        "cycles": rounds.len(),
        "checks": stats.checks,
        "corrected": stats.corrected,
        "lost": stats.lost,
        "worst_band": stats.worst.map(band_name),
        "passed": passed,
        "shot": shot_result,
        "rounds": rounds,
    });

    if as_json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "策略 {strategy_name}（{content}）：{} 轮 / 检查 {} 次 / 修正 {} 次 / 丢失 {} 次，最差层级 {}",
            rounds.len(),
            stats.checks,
            stats.corrected,
            stats.lost,
            stats.worst.map(band_name).unwrap_or("-")
        );
        println!("结论: {}", if passed { "通过" } else { "未通过" });
    }
    Ok(())
}

fn take_shot(name: &str, path: &Path, rect: PhysicalRect, minimize: bool) -> Result<Value> {
    // 截图前再收一次：有些前台窗口会自己弹回来，只在开头最小化一次不够。
    // 注意只能用最小化——SW_HIDE 的窗口会从任务栏和 Alt+Tab 里消失，进程一死就没法恢复。
    let extra = RestoreGuard::new(if minimize {
        let handles = wp_platform::minimize_other_windows();
        std::thread::sleep(Duration::from_millis(500));
        handles
    } else {
        Vec::new()
    });
    // 壁纸窗口刚画完一帧，稍等一下再截，避免抓到没刷新完的合成结果。
    std::thread::sleep(Duration::from_millis(300));

    let image = wp_platform::capture(rect)?;
    wp_platform::save_bmp(path, &image).with_context(|| format!("保存 {} 失败", path.display()))?;

    let ratio = image.ratio_of(wp_platform::TEST_RGB);
    let icon_rect = PhysicalRect::new(rect.x, rect.y, 400.min(rect.width), 300.min(rect.height));
    let icons_non_matching = image.non_matching(icon_rect, wp_platform::TEST_RGB);

    let point = wp_platform::class_at_point(rect.x + 60, rect.y + 60);
    let center = wp_platform::class_at_point(
        rect.x + rect.width as i32 / 2,
        rect.y + rect.height as i32 / 2,
    );

    eprintln!(
        "[{name}] 截图 {}：测试色 {:.1}%，图标区异色像素 {icons_non_matching}，\
         点(60,60) 命中 {point}，中心命中 {center}",
        path.display(),
        ratio * 100.0
    );

    // 统计和命中测试都要在截图状态下做，之后才恢复其它窗口。
    extra.release();

    Ok(json!({
        "path": path.display().to_string(),
        "wallpaper_ratio": ratio,
        "icons_non_matching": icons_non_matching,
        "point_60_class": point,
        "center_class": center,
    }))
}

/// 打印宿主窗口与 `Progman` 之间的 z 链片段，用来定位「谁插在了中间」。
fn z_band_report(host: isize) -> String {
    let chain = wp_platform::z_chain();
    let Some(hz) = chain.iter().position(|e| e.0 == host) else {
        return "宿主不在顶层 z 链里（可能被最小化或已隐藏）".into();
    };
    let pz = wp_platform::progman_z()
        .map(|z| z as usize)
        .unwrap_or(hz + 2);
    let from = hz.saturating_sub(1);
    let to = (pz + 2).min(chain.len().saturating_sub(1));
    if to < from {
        return format!("z 链异常：宿主 #{hz}，Progman #{pz}");
    }
    chain[from..=to]
        .iter()
        .enumerate()
        .map(|(i, (h, class, visible, minimized))| {
            format!(
                "#{}{} {class} visible={visible} min={minimized}",
                from + i,
                if *h == host { " <宿主>" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 保证「收起的窗口一定被恢复」：即使中途 `?` 提前返回，`Drop` 也会执行。
///
/// 这条守卫是必须的：进程崩溃后没人恢复窗口虽然只是被收进任务栏，
/// 但测试工具在失败路径上留下副作用会污染下一次测量。
struct RestoreGuard(Vec<isize>);

impl RestoreGuard {
    fn new(handles: Vec<isize>) -> Self {
        Self(handles)
    }

    fn release(mut self) {
        wp_platform::restore_windows(&self.0);
        self.0.clear();
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            wp_platform::restore_windows(&self.0);
        }
    }
}

fn band_name(state: BandState) -> &'static str {
    match state {
        BandState::Correct => "correct",
        BandState::Corrected => "corrected",
        BandState::Lost => "lost",
    }
}

/// 建一块 DComp 表面，失败只打日志不中断测试。
fn make_surface(
    name: &str,
    hwnd: isize,
    rect: PhysicalRect,
    topmost: bool,
) -> Option<wp_platform::DcompSurface> {
    match wp_platform::DcompSurface::new(hwnd, rect.width, rect.height, topmost) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("[{name}] DComp 初始化失败: {e}");
            None
        }
    }
}

/// 在屏幕中心模拟右键：桌面上下文菜单能弹出，说明壁纸没吃掉输入。
fn test_input(rect: PhysicalRect, minimize: bool) -> Value {
    let x = rect.x + rect.width as i32 / 2;
    let y = rect.y + rect.height as i32 / 2;

    // 右键必须打到桌面上。有些应用被最小化后会立刻弹回来，
    // 所以每次点击前再收一次（最多 5 次）。只最小化，绝不隐藏：
    // 隐藏的窗口会从任务栏 / Alt+Tab 里消失，本进程一死就再也回不来。
    let mut minimized = Vec::new();
    let mut hit = String::new();
    for _ in 0..5 {
        if minimize {
            minimized = wp_platform::minimize_other_windows();
            std::thread::sleep(Duration::from_millis(200));
        }
        hit = wp_platform::class_at_point(x, y);
        if is_desktop_class(&hit) {
            break;
        }
    }

    let before = wp_platform::snapshot_top_level();
    let explorer_pid = wp_platform::desktop_pid();
    let clicked = wp_platform::right_click(x, y).is_ok();
    std::thread::sleep(Duration::from_millis(700));
    let new_windows = wp_platform::new_top_level(&before);
    // 只认 explorer 自己弹出来的窗口，避免把别的应用顺带冒出来的窗口算进来。
    let menu = new_windows
        .iter()
        .any(|(_, pid)| Some(*pid) == explorer_pid);
    wp_platform::press_escape();
    std::thread::sleep(Duration::from_millis(300));

    if !minimized.is_empty() {
        wp_platform::restore_windows(&minimized);
    }

    let minimized_classes: Vec<String> = minimized
        .iter()
        .map(|h| wp_platform::input::class_of(*h))
        .collect();
    eprintln!(
        "[input] 在 ({x},{y}) 右键：命中={hit} 收起的窗口={minimized_classes:?} 发送={clicked} 新窗口={new_windows:?} explorer={explorer_pid:?} 菜单={menu}"
    );

    json!({
        "at": [x, y],
        "hit_class": hit,
        "on_desktop": is_desktop_class(&hit),
        "sent": clicked,
        "new_windows": new_windows,
        "explorer_pid": explorer_pid,
        "menu_opened": menu,
    })
}

/// 命中这些类名说明鼠标确实落在桌面上，而不是别的应用窗口。
fn is_desktop_class(class: &str) -> bool {
    matches!(
        class,
        "SysListView32" | "SHELLDLL_DefView" | "Progman" | "WorkerW" | "(none)"
    )
}

/// `wpctl pause`：采样暂停判定，可连续观察或自测遮挡规则。
fn pause_cmd(
    as_json: bool,
    monitor_idx: usize,
    watch: u64,
    self_test: bool,
    hold_ms: u64,
    size: Option<&str>,
) -> Result<()> {
    let monitors = wp_platform::enumerate_monitors();
    let info = monitors.get(monitor_idx).with_context(|| {
        format!(
            "显示器索引 {monitor_idx} 不存在（共 {} 台）",
            monitors.len()
        )
    })?;
    let rect = info.rect;

    if self_test {
        let probe_rect = match size {
            Some(spec) => parse_size(spec)?,
            None => rect,
        };
        return pause_self_test(rect, probe_rect, hold_ms, as_json);
    }

    if watch == 0 {
        let report = wp_platform::pause::probe(None, rect, wp_platform::PauseFlags::default());
        print_pause_report(&report, as_json)?;
        return Ok(());
    }

    // 逐秒采样：用户可以在另一块屏上切进切出全屏游戏，观察判定是否跟着变。
    for second in 0..watch {
        let report = wp_platform::pause::probe(None, rect, wp_platform::PauseFlags::default());
        if as_json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            println!(
                "[{second:>2}s] {:<10} {}",
                if report.paused { "paused" } else { "running" },
                report.summary()
            );
        }
        if second + 1 < watch {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    Ok(())
}

fn print_pause_report(report: &wp_platform::PauseReport, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!(
        "状态: {}\n原因: {}\n通知状态: {} ({})\n前台: {}\n远程会话: {} 电池: {}",
        if report.paused { "暂停" } else { "运行中" },
        report.summary(),
        report.notification_state,
        report.notification_state_name,
        match &report.foreground {
            Some(fg) => format!(
                "{} pid={} rect=({},{} {}x{}) 最大化={} 盖住本屏={}",
                fg.class,
                fg.pid,
                fg.rect.x,
                fg.rect.y,
                fg.rect.width,
                fg.rect.height,
                fg.maximized,
                fg.covers_monitor
            ),
            None => "(无)".into(),
        },
        report.remote_session,
        match report.battery_percent {
            Some(p) => format!(
                "{p}%{}",
                if report.on_battery {
                    "（电池供电）"
                } else {
                    "（外接电源）"
                }
            ),
            None => "未知".into(),
        },
    );
    Ok(())
}

/// 自测：造一个铺满屏幕的前台窗口，看判定会不会报「被遮挡」，销毁后能不能恢复。
///
/// 这条路径验证的是「前台窗口矩形 ⊇ 显示器矩形」规则本身。`SHQueryUserNotificationState`
/// 的全屏独占状态只有真正的全屏 D3D 应用才会给出，无法用普通窗口伪造。
fn pause_self_test(
    monitor: PhysicalRect,
    probe_rect: PhysicalRect,
    hold_ms: u64,
    as_json: bool,
) -> Result<()> {
    let before = wp_platform::pause::probe(None, monitor, wp_platform::PauseFlags::default());

    let hwnd = wp_platform::create_probe_window(probe_rect).context("创建自测窗口失败")?;
    let guard = WindowGuard(hwnd);

    let activated = wp_platform::activate_window(hwnd);
    std::thread::sleep(Duration::from_millis(hold_ms.max(100)));
    let covered = wp_platform::pause::probe(None, monitor, wp_platform::PauseFlags::default());

    drop(guard);
    std::thread::sleep(Duration::from_millis(500));
    let after = wp_platform::pause::probe(None, monitor, wp_platform::PauseFlags::default());

    let expected_cover = probe_rect.width >= monitor.width && probe_rect.height >= monitor.height;
    let detected = covered.reasons.iter().any(|r| {
        matches!(
            r,
            wp_platform::PauseReason::MaximizedCover | wp_platform::PauseReason::FullscreenApp
        )
    });
    // 恢复的判据是「回到测试前的状态」，而不是「一定没暂停」：
    // 用户可能本来就有最大化窗口或低电量。
    let restored = after.reasons == before.reasons;
    // 小窗口自测是用来观察 wp-host 恢复渲染的，不做通过/失败断言。
    let passed = if expected_cover {
        Some(activated && detected && restored)
    } else {
        None
    };

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "passed": passed,
                "activated": activated,
                "detected": detected,
                "restored": restored,
                "before": before,
                "covered": covered,
                "after": after,
            }))?
        );
    } else {
        println!("自测窗口已激活: {activated}");
        println!("遮挡判定: {}（{}）", detected, covered.summary());
        println!("销毁后恢复: {restored}（{}）", after.summary());
        match passed {
            Some(true) => println!("结论: 通过"),
            Some(false) => println!("结论: 未通过"),
            // 小窗口本来就不该触发遮挡，这是给 wp-host 观察恢复用的。
            None => println!("结论: 小窗口模式，不做判定（预期不遮挡）"),
        }
        if !activated {
            println!(
                "提示: 系统前台锁拒绝了 SetForegroundWindow，请先手动点一下当前控制台再重试。"
            );
        }
    }
    Ok(())
}

/// 解析 `宽x高`，只接受 ASCII 的 `x` 或 `*`。
fn parse_size(spec: &str) -> Result<PhysicalRect> {
    let normalized = spec.replace('*', "x");
    let (w, h) = normalized
        .split_once('x')
        .with_context(|| format!("尺寸格式应为 宽x高，例如 320x200（收到 {spec}）"))?;
    let width: u32 = w.trim().parse().context("宽度不是整数")?;
    let height: u32 = h.trim().parse().context("高度不是整数")?;
    if width == 0 || height == 0 {
        bail!("尺寸必须大于 0（收到 {spec}）");
    }
    Ok(PhysicalRect::new(0, 0, width, height))
}

/// 自测窗口的兜底销毁。
struct WindowGuard(isize);

impl Drop for WindowGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            wp_platform::destroy_host_window(self.0);
        }
    }
}
