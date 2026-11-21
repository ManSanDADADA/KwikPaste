//! Windows 集成。

pub mod admin;
pub mod apps;
pub mod args;
pub mod autostart;
pub mod crash;
pub mod drag_out;
pub mod foreground;
pub mod imaging;
pub mod ime;
pub mod keyboard;
pub mod keystroke;
pub mod material;
pub mod menu_theme;
pub mod monitor;
pub mod mouse;
pub mod panel;
pub mod single_instance;
pub mod system;
pub mod trigger_pause;
pub mod win_v;

use std::{ffi::c_void, io, mem::size_of};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow, SetForegroundWindow};

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

/// 把本进程的工作集交还给系统。页面进待机 / 修改列表，物理内存不会马上被别人拿走，
/// 再次访问是软缺页；任务管理器的「内存」列（专用工作集）随之降下来。
pub fn trim_working_set() -> io::Result<()> {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};

    // (-1, -1) 表示尽量清空工作集。
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) }
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
