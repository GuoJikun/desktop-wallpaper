//! supervisor 控制管道协议。
//!
//! 宿主（`wp-host`）的管道是「每显示器一个」，supervisor 的管道是「整台机器一个」：
//! 界面不需要知道当前哪个显示器由哪个进程负责，只要把「第 N 屏换成这张图」交给
//! supervisor，由它转给对应的 worker，再由 worker 走宿主管道。
//!
//! 帧格式与宿主管道完全一致（长度前缀 + MessagePack），所以复用
//! [`write_frame`][crate::write_frame] / [`read_frame`][crate::read_frame]。
//!
//! 与宿主管道一样，M1 只做「一问一答」：客户端发一条 [`Command`]，等一条 [`Event`]。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ErrorCode;

/// supervisor 控制管道名。带 session id，不同登录会话互不干扰。
pub fn pipe_name(session_id: u32) -> String {
    format!(r"\\.\pipe\wallpaper-supervisor-{session_id}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// 把指定显示器换成这张图。
    ///
    /// supervisor 会先找该屏的 worker；没有就按启动参数拉起一个，
    /// 然后通过宿主管道换图，不必重挂桌面。
    LoadFile { monitor: u32, path: PathBuf },
    /// 停掉所有 worker 并退出 supervisor。
    ///
    /// worker 收到的是宿主 `WM_CLOSE` / 管道 `Shutdown`，走优雅退出：
    /// 先把 `DefView` 还给 `Progman` 再结束进程。
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "evt", rename_all = "snake_case")]
pub enum Event {
    /// 指定显示器的图已生效。
    Loaded {
        monitor: u32,
        path: String,
        width: u32,
        height: u32,
    },
    /// 命令失败。`monitor` 为 `None` 表示错误发生在 supervisor 自身
    /// （例如显示器索引不存在）。
    Error {
        monitor: Option<u32>,
        code: ErrorCode,
        message: String,
    },
    /// 所有 worker 已退出，supervisor 正在收尾。
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{read_frame, write_frame};
    use std::io::Cursor;

    #[test]
    fn load_file_roundtrip() {
        let msg = Command::LoadFile {
            monitor: 1,
            path: PathBuf::from(r"E:\wallpapers\a.png"),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &msg).unwrap();
        let back: Command = read_frame(&mut Cursor::new(buf)).unwrap();
        match back {
            Command::LoadFile { monitor, path } => {
                assert_eq!(monitor, 1);
                assert_eq!(path, PathBuf::from(r"E:\wallpapers\a.png"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn loaded_roundtrip() {
        let msg = Event::Loaded {
            monitor: 0,
            path: "a.png".into(),
            width: 1920,
            height: 1080,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &msg).unwrap();
        let back: Event = read_frame(&mut Cursor::new(buf)).unwrap();
        assert!(matches!(
            back,
            Event::Loaded {
                monitor: 0,
                width: 1920,
                height: 1080,
                ..
            }
        ));
    }

    #[test]
    fn error_without_monitor_roundtrip() {
        let msg = Event::Error {
            monitor: None,
            code: ErrorCode::NotFound,
            message: "没有显示器 4".into(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &msg).unwrap();
        let back: Event = read_frame(&mut Cursor::new(buf)).unwrap();
        match back {
            Event::Error { monitor, code, .. } => {
                assert!(monitor.is_none());
                assert_eq!(code, ErrorCode::NotFound);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn pipe_name_shape() {
        assert_eq!(pipe_name(1), r"\\.\pipe\wallpaper-supervisor-1");
    }
}
