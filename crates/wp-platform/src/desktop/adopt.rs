//! 策略 E：把 `SHELLDLL_DefView`（图标层）挂到我们自己的窗口下。
//!
//! 思路来自 26H2 的实测事实：
//!
//! - Progman 子窗口放在 `DefView` **之上** → 内容可见，但图标被盖住；
//! - Progman 子窗口放在 `DefView` **之下** → 内容完全不可见（被 DefView 盖住）；
//! - 顶层窗口置底 → 能看见，但同样盖住图标。
//!
//! 也就是说「图标可见」和「壁纸可见」的分界线就在 `DefView` 这一层。
//! 于是反过来：把自己的窗口做成容器，`SetParent(DefView → 自己)`，
//! 让图标层变成我们的子窗口。这样壁纸画在父窗口上，图标画在子窗口上，
//! 两者天然叠在一起，而且不需要和 `Progman` 抢 z 序。
//!
//! 风险：这是跨进程改变 Explorer 的窗口树。任何一步失败都必须把 `DefView`
//! 还原回 `Progman`，否则桌面会没有图标（`explorer.exe` 重启可恢复）。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetParent, GetWindowRect, IsWindow, IsWindowVisible, SetParent, SetWindowPos, HWND_BOTTOM,
    HWND_TOP, SET_WINDOW_POS_FLAGS, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_SHOWWINDOW,
};

use wp_core::PhysicalRect;

use super::probe;
use super::{BandState, DesktopHost, PlatformError, Result, Strategy};
use crate::window::raw;

pub struct DefViewAdopt {
    rect: PhysicalRect,
    /// `HWND` 不是 `Send`，而 `DesktopHost` 要求 `Send`，所以存成 `isize`。
    defview: isize,
    progman: isize,
    host: isize,
    corrections: u64,
}

impl DefViewAdopt {
    pub fn new() -> Result<Self> {
        let progman = probe::progman().ok_or(PlatformError::NoDesktopHost)?;
        let defview = find_defview(progman).ok_or(PlatformError::NoDesktopHost)?;
        Ok(Self {
            rect: PhysicalRect::new(0, 0, 0, 0),
            defview: defview.0 as isize,
            progman: progman.0 as isize,
            host: 0,
            corrections: 0,
        })
    }

    pub fn corrections(&self) -> u64 {
        self.corrections
    }

    /// 把 DefView 挂到 `host` 下并铺满；`host` 为 0 表示还原给 Progman。
    fn adopt(&mut self, host: HWND, rect: PhysicalRect) -> Result<()> {
        let defview = raw(self.defview);
        let target = if host.0.is_null() {
            raw(self.progman)
        } else {
            host
        };

        // 目标父窗口的客户区原点。
        let mut pr = windows::Win32::Foundation::RECT::default();
        unsafe { GetWindowRect(target, &mut pr) }.map_err(|e| win32("GetWindowRect", e))?;
        let (x, y, w, h) = (
            rect.x - pr.left,
            rect.y - pr.top,
            rect.width as i32,
            rect.height as i32,
        );

        let flags = SET_WINDOW_POS_FLAGS(
            SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_SHOWWINDOW.0,
        );
        unsafe { SetParent(defview, Some(target)) }
            .map_err(|e| win32("SetParent(DefView)", e))?;
        let after = if host.0.is_null() { HWND_BOTTOM } else { HWND_TOP };
        unsafe { SetWindowPos(defview, Some(after), x, y, w, h, flags) }
            .map_err(|e| win32("SetWindowPos(DefView)", e))?;
        Ok(())
    }

    /// 还原：DefView 还给 Progman，并铺满整个屏。
    fn restore(&mut self, rect: PhysicalRect) -> Result<()> {
        if self.defview == 0 {
            return Ok(());
        }
        if !unsafe { IsWindow(Some(raw(self.defview))).as_bool() } {
            return Ok(());
        }
        self.adopt(HWND(std::ptr::null_mut()), rect)
    }
}

impl Drop for DefViewAdopt {
    fn drop(&mut self) {
        // 万一上层忘了 detach，至少把图标层还回去，别把桌面留在残废状态。
        let _ = self.restore(self.rect);
    }
}

impl DesktopHost for DefViewAdopt {
    fn strategy(&self) -> Strategy {
        Strategy::DefViewAdopt
    }

    fn attach(&mut self, hwnd: isize, rect: PhysicalRect) -> Result<()> {
        let host = raw(hwnd);
        if !unsafe { IsWindow(Some(host)).as_bool() } {
            return Err(PlatformError::Unsupported("attach 传入的窗口句柄无效"));
        }

        // Explorer 重启会换新句柄，每次 attach 都重新找。
        if let Some(progman) = probe::progman() {
            if let Some(defview) = find_defview(progman) {
                self.progman = progman.0 as isize;
                self.defview = defview.0 as isize;
            }
        }

        self.rect = rect;
        self.host = hwnd;
        self.adopt(host, rect)
    }

    fn ensure_band(&mut self, hwnd: isize) -> Result<BandState> {
        let host = raw(hwnd);
        let defview = raw(self.defview);
        if !unsafe { IsWindow(Some(host)).as_bool() }
            || !unsafe { IsWindow(Some(defview)) }.as_bool()
        {
            return Ok(BandState::Lost);
        }

        // 父窗口不是我们，或者被隐藏了，就重新挂一次。
        let parent = unsafe { GetParent(defview) }.ok();
        let visible = unsafe { IsWindowVisible(defview).as_bool() };
        if parent != Some(host) || !visible {
            self.adopt(host, self.rect)?;
            self.corrections += 1;
            return Ok(BandState::Corrected);
        }
        Ok(BandState::Correct)
    }

    fn detach(&mut self, _hwnd: isize) -> Result<()> {
        let rect = self.rect;
        self.restore(rect)
    }
}

/// 找到图标层：优先 Progman 下的 `SHELLDLL_DefView`，其次任意顶层窗口下的。
fn find_defview(progman: HWND) -> Option<HWND> {
    if let Some(d) = probe::find_child(progman, "SHELLDLL_DefView", None) {
        return Some(d);
    }
    probe::top_level_windows()
        .into_iter()
        .find_map(|h| probe::find_child(h, "SHELLDLL_DefView", None))
}

fn win32(context: &'static str, e: windows::core::Error) -> PlatformError {
    PlatformError::Win32 {
        context,
        code: e.code().0 as u32,
    }
}
