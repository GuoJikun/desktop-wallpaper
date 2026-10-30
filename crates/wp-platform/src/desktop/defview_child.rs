//! 策略 F：把自己做成 `SHELLDLL_DefView`（图标层）的子窗口，压在图标列表之下。
//!
//! 策略 E 的致命问题：宿主窗口是顶层窗口，`DefView` 被 `SetParent` 成它的子窗口，
//! 于是**渲染进程一死，Windows 会连 `DefView` 一起销毁**——桌面图标层整个消失，
//! 任务栏和 `Alt+Tab` 也跟着错乱，只能重启 Explorer 才能恢复（已实测）。
//!
//! 反过来就不一样：`DefView` 是父窗口，我们只是它下面的一个子窗口。
//! 进程崩溃只会销毁我们自己的窗口，图标层毫发无伤，下一次启动重新挂一次即可。
//!
//! 层级：`DefView` 的子窗口里，`SysListView32`（图标）必须在我们之上，
//! 所以用 `HWND_BOTTOM` 把宿主压到兄弟链最底。

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetParent, GetWindow, GetWindowRect, IsWindow, IsWindowVisible, SetParent,
    SetWindowPos, GW_CHILD, GW_HWNDNEXT, HWND_BOTTOM, SET_WINDOW_POS_FLAGS, SWP_NOACTIVATE,
    SWP_NOOWNERZORDER, SWP_SHOWWINDOW,
};

use wp_core::PhysicalRect;

use super::{BandState, DesktopHost, PlatformError, Result, Strategy};
use crate::window::raw;

pub struct DefViewChild {
    defview: isize,
    rect: PhysicalRect,
    corrections: u64,
}

impl DefViewChild {
    pub fn new() -> Result<Self> {
        let defview = super::adopt::defview_handle().ok_or(PlatformError::NoDesktopHost)?;
        Ok(Self {
            defview,
            rect: PhysicalRect::new(0, 0, 0, 0),
            corrections: 0,
        })
    }

    pub fn corrections(&self) -> u64 {
        self.corrections
    }

    /// 把宿主窗口放到 `DefView` 客户区的指定位置，并压到兄弟链最底。
    fn place(&self, host: HWND) -> Result<()> {
        let defview = raw(self.defview);
        let mut dr = RECT::default();
        unsafe { GetWindowRect(defview, &mut dr) }
            .map_err(|e| win32("GetWindowRect(DefView)", e))?;

        let flags = SET_WINDOW_POS_FLAGS(SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_SHOWWINDOW.0);
        unsafe {
            SetWindowPos(
                host,
                Some(HWND_BOTTOM),
                self.rect.x - dr.left,
                self.rect.y - dr.top,
                self.rect.width as i32,
                self.rect.height as i32,
                flags,
            )
        }
        .map_err(|e| win32("SetWindowPos(host)", e))?;
        Ok(())
    }
}

impl DesktopHost for DefViewChild {
    fn strategy(&self) -> Strategy {
        Strategy::DefViewChild
    }

    fn attach(&mut self, hwnd: isize, rect: PhysicalRect) -> Result<()> {
        let host = raw(hwnd);
        if !unsafe { IsWindow(Some(host)) }.as_bool() {
            return Err(PlatformError::Unsupported("attach 传入的窗口句柄无效"));
        }
        // Explorer 重启会换新图标层，每次 attach 都重新找。
        if let Some(defview) = super::adopt::defview_handle() {
            self.defview = defview;
        }
        self.rect = rect;

        let defview = raw(self.defview);
        if !unsafe { IsWindow(Some(defview)) }.as_bool() {
            return Err(PlatformError::NoDesktopHost);
        }
        unsafe { SetParent(host, Some(defview)) }.map_err(|e| win32("SetParent(host)", e))?;
        self.place(host)
    }

    fn ensure_band(&mut self, hwnd: isize) -> Result<BandState> {
        let host = raw(hwnd);
        if !unsafe { IsWindow(Some(host)) }.as_bool() {
            return Ok(BandState::Lost);
        }
        if !unsafe { IsWindow(Some(raw(self.defview))) }.as_bool() {
            return Ok(BandState::Lost);
        }
        let defview = raw(self.defview);

        if unsafe { GetParent(host) }.ok() != Some(defview) {
            unsafe { SetParent(host, Some(defview)) }.map_err(|e| win32("SetParent(host)", e))?;
            self.place(host)?;
            self.corrections += 1;
            return Ok(BandState::Corrected);
        }

        if unsafe { IsWindowVisible(host) }.as_bool() && is_below_icons(host, defview) {
            return Ok(BandState::Correct);
        }
        self.place(host)?;
        self.corrections += 1;
        Ok(BandState::Corrected)
    }

    fn detach(&mut self, hwnd: isize) -> Result<()> {
        // 宿主窗口马上会被销毁。这里只把它摘出来，免得销毁时牵动图标层。
        let host = raw(hwnd);
        if unsafe { IsWindow(Some(host)) }.as_bool() {
            let _ = unsafe { SetParent(host, None) };
        }
        Ok(())
    }
}

/// 宿主是否在图标列表（`SysListView32`）之下。
///
/// 兄弟链从最上面开始枚举，谁先出现谁在上面。宿主直接挂在 `DefView` 下、
/// 图标在更深的层级（`DefView → SysListView32`），所以逐级比较：先找到的是
/// `SysListView32` 说明图标在上，符合预期。
fn is_below_icons(host: HWND, defview: HWND) -> bool {
    let mut cur = unsafe { GetWindow(defview, GW_CHILD) }.ok();
    while let Some(child) = cur {
        if child == host {
            return false; // 宿主比图标先出现 = 盖住了图标
        }
        if class_name(child) == "SysListView32" {
            return true;
        }
        cur = unsafe { GetWindow(child, GW_HWNDNEXT) }.ok();
    }
    // 没找到图标层：桌面可能正在重建，先按正常处理。
    true
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn win32(context: &'static str, e: windows::core::Error) -> PlatformError {
    PlatformError::Win32 {
        context,
        code: e.code().0 as u32,
    }
}
