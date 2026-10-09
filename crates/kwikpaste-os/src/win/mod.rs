//! Windows 集成。

pub mod admin;
pub mod apps;
pub mod args;
pub mod autostart;
pub mod crash;
pub mod drag_out;
pub mod foreground;
pub mod ime;
pub mod keyboard;
pub mod keystroke;
pub mod material;
pub mod menu_theme;
pub mod monitor;
pub mod mouse;
pub mod panel;
pub mod paste_target;
pub mod single_instance;
pub mod system;
pub mod trigger_pause;
pub mod win_v;

use std::{ffi::c_void, io, mem::size_of};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
};

use crate::geometry::{Point, Rect};

/// 当前前台窗口的句柄（没有时为 0）。
pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow() }.0 as isize
}

/// 把 `hwnd` 设为前台窗口，返回之后它是否真的是前台。系统的前台锁可能拒绝，调用方要处理失败。
pub fn set_foreground(hwnd: isize) -> bool {
    let window = HWND(hwnd as *mut c_void);
    let accepted = unsafe { SetForegroundWindow(window) }.as_bool();
    accepted && foreground_window() == hwnd
}

/// 句柄是否仍是一个存在的窗口。
pub fn is_window(hwnd: isize) -> bool {
    hwnd != 0 && unsafe { IsWindow(Some(HWND(hwnd as *mut c_void))) }.as_bool()
}

/// Query the live owner every time; HWND values can be reused after a window is destroyed.
pub fn window_owner(hwnd: isize) -> Option<u32> {
    if !is_window(hwnd) {
        return None;
    }
    let mut process_id = 0;
    let thread_id =
        unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut c_void), Some(&mut process_id)) };
    (thread_id != 0 && process_id != 0).then_some(process_id)
}

/// Capture a currently live destination belonging to another process.
pub fn external_window(hwnd: isize) -> Option<paste_target::WindowTarget> {
    let target = paste_target::WindowTarget {
        window: hwnd,
        process_id: window_owner(hwnd)?,
    };
    is_live_paste_target(target).then_some(target)
}

/// Validate the retained window and owner pair without substituting its current owner.
pub fn is_live_paste_target(target: paste_target::WindowTarget) -> bool {
    paste_target::is_valid_target(target, window_owner(target.window), std::process::id())
}

/// 用 DWM 隐藏（cloak）或重新显示一个窗口：窗口仍是可见状态、照常绘制，只是不上屏。
pub fn set_window_cloaked(hwnd: isize, cloaked: bool) -> io::Result<()> {
    let value = i32::from(cloaked);
    unsafe {
        DwmSetWindowAttribute(
            HWND(hwnd as *mut c_void),
            DWMWA_CLOAK,
            (&raw const value).cast(),
            size_of::<i32>() as u32,
        )
    }
    .map_err(|error| io::Error::other(error.to_string()))
}

fn rect_from_win32(rect: RECT) -> Rect {
    Rect {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

fn point_from_win32(point: POINT) -> Point {
    Point {
        x: point.x,
        y: point.y,
    }
}
