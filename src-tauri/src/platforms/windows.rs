use std::ptr::null_mut;

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{BOOL, HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging,
    },
};

extern "system" fn enum_window(window: HWND, ref_worker_w: LPARAM) -> BOOL {
    unsafe {
        // 查找 defView 窗口
        let shell_dll_def_view_name = PCWSTR::from_raw(
            "SHELLDLL_DefView\0"
                .encode_utf16()
                .collect::<Vec<u16>>()
                .as_ptr(),
        );
        let shell_dll_def_view =
            WindowsAndMessaging::FindWindowExW(window, None, shell_dll_def_view_name, None);

        match shell_dll_def_view {
            Err(_) => {
                return true.into();
            }
            Ok(shell_dll_def_view) => {
                if shell_dll_def_view == HWND(null_mut()) {
                    return true.into(); // 继续枚举
                }
                println!("Found SHELLDLL_DefView window: {:?}", shell_dll_def_view);
                let worker_w_name =
                    PCWSTR::from_raw("WorkerW\0".encode_utf16().collect::<Vec<u16>>().as_ptr());

                let worker_w =
                    WindowsAndMessaging::FindWindowExW(window, None, worker_w_name, None);

                match worker_w {
                    Err(_) => {
                        return true.into();
                    }
                    Ok(worker_w) => {
                        if worker_w == HWND(null_mut()) {
                            return true.into(); // 继续枚举
                        }
                        *(ref_worker_w.0 as *mut HWND) = worker_w;
                        return false.into(); // 停止枚举
                    }
                }
            }
        }
    }
}

pub fn attach(hwnd: HWND) {
    unsafe {
        let progman_name =
            PCWSTR::from_raw("Progman\0".encode_utf16().collect::<Vec<u16>>().as_ptr());
        let progman = WindowsAndMessaging::FindWindowW(progman_name, PCWSTR::null()).unwrap();

        if progman == HWND(null_mut()) {
            panic!("Could not find progman window");
        }

        WindowsAndMessaging::SendMessageW(progman, 0x052C, WPARAM(0x0000000D), LPARAM(0));
        WindowsAndMessaging::SendMessageW(progman, 0x052C, WPARAM(0x0000000D), LPARAM(1));

        let mut worker_w: HWND = HWND(null_mut());

        let _ = WindowsAndMessaging::EnumWindows(
            Some(enum_window),
            LPARAM(&mut worker_w as *mut HWND as isize),
        );

        if worker_w == HWND(null_mut()) {
            panic!("Could not find worker_w window");
        }

        let _ = WindowsAndMessaging::SetParent(hwnd, worker_w);
    };
}

pub fn detach(hwnd: HWND) {
    unsafe {
        let _ = WindowsAndMessaging::SetParent(hwnd, None);
        let _ = WindowsAndMessaging::SystemParametersInfoA(
            WindowsAndMessaging::SPI_SETDESKWALLPAPER,
            0,
            Some(null_mut()),
            WindowsAndMessaging::SPIF_UPDATEINIFILE,
        );
    };
}
