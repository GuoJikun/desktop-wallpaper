//! 运行时探测：确认本机桌面窗口结构与可用宿主策略。
//!
//! 探测只读，除 `spawn_worker` 会向 `Progman` 发送一次 `0x052C`（系统允许的公开行为）外，
//! 不修改任何桌面状态。

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use windows::core::{PCWSTR, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetWindowTextW,
    IsWindowVisible,
};

use super::Strategy;

const WM_SPAWN_WORKER: u32 = 0x052C;
const SPAWN_WORKER_WPARAM: usize = 0x0D;

thread_local! {
    static COLLECT: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerWInfo {
    pub hwnd: String,
    pub visible: bool,
    pub child_count: usize,
    pub has_defview: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeReport {
    pub progman: Option<String>,
    pub progman_title: Option<String>,
    pub defview_under_progman: bool,
    pub workerw: Vec<WorkerWInfo>,
    /// 是否存在「包含 SHELLDLL_DefView 的 WorkerW」——旧系统的经典宿主。
    pub workerw_with_defview: Option<String>,
    /// `0x052C` 返回值（None 表示没有 Progman，未发送）。
    pub spawn_worker_result: Option<i64>,
    /// 发送 `0x052C` 之后是否出现了新的宿主。
    pub host_after_spawn: Option<String>,
    /// 推荐尝试顺序。
    pub recommended: Vec<String>,
}

pub fn probe() -> ProbeReport {
    let progman = find_window("Progman", None);
    let progman_title = progman.map(title_of);
    let defview_under_progman = progman
        .map(|p| find_child(p, "SHELLDLL_DefView", None).is_some())
        .unwrap_or(false);

    let workerw = collect_workerw();
    let workerw_with_defview = workerw
        .iter()
        .find(|w| w.has_defview)
        .map(|w| w.hwnd.clone());

    // 只有旧系统才需要这个握手；新系统上它通常静默无效。
    let spawn_worker_result = progman.map(|p| spawn_worker(p));
    let host_after_spawn = if spawn_worker_result.is_some() {
        collect_workerw()
            .into_iter()
            .find(|w| w.has_defview)
            .map(|w| w.hwnd)
    } else {
        None
    };

    let recommended = recommend(&workerw_with_defview, defview_under_progman);

    ProbeReport {
        progman: progman.map(|h| format!("{h:?}")),
        progman_title,
        defview_under_progman,
        workerw,
        workerw_with_defview,
        spawn_worker_result,
        host_after_spawn,
        recommended,
    }
}

fn recommend(workerw_with_defview: &Option<String>, defview_under_progman: bool) -> Vec<String> {
    if workerw_with_defview.is_some() {
        return vec![Strategy::WorkerW.as_str().to_string()];
    }
    if defview_under_progman {
        // 26H2 布局：Progman 持有 DefView，WorkerW 不再可用。
        return vec![
            Strategy::ProgmanChild.as_str().to_string(),
            Strategy::TopLevelAnchored.as_str().to_string(),
            Strategy::TopLevelBottom.as_str().to_string(),
        ];
    }
    vec![Strategy::TopLevelBottom.as_str().to_string()]
}

fn collect_workerw() -> Vec<WorkerWInfo> {
    top_level_windows()
        .into_iter()
        .filter(|h| class_of(*h) == "WorkerW")
        .map(|h| {
            let children = child_windows(h);
            WorkerWInfo {
                hwnd: format!("{h:?}"),
                visible: unsafe { IsWindowVisible(h).as_bool() },
                child_count: children.len(),
                has_defview: children.iter().any(|c| class_of(*c) == "SHELLDLL_DefView"),
            }
        })
        .collect()
}

fn spawn_worker(progman: HWND) -> i64 {
    unsafe {
        let mut result: usize = 0;
        let ok = windows::Win32::UI::WindowsAndMessaging::SendMessageTimeoutW(
            progman,
            WM_SPAWN_WORKER,
            WPARAM(SPAWN_WORKER_WPARAM),
            LPARAM(0),
            windows::Win32::UI::WindowsAndMessaging::SMTO_NORMAL,
            2000,
            Some(&mut result),
        );
        if ok.0 == 0 {
            -1
        } else {
            result as i64
        }
    }
}

fn top_level_windows() -> Vec<HWND> {
    COLLECT.with(|c| c.borrow_mut().clear());
    unsafe {
        let _ = EnumWindows(Some(collect_cb), LPARAM(0));
    }
    COLLECT.with(|c| c.borrow().clone())
}

fn child_windows(parent: HWND) -> Vec<HWND> {
    COLLECT.with(|c| c.borrow_mut().clear());
    unsafe {
        let _ = EnumChildWindows(Some(parent), Some(collect_cb), LPARAM(0));
    }
    COLLECT.with(|c| c.borrow().clone())
}

unsafe extern "system" fn collect_cb(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    COLLECT.with(|c| c.borrow_mut().push(hwnd));
    BOOL(1)
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn title_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 当前会话的 `Progman` 句柄。Explorer 重启后会换新句柄，不要缓存。
pub(crate) fn progman() -> Option<HWND> {
    find_window("Progman", None)
}

pub(crate) fn find_window(class: &str, title: Option<&str>) -> Option<HWND> {
    let cls = wide(class);
    let title = title.map(wide);
    unsafe {
        FindWindowW(
            PCWSTR(cls.as_ptr()),
            title
                .as_ref()
                .map(|t| PCWSTR(t.as_ptr()))
                .unwrap_or(PCWSTR::null()),
        )
    }
    .ok()
}

fn find_child(parent: HWND, class: &str, title: Option<&str>) -> Option<HWND> {
    let cls = wide(class);
    let title = title.map(wide);
    unsafe {
        FindWindowExW(
            Some(parent),
            None,
            PCWSTR(cls.as_ptr()),
            title
                .as_ref()
                .map(|t| PCWSTR(t.as_ptr()))
                .unwrap_or(PCWSTR::null()),
        )
    }
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommend_prefers_workerw_on_old_layout() {
        let r = recommend(&Some("0x1".into()), true);
        assert_eq!(r, vec!["workerw".to_string()]);
    }

    #[test]
    fn recommend_progman_child_on_26h2_layout() {
        let r = recommend(&None, true);
        assert_eq!(
            r,
            vec![
                "progman_child".to_string(),
                "toplevel_anchored".to_string(),
                "toplevel_bottom".to_string()
            ]
        );
    }
}
