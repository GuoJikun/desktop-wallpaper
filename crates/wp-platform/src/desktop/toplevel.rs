//! 策略 B：顶层窗口置于全局 z 序最底部。
//!
//! 26H2 实测结论：`Progman` 自己持有 `SHELLDLL_DefView`（图标层），
//! 因此壁纸窗口既不能当 Progman 的子窗口（不渲染），也不能盖在图标上面。
//! 唯一稳定的做法是把顶层窗口压到 z 序最底——低于 Progman，
//! 于是 Progman 连同图标仍然画在壁纸之上。
//!
//! 代价：z 序会被系统（前台切换、`Win+D`、Explorer 重启等）改回，
//! 必须由看门狗每 ~2 s 调一次 [`TopLevelBottom::ensure_band`]。

#![allow(dead_code)]

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetTopWindow, GetWindow, IsWindow, SetWindowPos, GW_HWNDNEXT, HWND_BOTTOM, SET_WINDOW_POS_FLAGS,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_SHOWWINDOW,
};

use wp_core::PhysicalRect;

use super::{BandState, DesktopHost, PlatformError, Result, Strategy};
use crate::desktop::probe;
use crate::window::raw;

pub struct TopLevelBottom {
    /// 层级锚点（`Progman` 的原始句柄）。以 `isize` 存储，
    /// 这样 `TopLevelBottom` 保持 `Send`（`HWND` 内含裸指针，不实现 `Send`）。
    anchor: isize,
    /// 目标矩形，`attach` 时记录，`ensure_band` 修正时复用。
    rect: PhysicalRect,
    /// 累计修正次数，用于判断策略是否不稳定。
    corrections: u64,
}

impl TopLevelBottom {
    pub fn new() -> Result<Self> {
        let anchor = probe::progman().ok_or(PlatformError::NoDesktopHost)?;
        Ok(Self {
            anchor: anchor.0 as isize,
            rect: PhysicalRect::new(0, 0, 0, 0),
            corrections: 0,
        })
    }

    /// 累计把窗口重新压回底部的次数。数值持续增长说明该策略不稳。
    pub fn corrections(&self) -> u64 {
        self.corrections
    }

    fn place(&self, hwnd: HWND) -> Result<()> {
        let flags = SET_WINDOW_POS_FLAGS(
            SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_SHOWWINDOW.0,
        );
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_BOTTOM),
                self.rect.x,
                self.rect.y,
                self.rect.width as i32,
                self.rect.height as i32,
                flags,
            )
        }
        .map_err(|e| PlatformError::Win32 {
            context: "SetWindowPos(HWND_BOTTOM)",
            code: e.code().0 as u32,
        })
    }
}

impl DesktopHost for TopLevelBottom {
    fn strategy(&self) -> Strategy {
        Strategy::TopLevelBottom
    }

    fn attach(&mut self, hwnd: isize, rect: PhysicalRect) -> Result<()> {
        let hwnd = raw(hwnd);
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Err(PlatformError::Unsupported("attach 传入的窗口句柄无效"));
        }
        self.rect = rect;
        // Explorer 重启后 Progman 会换新句柄，每次 attach 都重新取。
        self.anchor = probe::progman()
            .ok_or(PlatformError::NoDesktopHost)?
            .0 as isize;
        self.place(hwnd)
    }

    fn ensure_band(&mut self, hwnd: isize) -> Result<BandState> {
        let hwnd = raw(hwnd);
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return Ok(BandState::Lost);
        }
        // 锚点可能已失效（Explorer 重启），失效时重新取。
        if !unsafe { IsWindow(Some(raw(self.anchor))).as_bool() } {
            self.anchor = probe::progman()
                .ok_or(PlatformError::NoDesktopHost)?
                .0 as isize;
        }
        let anchor = raw(self.anchor);

        if is_at_bottom_of_band(hwnd, anchor) {
            return Ok(BandState::Correct);
        }

        self.place(hwnd)?;
        self.corrections += 1;

        Ok(if is_at_bottom_of_band(hwnd, anchor) {
            BandState::Corrected
        } else {
            // 压到最底了仍然不在锚点正上方，说明有别的窗口在干扰，需要重建。
            BandState::Lost
        })
    }

    fn detach(&mut self, _hwnd: isize) -> Result<()> {
        // 层级无需恢复：窗口随后会被销毁。保留接口以便未来支持热切换。
        Ok(())
    }
}

/// `a` 是否紧贴在桌面窗口 `anchor` 上方（z 序最底层的常规窗口）。
///
/// 注意：`Progman` 永远占据 z 链最后一个位置，`HWND_BOTTOM` 也无法把窗口
/// 放到它下面——实测压到底后索引恰好是 `anchor - 1`。因此「层级正确」
/// 的定义是「紧贴桌面上方且下面没有别的窗口」，而不是「在桌面下方」。
pub(crate) fn is_at_bottom_of_band(a: HWND, anchor: HWND) -> bool {
    match (z_index(a), z_index(anchor)) {
        (Some(ia), Some(ib)) => ia + 1 == ib,
        // 任一方不在顶层 z 链里，视为层级未知。
        _ => false,
    }
}

/// 在顶层窗口 z 链中的位置：0 最靠上，依次往下。
pub(crate) fn z_index(target: HWND) -> Option<u32> {
    unsafe {
        let mut cur = GetTopWindow(None).ok()?;
        let mut index = 0u32;
        loop {
            if cur == target {
                return Some(index);
            }
            cur = GetWindow(cur, GW_HWNDNEXT).ok()?;
            index += 1;
            if index > 4096 {
                // 链异常（正常桌面不会这么长），放弃判断。
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_index_of_progman_is_stable() {
        let Some(p) = probe::progman() else {
            eprintln!("跳过：没有 Progman");
            return;
        };
        let first = z_index(p).expect("Progman 应在顶层 z 链中");
        let second = z_index(p).expect("Progman 应在顶层 z 链中");
        assert_eq!(first, second);
    }

    #[test]
    fn same_window_is_not_at_bottom_of_band() {
        let Some(p) = probe::progman() else {
            return;
        };
        assert!(!is_at_bottom_of_band(p, p));
    }
}
