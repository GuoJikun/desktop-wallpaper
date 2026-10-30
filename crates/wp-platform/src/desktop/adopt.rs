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
//! 26H2 实测结果：壁纸铺满、图标正常显示、右键菜单/单击/双击全部正常。
//!
//! 风险：这是跨进程改变 Explorer 的窗口树。任何一步失败都必须把 `DefView`
//! 还原回 `Progman`，否则桌面会没有图标（`explorer.exe` 重启可恢复）。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetParent, GetWindowRect, IsWindow, IsWindowVisible, SetParent, SetWindowPos, HWND_BOTTOM,
    HWND_TOP, SET_WINDOW_POS_FLAGS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW,
};

use wp_core::PhysicalRect;

use super::probe;
use super::{BandState, DesktopHost, PlatformError, Result, Strategy};
use crate::window::raw;

/// Explorer 重启后，新的 `SHELLDLL_DefView` 是**不透明**的：挂在它前面的壁纸会被整个盖住。
/// 实测（26H2，杀 Explorer 后截图）：
///
/// | 处理方式 | 测试色占比 |
/// |---|---|
/// | 沿用旧宿主窗口，等 5 s / 15 s 再接管 | 0.0%（把图标层缩小后立刻 89.8%，证明壁纸本身在渲染） |
/// | 进程内重建宿主窗口后再接管 | 0.0% |
/// | 整个进程重启后再接管 | **94.6%**（图标正常） |
///
/// 唯一有效的是让 Explorer 看到「图标层的父进程已经死了」，它才会重新初始化背景。
/// 所以这一格必须交给上层：平台层报 [`BandState::Lost`]，由 supervisor 重启 `wp-host`。
pub struct DefViewAdopt {
    rect: PhysicalRect,
    /// `HWND` 不是 `Send`，而 `DesktopHost` 要求 `Send`，所以存成 `isize`。
    defview: isize,
    progman: isize,
    host: isize,
    corrections: u64,
    /// 连续「修正后仍不在底部」的次数。单次失败多半是过渡窗口插在中间，
    /// 连续失败才说明层级真的救不回来。
    stuck: u32,
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
            stuck: 0,
        })
    }

    pub fn corrections(&self) -> u64 {
        self.corrections
    }

    /// 把 DefView 挂到 `host` 下并铺满；`host` 为空表示还原给 Progman。
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

        let flags =
            SET_WINDOW_POS_FLAGS(SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_SHOWWINDOW.0);
        unsafe { SetParent(defview, Some(target)) }.map_err(|e| win32("SetParent(DefView)", e))?;
        let after = if host.0.is_null() {
            HWND_BOTTOM
        } else {
            HWND_TOP
        };
        unsafe { SetWindowPos(defview, Some(after), x, y, w, h, flags) }
            .map_err(|e| win32("SetWindowPos(DefView)", e))?;
        // 接管期间留一份记录：万一进程被强杀，下一次启动才知道「图标层是被我们弄丢的」。
        if host.0.is_null() {
            clear_marker();
        } else {
            write_marker(self.defview, self.progman);
        }
        Ok(())
    }

    /// 把宿主窗口压到桌面之上、其它窗口之下。
    fn place_bottom(&self, host: HWND) -> Result<()> {
        let flags =
            SET_WINDOW_POS_FLAGS(SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_SHOWWINDOW.0);
        unsafe {
            SetWindowPos(
                host,
                Some(HWND_BOTTOM),
                self.rect.x,
                self.rect.y,
                self.rect.width as i32,
                self.rect.height as i32,
                flags,
            )
        }
        .map_err(|e| win32("SetWindowPos(host)", e))
    }

    /// 把 `Progman` 压回 z 序最底。
    ///
    /// `Win+D`（显示桌面）会把 `Progman` 临时抬到所有窗口之上。这时宿主窗口在它下面，
    /// 再对宿主调 `HWND_BOTTOM` 只会更糟：实测壁纸占比 94.6% → 0%，整屏被 `Progman` 盖住。
    /// 正确的做法是把「抬起的那一方」放回去。
    fn lower_progman(&self) -> Result<()> {
        let flags = SET_WINDOW_POS_FLAGS(
            SWP_NOMOVE.0 | SWP_NOSIZE.0 | SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0,
        );
        unsafe { SetWindowPos(raw(self.progman), Some(HWND_BOTTOM), 0, 0, 0, 0, flags) }
            .map_err(|e| win32("SetWindowPos(Progman)", e))
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
        if !unsafe { IsWindow(Some(host)) }.as_bool() {
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
        self.adopt(host, rect)?;
        // 壁纸必须在其它应用之下，否则会挡住用户正在用的窗口。
        self.place_bottom(host)
    }

    fn ensure_band(&mut self, hwnd: isize) -> Result<BandState> {
        let host = raw(hwnd);
        if !unsafe { IsWindow(Some(host)) }.as_bool() {
            return Ok(BandState::Lost);
        }

        // 图标层句柄失效 = Explorer 换了桌面壳层。
        // 新的 `SHELLDLL_DefView` 是不透明的，本进程内怎么补救都没用，
        // 只能报 `Lost`，由上层重启宿主进程（详见类型文档里的实测对照）。
        if !unsafe { IsWindow(Some(raw(self.defview))) }.as_bool() {
            return Ok(BandState::Lost);
        }

        let defview = raw(self.defview);

        // 父窗口不是我们，或者被隐藏了，就重新挂一次。
        let parent = unsafe { GetParent(defview) }.ok();
        let visible = unsafe { IsWindowVisible(defview).as_bool() };
        if parent != Some(host) || !visible {
            self.adopt(host, self.rect)?;
            self.corrections += 1;
            self.stuck = 0;
            return Ok(BandState::Corrected);
        }

        // z 序被前台切换推到桌面上方时，重新压回底部。
        let progman = raw(self.progman);
        if !super::toplevel::is_at_bottom_of_band(host, progman) {
            // 「显示桌面」把 Progman 抬到了我们上面：此时压宿主只会被 Progman 整块盖住，
            // 必须反过来把 Progman 放回最底。
            if progman_is_above(host, progman) {
                self.lower_progman()?;
            } else {
                self.place_bottom(host)?;
            }
            self.corrections += 1;
            if super::toplevel::is_at_bottom_of_band(host, progman) {
                self.stuck = 0;
                return Ok(BandState::Corrected);
            }
            // 单次失败多半是有过渡窗口插在中间（开始菜单动画、显示桌面动画），
            // 下个周期再试；只有连续失败才说明真的救不回来。
            self.stuck += 1;
            if self.stuck >= STUCK_LIMIT {
                return Ok(BandState::Lost);
            }
            return Ok(BandState::Corrected);
        }
        self.stuck = 0;
        Ok(BandState::Correct)
    }

    fn detach(&mut self, _hwnd: isize) -> Result<()> {
        let rect = self.rect;
        self.restore(rect)
    }
}

/// 连续多少次「修完仍不在底部」才判定为真的失去层级（看门狗约 2 s 一次，即 10 s）。
const STUCK_LIMIT: u32 = 5;

/// `Progman` 是否被抬到了宿主窗口之上（「显示桌面」期间会这样）。
fn progman_is_above(host: HWND, progman: HWND) -> bool {
    match (
        super::toplevel::z_index(host),
        super::toplevel::z_index(progman),
    ) {
        (Some(h), Some(p)) => p < h,
        _ => false,
    }
}

/// 当前图标层 `SHELLDLL_DefView` 的句柄。
///
/// 策略 F 把它当父窗口，所以必须在创建窗口之前拿到。
pub fn defview_handle() -> Option<isize> {
    let progman = probe::progman()?;
    find_defview(progman).map(|h| h.0 as isize)
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

/// 诊断：图标层当前挂在哪个父窗口下。
pub fn defview_state() -> String {
    let Some(progman) = probe::progman() else {
        return "无 Progman".into();
    };
    let found = find_defview(progman);
    let all = probe::all_defviews();
    let list: Vec<String> = all
        .iter()
        .map(|(h, p, v)| format!("{h:?}parent={p:?},visible={v}"))
        .collect();
    match found {
        None => format!("无 DefView（全机 0 个: {list:?}）"),
        Some(defview) => {
            let parent = unsafe { GetParent(defview) }.ok();
            let visible = unsafe { IsWindowVisible(defview).as_bool() };
            let mut r = windows::Win32::Foundation::RECT::default();
            let _ = unsafe { GetWindowRect(defview, &mut r) };
            format!(
                "defview={defview:?} parent={parent:?} visible={visible} rect=({},{},{},{}) 全机共 {} 个 {list:?}",
                r.left,
                r.top,
                r.right,
                r.bottom,
                all.len()
            )
        }
    }
}

/// 诊断：把当前图标层改成指定矩形（相对其父窗口客户区）。
///
/// 用来区分「图标层不透明把壁纸盖住」和「宿主窗口压根没合成」：
/// 把图标层缩到角落，如果屏幕其余部分能看见壁纸，就是前者。
pub fn resize_defview(x: i32, y: i32, w: i32, h: i32) -> bool {
    let Some(progman) = probe::progman() else {
        return false;
    };
    let Some(defview) = find_defview(progman) else {
        return false;
    };
    let flags = SET_WINDOW_POS_FLAGS(
        SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_NOZORDER.0 | SWP_SHOWWINDOW.0,
    );
    unsafe { SetWindowPos(defview, None, x, y, w, h, flags) }.is_ok()
}

/// 自愈：如果图标层被上一次异常退出留在了别的父窗口下，把它还给 `Progman`。
///
/// 渲染进程崩溃时 `Drop` 不会执行，下次启动必须先修复桌面，再谈挂壁纸。
/// 三种情况：
///
/// 1. 图标层还在，只是父窗口不对 → 挂回 `Progman`。
/// 2. 图标层被连根销毁（进程被强杀时 Windows 会带走子窗口）→ 重启 Explorer。
/// 3. 没有崩溃记录 → 什么都不做（不替 Explorer 自己的问题背锅）。
pub fn heal_desktop() -> Option<String> {
    let marker = read_marker();

    if let Some(progman) = probe::progman() {
        if let Some(defview) = find_defview(progman) {
            let parent = unsafe { GetParent(defview) }.ok();
            if parent == Some(progman) {
                // 桌面本来就是好的；顺手清掉上次没删干净的记录。
                clear_marker();
                return None;
            }
            let mut pr = windows::Win32::Foundation::RECT::default();
            unsafe { GetWindowRect(progman, &mut pr) }.ok()?;
            let flags =
                SET_WINDOW_POS_FLAGS(SWP_NOACTIVATE.0 | SWP_NOOWNERZORDER.0 | SWP_SHOWWINDOW.0);
            let result = unsafe {
                SetParent(defview, Some(progman))
                    .and_then(|_| {
                        SetWindowPos(
                            defview,
                            Some(HWND_BOTTOM),
                            pr.left,
                            pr.top,
                            pr.right - pr.left,
                            pr.bottom - pr.top,
                            flags,
                        )
                    })
                    .and_then(|_| check_writable(defview, progman))
            };
            return Some(match result {
                Ok(()) => {
                    clear_marker();
                    format!("图标层被挂到了 {parent:?}，已还原给 Progman")
                }
                Err(e) => format!("图标层还原失败: {}", win32("heal", e)),
            });
        }
    }

    // 找不到图标层：只有「上次确实是我们接管的」才动手，别替 Explorer 背锅。
    let (owner_pid, _) = marker?;
    if owner_pid != std::process::id() && crate::window::process_alive(owner_pid) {
        return None; // 另一个实例还在跑
    }
    Some(repair_shell())
}

/// 图标层被我们自己弄丢了：重启 Explorer 让桌面重建。
///
/// 这一步很重（会关掉文件资源管理器窗口），所以只在有崩溃记录时执行。
fn repair_shell() -> String {
    use std::process::Command;
    use std::time::Duration;

    let _ = Command::new("taskkill")
        .args(["/f", "/im", "explorer.exe"])
        .status();
    // 有的机器上系统不会自动拉起 shell，主动补一个；已经在跑时它就是一次空操作。
    std::thread::sleep(Duration::from_millis(400));
    let _ = Command::new("explorer.exe").spawn();
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(250));
        if let Some(progman) = probe::progman() {
            if find_defview(progman).is_some() {
                clear_marker();
                return "图标层已被销毁，已重启 Explorer 修复桌面".into();
            }
        }
    }
    clear_marker();
    "图标层已被销毁，已请求重启 Explorer（10 s 内未确认桌面恢复）".into()
}

/// 崩溃记录：接管期间存在，正常还原后删除。
///
/// 内容是 `<defview>|<progman>|<pid>`。万一记录过期（句柄被复用），
/// 修复逻辑只把它当作「上次是我们干的」的凭据，不会按句柄去动窗口。
fn marker_path() -> std::path::PathBuf {
    std::env::temp_dir().join("wallpaper-defview-adopted.txt")
}

fn write_marker(defview: isize, progman: isize) {
    let body = format!("{defview}|{progman}|{}", std::process::id());
    let _ = std::fs::write(marker_path(), body);
}

fn clear_marker() {
    let _ = std::fs::remove_file(marker_path());
}

fn read_marker() -> Option<(u32, (isize, isize))> {
    let text = std::fs::read_to_string(marker_path()).ok()?;
    let mut parts = text.trim().split('|');
    let (defview, progman, pid) = (parts.next()?, parts.next()?, parts.next()?);
    Some((
        pid.parse().ok()?,
        (defview.parse().ok()?, progman.parse().ok()?),
    ))
}

/// `SetWindowPos` 成功不代表桌面真的好了，再核一次父子关系。
fn check_writable(defview: HWND, progman: HWND) -> windows::core::Result<()> {
    if unsafe { GetParent(defview) }.ok() == Some(progman) {
        Ok(())
    } else {
        Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
    }
}

fn win32(context: &'static str, e: windows::core::Error) -> PlatformError {
    PlatformError::Win32 {
        context,
        code: e.code().0 as u32,
    }
}
