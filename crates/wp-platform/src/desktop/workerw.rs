//! 策略 D：经典 `WorkerW` 宿主。
//!
//! 这是 Win10 / 22H2–23H2 以及大多数开源实现的通用做法，参考
//! `E:\private\Rust\wallpaper\src-tauri\src\wallpaper.rs`：
//!
//! 1. 向 `Progman` 发送 `0x052C`（**wParam = 0**，不是旧脚本里的 `0xD`），
//!    让 Explorer 在 `SHELLDLL_DefView` 之后生成一个 `WorkerW` 宿主窗口；
//! 2. `EnumWindows` 找「含 `SHELLDLL_DefView` 的顶层窗口」，
//!    再取它后面的第一个 `WorkerW`——那个就是壁纸层；
//! 3. 把自己的窗口改成 `WS_CHILD | WS_VISIBLE`，`SetParent` 到该 `WorkerW`，
//!    再用 `SetWindowPos(HWND_TOP)` 铺满。
//!
//! 26H2 上握手可能无效（`0x052C` 返回 0 且不产生新窗口），此时 [`WorkerWHost::new`]
//! 返回 `NoDesktopHost`，由上层回退到其它策略，不会黑屏。

use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetParent, GetWindowRect, IsWindow, IsWindowVisible, SetParent, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, GWL_STYLE, HWND_TOP, SET_WINDOW_POS_FLAGS, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
    SWP_SHOWWINDOW, SW_SHOW, WS_CHILD, WS_POPUP, WS_VISIBLE,
};

use wp_core::PhysicalRect;

use super::probe;
use super::{BandState, DesktopHost, PlatformError, Result, Strategy};
use crate::window::raw;

/// 握手后等待 Explorer 建窗口的次数与间隔。
const SPAWN_RETRIES: u32 = 10;
const SPAWN_INTERVAL: Duration = Duration::from_millis(100);

pub struct WorkerWHost {
    rect: PhysicalRect,
    /// `HWND` 不是 `Send`，而 `DesktopHost` 要求 `Send`，所以存成 `isize`。
    host: isize,
    corrections: u64,
}

impl WorkerWHost {
    pub fn new() -> Result<Self> {
        let host = ensure_host()?;
        Ok(Self {
            rect: PhysicalRect::new(0, 0, 0, 0),
            host: host.0 as isize,
            corrections: 0,
        })
    }

    pub fn corrections(&self) -> u64 {
        self.corrections
    }

    pub fn host(&self) -> isize {
        self.host
    }

    /// 子窗口坐标相对父窗口客户区，必须把屏幕坐标换算过去。
    fn child_rect(&self, host: HWND) -> Result<(i32, i32, i32, i32)> {
        let mut hr = windows::Win32::Foundation::RECT::default();
        unsafe { GetWindowRect(host, &mut hr) }.map_err(|e| win32("GetWindowRect(WorkerW)", e))?;
        Ok((
            self.rect.x - hr.left,
            self.rect.y - hr.top,
            self.rect.width as i32,
            self.rect.height as i32,
        ))
    }

    fn reparent(&self, hwnd: HWND, host: HWND) -> Result<()> {
        // 顶层弹出窗口改成子窗口，否则 SetParent 后内容不会跟着父窗口绘制。
        let style = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWL_STYLE) }
            as u32;
        let child_style = (style & !WS_POPUP.0) | WS_CHILD.0 | WS_VISIBLE.0;
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_STYLE, child_style as isize);
        }

        let (x, y, w, h) = self.child_rect(host)?;
        let flags = SET_WINDOW_POS_FLAGS(
            SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_SHOWWINDOW.0,
        );
        unsafe { SetParent(hwnd, Some(host)) }.map_err(|e| win32("SetParent(WorkerW)", e))?;
        unsafe { SetWindowPos(hwnd, Some(HWND_TOP), x, y, w, h, flags) }
            .map_err(|e| win32("SetWindowPos(WorkerW)", e))?;
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        Ok(())
    }
}

/// 拿到可用的 `WorkerW` 宿主：先找现成的，没有就发握手再等。
fn ensure_host() -> Result<HWND> {
    if let Some(h) = probe::find_workerw_host() {
        return Ok(h);
    }
    let progman = probe::progman().ok_or(PlatformError::NoDesktopHost)?;
    probe::spawn_worker(progman, probe::SPAWN_WORKER_WPARAM_CLASSIC);

    for _ in 0..SPAWN_RETRIES {
        if let Some(h) = probe::find_workerw_host() {
            return Ok(h);
        }
        std::thread::sleep(SPAWN_INTERVAL);
    }
    // 26H2 上大概率走到这里：握手无效，桌面不再有 WorkerW 宿主。
    Err(PlatformError::NoDesktopHost)
}

impl DesktopHost for WorkerWHost {
    fn strategy(&self) -> Strategy {
        Strategy::WorkerW
    }

    fn attach(&mut self, hwnd: isize, rect: PhysicalRect) -> Result<()> {
        let hwnd = raw(hwnd);
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Err(PlatformError::Unsupported("attach 传入的窗口句柄无效"));
        }
        self.rect = rect;

        // 宿主可能被 Explorer 重启换掉，每次都重新确认。
        let host = if unsafe { IsWindow(Some(raw(self.host))).as_bool() } {
            raw(self.host)
        } else {
            let host = ensure_host()?;
            self.host = host.0 as isize;
            host
        };
        self.reparent(hwnd, host)
    }

    fn ensure_band(&mut self, hwnd: isize) -> Result<BandState> {
        let hwnd = raw(hwnd);
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Ok(BandState::Lost);
        }
        let host = raw(self.host);
        let alive = unsafe { IsWindow(Some(host)).as_bool() }
            && unsafe { IsWindowVisible(host).as_bool() };

        // 父窗口丢了（Explorer 重启）或内容被隐藏，就整条重挂。
        let parent = unsafe { GetParent(hwnd) }.ok();
        if !alive || parent != Some(host) {
            let next = ensure_host()?;
            self.host = next.0 as isize;
            self.reparent(hwnd, next)?;
            self.corrections += 1;
            return Ok(BandState::Corrected);
        }
        Ok(BandState::Correct)
    }

    fn detach(&mut self, hwnd: isize) -> Result<()> {
        let hwnd = raw(hwnd);
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Ok(());
        }
        // 归还给桌面，否则销毁时会被当成 WorkerW 的子窗口。
        unsafe { SetParent(hwnd, None) }.map_err(|e| win32("SetParent(None)", e))?;
        Ok(())
    }
}

fn win32(context: &'static str, e: windows::core::Error) -> PlatformError {
    PlatformError::Win32 {
        context,
        code: e.code().0 as u32,
    }
}
