//! 当前进程所在的 Windows 登录会话。
//!
//! 命名管道名里必须带 session id：同一台机器切换用户 / 多用户同时登录时，
//! 不同会话的 `wp-host` 绝不能连到彼此的管道上去。

use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;

/// 当前进程的会话 id。
///
/// 查询失败时退回 `1`：单机单用户是常态，宁可偶发撞名也不要拼出非法管道名
/// （0 会让所有会话共用同一个管道）。
pub fn current_session_id() -> u32 {
    let mut id = 0u32;
    let ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id).is_ok() };
    if ok && id != 0 {
        id
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_usable() {
        let id = current_session_id();
        assert_ne!(id, 0);
    }
}
