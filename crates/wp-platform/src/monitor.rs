//! 显示器枚举。使用设备名作为稳定键，避免热插拔后索引漂移。

use std::cell::RefCell;
use std::mem::size_of;

use serde::{Deserialize, Serialize};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};

use wp_core::{MonitorKey, PhysicalRect};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub key: MonitorKey,
    pub device: String,
    pub rect: PhysicalRect,
    pub primary: bool,
}

thread_local! {
    static FOUND: RefCell<Vec<HMONITOR>> = const { RefCell::new(Vec::new()) };
}

pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    FOUND.with(|f| f.borrow_mut().clear());
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_cb), LPARAM(0));
    }

    FOUND.with(|f| {
        f.borrow()
            .iter()
            .map(|h| read_monitor(*h))
            .collect::<Vec<_>>()
    })
}

fn read_monitor(hmon: HMONITOR) -> MonitorInfo {
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;

    unsafe {
        let _ = GetMonitorInfoW(hmon, &mut mi.monitorInfo as *mut MONITORINFO);
    }

    let device = String::from_utf16_lossy(
        &mi.szDevice
            .iter()
            .copied()
            .take_while(|c| *c != 0)
            .collect::<Vec<u16>>(),
    );

    let r = mi.monitorInfo.rcMonitor;
    MonitorInfo {
        key: MonitorKey(device.clone()),
        device,
        rect: PhysicalRect::new(r.left, r.top, (r.right - r.left) as u32, (r.bottom - r.top) as u32),
        primary: (mi.monitorInfo.dwFlags & 1) != 0,
    }
}

unsafe extern "system" fn monitor_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    _data: LPARAM,
) -> BOOL {
    FOUND.with(|f| f.borrow_mut().push(hmon));
    TRUE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_at_least_one_monitor() {
        let monitors = enumerate_monitors();
        assert!(!monitors.is_empty(), "至少应有一个显示器");
        assert!(monitors.iter().any(|m| m.primary), "应有主显示器");
        for m in &monitors {
            assert!(!m.device.is_empty());
            assert!(m.rect.width > 0 && m.rect.height > 0);
        }
    }
}
