//! Windows 便携模式。
//!
//! exe 同目录存在 [`MARKER_FILENAME`] 时进入便携模式：exe 旁的 `data/` 取代
//! `%LOCALAPPDATA%\<identifier>`，内部布局与之一致（`<env>/`、`logs/`、WebView2 的 `EBWebView/`），
//! 整个文件夹可以随 U 盘带走，本机不留数据。
//!
//! 判定只看 exe 路径、不依赖 Tauri，Tauri 启动前的提权检查和日志插件初始化也能用。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tauri::plugin::TauriPlugin;
use tauri::Runtime;

#[cfg(target_os = "windows")]
use crate::i18n::startup::Key as StartupKey;

/// 便携标记文件名，随便携包分发（`scripts/package-portable.ps1`）；内容只是给用户看的说明。
#[cfg(target_os = "windows")]
const MARKER_FILENAME: &str = "portable.txt";
#[cfg(target_os = "windows")]
const DATA_DIR_NAME: &str = "data";
/// 前端同步读取运行形态的全局变量名，与 `src/constants/runtime.ts` 保持一致。
const RUNTIME_GLOBAL: &str = "__KWIKPASTE_RUNTIME__";
const LOGS_DIR_NAME: &str = "logs";

static DATA_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 便携数据根 `<exe 目录>/data`；非便携模式返回 `None`。
pub fn data_root() -> Option<&'static Path> {
    DATA_ROOT.get_or_init(detect).as_deref()
}

pub fn is_portable() -> bool {
    data_root().is_some()
}

/// 便携模式的日志目录 `data/logs`，对应安装版的 `%LOCALAPPDATA%\<identifier>\logs`。
pub fn logs_dir() -> Option<PathBuf> {
    data_root().map(|root| root.join(LOGS_DIR_NAME))
}

/// 把运行形态注入每个 webview，前端据此同步隐藏便携版不适用的设置项（同 plugin-os 的做法）。
pub fn runtime_plugin<R: Runtime>() -> TauriPlugin<R> {
    let script = format!(
        "Object.defineProperty(window, \"{RUNTIME_GLOBAL}\", {{ value: Object.freeze({{ portable: {} }}) }});",
        is_portable()
    );

    tauri::plugin::Builder::new("kwikpaste-runtime")
        .js_init_script(script)
        .build()
}

/// 便携模式启动前自检：数据目录必须可写、系统必须装有 WebView2，否则弹原生提示后退出。
///
/// 不满足时不回退到 `%LOCALAPPDATA%`：那里可能是安装版的数据，两个版本交替使用会让
/// 数据库迁移互相踩踏。WebView2 缺失时 Tauri 会直接闪退，安装版由安装器兜底，便携版只能自己查。
pub fn ensure_runnable() {
    #[cfg(target_os = "windows")]
    {
        let Some(root) = data_root() else {
            return;
        };

        if let Err(err) = probe_writable(root) {
            let message = format!(
                "{}\n\n{}\n({err})",
                startup_label(StartupKey::PortableDirNotWritable),
                root.display()
            );
            windows_dialog::show_error(&message);
            std::process::exit(1);
        }

        if tauri::webview_version().is_err() {
            if windows_dialog::confirm(startup_label(StartupKey::WebviewMissing)) {
                windows_dialog::open_url(WEBVIEW2_DOWNLOAD_URL);
            }
            std::process::exit(1);
        }
    }
}

/// 微软官方 WebView2 Evergreen 引导安装程序（约 2 MB，安装时联网下载运行时）。
#[cfg(target_os = "windows")]
const WEBVIEW2_DOWNLOAD_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// 自检发生在设置加载之前，文案语言跟随系统。
#[cfg(target_os = "windows")]
fn startup_label(key: StartupKey) -> &'static str {
    let locale = tauri_plugin_os::locale().unwrap_or_default();

    crate::i18n::startup::label(crate::settings::Language::from_system_locale(&locale), key)
}

#[cfg(target_os = "windows")]
fn probe_writable(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let probe = root.join(format!(".write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

#[cfg(target_os = "windows")]
fn detect() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    exe_dir
        .join(MARKER_FILENAME)
        .is_file()
        .then(|| exe_dir.join(DATA_DIR_NAME))
}

#[cfg(not(target_os = "windows"))]
fn detect() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "windows")]
mod windows_dialog {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDOK, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_OKCANCEL, SW_SHOWNORMAL,
    };

    use super::StartupKey;

    pub fn show_error(message: &str) {
        let title = wide(super::startup_label(StartupKey::DialogTitle));
        let text = wide(message);

        unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    pub fn confirm(message: &str) -> bool {
        let title = wide(super::startup_label(StartupKey::DialogTitle));
        let text = wide(message);

        unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OKCANCEL | MB_ICONWARNING,
            ) == IDOK
        }
    }

    pub fn open_url(url: &str) {
        let operation = wide("open");
        let file = wide(url);

        unsafe {
            ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(file.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}
