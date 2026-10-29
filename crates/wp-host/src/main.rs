//! 渲染宿主进程：每台显示器启动一个。
//!
//! M0 阶段只做参数解析与启动日志；窗口创建与管道接入在后续步骤补齐。

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let monitor = parse_arg(&args, "--monitor").unwrap_or_else(|| "0".to_string());
    let token = parse_arg(&args, "--token").unwrap_or_default();

    tracing::info!(
        monitor = %monitor,
        token_len = token.len(),
        "wp-host 启动（骨架阶段，尚未创建宿主窗口）"
    );

    // M0 第 2 步：创建宿主窗口 → attach 桌面 → 连接管道 → 进入消息循环。
    println!("wp-host: monitor={monitor} (skeleton)");
    Ok(())
}

fn parse_arg(args: &[String], key: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == key)?;
    args.get(idx + 1).cloned()
}
