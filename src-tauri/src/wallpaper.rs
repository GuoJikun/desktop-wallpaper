#[cfg(target_os = "windows")]
mod windows_wallpaper {
    use std::ptr::null_mut;
    use tauri::WebviewWindow;
    use winapi::um::winuser::{
        EnumWindows, FindWindowExW, FindWindowW, GetWindowLongPtrW, SendMessageTimeoutW, SetParent, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_STYLE, HWND_TOP, SMTO_NORMAL, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOW, WS_CHILD, WS_VISIBLE
    };
    use winapi::shared::windef::HWND;
    use winapi::shared::minwindef::{LPARAM, BOOL};

    const WM_SHELLHOOK: u32 = 0x052C;

    pub fn set_wallpaper(window: WebviewWindow) {
        unsafe {
            // Find the Progman window
            let progman = FindWindowW("Progman\0".as_ptr() as *const _, null_mut());
            SendMessageTimeoutW(progman, WM_SHELLHOOK, 0, 0, SMTO_NORMAL, 1000, null_mut());

            let mut workerw: HWND = null_mut();
            EnumWindows(Some(enum_windows_proc), &mut workerw as *mut _ as LPARAM);

            if workerw != null_mut() {
                match window.hwnd() {
                    Ok(hwnd) => {
                        let hwnd_ptr = hwnd.0 as HWND;

                        // Change the window style to WS_CHILD and WS_VISIBLE
                        let style = GetWindowLongPtrW(hwnd_ptr, GWL_STYLE);
                        SetWindowLongPtrW(hwnd_ptr, GWL_STYLE, style | WS_CHILD as isize | WS_VISIBLE as isize);

                        // Set the window's parent to the WorkerW window
                        SetParent(hwnd_ptr, workerw);

                        // Position the window
                        SetWindowPos(hwnd_ptr, HWND_TOP, 0, 0, 1920, 1080, SWP_NOACTIVATE | SWP_SHOWWINDOW);

                        // Ensure the window is visible
                        ShowWindow(hwnd_ptr, SW_SHOW);
                    },
                    Err(e) => eprintln!("Failed to get HWND from window: {:?}", e),
                }
            } else {
                eprintln!("Failed to find WorkerW window.");
            }
        }
    }

    extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let shell = FindWindowExW(hwnd, null_mut(), "SHELLDLL_DefView\0".as_ptr() as *const _, null_mut());
            if shell != null_mut() {
                let workerw = FindWindowExW(null_mut(), hwnd, "WorkerW\0".as_ptr() as *const _, null_mut());
                if workerw != null_mut() {
                    *(lparam as *mut HWND) = workerw;
                    return 0;
                }
            }
        }
        1
    }
}

#[cfg(target_os = "linux")]
mod linux_wallpaper {
    use std::process::Command;
    use tauri::Window;

    pub fn set_wallpaper(window: Window) {
        if let Ok(hwnd) = window.gtk_window() {
            let gtk_window_id = hwnd.as_ref().unwrap().as_ptr() as u64;
            window.set_fullscreen(true).unwrap();
            
            let output = Command::new("sh")
                .arg("-c")
                .arg(format!("wmctrl -r :ACTIVE: -b add,below,sticky,skip_taskbar,skip_pager; xdotool windowunmap {gtk_window_id}; xdotool windowmap {gtk_window_id}"))
                .output()
                .expect("Failed to execute command");
            
            if !output.status.success() {
                eprintln!("Failed to set wallpaper: {}", String::from_utf8_lossy(&output.stderr));
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows_wallpaper::set_wallpaper;

#[cfg(target_os = "linux")]
pub use linux_wallpaper::set_wallpaper;
