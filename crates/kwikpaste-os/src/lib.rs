//! 只需要原生句柄或系统 API 的集成：不依赖 GPUI，可以脱离窗口框架单测。
//!
//! 判定规则：代码要引用 `gpui::*` 就放 `kwikpaste-app`；只用到 `HWND` / `NSView*` / Win32 / AppKit
//! 的放这里。

pub mod autostart;
pub mod clock;
pub mod dialogs;
pub mod drag_out;
pub mod geometry;
pub mod hook_keys;
pub mod keystroke;
pub mod locale;
pub mod services;
pub mod single_instance;
mod sound;

#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "windows")]
pub mod win;
