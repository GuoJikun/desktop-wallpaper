//! 进程级单实例锁。
//!
//! 用命名互斥量而不是「探测管道是否存在」：互斥量由内核保证原子性，
//! 两个进程同时启动时只有一个能抢到；管道探测存在「都还没创建」的竞态窗口。
//!
//! 名字建议带 `Local\` 前缀与会话 id：不同登录会话各跑一份互不干扰。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

/// 持有期间独占一个名字；`Drop` 时释放。
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    /// 尝试独占 `name`；已有进程持有时返回 `None`。
    pub fn acquire(name: &str) -> Option<Self> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let handle = CreateMutexW(None, false, PCWSTR(wide.as_ptr())).ok()?;
            // 已存在时 CreateMutexW 仍然返回有效句柄，只是 GetLastError 为
            // ERROR_ALREADY_EXISTS——必须判断这个，不能只看句柄是否有效。
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                None
            } else {
                Some(Self { handle })
            }
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_name(tag: &str) -> String {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        format!(
            r"Local\wp-single-instance-test-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn second_acquire_is_rejected_until_first_is_dropped() {
        let name = unique_name("basic");
        let first = SingleInstance::acquire(&name).expect("第一次应该成功");
        assert!(SingleInstance::acquire(&name).is_none(), "第二次应该被拒绝");
        drop(first);
        assert!(
            SingleInstance::acquire(&name).is_some(),
            "释放后应该能重新获取"
        );
    }

    #[test]
    fn different_names_do_not_block_each_other() {
        let a = SingleInstance::acquire(&unique_name("a")).expect("a 应该成功");
        let b = SingleInstance::acquire(&unique_name("b")).expect("b 应该成功");
        drop((a, b));
    }
}
