//! 开机自启与以管理员身份运行的接线，行为与 1.x 相同（2.0 覆盖安装后接管同一个启动项和计划任务）。
//!
//! - 名字：正式版（`production-identity` 且不是自测）用 1.x 的 `KwikPaste` / `KwikPaste Portable` 和
//!   `KwikPasteAdmin`；开发版、自测用 identifier 派生的名字（如
//!   `com.fastthree.kwikpaste.native-dev`、`com.fastthree.kwikpaste.native-dev.admin`），绝不碰本机
//!   已安装应用的启动项和任务。
//! - [`elevate_if_configured`]：启动最早期、单实例判重之前（与 1.x 相同），设置要求以管理员运行而
//!   当前没提权时拉起提权的进程，调用方退出；已提权时（重）建计划任务。便携版、自测跳过。
//! - [`sync_at_startup`]：core 起来之后，自启按设置写或删；便携版反过来以本机启动项为准、改设置
//!   （设置随文件夹带到别的电脑时不应自动注册）。首次运行（引导未完成）设置为关时也以本机为准，
//!   不删 1.x 留下的启动项。
//! - [`apply`]：设置变更时跟随 `general.autoStart` 与 `general.runAsAdmin`。
//!
//! # UI 怎么接（偏好设置）
//! 开关直接改设置（`general.autoStart`、`general.runAsAdmin`），这里跟随；失败只记日志。
//! 需要显示真实状态时用 [`autostart_registered`]、[`running_as_admin`]；打开管理员权限启动用
//! [`enable_run_as_admin`]。

use gpui::App;
use kwikpaste_core::settings::{Settings, SettingsDelta};
use kwikpaste_os::autostart::Autostart;

#[cfg(target_os = "windows")]
use crate::selftest;
use crate::{core_host, identity};

/// 1.x 的启动项名（`productName`）与计划任务名。
const OFFICIAL_ENTRY: &str = "KwikPaste";
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const OFFICIAL_TASK: &str = "KwikPasteAdmin";
const PORTABLE_SUFFIX: &str = " Portable";

/// 是否用正式版的名字：只有正式身份的非自测进程。
fn official() -> bool {
    identity::identifier() == kwikpaste_core::APP_IDENTIFIER
}

fn portable() -> bool {
    kwikpaste_core::portable::detect().is_some()
}

/// 启动项名：便携版另加后缀，和安装版同名时两边启动都会按自己的路径重写、互相覆盖。
fn entry_name() -> String {
    let base = if official() {
        OFFICIAL_ENTRY.to_owned()
    } else {
        identity::identifier().to_owned()
    };
    if portable() {
        format!("{base}{PORTABLE_SUFFIX}")
    } else {
        base
    }
}

#[cfg(target_os = "windows")]
fn task_name() -> String {
    if official() {
        OFFICIAL_TASK.to_owned()
    } else {
        format!("{}.admin", identity::identifier())
    }
}

fn entry() -> anyhow::Result<Autostart> {
    let exe = std::env::current_exe()?;
    Ok(Autostart::new(&entry_name(), &exe)?)
}

/// 启动项当前是否注册着（偏好页显示真实状态用）。
#[allow(dead_code, reason = "UI 接线用的接口，见本模块文档")]
pub fn autostart_registered() -> anyhow::Result<bool> {
    Ok(entry()?.is_enabled()?)
}

/// core 起来之后调用一次：让系统启动项与设置一致。
pub fn sync_at_startup(cx: &mut App) {
    let Some(core) = core_host::core(cx).cloned() else {
        return;
    };
    let entry = match entry() {
        Ok(entry) => entry,
        Err(err) => {
            log::error!("autostart is unavailable: {err:#}");
            return;
        }
    };
    let wanted = core.settings().general.auto_start;
    // 首次运行（引导未完成）的设置是刚建的默认值：已有的启动项（1.x 留下、安装包保住的）不删，
    // 和便携版一样以本机为准。
    let follow_system = portable() || (!wanted && !core.settings().onboarding.completed);

    if !follow_system {
        set_autostart(&entry, wanted);
        return;
    }

    // 本机启动项为准；注册着就按当前 exe 路径重写（文件夹挪动后仍然有效）。
    let registered = match entry.is_enabled() {
        Ok(registered) => registered,
        Err(err) => {
            log::error!("autostart state could not be read: {err}");
            return;
        }
    };
    if registered {
        set_autostart(&entry, true);
    }
    if registered != wanted {
        cx.spawn(async move |_| {
            let patch = serde_json::json!({ "general": { "autoStart": registered } });
            if let Err(err) = core.update_settings(patch).await {
                log::error!("autostart setting could not follow the system: {err}");
            }
        })
        .detach();
    }
}

/// 设置变更时调用：开关变了就改启动项、计划任务。
pub fn apply(settings: &Settings, delta: &SettingsDelta) {
    if delta.touches("general.autoStart") {
        match entry() {
            Ok(entry) => set_autostart(&entry, settings.general.auto_start),
            Err(err) => log::error!("autostart is unavailable: {err:#}"),
        }
    }
    if delta.touches("general.runAsAdmin") {
        sync_admin_task(settings.general.run_as_admin);
    }
}

fn set_autostart(entry: &Autostart, enabled: bool) {
    match entry.set_enabled(enabled) {
        Ok(()) => log::info!(
            "autostart {} ({})",
            if enabled { "on" } else { "off" },
            entry.name()
        ),
        Err(err) => log::error!("autostart could not be turned {enabled}: {err}"),
    }
}

/// 当前进程是否已提权（偏好页显示状态用，设置里的 `general.runAsAdmin` 只是意图）。
pub fn running_as_admin() -> bool {
    #[cfg(target_os = "windows")]
    {
        kwikpaste_os::win::admin::is_running_as_admin()
    }
    #[cfg(target_os = "macos")]
    false
}

/// 已提权时按设置建或删计划任务（建任务要管理员权限）；便携版不注册（任务里记着 exe 路径，
/// 文件夹挪动后就失效，名字还会和安装版冲突）。
fn sync_admin_task(configured: bool) {
    #[cfg(target_os = "windows")]
    {
        use kwikpaste_os::win::admin::{AdminTask, is_running_as_admin};

        if portable() || !is_running_as_admin() {
            return;
        }
        let task = AdminTask::new(&task_name());
        let result = if configured {
            std::env::current_exe().and_then(|exe| task.create(&exe))
        } else {
            task.delete()
        };
        match result {
            Ok(()) => log::info!(
                "admin task {} {}",
                task.name(),
                if configured { "created" } else { "removed" }
            ),
            Err(err) => log::warn!("admin task {} not synced: {err}", task.name()),
        }
    }
    #[cfg(target_os = "macos")]
    let _ = configured;
}

/// 启动最早期调用（单实例判重之前）：设置要求以管理员运行、当前没提权时拉起提权的进程并返回
/// `true`，调用方应直接退出；已提权时（重）建计划任务。便携版、自测、带了重启标记的进程跳过。
pub fn elevate_if_configured() -> bool {
    #[cfg(target_os = "windows")]
    {
        use kwikpaste_os::win::admin::{
            AdminTask, has_restart_marker, is_running_as_admin, launch_elevated,
        };

        if selftest::active() || portable() || !configured_run_as_admin() {
            return false;
        }
        let task = AdminTask::new(&task_name());
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        if is_running_as_admin() {
            if let Err(err) = task.create(&exe) {
                log::warn!("admin task {} not synced at startup: {err}", task.name());
            }
            return false;
        }
        if has_restart_marker(kwikpaste_os::args()) {
            return false;
        }
        let args: Vec<String> = kwikpaste_os::args().skip(1).collect();
        let launched = launch_elevated(&task, &exe, &args);
        if launched {
            log::info!("relaunched as administrator");
        } else {
            log::warn!("the administrator relaunch was cancelled or failed; running unelevated");
        }
        launched
    }
    #[cfg(target_os = "macos")]
    false
}

/// 启动早期读设置 `general.runAsAdmin`（core 还没起来）。读不到当作没打开。
#[cfg(target_os = "windows")]
fn configured_run_as_admin() -> bool {
    #[derive(Default, serde::Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    struct Early {
        general: EarlyGeneral,
    }
    #[derive(Default, serde::Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    struct EarlyGeneral {
        run_as_admin: bool,
    }

    let identity = identity::current();
    let settings: Option<std::path::PathBuf> =
        kwikpaste_core::CorePaths::for_native(identity.identifier, identity.env)
            .and_then(|paths| paths.config_dir())
            .map(|dir| dir.join("settings.json"))
            .ok();
    settings
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Early>(&bytes).ok())
        .is_some_and(|early| early.general.run_as_admin)
}

/// 打开「管理员权限启动」：先把意图写进设置（否则只提权这一次，下次启动又回到普通权限），
/// 再以管理员身份重启；写设置失败就不重启。
pub fn enable_run_as_admin(cx: &mut App) {
    let Some(core) = core_host::core(cx).cloned() else {
        return;
    };
    cx.spawn(async move |cx| {
        let patch = serde_json::json!({ "general": { "runAsAdmin": true } });
        if let Err(err) = core.update_settings(patch).await {
            log::error!("the run-as-admin setting could not be saved: {err}");
            return;
        }
        if let Err(err) = cx.update(restart_as_admin) {
            log::warn!("administrator restart was not started: {err:#}");
        }
    })
    .detach();
}

/// 「立即以管理员身份重启」：先放掉单实例（新进程直接成为主实例），拉起提权的进程后退出；
/// 没拉起来（用户取消 UAC）就重新占回单实例并返回错误。
fn restart_as_admin(cx: &mut App) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use kwikpaste_os::win::admin::{AdminTask, launch_elevated};

        let exe = std::env::current_exe()?;
        let args: Vec<String> = kwikpaste_os::args().skip(1).collect();
        super::hotkey::unregister_all(cx);
        super::instance::release(cx);
        if launch_elevated(&AdminTask::new(&task_name()), &exe, &args) {
            cx.quit();
            return Ok(());
        }
        super::hotkey::resume(cx);
        super::instance::reclaim(cx)?;
        anyhow::bail!("the administrator relaunch was cancelled or failed")
    }
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        anyhow::bail!("administrator launch is only available on Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_builds_never_use_the_official_names() {
        // 测试进程用的是开发身份。
        assert!(!official());
        assert_ne!(entry_name(), OFFICIAL_ENTRY);
        assert!(entry_name().starts_with("com.fastthree.kwikpaste.native-dev"));
        #[cfg(target_os = "windows")]
        {
            assert_ne!(task_name(), OFFICIAL_TASK);
            assert!(task_name().ends_with(".admin"));
        }
    }
}
