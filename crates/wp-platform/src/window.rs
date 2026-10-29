//! 宿主窗口的创建、销毁与样式。
//!
//! Windows 相关细节全部收在这里：DPI 感知、窗口类注册、`WM_NCHITTEST` 穿透。
//! 上层只拿到一个 `isize` 句柄，不依赖 `windows` crate 的类型。

use std::mem::size_of;

use windows::core::{PCWSTR, BOOL};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, GetDC, ReleaseDC, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GetClassNameW,
    GetClientRect, GetParent, GetWindowLongPtrW, GetWindowRect, IsWindow, IsWindowVisible,
    PeekMessageW, RegisterClassExW, ShowWindow, TranslateMessage, WindowFromPoint, GWL_EXSTYLE,
    GWL_STYLE, HTTRANSPARENT, MSG, PM_REMOVE, SW_HIDE, SW_MINIMIZE, SW_RESTORE, SW_SHOW,
    WINDOW_EX_STYLE,
    WINDOW_STYLE, WNDCLASSEXW, WNDCLASS_STYLES, WM_ERASEBKGND, WM_NCHITTEST,
};

use wp_core::PhysicalRect;

use crate::desktop::{PlatformError, Result};
use crate::HOST_EX_STYLE;

/// 宿主窗口的窗口样式（WS_*）。
pub const HOST_STYLE: u32 = 0x8000_0000 // WS_POPUP
    | 0x1000_0000 // WS_VISIBLE
    | 0x0400_0000; // WS_CLIPSIBLINGS

/// M0 验证用的纯色（COLORREF，0x00BBGGRR）。颜色够醒目，便于截图判读。
pub const TEST_COLOR: u32 = 0x00_30_30_D0;
/// 上面对应的 RGB（注意 COLORREF 是 BGR 顺序，低字节是红）。
pub const TEST_RGB: [u8; 3] = [0xD0, 0x30, 0x30];

/// 窗口类名。必须是全局唯一的常量，改名等于换一类窗口。
pub const WINDOW_CLASS: &str = "WallpaperHostWindow";

/// `WS_EX_NOREDIRECTIONBITMAP`：不分配重定向表面，内容只能由 DComp 提供。
pub const WS_EX_NOREDIRECTIONBITMAP: u32 = 0x0020_0000;

#[derive(Debug, Clone, Copy)]
pub struct HostWindowSpec {
    pub rect: PhysicalRect,
    pub ex_style: u32,
    pub style: u32,
}

impl HostWindowSpec {
    pub fn for_monitor(rect: PhysicalRect) -> Self {
        Self {
            rect,
            ex_style: HOST_EX_STYLE,
            style: HOST_STYLE,
        }
    }

    /// 打开后 GDI 绘制必然无效，内容必须走 [`crate::DcompSurface`]。
    pub fn with_noredirection_bitmap(mut self, enabled: bool) -> Self {
        if enabled {
            self.ex_style |= WS_EX_NOREDIRECTIONBITMAP;
        } else {
            self.ex_style &= !WS_EX_NOREDIRECTIONBITMAP;
        }
        self
    }
}

/// 创建壁纸宿主窗口，返回原始 `HWND`（以 `isize` 表示）。
///
/// 步骤：
/// 1. 进程线程切到 `PER_MONITOR_AWARE_V2`，避免混合 DPI 下尺寸被系统缩放
/// 2. 注册窗口类（重复注册视为成功）
/// 3. `CreateWindowExW`，父窗口为 `None`（顶层窗口）
///
/// 窗口此时是可见但位于最顶层的，层级由 `desktop::Strategy` 负责。
pub fn create_host_window(spec: &HostWindowSpec) -> Result<isize> {
    unsafe {
        set_dpi_awareness();
        let module = GetModuleHandleW(None)
            .map_err(|e| win32("GetModuleHandleW", e.code().0 as u32))?;
        let instance = HINSTANCE(module.0);

        register_class(instance)?;

        let class = wide(WINDOW_CLASS);
        let title = wide("wallpaper host");
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(spec.ex_style),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(spec.style),
            spec.rect.x,
            spec.rect.y,
            spec.rect.width as i32,
            spec.rect.height as i32,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|e| win32("CreateWindowExW", e.code().0 as u32))?;

        Ok(hwnd.0 as isize)
    }
}

/// 销毁宿主窗口。句柄已失效时静默成功——看门狗会重复调用。
pub fn destroy_host_window(hwnd: isize) {
    unsafe {
        let _ = DestroyWindow(raw(hwnd));
    }
}

/// 句柄是否仍然有效。
pub fn is_host_window_alive(hwnd: isize) -> bool {
    unsafe { IsWindow(Some(raw(hwnd))).as_bool() }
}

/// 把 `isize` 还原成 `HWND`。仅在本 crate 内部使用。
pub(crate) fn raw(hwnd: isize) -> HWND {
    HWND(hwnd as *mut std::ffi::c_void)
}

/// 用 GDI 把窗口客户区刷成纯色。
///
/// 只用于 M0 的可见性验证（确认层级对不对、有没有被图标盖住）。
/// 正式渲染必须走 D3D11 / DirectComposition，见 AGENTS.md 硬规则 2。
pub fn paint_solid(hwnd: isize, color: u32) -> Result<()> {
    unsafe {
        let w = raw(hwnd);
        let mut rc = RECT::default();
        GetClientRect(w, &mut rc).map_err(|e| win32("GetClientRect", e.code().0 as u32))?;

        let hdc = GetDC(Some(w));
        if hdc.0.is_null() {
            return Err(win32("GetDC", 0));
        }
        let brush = CreateSolidBrush(COLORREF(color));
        if !brush.0.is_null() {
            FillRect(hdc, &rc, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
        ReleaseDC(Some(w), hdc);
        Ok(())
    }
}

/// 抽干当前线程的消息队列，返回处理的消息数。
///
/// 没有真正的消息循环时，`WM_NCHITTEST` 这类 `SendMessage` 仍会被派发，
/// 但 `WM_PAINT` / `WM_TIMER`（posted 消息）不会，所以必须主动抽。
pub fn pump_messages() -> u32 {
    let mut handled = 0u32;
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            handled += 1;
        }
    }
    handled
}

thread_local! {
    static MINIMIZED: std::cell::RefCell<Vec<isize>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// 最小化所有可见的普通顶层窗口，返回被最小化的句柄。
///
/// 只为 M0 的截图验证服务：先把桌面亮出来，再挂壁纸。
/// 桌面壳层（Progman / 任务栏 / WorkerW）和工具窗口会被跳过。
pub fn minimize_other_windows() -> Vec<isize> {
    MINIMIZED.with(|v| v.borrow_mut().clear());
    unsafe {
        let _ = EnumWindows(Some(minimize_cb), LPARAM(0));
    }
    MINIMIZED.with(|v| v.borrow().clone())
}

/// 恢复 [`minimize_other_windows`] 最小化的窗口。
pub fn restore_windows(handles: &[isize]) {
    unsafe {
        for &h in handles {
            let _ = ShowWindow(raw(h), SW_RESTORE);
        }
    }
}

thread_local! {
    static HIDDEN: std::cell::RefCell<Vec<isize>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// 直接把其它窗口藏起来（比最小化更彻底：不受最小化动画和应用自恢复影响）。
///
/// 截图与输入验证都需要「桌面真的在最前面」，最小化对某些应用不可靠。
pub fn hide_other_windows() -> Vec<isize> {
    HIDDEN.with(|v| v.borrow_mut().clear());
    unsafe {
        let _ = EnumWindows(Some(hide_cb), LPARAM(0));
    }
    HIDDEN.with(|v| v.borrow().clone())
}

/// 恢复 [`hide_other_windows`] 藏起来的窗口。
pub fn show_windows(handles: &[isize]) {
    unsafe {
        for &h in handles {
            let _ = ShowWindow(raw(h), SW_SHOW);
        }
    }
}

unsafe extern "system" fn hide_cb(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    if should_skip(hwnd) {
        return BOOL(1);
    }
    let _ = ShowWindow(hwnd, SW_HIDE);
    HIDDEN.with(|v| v.borrow_mut().push(hwnd.0 as isize));
    BOOL(1)
}

/// 桌面壳层、工具窗口和宿主自己都不能动。
fn should_skip(hwnd: HWND) -> bool {
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return true;
    }
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    if ex_style & 0x0000_0080 != 0 {
        return true;
    }
    matches!(
        class_name(hwnd).as_str(),
        "Progman" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "WorkerW" | WINDOW_CLASS
    )
}

unsafe extern "system" fn minimize_cb(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    if should_skip(hwnd) {
        return BOOL(1);
    }
    let _ = ShowWindow(hwnd, SW_MINIMIZE);
    MINIMIZED.with(|v| v.borrow_mut().push(hwnd.0 as isize));
    BOOL(1)
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 屏幕坐标处最顶层窗口的类名。用于判断某点是否被壁纸窗口吃掉。
pub fn class_at_point(x: i32, y: i32) -> String {
    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.0.is_null() {
            return String::from("(none)");
        }
        let name = class_name(hwnd);
        if name.is_empty() {
            format!("{hwnd:?}")
        } else {
            name
        }
    }
}

/// 打印一个窗口的结构化状态，用于排查「attach 成功但画面不可见」。
pub fn describe(hwnd: isize) -> String {
    let h = raw(hwnd);
    unsafe {
        let visible = IsWindowVisible(h).as_bool();
        let parent = GetParent(h).ok();
        let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        let mut r = RECT::default();
        let _ = GetWindowRect(h, &mut r);
        format!(
            "visible={visible} parent={parent:?} style=0x{style:08X} exstyle=0x{ex_style:08X} \
             rect=({},{},{},{})",
            r.left, r.top, r.right, r.bottom
        )
    }
}

fn set_dpi_awareness() {
    unsafe {
        // 返回值是旧 context，不关心；失败也无害（进程级 manifest 可能已设置）。
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn register_class(instance: HINSTANCE) -> Result<()> {
    unsafe {
        let class = wide(WINDOW_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(host_wnd_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };

        if RegisterClassExW(&wc) != 0 {
            return Ok(());
        }

        // 多显示器各起一个进程时，第二个进程注册同名类会失败；这不是错误。
        let err = GetLastError();
        if err == ERROR_CLASS_ALREADY_EXISTS {
            return Ok(());
        }
        Err(win32("RegisterClassExW", err.0))
    }
}

unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // 关键：让鼠标事件全部落到下层（桌面图标 / 右键菜单）。
        // 只加 WS_EX_TRANSPARENT 不够，实测 WindowFromPoint 仍会命中本窗口。
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        // 不擦背景，避免闪烁并让 D3D/DComp 表面完全接管绘制。
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win32(context: &'static str, code: u32) -> PlatformError {
    PlatformError::Win32 { context, code }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_has_no_activate_and_toolwindow() {
        let spec = HostWindowSpec::for_monitor(PhysicalRect::new(0, 0, 1920, 1080));
        assert_ne!(spec.ex_style & 0x0800_0000, 0, "缺少 WS_EX_NOACTIVATE");
        assert_ne!(spec.ex_style & 0x0000_0080, 0, "缺少 WS_EX_TOOLWINDOW");
        assert_ne!(spec.ex_style & 0x0000_0020, 0, "缺少 WS_EX_TRANSPARENT");
        assert_ne!(spec.style & 0x8000_0000, 0, "缺少 WS_POPUP");
    }

    #[test]
    fn test_rgb_matches_colorref_byte_order() {
        let c = TEST_COLOR;
        let expected = [c as u8, (c >> 8) as u8, (c >> 16) as u8];
        assert_eq!(TEST_RGB, expected, "COLORREF 是 0x00BBGGRR，低字节为红");
    }

    /// 真实创建/销毁一次窗口，验证窗口类注册与样式没有拼错。
    /// 没有桌面会话（CI）时会创建失败，此时跳过而不是失败。
    #[test]
    fn create_and_destroy_host_window() {
        let spec = HostWindowSpec::for_monitor(PhysicalRect::new(0, 0, 320, 200));
        let Ok(hwnd) = create_host_window(&spec) else {
            eprintln!("跳过：当前会话无法创建窗口");
            return;
        };
        assert!(is_host_window_alive(hwnd));
        destroy_host_window(hwnd);
        assert!(!is_host_window_alive(hwnd), "销毁后句柄应失效");
    }
}
