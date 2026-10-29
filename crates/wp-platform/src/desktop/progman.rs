//! 策略 A：把宿主窗口挂成 `Progman` 的子窗口。
//!
//! 26H2 上 `Progman` 自己持有 `SHELLDLL_DefView`（图标层），所以把壁纸做成
//! Progman 的子窗口、并压在 DefView **下面**，理论上就能既不挡图标、也不抢输入。
//!
//! 已知风险：早期用 PowerShell 试过 Progman 子窗口 + GDI，画面不显示（黑屏）。
//! 这里用同一套截图验证重测一次，确认是「GDI 画不进去」还是「窗口根本没进合成」。
//! 如果 GDI 确实不行，下一步换 DirectComposition 再测一格。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetParent, GetWindowLongPtrW, GetWindowRect, IsWindow, SetParent, SetWindowLongPtrW, SetWindowPos,
    GWL_STYLE, HWND_BOTTOM, HWND_TOP, SET_WINDOW_POS_FLAGS, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
    SWP_SHOWWINDOW, WS_CHILD, WS_POPUP,
};

use wp_core::PhysicalRect;

use super::{BandState, DesktopHost, PlatformError, Result, Strategy};
use crate::desktop::probe;
use crate::window::raw;

pub struct ProgmanChild {
    rect: PhysicalRect,
    corrections: u64,
    /// 放在 DefView 之上（`true`）还是之下（`false`，正常用法）。
    /// 只是为了做判别实验：如果「之上」能看见、「之下」看不见，
    /// 说明子窗口合成正常，是 DefView 把它盖住了。
    above_defview: bool,
}

impl ProgmanChild {
    pub fn new() -> Result<Self> {
        Self::with_options(false)
    }

    /// 判别实验用：把宿主放在 `SHELLDLL_DefView` 之上。
    pub fn above_defview() -> Result<Self> {
        Self::with_options(true)
    }

    fn with_options(above_defview: bool) -> Result<Self> {
        // 提前确认桌面窗口存在，免得 attach 时才报错。
        probe::progman().ok_or(PlatformError::NoDesktopHost)?;
        Ok(Self {
            rect: PhysicalRect::new(0, 0, 0, 0),
            corrections: 0,
            above_defview,
        })
    }

    pub fn corrections(&self) -> u64 {
        self.corrections
    }

    /// 子窗口坐标是相对父窗口客户区的，必须把屏幕坐标换算过去。
    fn child_rect(&self, progman: HWND) -> Result<(i32, i32, i32, i32)> {
        let mut pr = windows::Win32::Foundation::RECT::default();
        unsafe { GetWindowRect(progman, &mut pr) }.map_err(|e| win32("GetWindowRect", e))?;
        Ok((
            self.rect.x - pr.left,
            self.rect.y - pr.top,
            self.rect.width as i32,
            self.rect.height as i32,
        ))
    }

    fn reparent(&self, hwnd: HWND, progman: HWND) -> Result<()> {
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        let child_style = (style & !WS_POPUP.0) | WS_CHILD.0;
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_STYLE, child_style as isize);
        }

        let (x, y, w, h) = self.child_rect(progman)?;
        let flags = SET_WINDOW_POS_FLAGS(SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_SHOWWINDOW.0);
        unsafe { SetParent(hwnd, Some(progman)) }.map_err(|e| win32("SetParent", e))?;
        // 正常用法放到兄弟链最底（DefView 下面）；判别实验放到最顶。
        let insert_after = if self.above_defview {
            HWND_TOP
        } else {
            HWND_BOTTOM
        };
        unsafe { SetWindowPos(hwnd, Some(insert_after), x, y, w, h, flags) }
            .map_err(|e| win32("SetWindowPos(child)", e))?;
        Ok(())
    }
}

impl DesktopHost for ProgmanChild {
    fn strategy(&self) -> Strategy {
        Strategy::ProgmanChild
    }

    fn attach(&mut self, hwnd: isize, rect: PhysicalRect) -> Result<()> {
        let hwnd = raw(hwnd);
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Err(PlatformError::Unsupported("attach 传入的窗口句柄无效"));
        }
        self.rect = rect;
        let progman = probe::progman().ok_or(PlatformError::NoDesktopHost)?;
        self.reparent(hwnd, progman)
    }

    fn ensure_band(&mut self, hwnd: isize) -> Result<BandState> {
        let hwnd = raw(hwnd);
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Ok(BandState::Lost);
        }
        let Some(progman) = probe::progman() else {
            return Ok(BandState::Lost);
        };

        // 父窗口被改掉（Explorer 重启换 Progman）时重新挂一次。
        let parent = unsafe { GetParent(hwnd) }.ok();
        if parent != Some(progman) {
            self.reparent(hwnd, progman)?;
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
        // 归还给桌面，否则销毁时会被当成 Progman 的子窗口。
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
