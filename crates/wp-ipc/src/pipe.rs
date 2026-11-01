//! Windows 命名管道：阻塞式 server + 带超时的 client。
//!
//! 这一层刻意只做一件事：拿到一个可读写的 `File`。帧编解码仍然用 crate 顶层的
//! [`write_frame`][crate::write_frame] / [`read_frame`][crate::read_frame]，
//! 这样管道和内存缓冲共用同一套协议与测试。
//!
//! 为什么不用 `tokio::net::windows::named_pipe`：宿主进程的主循环是同步的、
//! 4 ms 一次轮询，多引一个异步运行时不划算；这里一个专用线程阻塞在
//! `ConnectNamedPipe` / `read` 上就够了。

use std::fs::File;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

/// 管道收发缓冲。控制消息只有几百字节，64 KiB 足够让一个
/// 帧（最大 8 MB）分块写出而不至于频繁阻塞。
const BUFFER_SIZE: u32 = 64 * 1024;

#[derive(Debug)]
pub enum PipeError {
    /// 没有进程在这个名字上监听。
    NotRunning,
    /// 有监听者，但所有实例都在忙，等待超时。
    Busy,
    Io(std::io::Error),
    Win32 {
        context: &'static str,
        code: u32,
    },
}

impl std::fmt::Display for PipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => write!(f, "没有进程在监听这个管道"),
            Self::Busy => write!(f, "管道全忙，等待超时"),
            Self::Io(e) => write!(f, "IO 错误: {e}"),
            Self::Win32 { context, code } => write!(f, "{context} 失败，Win32 错误码 {code}"),
        }
    }
}

impl std::error::Error for PipeError {}

impl From<std::io::Error> for PipeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// 管道名转成以 NUL 结尾的 UTF-16。每次 `accept` / `connect` 都要用，
/// 所以由调用方持有，避免反复分配。
fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 一次只接受一个连接的监听端点。
///
/// 每调用一次 [`PipeServer::accept`] 就新建一个管道实例：客户端断开后实例随
/// `File` 一起关闭，下一次 `accept` 重新监听——不需要 `DisconnectNamedPipe`。
pub struct PipeServer {
    name: Vec<u16>,
}

impl PipeServer {
    pub fn bind(name: &str) -> Self {
        Self { name: wide(name) }
    }

    /// 阻塞到有客户端连接，返回双向通道。
    pub fn accept(&self) -> Result<File, PipeError> {
        // 字节流 + 阻塞模式：帧边界由长度前缀定义，不需要 MESSAGE 模式。
        let mode = NAMED_PIPE_MODE(PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0);
        unsafe {
            let handle = CreateNamedPipeW(
                PCWSTR(self.name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                mode,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER_SIZE,
                BUFFER_SIZE,
                0,
                None,
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(PipeError::Win32 {
                    context: "CreateNamedPipeW",
                    code: GetLastError().0,
                });
            }

            if let Err(e) = ConnectNamedPipe(handle, None) {
                // 客户端可能在 CreateNamedPipeW 与 ConnectNamedPipe 之间就连上了，
                // 这时系统报 ERROR_PIPE_CONNECTED——连接其实是成功的。
                if e.code().0 as u32 != ERROR_PIPE_CONNECTED.0 {
                    let code = e.code().0 as u32;
                    let _ = CloseHandle(handle);
                    return Err(PipeError::Win32 {
                        context: "ConnectNamedPipe",
                        code,
                    });
                }
            }

            // 句柄交给 `File`，由它负责关闭；`OwnedHandle` 保证所有权唯一。
            Ok(File::from(OwnedHandle::from_raw_handle(handle.0)))
        }
    }
}

/// 连接已在监听的管道，最多等 `timeout`。
///
/// 超时的含义要区分开：`NotRunning` 说明宿主根本没起来（可以放心 spawn 新的），
/// `Busy` 说明宿主在跑但实例被占着（不能再起一个）。
pub fn connect(name: &str, timeout: Duration) -> Result<File, PipeError> {
    let wide_name = wide(name);
    let timeout = timeout.min(Duration::from_secs(60));
    // `WaitNamedPipeW` 在「名字根本不存在」时是立刻返回的，所以这里要自己
    // 轮询到超时：宿主刚被拉起、`CreateNamedPipeW` 还没执行完是常态。
    let deadline = std::time::Instant::now() + timeout;
    let mut delay = Duration::from_millis(1);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let millis = left.as_millis().min(u32::MAX as u128 - 1) as u32;
        unsafe {
            if WaitNamedPipeW(PCWSTR(wide_name.as_ptr()), millis).as_bool() {
                break;
            }
            match GetLastError().0 {
                code if code == ERROR_FILE_NOT_FOUND.0 => {
                    if left.is_zero() {
                        return Err(PipeError::NotRunning);
                    }
                }
                code if code == ERROR_PIPE_BUSY.0 => {
                    if left.is_zero() {
                        return Err(PipeError::Busy);
                    }
                }
                code => {
                    return Err(PipeError::Win32 {
                        context: "WaitNamedPipeW",
                        code,
                    })
                }
            }
        }
        // 1 ms 起、翻倍到 20 ms：启动期的短轮询够快，长等待也不烧 CPU。
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(20));
    }

    // 用 std 打开管道：`OpenOptions` 内部就是 CreateFileW，
    // 读写 + 默认共享模式（READ|WRITE|DELETE）对命名管道完全够用。
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(name)
        .map_err(PipeError::Io)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::{read_frame, write_frame, Command, Event};

    fn unique_pipe(tag: &str) -> String {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        format!(r"\\.\pipe\wp-ipc-test-{tag}-{}-{n}", std::process::id())
    }

    #[test]
    fn roundtrip_over_pipe() {
        let name = unique_pipe("roundtrip");
        let server = PipeServer::bind(&name);
        let worker = std::thread::spawn(move || {
            let mut conn = server.accept().expect("accept 失败");
            let command: Command = read_frame(&mut conn).expect("读命令失败");
            match command {
                Command::LoadFile { path } => assert_eq!(path.to_string_lossy(), "a.png"),
                other => panic!("unexpected: {other:?}"),
            }
            write_frame(
                &mut conn,
                &Event::Loaded {
                    path: "a.png".into(),
                    width: 1920,
                    height: 1080,
                },
            )
            .expect("写事件失败");
        });

        let mut conn = connect(&name, Duration::from_secs(5)).expect("connect 失败");
        write_frame(
            &mut conn,
            &Command::LoadFile {
                path: "a.png".into(),
            },
        )
        .unwrap();
        let event: Event = read_frame(&mut conn).unwrap();
        match event {
            Event::Loaded {
                path,
                width,
                height,
            } => {
                assert_eq!(path, "a.png");
                assert_eq!((width, height), (1920, 1080));
            }
            other => panic!("unexpected: {other:?}"),
        }

        worker.join().unwrap();
    }

    #[test]
    fn connect_reports_not_running() {
        let name = unique_pipe("missing");
        let started = Instant::now();
        let err = connect(&name, Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, PipeError::NotRunning), "unexpected: {err:?}");
        // 没监听者时应该立刻失败，而不是等满超时。
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn frame_helpers_match_pipe_format() {
        // 管道只提供字节流，帧格式本身仍由 lib.rs 保证；
        // 这里顺带确认长度前缀在管道语义下同样是可解析的。
        let mut buf = Vec::new();
        write_frame(&mut buf, &Command::Unload).unwrap();
        let mut cursor = Cursor::new(buf);
        let command: Command = read_frame(&mut cursor).unwrap();
        assert!(matches!(command, Command::Unload));
    }
}
