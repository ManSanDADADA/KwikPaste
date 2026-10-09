//! 所有粘贴与复制写回共享进程内租约；粘贴先取得外部目标交接票据，再等待原生就绪。
//! 最终就绪检查与按键注入在面板命令循环同一轮执行，异步等待不会授权过期任务。

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::{Either, select};
use gpui::{App, AsyncApp, Global, Task};
use kwikpaste_core::clipboard::ClipboardFragment;
use kwikpaste_core::i18n::commands::{Key, label};
use kwikpaste_core::ops::CopyOutcome;
use kwikpaste_core::{AppError, Core, Result};
use kwikpaste_os::{keystroke, paste_target::PasteTarget};

use super::panel::{PanelCommand, Trigger, TriggerSource};
use super::paste_coordinator::{PasteCoordinator, PasteLease, PasteToken, shared_coordinator};
use super::probe;
use crate::core_host;

const HANDOFF_TIMEOUT: Duration = Duration::from_secs(1);
const READINESS_POLL: Duration = Duration::from_millis(15);
const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

struct PasteState(Arc<PasteCoordinator>);

impl Default for PasteState {
    fn default() -> Self {
        Self(shared_coordinator())
    }
}

impl Global for PasteState {}

pub(super) fn init(cx: &mut App) {
    cx.set_global(PasteState::default());
}

pub(super) fn cancel_pending_before(cx: &App, ticks: i64) {
    if let Some(state) = cx.try_global::<PasteState>() {
        state.0.cancel_before(ticks);
    }
}

pub(super) fn control_is_stale(cx: &App, ticks: i64) -> bool {
    cx.try_global::<PasteState>()
        .is_some_and(|state| state.0.started_after(ticks))
}

/// Invalidate asynchronous work immediately and reset the native ticket through its owning loop.
pub(super) fn cancel(cx: &App) {
    super::request(cx, PanelCommand::CancelPaste);
}

fn acquire(cx: &mut App, core: &Core) -> Result<PasteLease> {
    cx.default_global::<PasteState>()
        .0
        .try_begin(kwikpaste_os::clock::now_ticks())
        .ok_or_else(|| AppError::Clipboard(label(core.language(), Key::PasteBusy).to_owned()))
}

/// 粘贴一条记录：租约在 core 写回之前取得，直到注入结果回读后才释放。
pub fn paste(cx: &mut App, id: String, plain: bool, keep_visible: bool) -> Task<Result<()>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };
    let lease = match acquire(cx, &core) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };

    let capture = start_capture(cx, &lease);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let _phase = super::enter_phase(crate::health::Phase::Paste);
        let started = Instant::now();
        let captured = capture.wait(cx).await;
        core.prepare_paste(&id, plain).await?;
        let captured = captured.map_err(|error| failed_handoff(cx, &core, error))?;
        let report = yield_and_inject(cx, keep_visible, &lease, &core, &captured).await?;
        probe::pasted("item", &id, plain, report, started.elapsed());
        Ok(())
    })
}

/// 粘贴一条记录里的片段，流程同 [`paste`]。
pub fn paste_fragment(
    cx: &mut App,
    id: String,
    fragment: ClipboardFragment,
    keep_visible: bool,
) -> Task<Result<()>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };
    let lease = match acquire(cx, &core) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };

    let capture = start_capture(cx, &lease);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let _phase = super::enter_phase(crate::health::Phase::Paste);
        let started = Instant::now();
        let captured = capture.wait(cx).await;
        core.prepare_paste_fragment(&id, fragment).await?;
        let captured = captured.map_err(|error| failed_handoff(cx, &core, error))?;
        let report = yield_and_inject(cx, keep_visible, &lease, &core, &captured).await?;
        probe::pasted("fragment", &id, false, report, started.elapsed());
        Ok(())
    })
}

/// 显式复制先取消旧注入；旧写回尚未完成时明确返回繁忙，避免迟到写回覆盖本次复制。
pub fn copy(
    cx: &mut App,
    id: String,
    plain: bool,
    keep_visible: bool,
) -> Task<Result<CopyOutcome>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };
    cancel(cx);
    let lease = match acquire(cx, &core) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        let outcome = core.copy_item(&id, plain).await?;
        finish_copy(cx, keep_visible, outcome.hide_window);
        probe::copied(&id, plain, outcome.hide_window);
        drop(lease);
        Ok(outcome)
    })
}

/// 复制图片识别文本；同其它复制路径一起取消旧注入并互斥写回。
pub fn copy_image_text(cx: &mut App, id: String, keep_visible: bool) -> Task<Result<CopyOutcome>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };
    cancel(cx);
    let lease = match acquire(cx, &core) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let outcome = core.copy_image_text(&id).await?;
        finish_copy(cx, keep_visible, outcome.hide_window);
        probe::copied(&id, false, outcome.hide_window);
        drop(lease);
        Ok(outcome)
    })
}

/// 把记录里的片段写回剪贴板（不粘贴），互斥与隐藏规则同 [`copy`]。
pub fn copy_fragment(
    cx: &mut App,
    id: String,
    fragment: ClipboardFragment,
    keep_visible: bool,
) -> Task<Result<CopyOutcome>> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(Err(core_missing()));
    };
    cancel(cx);
    let lease = match acquire(cx, &core) {
        Ok(lease) => lease,
        Err(error) => return Task::ready(Err(error)),
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        let outcome = core.copy_fragment(&id, fragment).await?;
        finish_copy(cx, keep_visible, outcome.hide_window);
        probe::copied(&id, false, outcome.hide_window);
        drop(lease);
        Ok(outcome)
    })
}

fn finish_copy(cx: &mut AsyncApp, keep_visible: bool, hide_window: bool) {
    cx.update(|cx| {
        if keep_visible {
            super::request(cx, PanelCommand::SetInputCapture(false));
        } else if hide_window {
            super::request(cx, PanelCommand::Hide(Trigger::now(TriggerSource::Copy)));
        }
    });
}

/// 全局快速粘贴；繁忙或失败沿用快捷键路径只记日志。
pub fn quick_paste(cx: &mut App, offset: i64) -> Task<()> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(());
    };
    let lease = match acquire(cx, &core) {
        Ok(lease) => lease,
        Err(error) => {
            log::warn!("quick paste of item {} refused: {error}", offset + 1);
            return Task::ready(());
        }
    };

    let capture = start_capture(cx, &lease);
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(err) = run_quick_paste(&core, offset, cx, &lease, capture).await {
            log::warn!("quick paste of item {} failed: {err}", offset + 1);
        }
    })
}

/// 全局粘贴当前剪贴板的纯文本表示；没有可用文本时不注入按键。
pub fn paste_plain(cx: &mut App) -> Task<()> {
    let Some(core) = core_host::core(cx).cloned() else {
        return Task::ready(());
    };
    let lease = match acquire(cx, &core) {
        Ok(lease) => lease,
        Err(error) => {
            log::warn!("plain paste refused: {error}");
            return Task::ready(());
        }
    };

    let capture = start_capture(cx, &lease);
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(err) = run_plain_paste(&core, cx, &lease, capture).await {
            log::warn!("plain paste failed: {err}");
        }
    })
}

async fn run_quick_paste(
    core: &Core,
    offset: i64,
    cx: &mut AsyncApp,
    lease: &PasteLease,
    capture: PendingCapture,
) -> Result<()> {
    let _phase = super::enter_phase(crate::health::Phase::Paste);
    let started = Instant::now();
    let captured = capture.wait(cx).await;
    let Some(ticket) = core.prepare_quick_paste(offset).await? else {
        return Ok(());
    };
    let captured = captured.map_err(|error| failed_handoff(cx, core, error))?;
    if !wait_for_modifiers_released(cx, lease).await {
        return Err(failed_handoff(
            cx,
            core,
            anyhow::anyhow!("paste modifiers remained held or the request was cancelled"),
        ));
    }

    let report = yield_and_inject(cx, false, lease, core, &captured).await?;
    probe::pasted("quick", &ticket.item_id, false, report, started.elapsed());
    drop(ticket);
    Ok(())
}

async fn run_plain_paste(
    core: &Core,
    cx: &mut AsyncApp,
    lease: &PasteLease,
    capture: PendingCapture,
) -> Result<()> {
    let _phase = super::enter_phase(crate::health::Phase::Paste);
    let started = Instant::now();
    let captured = capture.wait(cx).await;
    let Some(ticket) = core.prepare_plain_paste_from_clipboard().await? else {
        log::debug!("plain paste skipped: current clipboard has no text or files");
        return Ok(());
    };
    let captured = captured.map_err(|error| failed_handoff(cx, core, error))?;
    if !wait_for_modifiers_released(cx, lease).await {
        return Err(failed_handoff(
            cx,
            core,
            anyhow::anyhow!("paste modifiers remained held or the request was cancelled"),
        ));
    }

    let report = yield_and_inject(cx, false, lease, core, &captured).await?;
    probe::pasted("plain", &ticket.item_id, true, report, started.elapsed());
    drop(ticket);
    Ok(())
}

/// The acknowledgment payload itself owns cleanup, so cancelled receivers cannot leak a retained ticket.
#[derive(Debug)]
pub struct PasteCapture {
    target: PasteTarget,
    commands: async_channel::Sender<PanelCommand>,
}

impl PasteCapture {
    pub(super) fn new(target: PasteTarget, commands: async_channel::Sender<PanelCommand>) -> Self {
        Self { target, commands }
    }
}

impl Drop for PasteCapture {
    fn drop(&mut self) {
        let _ = self
            .commands
            .try_send(PanelCommand::CancelCapturedPaste(self.target));
    }
}

struct PendingCapture {
    reply: async_channel::Receiver<anyhow::Result<PasteCapture>>,
    deadline: Instant,
}

impl PendingCapture {
    async fn wait(self, cx: &mut AsyncApp) -> anyhow::Result<PasteCapture> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("paste target capture timed out"))?;
        receive_reply(self.reply, cx.background_executor().timer(remaining)).await
    }
}

/// Enqueue capture before spawning any clipboard-preparation future; the panel loop performs it outside GPUI borrows.
fn start_capture(cx: &App, lease: &PasteLease) -> PendingCapture {
    let (done, reply) = async_channel::bounded(1);
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    super::request(
        cx,
        PanelCommand::CapturePasteTarget {
            token: lease.token(),
            deadline,
            done,
        },
    );
    PendingCapture { reply, deadline }
}

/// Pending captures are cancellable even before a native ticket exists.
pub(super) fn capture_if_current<T>(
    token: &PasteToken,
    deadline: Instant,
    begin: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if !token.is_current() || Instant::now() >= deadline {
        anyhow::bail!("paste target capture expired or was cancelled");
    }
    begin()
}

/// Yield uses the original ticket and must never establish a replacement session after an asynchronous wait.
pub(super) fn yield_captured(
    target: PasteTarget,
    visible: bool,
    validate: impl FnOnce(PasteTarget) -> anyhow::Result<()>,
    release: impl FnOnce(),
) -> anyhow::Result<PasteHandoff> {
    validate(target)?;
    release();
    Ok(PasteHandoff {
        target,
        panel_was_visible: visible,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct PasteHandoff {
    pub target: PasteTarget,
    pub panel_was_visible: bool,
}

/// 一次注入的经过，供探针记录。
#[derive(Debug, Clone, Copy)]
pub struct InjectReport {
    pub panel_was_visible: bool,
    pub foreground: isize,
}

/// Permission UI runs after copying; the already captured target must survive it without reselection.
async fn yield_and_inject(
    cx: &mut AsyncApp,
    keep_visible: bool,
    lease: &PasteLease,
    core: &Core,
    captured: &PasteCapture,
) -> Result<InjectReport> {
    let result = async {
        if !lease.is_current() {
            anyhow::bail!("paste request was cancelled before handoff");
        }
        keystroke::ensure_accessibility_trusted()?;
        if !lease.is_current() {
            anyhow::bail!("paste request was cancelled by permission UI");
        }
        let (done, yielded) = async_channel::bounded(1);
        let token = lease.token();
        let deadline = Instant::now() + HANDOFF_TIMEOUT;
        cx.update(|cx| {
            super::request(
                cx,
                PanelCommand::YieldForPaste {
                    keep_visible,
                    target: captured.target,
                    token: token.clone(),
                    deadline,
                    done,
                },
            )
        });
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("paste input yield timed out"))?;
        let handoff = receive_reply(yielded, cx.background_executor().timer(remaining)).await?;
        loop {
            if !token.is_current() {
                anyhow::bail!("paste request was superseded while waiting for native readiness");
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| anyhow::anyhow!("paste native handoff timed out"))?;
            let (done, ready) = async_channel::bounded(1);
            cx.update(|cx| {
                super::request(
                    cx,
                    PanelCommand::PasteReady {
                        target: handoff.target,
                        token: token.clone(),
                        deadline,
                        done,
                    },
                )
            });
            if receive_reply(ready, cx.background_executor().timer(remaining)).await? {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or_else(|| anyhow::anyhow!("paste injection timed out"))?;
                let (done, injected) = async_channel::bounded(1);
                cx.update(|cx| {
                    super::request(
                        cx,
                        PanelCommand::InjectPaste {
                            handoff,
                            token: token.clone(),
                            deadline,
                            done,
                        },
                    )
                });
                return receive_reply(injected, cx.background_executor().timer(remaining)).await;
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| anyhow::anyhow!("paste native handoff timed out"))?;
            cx.background_executor()
                .timer(READINESS_POLL.min(remaining))
                .await;
        }
    }
    .await;
    result.map_err(|error| failed_handoff(cx, core, error))
}

/// A closed or timed-out acknowledgment is a failure, never an invisible-panel success.
async fn receive_reply<T>(
    reply: async_channel::Receiver<anyhow::Result<T>>,
    timeout: impl Future<Output = ()>,
) -> anyhow::Result<T> {
    match select(Box::pin(reply.recv()), Box::pin(timeout)).await {
        Either::Left((result, _)) => {
            result.map_err(|_| anyhow::anyhow!("paste acknowledgment channel closed"))?
        }
        Either::Right(_) => anyhow::bail!("paste acknowledgment timed out"),
    }
}

/// This function executes without awaits in the native panel's command turn.
pub(super) fn inject_if_current<T>(
    token: &PasteToken,
    native_ready: impl FnOnce() -> anyhow::Result<bool>,
    inject: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if !token.is_current() {
        anyhow::bail!("paste injection request was superseded");
    }
    if !native_ready()? {
        anyhow::bail!("paste destination is not ready for injection");
    }
    if !token.is_current() {
        anyhow::bail!("paste injection request changed during native validation");
    }
    inject()
}

fn failed_handoff(cx: &mut AsyncApp, core: &Core, error: anyhow::Error) -> AppError {
    log::warn!("paste handoff stopped: {error:#}");
    cx.update(|cx| cancel(cx));
    let key = if error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
    {
        Key::PastePermissionMissing
    } else {
        Key::PasteHandoffFailed
    };
    AppError::Clipboard(label(core.language(), key).to_owned())
}

async fn wait_for_modifiers_released(cx: &mut AsyncApp, lease: &PasteLease) -> bool {
    let deadline = Instant::now() + MODIFIER_RELEASE_TIMEOUT;
    loop {
        if !lease.is_current() {
            return false;
        }
        if !keystroke::modifiers_pressed() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        cx.background_executor().timer(READINESS_POLL).await;
    }
}

fn core_missing() -> AppError {
    AppError::Other(anyhow::anyhow!("kwikpaste-core is not running"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const EARLY_TARGET: PasteTarget = PasteTarget {
        generation: 1,
        window: 100,
        process_id: 20,
    };

    #[test]
    fn inside_click_observed_before_capture_processing_prevents_creating_a_ticket() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(100).unwrap();
        let inside = PanelCommand::SetInputCapture(true).observed_with(150, &state);
        let PanelCommand::Observed { ticks, .. } = inside else {
            panic!("missing inside stamp");
        };
        state.cancel_before(ticks);
        let began = Cell::new(false);
        let result = capture_if_current(&lease.token(), Instant::now() + HANDOFF_TIMEOUT, || {
            began.set(true);
            Ok(EARLY_TARGET)
        });
        assert!(result.is_err());
        assert!(!began.get());
    }

    #[test]
    fn cancelling_a_preparation_future_drops_the_captured_ticket() {
        use std::task::{Context, Poll};
        let (commands, receiver) = async_channel::bounded(1);
        let captured = PasteCapture::new(EARLY_TARGET, commands);
        let mut preparing = Box::pin(async move {
            let _capture = captured;
            futures::future::pending::<()>().await;
        });
        let mut context = Context::from_waker(futures::task::noop_waker_ref());
        assert!(matches!(
            preparing.as_mut().poll(&mut context),
            Poll::Pending
        ));
        drop(preparing);
        assert!(
            matches!(receiver.try_recv(), Ok(PanelCommand::CancelCapturedPaste(target)) if target == EARLY_TARGET)
        );
    }

    #[test]
    fn cancelled_early_ticket_cannot_release_input_or_create_a_new_handoff() {
        let current_generation = Cell::new(EARLY_TARGET.generation);
        current_generation.set(2); // Native inside-click canceled the captured session during prepare.
        let released = Cell::new(false);
        let result = yield_captured(
            EARLY_TARGET,
            true,
            |target| {
                anyhow::ensure!(
                    target.generation == current_generation.get(),
                    "early ticket was cancelled"
                );
                Ok(())
            },
            || released.set(true),
        );
        assert!(result.is_err());
        assert!(!released.get());
        assert_eq!(current_generation.get(), 2);
    }

    #[test]
    fn target_selected_before_modifier_wait_cannot_be_replaced_by_new_foreground() {
        let foreground_after_wait = 200;
        let released = Cell::new(false);
        let result = yield_captured(
            EARLY_TARGET,
            false,
            |target| {
                anyhow::ensure!(
                    target.window == foreground_after_wait,
                    "user chose another external window"
                );
                Ok(())
            },
            || released.set(true),
        );
        assert!(result.is_err());
        assert!(!released.get());
    }

    #[test]
    fn dropped_capture_reply_cleans_up_only_its_own_ticket() {
        let (commands, receiver) = async_channel::bounded(1);
        let (done, reply) = async_channel::bounded(1);
        done.try_send(Ok::<_, anyhow::Error>(PasteCapture::new(
            EARLY_TARGET,
            commands,
        )))
        .unwrap();
        drop(done);
        drop(reply);
        assert!(
            matches!(receiver.try_recv(), Ok(PanelCommand::CancelCapturedPaste(target)) if target == EARLY_TARGET)
        );
    }

    #[test]
    fn final_injection_rejects_a_cancelled_lease() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(kwikpaste_os::clock::now_ticks()).unwrap();
        let token = lease.token();
        state.cancel();
        let injected = Cell::new(false);
        let result = inject_if_current(
            &token,
            || Ok(true),
            || {
                injected.set(true);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!injected.get());
    }

    #[test]
    fn final_injection_rechecks_the_token_after_native_readiness() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(kwikpaste_os::clock::now_ticks()).unwrap();
        let injected = Cell::new(false);
        let result = inject_if_current(
            &lease.token(),
            || {
                state.cancel();
                Ok(true)
            },
            || {
                injected.set(true);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!injected.get());
    }

    #[test]
    fn final_injection_rejects_native_waiting_and_errors() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(kwikpaste_os::clock::now_ticks()).unwrap();
        for readiness in [Ok(false), Err(anyhow::anyhow!("target owner changed"))] {
            let injected = Cell::new(false);
            let result = inject_if_current(
                &lease.token(),
                || readiness,
                || {
                    injected.set(true);
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert!(!injected.get());
        }
    }

    #[test]
    fn final_injection_runs_once_for_a_live_ready_target() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(kwikpaste_os::clock::now_ticks()).unwrap();
        let calls = Cell::new(0);
        let result = inject_if_current(
            &lease.token(),
            || Ok(true),
            || {
                calls.set(calls.get() + 1);
                Ok(42)
            },
        );
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn closed_handoff_channel_is_an_error_not_an_invisible_panel() {
        let (sender, receiver) = async_channel::bounded::<anyhow::Result<PasteHandoff>>(1);
        drop(sender);
        let result =
            futures::executor::block_on(receive_reply(receiver, futures::future::pending()));
        assert!(result.unwrap_err().to_string().contains("channel closed"));
    }

    #[test]
    fn late_acknowledgment_times_out_without_authorizing_injection() {
        let (_sender, receiver) = async_channel::bounded::<anyhow::Result<bool>>(1);
        let result =
            futures::executor::block_on(receive_reply(receiver, futures::future::ready(())));
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }
}
