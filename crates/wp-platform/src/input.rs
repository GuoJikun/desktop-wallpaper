//! 输入验证：模拟桌面右键，判断壁纸窗口有没有吃掉桌面输入。
//!
//! 判据不是「命中哪个窗口」（`WindowFromPoint` 会跳过 `WS_EX_TRANSPARENT`，
//! 结论不可靠），而是「右键之后桌面上下文菜单有没有真的弹出来」。

use std::mem::size_of;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    MOUSEINPUT, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSE_EVENT_FLAGS, VIRTUAL_KEY, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetSystemMetrics, IsWindowVisible, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::desktop::{PlatformError, Result};
use crate::window::raw;

/// 弹出菜单的标准窗口类名。
const MENU_CLASS: &str = "#32768";

/// 在屏幕坐标 `(x, y)` 处模拟一次右键单击。
pub fn right_click(x: i32, y: i32) -> Result<()> {
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if vw <= 0 || vh <= 0 {
        return Err(PlatformError::Unsupported("拿不到虚拟屏幕尺寸"));
    }
    // SendInput 的绝对坐标是 0..65535 的归一化值。
    let nx = ((x - vx) as i64 * 65535 / (vw as i64 - 1).max(1)) as i32;
    let ny = ((y - vy) as i64 * 65535 / (vh as i64 - 1).max(1)) as i32;

    let absolute = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE;
    let events = [
        mouse_event(absolute, nx, ny),
        mouse_event(absolute | MOUSEEVENTF_RIGHTDOWN, nx, ny),
        mouse_event(absolute | MOUSEEVENTF_RIGHTUP, nx, ny),
    ];
    let sent = unsafe { SendInput(&events, size_of::<INPUT>() as i32) };
    if sent as usize != events.len() {
        return Err(PlatformError::Unsupported("SendInput 未全部送达"));
    }
    Ok(())
}

/// 按一次 Esc，用于关掉测试弹出来的菜单。
pub fn press_escape() {
    let down = key_event(VK_ESCAPE, KEYBD_EVENT_FLAGS(0));
    let up = key_event(VK_ESCAPE, KEYEVENTF_KEYUP);
    unsafe {
        let _ = SendInput(&[down, up], size_of::<INPUT>() as i32);
    }
}

/// 当前是否有可见的弹出菜单窗口。
pub fn menu_visible() -> bool {
    !new_top_level(&snapshot_top_level()).is_empty()
}

/// 快照当前可见顶层窗口句柄，用于对比右键前后有没有新窗口。
pub fn snapshot_top_level() -> Vec<isize> {
    let mut out = Vec::new();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(snapshot_cb),
            windows::Win32::Foundation::LPARAM(&mut out as *mut Vec<isize> as isize),
        );
    }
    out
}

/// 相对快照新增的可见顶层窗口（类名 + 进程 ID）。
///
/// 桌面右键菜单的窗口类名在 Win11 上并不总是 `#32768`（XAML 菜单会换类），
/// 所以用「新窗口」做判据；再用进程 ID 过滤掉其它应用弹出的窗口。
pub fn new_top_level(before: &[isize]) -> Vec<(String, u32)> {
    let now = snapshot_top_level();
    now.into_iter()
        .filter(|h| !before.contains(h))
        .map(|h| (class_of(h), pid_of(h)))
        .filter(|(c, _)| !c.is_empty())
        .collect()
}

/// 桌面（`Progman`）所在进程，也就是 `explorer.exe` 的 PID。
pub fn desktop_pid() -> Option<u32> {
    let progman = crate::desktop::probe::progman()?;
    Some(pid_of(progman.0 as isize))
}

fn pid_of(hwnd: isize) -> u32 {
    let mut pid = 0u32;
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(raw(hwnd), Some(&mut pid));
    }
    pid
}

unsafe extern "system" fn snapshot_cb(
    hwnd: HWND,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::core::BOOL {
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        let out = &mut *(lparam.0 as *mut Vec<isize>);
        out.push(hwnd.0 as isize);
    }
    windows::core::BOOL(1)
}

/// 销毁窗口前清掉输入状态，避免句柄悬空（当前无状态，占位保持接口稳定）。
pub fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(200));
}

fn mouse_event(flags: MOUSE_EVENT_FLAGS, x: i32, y: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: x,
                dy: y,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_event(key: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: windows::Win32::UI::Input::KeyboardAndMouse::KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// 调试用：收到句柄时打印其类名。
pub fn class_of(hwnd: isize) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(raw(hwnd), &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}
