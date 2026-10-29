//! 主进程 ↔ 渲染进程的管道协议。
//!
//! 帧格式：`u32 LE 长度` + MessagePack 负载。

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use wp_core::{PhysicalRect, PropertyValues, WallpaperId, WallpaperSpec};

/// 单帧上限，防止异常输入把内存撑爆。
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    FullscreenApp,
    MaximizedCover,
    BatteryLow,
    SessionLocked,
    DisplayOff,
    RemoteSession,
    UserManual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BackendInit,
    Decode,
    NotFound,
    Permission,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Load {
        item: WallpaperId,
        spec: WallpaperSpec,
    },
    Unload,
    Pause {
        reason: PauseReason,
    },
    Resume,
    SetProperties {
        values: PropertyValues,
    },
    SetRect {
        rect: PhysicalRect,
    },
    SetCursor {
        x: i32,
        y: i32,
        inside: bool,
    },
    SetMuted {
        muted: bool,
    },
    GetStats,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "evt", rename_all = "snake_case")]
pub enum Event {
    Ready { backend: String },
    Stats {
        fps: f32,
        cpu: f32,
        gpu_mem_mb: u32,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
    Log {
        level: u8,
        message: String,
    },
}

#[derive(Debug)]
pub enum FrameError {
    Io(std::io::Error),
    TooLarge(usize),
    Codec(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误: {e}"),
            Self::TooLarge(n) => write!(f, "帧过大: {n} 字节"),
            Self::Codec(e) => write!(f, "编解码错误: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<std::io::Error> for FrameError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), FrameError> {
    let body = rmp_serde::to_vec_named(msg).map_err(|e| FrameError::Codec(e.to_string()))?;
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(body.len()));
    }
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

pub fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(r: &mut R) -> Result<T, FrameError> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    rmp_serde::from_slice(&buf).map_err(|e| FrameError::Codec(e.to_string()))
}

/// 管道名。包含 session id，避免多用户/多会话冲突。
pub fn pipe_name(session_id: u32, monitor_index: u32) -> String {
    format!(r"\\.\pipe\wallpaper-host-{session_id}-{monitor_index}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn command_roundtrip() {
        let msg = Command::SetCursor {
            x: 12,
            y: -3,
            inside: true,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &msg).unwrap();

        let mut cur = Cursor::new(buf);
        let back: Command = read_frame(&mut cur).unwrap();
        match back {
            Command::SetCursor { x, y, inside } => {
                assert_eq!((x, y, inside), (12, -3, true));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn event_roundtrip() {
        let msg = Event::Error {
            code: ErrorCode::Decode,
            message: "bad codec".into(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &msg).unwrap();
        let back: Event = read_frame(&mut Cursor::new(buf)).unwrap();
        assert!(matches!(back, Event::Error { .. }));
    }

    #[test]
    fn rejects_oversized_frame_header() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&((MAX_FRAME + 1) as u32).to_le_bytes());
        let err = read_frame::<_, Command>(&mut Cursor::new(buf)).unwrap_err();
        assert!(matches!(err, FrameError::TooLarge(_)));
    }

    #[test]
    fn pipe_name_shape() {
        assert_eq!(
            pipe_name(1, 2),
            r"\\.\pipe\wallpaper-host-1-2"
        );
    }
}
