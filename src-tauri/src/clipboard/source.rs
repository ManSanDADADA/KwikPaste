//! 「这次剪贴板变更来自哪个应用」的探测：在剪贴板事件回调里调用，
//! 返回稳定 id（macOS bundle id / Windows exe 绝对路径）、显示名、抽取图标用的路径。
//!
//! 必须**同步**在监听回调一发生时立即抓——延后到 await 之后再问，前台应用很可能已经切走。
//! 探测失败（无前台应用 / 自身复制 / 平台 API 错误）一律返回 `None`，不阻断入库。
//!
//! 平台 API：macOS 走 `NSWorkspace.frontmostApplication`，Windows 走 `GetForegroundWindow`
//! + `QueryFullProcessImageNameW`。
//!
//! 这里只记下图标来源路径，不抽图标：绝大多数复制来自已登记的应用，
//! 图标由 [`crate::clipboard::materialize_source`] 在缓存未命中时才抽取。

use std::path::PathBuf;

use crate::db::models::Platform;

#[derive(Debug, Clone)]
pub struct FrontmostApp {
    /// 稳定主键。macOS = bundle id（如 `com.apple.Safari`），Windows = exe 绝对路径。
    pub id: String,
    /// 显示名（localizedName / FileDescription / exe stem 的优先回落）。
    pub name: String,
    pub platform: Platform,
    /// 抽取应用图标用的路径（macOS `.app` 包 / Windows exe）；拿不到则 `None`。
    pub icon_source: Option<PathBuf>,
}

/// 探测当前前台应用。失败不报错，只在 trace 级别记日志（监听回调高频，避免噪声）。
pub fn detect_frontmost() -> Option<FrontmostApp> {
    #[cfg(target_os = "macos")]
    {
        macos::detect()
    }
    #[cfg(target_os = "windows")]
    {
        windows::detect()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{FrontmostApp, Platform};

    use std::path::PathBuf;

    use objc2::msg_send;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2_app_kit::{NSRunningApplication, NSWorkspace};
    use objc2_foundation::{NSString, NSURL};

    pub(super) fn detect() -> Option<FrontmostApp> {
        autoreleasepool(|_| {
            let workspace = NSWorkspace::sharedWorkspace();
            let app = workspace.frontmostApplication()?;

            // 没 bundle id 的进程（命令行子进程等）不入表，避免主键不稳定。
            let id = app.bundleIdentifier().map(|s| s.to_string())?;
            let name = app
                .localizedName()
                .map(|s| s.to_string())
                .unwrap_or_else(|| id.clone());

            Some(FrontmostApp {
                id,
                name,
                platform: Platform::Macos,
                icon_source: unsafe { bundle_path(&app) },
            })
        })
    }

    /// 通过 NSRunningApplication.bundleURL 拿到 .app 路径。objc2-app-kit 当前 feature 没生成
    /// 该 getter，只能 msg_send!；返回 NSURL 后用 path 取 NSString → Rust String。
    unsafe fn bundle_path(app: &NSRunningApplication) -> Option<PathBuf> {
        let url: Option<Retained<NSURL>> = msg_send![app, bundleURL];
        let url = url?;
        let path: Option<Retained<NSString>> = msg_send![&*url, path];
        Some(PathBuf::from(path?.to_string()))
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{FrontmostApp, Platform};

    use std::path::{Path, PathBuf};

    use winapi::shared::minwindef::{DWORD, FALSE};
    use winapi::shared::windef::HWND;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::winbase::QueryFullProcessImageNameW;
    use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;
    use winapi::um::winuser::{GetForegroundWindow, GetWindowThreadProcessId};

    pub(super) fn detect() -> Option<FrontmostApp> {
        let exe_path = unsafe { foreground_exe_path() }?;
        // 自身写回事件依赖 WritebackGuard 的 content_hash 判定，这里不过滤自身——
        // 与 macOS 行为一致：哪怕拿到的是 KwikPaste 自己，guard 也会在下游 short-circuit。
        let name = Path::new(&exe_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&exe_path)
            .to_owned();

        Some(FrontmostApp {
            icon_source: Some(PathBuf::from(&exe_path)),
            id: exe_path,
            name,
            platform: Platform::Windows,
        })
    }

    unsafe fn foreground_exe_path() -> Option<String> {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid: DWORD = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        // PROCESS_QUERY_LIMITED_INFORMATION 足够 QueryFullProcessImageNameW，且不需要管理员权限。
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if handle.is_null() {
            return None;
        }

        let mut buf = [0u16; 1024];
        let mut size: DWORD = buf.len() as DWORD;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);

        if ok == 0 || size == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}
