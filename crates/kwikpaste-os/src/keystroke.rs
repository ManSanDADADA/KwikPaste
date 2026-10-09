//! 模拟系统级粘贴（1.x 的 `keystroke/`）。
//!
//! 写回剪贴板由 core 负责；这里只做「按键注入」这一步，监听回环由 core 的写回守卫抑制。
//!
//! - Windows：`SendInput` 的 Ctrl+V。不用 Shift+Insert：Monaco 等编辑器把 Insert 当成切换改写模式。
//! - macOS：CGEvent 的 ⌘V，需要「辅助功能」权限，未授权时系统静默丢弃。

#[cfg(target_os = "macos")]
use crate::mac::keystroke as platform;
#[cfg(target_os = "windows")]
use crate::win::keystroke as platform;

/// 向系统事件队列投递一次粘贴（Windows Ctrl+V，macOS ⌘V），落到当前前台窗口。
pub fn simulate_paste() -> std::io::Result<()> {
    platform::simulate_paste()
}

/// Inject only after the application has rechecked the short-lived handoff in the same main-thread turn.
pub fn simulate_paste_to(target: crate::paste_target::PasteTarget) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        platform::simulate_paste_to(target)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = target;
        platform::simulate_paste()
    }
}

/// 确认 macOS 辅助功能权限；未授权时打开系统设置引导用户授权。
pub fn ensure_accessibility_trusted() -> std::io::Result<()> {
    platform::ensure_accessibility_trusted()
}

/// 全局快捷键命中时用户还按着修饰键；Windows 上 Alt / Win 在没有其它按键参与时松开，会激活目标窗口的
/// 菜单栏或弹出开始菜单。趁它们还按着时注入一次无意义的按键，让这次松开不再被当成单独按下。
/// macOS 松开 ⌥ / ⌘ 没有这个副作用，什么都不做。
pub fn mask_modifier_release() -> std::io::Result<()> {
    platform::mask_modifier_release()
}

/// 用户是否还按着任意修饰键（Ctrl / Shift / Alt / Win，macOS 是 ⌃ ⇧ ⌥ ⌘）。
pub fn modifiers_pressed() -> bool {
    platform::modifiers_pressed()
}
