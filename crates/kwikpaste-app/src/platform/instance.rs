//! 单实例的主实例一侧：保管守卫，处理后启动的实例转交来的参数。

use async_channel::{Receiver, Sender};
use gpui::{App, AsyncApp, Global};
use kwikpaste_os::single_instance::{self, Claim, Invocation, PrimaryInstance};

use super::editing::EditTrigger;
use super::panel::{PanelCommand, Trigger, TriggerSource};
use super::paste_coordinator::{PasteCoordinator, shared_coordinator};
use super::{host, paste, probe, updater, watchdog};
use crate::{core_host, selftest};

/// 传给正在运行的主实例，让它走和托盘「退出应用」相同的有序退出路径。
pub const QUIT: &str = "--quit";

/// Application-only arrival metadata; the native IPC payload and protocol are unchanged.
#[derive(Debug)]
pub(super) struct ObservedInvocation {
    invocation: Invocation,
    ticks: i64,
}

impl ObservedInvocation {
    pub(super) fn received(invocation: Invocation) -> Self {
        Self::received_with(invocation, &shared_coordinator())
    }

    /// 在桥接生产端分类并取消；只读探针和重复自启不会影响待粘贴任务。
    fn received_with(invocation: Invocation, coordinator: &PasteCoordinator) -> Self {
        let ticks = kwikpaste_os::clock::now_ticks();
        if invocation_controls_window(&invocation) {
            coordinator.cancel_before(ticks);
        }
        Self { invocation, ticks }
    }
}

/// Autolaunch and read-only selftest probes are not window-control observations.
fn invocation_controls_window(invocation: &Invocation) -> bool {
    let args = invocation.args.get(1..).unwrap_or_default();
    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            QUIT | selftest::QUIT
                | selftest::SHOW
                | selftest::HIDE
                | selftest::TOGGLE
                | selftest::EDIT
                | selftest::END_EDIT
        ) || arg.starts_with(selftest::COPY_ITEM)
    }) {
        return true;
    }
    if args.iter().any(|arg| arg.starts_with("--selftest-")) {
        return false;
    }
    host::request_for_invocation(args, &invocation.cwd).is_some()
}

/// 持有主实例守卫：退出前丢弃，释放单实例名字。`sender` 是转交参数的入口，重新占回单实例时用。
struct Instance {
    guard: Option<PrimaryInstance>,
    sender: Sender<ObservedInvocation>,
}

impl Global for Instance {}

pub fn serve(
    cx: &mut App,
    guard: PrimaryInstance,
    (sender, invocations): (Sender<ObservedInvocation>, Receiver<ObservedInvocation>),
    commands: Sender<PanelCommand>,
) {
    cx.set_global(Instance {
        guard: Some(guard),
        sender,
    });
    cx.on_app_quit(|cx| {
        release(cx);
        async {}
    })
    .detach();

    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(invocation) = invocations.recv().await {
            handle(&invocation.invocation, invocation.ticks, &commands, cx).await;
        }
    })
    .detach();
}

/// 释放单实例（关互斥体、销毁消息窗口）：退出前、更新交接拉起新进程之前调用。必须在主线程上：
/// 消息窗口只能由创建它的线程销毁。
pub fn release(cx: &mut App) {
    if cx.has_global::<Instance>() {
        cx.global_mut::<Instance>().guard = None;
    }
}

/// [`release`] 之后没能交出去（例如提权重启被取消）时重新占回单实例。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn reclaim(cx: &mut App) -> anyhow::Result<()> {
    let Some(instance) = cx.try_global::<Instance>() else {
        anyhow::bail!("the single instance is not served");
    };
    if instance.guard.is_some() {
        return Ok(());
    }
    let sender = instance.sender.clone();
    let claim = single_instance::claim(crate::identity::identifier(), move |invocation| {
        let _ = sender.try_send(ObservedInvocation::received(invocation));
    })?;
    match claim {
        Claim::Primary(guard) => {
            cx.global_mut::<Instance>().guard = Some(guard);
            Ok(())
        }
        Claim::Forwarded => anyhow::bail!("another instance took over the single instance name"),
    }
}

async fn handle(
    invocation: &Invocation,
    ticks: i64,
    commands: &Sender<PanelCommand>,
    cx: &mut AsyncApp,
) {
    let args = invocation.args.get(1..).unwrap_or_default();
    log::info!("another launch handed over {args:?}");

    if args.iter().any(|arg| arg == QUIT) {
        crate::health::suppress_watchdog_restart();
        cx.update(|cx| cx.quit());
        return;
    }

    if selftest::enabled(selftest::PLATFORM) && handle_selftest(args, ticks, commands, cx).await {
        return;
    }
    // 备份文件 → 导入，重复的自启 → 忽略，其它 → 偏好设置（与 1.x 相同），交给 UI。
    if let Some(request) = host::request_for_invocation(args, &invocation.cwd) {
        cx.update(|cx| host::dispatch_observed(cx, request, ticks));
    }
}

/// 平台自测的远程命令（主实例本身也处于 `--selftest-platform` 时才接受）；处理了返回 `true`。
async fn handle_selftest(
    args: &[String],
    ticks: i64,
    commands: &Sender<PanelCommand>,
    cx: &mut AsyncApp,
) -> bool {
    let trigger = Trigger {
        source: TriggerSource::SecondInstance,
        ticks,
    };
    for arg in args {
        let command = match arg.as_str() {
            selftest::SHOW => Some(PanelCommand::Show(trigger)),
            selftest::HIDE => Some(PanelCommand::Hide(trigger)),
            selftest::TOGGLE => Some(PanelCommand::Toggle(trigger)),
            selftest::EDIT => Some(PanelCommand::BeginEditing(EditTrigger::Keyboard)),
            selftest::END_EDIT => Some(PanelCommand::EndEditing),
            _ => None,
        };
        if let Some(command) = command {
            let _ = commands.try_send(command.observed_at(ticks));
            return true;
        }

        match arg.as_str() {
            selftest::IME_STATE => probe::ime_state(),
            selftest::IME_NATIVE => probe::set_ime_native_mode(),
            selftest::READ_NOW => read_now(cx).await,
            selftest::COUNT => count(cx).await,
            selftest::QUIT => {
                probe::quitting();
                cx.update(|cx| cx.quit());
            }
            selftest::VSYNC_DEAD => watchdog::simulate_dead_render_thread(),
            selftest::DISPLAY_SLEEP => watchdog::simulate_display_sleep(),
            selftest::WATCHDOG => probe::watchdog_started(),
            _ => {
                if let Some(delay) = arg.strip_prefix(selftest::ASYNC_FRAME) {
                    let delay = std::time::Duration::from_millis(delay.parse().unwrap_or(0));
                    cx.background_executor().timer(delay).await;
                    probe::arm_async_frame();
                    cx.update(|cx| cx.refresh_windows());
                } else if let Some(attempts) = arg.strip_prefix(selftest::DEVICE_LOST) {
                    let attempts = attempts.parse().unwrap_or(0);
                    cx.update(|cx| watchdog::simulate_device_lost(attempts, cx));
                } else if let Some(place) = arg.strip_prefix(selftest::PANIC) {
                    selftest_panic(place);
                } else if let Some(count) = arg.strip_prefix(selftest::SEED) {
                    seed_history(count, cx).await;
                } else if let Some(json) = arg.strip_prefix(selftest::DRAG_PAYLOAD) {
                    match super::drag_out::set_selftest_payload(json) {
                        Ok(()) => probe::view_event("drag_payload", json),
                        Err(err) => log::error!("selftest drag payload rejected: {err:#}"),
                    }
                } else if let Some(patch) = arg.strip_prefix(selftest::SETTINGS) {
                    update_settings(patch, cx).await;
                } else if let Some(code) = arg.strip_prefix(selftest::HANDOFF) {
                    let code = code.parse().unwrap_or(0);
                    cx.update(|cx| updater::rehearse_handoff(cx, code));
                } else if let Some(id) = arg.strip_prefix(selftest::COPY_ITEM) {
                    if cx.update(|cx| paste::control_is_stale(cx, ticks)) {
                        return true;
                    }
                    let copied = cx.update(|cx| paste::copy(cx, id.to_owned(), false, true));
                    if let Err(err) = copied.await {
                        log::error!("selftest copy of {id} failed: {err}");
                    }
                } else {
                    continue;
                }
            }
        }
        return true;
    }

    false
}

/// `--selftest-panic=main|thread|native`：故意崩溃，验证 panic hook、原生崩溃处理和崩溃重启。
/// `main` 在这个前台任务里 panic（主线程的窗口过程内，进程随即 abort）；`thread` 在一个新线程上
/// panic（`panic = "unwind"` 时进程活着，走有序重启；`abort` 时进程结束）；`native` 在主线程上抛
/// 一个没人处理的访问冲突（Windows）。
fn selftest_panic(place: &str) {
    log::warn!("selftest: crashing on {place}");
    match place {
        "thread" => {
            let spawned = std::thread::Builder::new()
                .name("selftest-panic".to_owned())
                .spawn(|| panic!("selftest panic on a worker thread"));
            if let Err(err) = spawned {
                log::error!("selftest panic thread did not start: {err}");
            }
        }
        #[cfg(target_os = "windows")]
        "native" => kwikpaste_os::win::crash::raise_access_violation(),
        _ => panic!("selftest panic on the main thread"),
    }
}

/// `--selftest-seed=<n>`：在后台线程上灌合成记录，完成后写探针事件 `seeded`。
async fn seed_history(count: &str, cx: &mut AsyncApp) {
    let Ok(count) = count.parse::<usize>() else {
        log::error!("selftest seed count {count:?} is not a number");
        return;
    };
    let Some(core) = cx.update(|cx| core_host::core(cx).cloned()) else {
        return;
    };
    match cx
        .background_executor()
        .spawn(super::seed::seed(core, count))
        .await
    {
        Ok(seeded) => probe::seeded(&seeded),
        Err(err) => log::error!("selftest seed failed: {err:#}"),
    }
}

/// `--selftest-read-now`：手动读取一次剪贴板，结果写进探针日志。
async fn read_now(cx: &mut AsyncApp) {
    let Some(core) = cx.update(|cx| core_host::core(cx).cloned()) else {
        return;
    };
    probe::read_now(&core.read_clipboard_now().await);
}

/// `--selftest-count`：历史记录总数写进探针日志。
async fn count(cx: &mut AsyncApp) {
    let Some(core) = cx.update(|cx| core_host::core(cx).cloned()) else {
        return;
    };
    let query = kwikpaste_core::db::models::ClipboardItemQuery {
        limit: 1,
        ..Default::default()
    };
    match core.list_items(query).await {
        Ok(page) => probe::count(page.total),
        Err(err) => log::error!("selftest count failed: {err}"),
    }
}

/// `--selftest-settings=<JSON patch>`：经 core 更新设置，走与偏好页相同的 `SettingsUpdated` 路径。
async fn update_settings(patch: &str, cx: &mut AsyncApp) {
    let patch = match serde_json::from_str::<serde_json::Value>(patch) {
        Ok(patch) => patch,
        Err(err) => {
            log::error!("selftest settings patch is not JSON: {err}");
            return;
        }
    };
    let Some(core) = cx.update(|cx| core_host::core(cx).cloned()) else {
        return;
    };
    if let Err(err) = core.update_settings(patch).await {
        log::error!("selftest settings patch was rejected: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::super::paste_coordinator::PasteCoordinator;
    use super::*;
    use std::sync::Arc;

    #[test]
    fn delayed_second_launch_keeps_arrival_time_and_cannot_cancel_a_newer_paste() {
        for extra_args in [vec![], vec!["history.kwikpastebak".to_owned()]] {
            let coordinator = Arc::new(PasteCoordinator::default());
            let mut args = vec!["KwikPaste".to_owned()];
            args.extend(extra_args);
            let event = ObservedInvocation::received_with(
                Invocation {
                    cwd: String::new(),
                    args,
                },
                &coordinator,
            );
            let arrival_ticks = event.ticks;
            let (sender, receiver) = async_channel::bounded(1);
            sender
                .try_send(event)
                .expect("queue the arrived invocation");

            let newer_paste = coordinator.try_begin(arrival_ticks + 1).unwrap();
            let delivered = receiver.try_recv().expect("consume the delayed invocation");
            assert_eq!(delivered.ticks, arrival_ticks);
            assert!(
                host::request_for_invocation(
                    &delivered.invocation.args[1..],
                    &delivered.invocation.cwd
                )
                .is_some()
            );
            assert!(coordinator.started_after(delivered.ticks));
            coordinator.cancel_before(delivered.ticks);
            assert!(newer_paste.is_current());

            let control = PanelCommand::EndEditing.observed_with(delivered.ticks, &coordinator);
            assert!(
                matches!(control, PanelCommand::Observed { ticks, .. } if ticks == arrival_ticks)
            );
        }
    }

    #[test]
    fn only_ipc_window_controls_cancel_at_arrival_not_autolaunch_or_read_probes() {
        for (argument, cancels) in [
            ("--auto-launch", false),
            (selftest::READ_NOW, false),
            (selftest::COUNT, false),
            (selftest::SHOW, true),
            ("history.kwikpastebak", true),
            ("", true),
        ] {
            let coordinator = Arc::new(PasteCoordinator::default());
            let lease = coordinator.try_begin(-1).unwrap();
            let mut args = vec!["KwikPaste".to_owned()];
            if !argument.is_empty() {
                args.push(argument.to_owned());
            }
            let _event = ObservedInvocation::received_with(
                Invocation {
                    cwd: String::new(),
                    args,
                },
                &coordinator,
            );
            assert_eq!(!lease.is_current(), cancels, "IPC argument {argument}");
        }
    }
}
