//! 交给宿主（UI）处理的请求：后启动的实例转交的参数（含 `.kwikpastebak`）、冷启动带的备份文件、
//! 托盘「偏好设置」、托盘单击（`general.trayClick = preference`）、偏好快捷键（`shortcuts.openPreference`，
//! 默认 Alt+X）。与 1.x 相同的分派：
//!
//! - 参数里有 `.kwikpastebak` → [`HostRequest::ImportBackup`]（相对路径按对方的工作目录解析）；
//! - 只是 `--auto-launch`（重复的自启）→ 忽略；
//! - 其它 → [`HostRequest::OpenPreferences`]（UI 在未完成引导时改开引导窗）。
//!
//! # UI 怎么接
//! 启动时 [`set_handler`] 注册处理函数，之后的请求都交给它（在主线程上调用）。冷启动带的备份文件在
//! 注册之前就已排队，注册时立刻交出。没注册处理函数时的回退：打开偏好的请求显示面板，导入备份只记
//! 日志。

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{App, Global};
use kwikpaste_os::autostart::AUTO_LAUNCH_ARG;

use super::panel::{PanelCommand, Trigger, TriggerSource};

/// 请求从哪来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestSource {
    /// 用户又启动了一次（第二实例转交了参数）。
    SecondLaunch,
    /// 冷启动的命令行。
    Launch,
    /// 托盘菜单「偏好设置」。
    TrayMenu,
    /// 托盘单击（设置 `trayClick = preference`）。
    TrayClick,
    /// 全局快捷键 `shortcuts.openPreference`。
    Hotkey,
    /// macOS Dock 图标重新打开应用。
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the macOS Dock reopen sends it")
    )]
    Dock,
    /// 剪贴板面板头部的偏好设置按钮。
    Panel,
}

/// 交给 UI 的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRequest {
    /// 打开偏好设置；未完成引导时 UI 应改开引导窗（与 1.x 相同）。
    OpenPreferences { source: RequestSource },
    /// 导入备份文件（双击 `.kwikpastebak`、或把它作为参数启动）。
    ImportBackup {
        path: PathBuf,
        source: RequestSource,
    },
}

type Handler = Rc<dyn Fn(&HostRequest, &mut App)>;

#[derive(Default)]
struct Host {
    handler: Option<Handler>,
    pending: Vec<HostRequest>,
}

impl Global for Host {}

/// 注册 UI 的处理函数；之前排队的请求（冷启动带的备份文件）立刻交出。
#[allow(dead_code, reason = "UI 接线用的接口，见本模块文档")]
pub fn set_handler(cx: &mut App, handler: impl Fn(&HostRequest, &mut App) + 'static) {
    let handler: Handler = Rc::new(handler);
    let host = cx.default_global::<Host>();
    host.handler = Some(handler.clone());
    let pending = std::mem::take(&mut host.pending);
    for request in pending {
        handler(&request, cx);
    }
}

/// 分派一个请求：有处理函数就交给它，否则走回退。
pub fn dispatch(cx: &mut App, request: HostRequest) {
    dispatch_observed(cx, request, kwikpaste_os::clock::now_ticks());
}

/// Preserve observation time across bridge queues; late older preferences cannot supersede a newer paste.
pub fn dispatch_observed(cx: &mut App, request: HostRequest, ticks: i64) {
    if super::paste::control_is_stale(cx, ticks) {
        log::debug!("dropped stale host request: {request:?}");
        return;
    }
    super::request(cx, PanelCommand::CancelPaste.observed_at(ticks));
    log::info!("host request: {request:?}");
    let handler = cx
        .try_global::<Host>()
        .and_then(|host| host.handler.clone());
    if let Some(handler) = handler {
        handler(&request, cx);
        return;
    }
    match request {
        HostRequest::OpenPreferences { source } => {
            let trigger = Trigger {
                ticks,
                source: match source {
                    RequestSource::TrayMenu | RequestSource::TrayClick => TriggerSource::Tray,
                    RequestSource::Hotkey => TriggerSource::Hotkey,
                    RequestSource::Panel | RequestSource::Dock => TriggerSource::Ui,
                    RequestSource::SecondLaunch | RequestSource::Launch => {
                        TriggerSource::SecondInstance
                    }
                },
            };
            super::request(cx, PanelCommand::Show(trigger).observed_at(ticks));
        }
        HostRequest::ImportBackup { path, .. } => {
            log::warn!(
                "backup import of {} requested, but no import UI is registered",
                path.display()
            );
        }
    }
}

/// 冷启动：命令行带了备份文件就排队，等 UI 注册处理函数时交出。
pub fn queue_launch_arguments(cx: &mut App) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Some(path) = backup_path(&args, &cwd) {
        let request = HostRequest::ImportBackup {
            path,
            source: RequestSource::Launch,
        };
        log::info!("host request queued: {request:?}");
        cx.default_global::<Host>().pending.push(request);
    }
}

/// 后启动的实例转交来的参数（不含 `args[0]`）对应的请求；重复的自启返回 `None`。
pub fn request_for_invocation(args: &[String], cwd: &str) -> Option<HostRequest> {
    if let Some(path) = backup_path(args, Path::new(cwd)) {
        return Some(HostRequest::ImportBackup {
            path,
            source: RequestSource::SecondLaunch,
        });
    }
    // 登录时历史遗留的多个启动项可能并发拉起实例：第二实例带 `--auto-launch` 时什么也不做。
    if args.iter().any(|arg| arg == AUTO_LAUNCH_ARG) {
        return None;
    }

    Some(HostRequest::OpenPreferences {
        source: RequestSource::SecondLaunch,
    })
}

/// 参数里的第一个 `.kwikpastebak`；相对路径按 `cwd` 解析。
fn backup_path(args: &[String], cwd: &Path) -> Option<PathBuf> {
    let path = kwikpaste_core::backup::backup_path_from_args(args)?;
    if path.is_relative() && !cwd.as_os_str().is_empty() {
        return Some(cwd.join(path));
    }

    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn second_launches_map_to_the_1x_actions() {
        #[cfg(target_os = "windows")]
        let (root, cwd, absolute) = (r"C:\", r"C:\work", r"D:\backup\history.KwikPasteBak");
        #[cfg(target_os = "macos")]
        let (root, cwd, absolute) = ("/", "/work", "/backup/history.KwikPasteBak");
        assert_eq!(
            request_for_invocation(&args(&[]), root),
            Some(HostRequest::OpenPreferences {
                source: RequestSource::SecondLaunch
            })
        );
        assert_eq!(
            request_for_invocation(&args(&["--auto-launch"]), root),
            None
        );
        assert_eq!(
            request_for_invocation(&args(&[absolute]), root),
            Some(HostRequest::ImportBackup {
                path: PathBuf::from(absolute),
                source: RequestSource::SecondLaunch
            })
        );
        // 备份文件优先于 --auto-launch。
        assert!(matches!(
            request_for_invocation(&args(&["--auto-launch", "a.kwikpastebak"]), cwd),
            Some(HostRequest::ImportBackup { path, .. }) if path == Path::new(cwd).join("a.kwikpastebak")
        ));
    }
}
