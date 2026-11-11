//! 只需要原生句柄或系统 API 的集成：不依赖 GPUI，可以脱离窗口框架单测。
//!
//! 判定规则：代码要引用 `gpui::*` 就放 `kwikpaste-app`；只用到 `HWND` / `NSView*` / Win32 / AppKit
//! 的放这里。

pub mod autostart;
pub mod clock;
pub mod diagnostics;
pub mod dialogs;
pub mod double_tap;
pub mod drag_out;
pub mod geometry;
pub mod hook_keys;
pub mod idle;
pub mod keystroke;
pub mod locale;
pub mod services;
pub mod single_instance;
mod sound;

/// 命令行参数（有损转成 UTF-8）。`std::env::args` 遇到非 Unicode 参数（如带孤立代理项的 NTFS 路径）
/// 会 panic，release 下进程直接 abort。
pub fn args() -> impl Iterator<Item = String> {
    std::env::args_os().map(|arg| arg.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "windows")]
pub mod win;
