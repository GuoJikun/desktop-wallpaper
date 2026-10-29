//! wpctl：不开 UI 也能探测桌面结构、跑挂载矩阵、截图验证、查看状态。

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
    },
    /// 截取整屏到 BMP，不做其它事
    Shot {
        /// 输出文件
        #[arg(long, default_value = "out/shot.bmp")]
        out: PathBuf,
        /// 显示器索引
        #[arg(long, default_value_t = 0)]
        monitor: usize,
    },
    /// 挂载一张壁纸到指定显示器（M0 第 3 步后可用）
    Attach {
        /// 显示器索引（0 起）
        #[arg(long, default_value_t = 0)]
        monitor: usize,
        /// 媒体文件或壁纸包路径
        #[arg(long)]
        source: PathBuf,
    },
    /// 暂停所有渲染
    Pause,
    /// 查看渲染进程状态
    Stats,
}

fn main() -> Result<()> {
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
        } => probe(
            json,
            report,
            strategy,
            RunOpts {
                content: &content,
                duration,
                shot: shot.as_deref(),
                minimize,
            },
            monitor,
        ),
        Cmd::Shot { out, monitor } => shot_once(&out, monitor),
        Cmd::Attach { monitor, source } => {
            let monitors = wp_platform::enumerate_monitors();
            let info = monitors
                .get(monitor)
                .with_context(|| format!("显示器索引 {monitor} 不存在（共 {} 台）", monitors.len()))?;
            bail!(
                "attach 尚未实现（M0 第 3 步）。目标显示器: {} {}x{}，源: {}",
                info.device,
                info.rect.width,
                info.rect.height,
                source.display()
            );
        }
        Cmd::Pause => bail!("pause 尚未实现（M1）"),
        Cmd::Stats => bail!("stats 尚未实现（M1）"),
    }
}

/// 一次挂载测试的参数。
struct RunOpts<'a> {
    content: &'a str,
    duration: u64,
    shot: Option<&'a Path>,
    minimize: bool,
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

fn shot_once(out: &Path, monitor_idx: usize) -> Result<()> {
    let monitors = wp_platform::enumerate_monitors();
    let monitor = monitors
        .get(monitor_idx)
        .with_context(|| format!("显示器索引 {monitor_idx} 不存在（共 {} 台）", monitors.len()))?;

    let image = wp_platform::capture(monitor.rect)?;
    wp_platform::save_bmp(out, &image)
        .with_context(|| format!("保存 {} 失败", out.display()))?;

    let ratio = image.ratio_of(wp_platform::TEST_RGB);
    println!(
        "已截屏 {}（{}x{}），测试色占比 {:.2}%",
        out.display(),
        image.width,
        image.height,
        ratio * 100.0
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
        Some(code) => println!("  0x052C 返回        : {code}"),
        None => println!("  0x052C             : 未发送（无 Progman）"),
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
    }
}

fn run_matrix(selection: &str, opts: &RunOpts<'_>, monitor_idx: usize) -> Result<Vec<Value>> {
    let monitors = wp_platform::enumerate_monitors();
    let monitor = monitors
        .get(monitor_idx)
        .with_context(|| format!("显示器索引 {monitor_idx} 不存在（共 {} 台）", monitors.len()))?
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

    // 先把桌面露出来，避免截图被别的窗口糊住。
    let minimized = if opts.minimize {
        let handles = wp_platform::minimize_other_windows();
        std::thread::sleep(Duration::from_millis(400));
        handles
    } else {
        Vec::new()
    };

    let spec = wp_platform::HostWindowSpec::for_monitor(monitor.rect);
    let hwnd = wp_platform::create_host_window(&spec)
        .with_context(|| format!("创建宿主窗口失败（{name}）"))?;

    eprintln!("[{name}] 窗口已创建，开始 {duration}s 挂载测试（内容={content}）");

    let attach_error = host.attach(hwnd, monitor.rect).err().map(|e| e.to_string());
    let attached = attach_error.is_none();
    if let Some(err) = &attach_error {
        eprintln!("[{name}] attach 失败: {err}");
    }

    let start = Instant::now();
    let mut next_check = start;
    let mut next_paint = start;
    let mut shot_at = opts.shot.map(|_| start + Duration::from_secs(duration.max(2) / 2));
    let mut shot_result: Option<Value> = None;
    let mut checks = 0u64;
    let mut corrected = 0u64;
    let mut lost = 0u64;
    let mut final_band: Option<BandState> = None;

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

        if let (Some(path), Some(at)) = (opts.shot, shot_at) {
            if now >= at {
                shot_result = Some(take_shot(name, path, monitor.rect)?);
                shot_at = None;
            }
        }

        std::thread::sleep(Duration::from_millis(16));
    }

    let handle_alive = wp_platform::is_host_window_alive(hwnd);
    wp_platform::destroy_host_window(hwnd);
    if !minimized.is_empty() {
        wp_platform::restore_windows(&minimized);
    }

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
        "handle_alive": handle_alive,
        "shot": shot_result,
    }))
}

fn take_shot(name: &str, path: &Path, rect: PhysicalRect) -> Result<Value> {
    // 壁纸窗口刚画完一帧，稍等一下再截，避免抓到没刷新完的合成结果。
    std::thread::sleep(Duration::from_millis(300));

    let image = wp_platform::capture(rect)?;
    wp_platform::save_bmp(path, &image)
        .with_context(|| format!("保存 {} 失败", path.display()))?;

    let ratio = image.ratio_of(wp_platform::TEST_RGB);
    let icon_rect = PhysicalRect::new(
        rect.x,
        rect.y,
        400.min(rect.width),
        300.min(rect.height),
    );
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

    Ok(json!({
        "path": path.display().to_string(),
        "wallpaper_ratio": ratio,
        "icons_non_matching": icons_non_matching,
        "point_60_class": point,
        "center_class": center,
    }))
}

fn band_name(state: BandState) -> &'static str {
    match state {
        BandState::Correct => "correct",
        BandState::Corrected => "corrected",
        BandState::Lost => "lost",
    }
}
