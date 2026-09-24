//! 自启动：直接用 `auto-launch` crate 实现，绕过 `tauri-plugin-autostart` 上游 bug
//! （tauri-apps/plugins-workspace#1922：macOS 下 `is_enabled` 误报、`enable` 路径写错）。
//!
//! 启动参数固定追加 `--auto-launch`，用于识别本次启动来源。

use std::env;

use tauri::{AppHandle, Manager};

use crate::core::{AppError, Result};
use crate::settings::SettingsStore;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos::PlatformAutostart;
#[cfg(target_os = "windows")]
use windows::PlatformAutostart;

pub(super) const AUTO_LAUNCH_ARG: &str = "--auto-launch";
const PORTABLE_ENTRY_SUFFIX: &str = "Portable";

pub struct AutostartManager {
    platform: PlatformAutostart,
}

pub fn init(app: &AppHandle) -> Result<()> {
    let exe = env::current_exe().map_err(|err| {
        log::error!("autostart init: current_exe failed: {err}");
        AppError::Other(anyhow::anyhow!("{err}"))
    })?;
    let exe_path = exe.to_string_lossy().to_string();

    // 便携版用独立的启动项名：和安装版同名时两边启动都会按自己的路径重写，互相覆盖。
    let app_name = if crate::core::portable::is_portable() {
        format!("{} {PORTABLE_ENTRY_SUFFIX}", app.package_info().name)
    } else {
        app.package_info().name.clone()
    };

    let platform = PlatformAutostart::new(&app_name, &exe_path)?;

    app.manage(AutostartManager { platform });
    Ok(())
}

pub fn is_enabled(app: &AppHandle) -> Result<bool> {
    let manager = app.state::<AutostartManager>();
    manager.platform.is_enabled()
}

pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<()> {
    let manager = app.state::<AutostartManager>();
    manager.platform.set_enabled(enabled)
}

/// Align the OS autostart entry with the persisted setting during startup.
///
/// 便携版反过来以本机启动项为准：设置随文件夹带到别的电脑时不应自动注册自启；
/// 已注册的按当前 exe 路径重写，文件夹挪动后自启仍然有效。
pub fn sync_enabled(app: &AppHandle, enabled: bool) -> Result<()> {
    if !crate::core::portable::is_portable() {
        return set_enabled(app, enabled);
    }

    let registered = is_enabled(app)?;
    if registered {
        set_enabled(app, true)?;
    }

    if registered != enabled {
        let next = app.state::<SettingsStore>().update(serde_json::json!({
            "general": {
                "autoStart": registered,
            },
        }))?;
        crate::commands::emit_settings_updated(app, &next);
    }

    Ok(())
}

/// 判断进程参数是否来自 KwikPaste 注册的系统自启动项。
pub fn is_autostart_launch(args: &[String]) -> bool {
    args.iter().any(|arg| arg == AUTO_LAUNCH_ARG)
}

#[cfg(test)]
mod tests {
    use super::is_autostart_launch;

    #[test]
    fn detects_autostart_launch_argument() {
        let args = vec!["KwikPaste.exe".to_owned(), "--auto-launch".to_owned()];

        assert!(is_autostart_launch(&args));
    }

    #[test]
    fn rejects_regular_launch_arguments() {
        let args = vec!["KwikPaste.exe".to_owned()];

        assert!(!is_autostart_launch(&args));
    }
}
