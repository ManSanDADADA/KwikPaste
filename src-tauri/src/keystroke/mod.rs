//! 模拟系统级粘贴。
//!
//! 写回剪贴板由 `clipboard::write` 负责；本模块只负责「按键模拟」这一步——
//! 配合 watcher 的 `WritebackGuard` 抑制自身写回带来的回环。
//!
//! - macOS：⌘V（CGEvent）
//! - Windows：Ctrl+V（SendInput）

use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

pub use platform::{mask_modifier_release, simulate_paste};

const MODIFIER_POLL_INTERVAL: Duration = Duration::from_millis(15);

/// 等用户松开全部修饰键，最多等 `timeout`；返回是否已全部松开。
///
/// 全局快捷键在按下时就触发，此时修饰键还按着，立即注入的粘贴会被目标应用读成
/// Ctrl+Shift+V 这类别的组合，所以要等松开后再粘贴。
pub async fn wait_for_modifiers_released(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;

    while platform::modifiers_pressed() {
        if Instant::now() >= deadline {
            return false;
        }

        tokio::time::sleep(MODIFIER_POLL_INTERVAL).await;
    }

    true
}
