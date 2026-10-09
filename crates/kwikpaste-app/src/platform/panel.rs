//! 剪贴板面板：启动时以隐藏状态预创建、永不销毁；显示、隐藏、定位只走原生调用，从不激活。
//!
//! 热键、托盘、第二实例、钩子和 UI 自己的请求都送进同一个 channel，由一个 `cx.spawn` 循环按顺序
//! 执行。原生调用都在 `cx.update` 之外：GPUI 的窗口过程因此能同步拿到 `App`，`ShowWindow` 发出的
//! `WM_SHOWWINDOW` 会当场画出首帧，改位置、改尺寸、激活的回调也不会因为借用冲突被丢掉。

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::Context as _;
use async_channel::{Receiver, Sender};
use gpui::{
    AnyView, AnyWindowHandle, App, AppContext as _, AsyncApp, Bounds, Capslock, Context, Entity,
    EventEmitter, FocusHandle, Global, InteractiveElement as _, IntoElement, Modifiers,
    ModifiersChangedEvent, ParentElement as _, PlatformInput, Render, Styled as _, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, px, size,
};
use kwikpaste_os::clock;
use kwikpaste_os::paste_target::PasteTarget;

use super::editing::EditTrigger;
use super::material::WindowMaterial;
use super::native::NativePanel;
use super::paste::{self, InjectReport, PasteCapture, PasteHandoff};
use super::paste_coordinator::{PasteCoordinator, PasteToken, shared_coordinator};
use super::{probe, window_state};

/// 面板的默认、最小内容区尺寸（逻辑像素），与 1.x 相同；Windows 上再乘系统「文本大小」。
pub const PANEL_SIZE: (f64, f64) = (360.0, 600.0);

/// 显示 / 隐藏请求的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerSource {
    Hotkey,
    Tray,
    SecondInstance,
    OutsideClick,
    #[cfg_attr(
        target_os = "macos",
        expect(dead_code, reason = "Win+V 只在 Windows 上")
    )]
    WinV,
    MouseButton,
    Ui,
    /// 粘贴前让出前台。
    Paste,
    /// 复制后按设置隐藏。
    Copy,
    Selftest,
}

impl TriggerSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Hotkey => "hotkey",
            Self::Tray => "tray",
            Self::SecondInstance => "second-instance",
            Self::OutsideClick => "outside-click",
            Self::WinV => "win-v",
            Self::MouseButton => "mouse-button",
            Self::Ui => "ui",
            Self::Paste => "paste",
            Self::Copy => "copy",
            Self::Selftest => "selftest",
        }
    }
}

/// 一次请求的来源和发出时刻（[`clock::now_ticks`]），用来量“触发到首帧”。
#[derive(Debug, Clone, Copy)]
pub struct Trigger {
    pub source: TriggerSource,
    pub ticks: i64,
}

impl Trigger {
    pub fn now(source: TriggerSource) -> Self {
        Self {
            source,
            ticks: clock::now_ticks(),
        }
    }
}

/// 送给面板循环的命令。
#[derive(Debug, Clone)]
pub enum PanelCommand {
    /// Observation time is captured before user controls enter the asynchronous command queue.
    Observed {
        ticks: i64,
        command: Box<PanelCommand>,
    },
    Toggle(Trigger),
    Show(Trigger),
    Hide(Trigger),
    /// 进入编辑态；结果以 [`PanelEvent::EditingStarted`] 或 [`PanelEvent::EditingRefused`] 送回。
    BeginEditing(EditTrigger),
    /// 退出编辑态，前台还给进入前的窗口；完成后发 [`PanelEvent::EditingEnded`]。
    EndEditing,
    /// 系统「文本大小」变了，面板的最小、默认尺寸跟着补偿。
    #[cfg_attr(
        target_os = "macos",
        expect(dead_code, reason = "macOS 没有单独的文本大小设置")
    )]
    SetTextScale(f64),
    /// 应用当前窗口材质；macOS 需要由持有原生面板的命令循环更新 NSVisualEffectView。
    SetMaterial(WindowMaterial),
    /// Capture and retain a ticket before any asynchronous clipboard preparation or modifier wait.
    CapturePasteTarget {
        token: PasteToken,
        deadline: Instant,
        done: Sender<anyhow::Result<PasteCapture>>,
    },
    /// 粘贴前让出前台：编辑态先把前台还给进入前的窗口，`keep_visible` 为假时再隐藏面板。
    /// 处理完后经 `done` 回报原生目标票据和面板此前是否可见。
    YieldForPaste {
        keep_visible: bool,
        target: PasteTarget,
        token: PasteToken,
        deadline: Instant,
        done: Sender<anyhow::Result<PasteHandoff>>,
    },
    PasteReady {
        target: PasteTarget,
        token: PasteToken,
        deadline: Instant,
        done: Sender<anyhow::Result<bool>>,
    },
    InjectPaste {
        handoff: PasteHandoff,
        token: PasteToken,
        deadline: Instant,
        done: Sender<anyhow::Result<InjectReport>>,
    },
    /// Async cancellation already invalidated the coordinator; this resets its native ticket.
    CancelPaste,
    /// Dropping a capture clears only that ticket, including after no-item, errors or task cancellation.
    CancelCapturedPaste(PasteTarget),
    /// 点击面板外部时是否隐藏（默认是）。UI 在固定面板、打开系统文件对话框期间关掉它。
    SetHideOnOutsideClick(bool),
    /// 非激活窗口上的鼠标按下重新捕获或释放导航键。
    SetInputCapture(bool),
    /// 原生层截获了面板拖动区/缩放边的按下，GPUI 没有机会让弹出菜单自行收起。
    #[cfg_attr(
        all(target_os = "macos", not(test)),
        expect(dead_code, reason = "native drag interception is Windows-only")
    )]
    DismissPopup,
}

impl PanelCommand {
    /// User window actions supersede a paste; internal paste release/reset commands do not.
    pub(super) fn cancels_paste(&self) -> bool {
        match self {
            Self::Observed { command, .. } => command.cancels_paste(),
            Self::Toggle(_)
            | Self::Show(_)
            | Self::BeginEditing(_)
            | Self::EndEditing
            | Self::SetInputCapture(_)
            | Self::DismissPopup
            | Self::CancelPaste => true,
            Self::Hide(trigger) => trigger.source != TriggerSource::Paste,
            _ => false,
        }
    }

    /// Producers without GPUI access also stamp controls before sending them to the loop.
    pub(super) fn observed(self) -> Self {
        self.observed_at(clock::now_ticks())
    }

    /// 保留事件生产时刻，入队前先取消同一进程中的旧粘贴。
    pub(super) fn observed_at(self, ticks: i64) -> Self {
        if matches!(self, Self::Observed { .. }) || !self.cancels_paste() {
            return self;
        }
        self.observed_with(ticks, &shared_coordinator())
    }

    /// 同步失效旧租约再包装原生命令；测试可传入独立协调器。
    pub(super) fn observed_with(self, ticks: i64, coordinator: &PasteCoordinator) -> Self {
        if matches!(self, Self::Observed { .. }) || !self.cancels_paste() {
            return self;
        }
        coordinator.cancel_before(ticks);
        Self::Observed {
            ticks,
            command: Box::new(self),
        }
    }
}

/// 面板状态变化，从 [`Panel::events`] 发出。
///
/// `Shown` 在原生显示之前、与重置焦点同一次 update 里发出，订阅者在这里重置的视图状态会进首帧；
/// `Hidden` 在原生隐藏之后发出，订阅者据此停止刷新、释放缓存。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelEvent {
    Shown,
    Hidden,
    /// 面板已是前台窗口，可以聚焦输入框。
    EditingStarted,
    /// 没能进入编辑态（面板不可见、没拿到前台），焦点留在原处。
    EditingRefused,
    /// 已退出编辑态（包括随面板隐藏退出），焦点应还给列表。
    EditingEnded,
    /// 原生鼠标按下发生在菜单之外（包括不激活的拖动/缩放区域）。
    PopupDismissed,
}

/// 发出 [`PanelEvent`] 的实体。面板窗口打开之前就已建好，UI 在构造自己的视图时即可订阅。
pub struct PanelEvents;

impl EventEmitter<PanelEvent> for PanelEvents {}

/// 面板窗口的根视图：包一层 UI 的视图，提供兜底焦点和首帧计时。
pub struct PanelRoot {
    content: AnyView,
    focus: FocusHandle,
}

impl Render for PanelRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        frame_rendered();

        div()
            .track_focus(&self.focus)
            .size_full()
            .child(self.content.clone())
    }
}

/// 面板的全局句柄。面板窗口打开之前就挂上（只是还没有窗口），UI 的视图构造时就能订阅事件、发命令。
pub struct Panel {
    events: Entity<PanelEvents>,
    window: Option<AnyWindowHandle>,
    commands: Sender<PanelCommand>,
}

impl Global for Panel {}

impl Panel {
    pub fn commands(&self) -> Sender<PanelCommand> {
        self.commands.clone()
    }

    /// 发一条命令，在下一轮主循环里执行。
    pub fn request(&self, command: PanelCommand) {
        if let Err(err) = self.commands.try_send(command.observed()) {
            log::warn!(
                "panel command loop has stopped; dropped {:?}",
                err.into_inner()
            );
        }
    }

    /// 订阅 [`PanelEvent`] 用。
    pub fn events(&self) -> &Entity<PanelEvents> {
        &self.events
    }

    /// 面板窗口，钩子按键派发用；窗口打开之前为 `None`。
    pub fn window(&self) -> Option<AnyWindowHandle> {
        self.window
    }
}

thread_local! {
    static FIRST_FRAME: Cell<Option<Sender<i64>>> = const { Cell::new(None) };
}

static RENDERED_FRAMES: AtomicU64 = AtomicU64::new(0);
/// 面板当前是否显示着（崩溃记录的阶段用）。
static SHOWN: AtomicBool = AtomicBool::new(false);

/// 面板当前是否显示着。
pub fn is_shown() -> bool {
    SHOWN.load(Ordering::SeqCst)
}

/// 面板根视图渲染过的帧数。
pub fn rendered_frames() -> u64 {
    RENDERED_FRAMES.load(Ordering::Relaxed)
}

fn frame_rendered() {
    RENDERED_FRAMES.fetch_add(1, Ordering::Relaxed);
    if let Some(sender) = FIRST_FRAME.take() {
        let _ = sender.try_send(clock::now_ticks());
    }
}

/// 下一次渲染面板根视图时，把渲染时刻送进返回的 channel。
fn arm_first_frame() -> Receiver<i64> {
    let (sender, receiver) = async_channel::bounded(1);
    FIRST_FRAME.set(Some(sender));
    receiver
}

fn window_options() -> WindowOptions {
    let panel_size = size(px(PANEL_SIZE.0 as f32), px(PANEL_SIZE.1 as f32));

    WindowOptions {
        // 隐藏建窗时 GPUI 不应用这里的位置；每次显示都由原生代码重设几何。
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            panel_size,
        ))),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: true,
        // macOS 面板顶部的隐藏标题栏不归系统管：双击不再按系统设置缩放到铺满屏幕，
        // 拖动只走头部的 `window_drag_area`。
        app_owns_titlebar_drag: true,
        is_resizable: true,
        is_minimizable: false,
        // 面板从不激活；默认值会把非活动窗口的动画压到 30 fps。面板因此不能放常驻动画。
        inactive_frame_interval: None,
        window_min_size: Some(panel_size),
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    }
}

/// 以隐藏状态创建面板，挂上全局句柄并启动命令循环；返回 UI 的视图。
pub fn open<V: Render>(
    cx: &mut App,
    text_scale: f64,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<Entity<V>> {
    let events = cx.new(|_| PanelEvents);
    let (commands, receiver) = async_channel::unbounded();
    cx.set_global(Panel {
        events: events.clone(),
        window: None,
        commands: commands.clone(),
    });

    let mut native = None;
    let mut content = None;
    let (window, root) = crate::platform::open_window(window_options(), cx, |window, cx| {
        native = Some(NativePanel::attach(window, text_scale));
        let view = build(window, cx);
        content = Some(view.clone());
        let focus = cx.focus_handle();
        cx.new(|_| PanelRoot {
            content: view.into(),
            focus,
        })
    })?;
    let native = native.context("the panel window was not built")??;
    let content = content.context("the panel window was not built")?;
    native.set_command_sender(commands.clone());

    cx.global_mut::<Panel>().window = Some(window);
    let parts = Parts {
        native,
        window,
        root,
        events,
    };
    cx.spawn(async move |cx| run(parts, receiver, cx).await)
        .detach();

    Ok(content)
}

/// 命令循环持有的面板部件。
struct Parts {
    native: NativePanel,
    window: AnyWindowHandle,
    root: Entity<PanelRoot>,
    events: Entity<PanelEvents>,
}

impl Parts {
    fn emit(&self, event: PanelEvent, cx: &mut AsyncApp) {
        cx.update(|cx| self.events.update(cx, |_, cx| cx.emit(event)));
    }
}

async fn run(parts: Parts, commands: Receiver<PanelCommand>, cx: &mut AsyncApp) {
    if let Err(err) = parts.native.install() {
        log::error!("panel native setup failed: {err:#}");
    }
    if let Some(material) = cx.update(|cx| cx.try_global::<WindowMaterial>().copied()) {
        apply_material(&parts, material, cx);
    }
    probe::ready(&parts.native);
    let mut hide_on_outside_click = true;

    while let Ok(command) = commands.recv().await {
        let command = if let PanelCommand::Observed { ticks, command } = command {
            if cx.update(|cx| paste::control_is_stale(cx, ticks)) {
                continue;
            }
            cx.update(|cx| paste::cancel_pending_before(cx, ticks));
            parts.native.cancel_paste_handoff();
            *command
        } else {
            command
        };
        let visible = parts.native.is_visible();
        let (want_visible, trigger) = match command {
            PanelCommand::Observed { .. } => continue,
            PanelCommand::Toggle(trigger) => {
                let summon = matches!(
                    trigger.source,
                    TriggerSource::Hotkey | TriggerSource::WinV | TriggerSource::MouseButton
                );
                if parts.native.should_recapture_on_toggle(visible, summon) {
                    parts.native.raise();
                    parts.native.set_input_capture(true);
                    continue;
                }
                (!visible, trigger)
            }
            PanelCommand::Show(trigger) => (true, trigger),
            PanelCommand::Hide(trigger)
                if !hide_on_outside_click
                    && matches!(trigger.source, TriggerSource::OutsideClick) =>
            {
                parts.native.set_input_capture(false);
                parts.emit(PanelEvent::PopupDismissed, cx);
                continue;
            }
            PanelCommand::Hide(trigger) => (false, trigger),
            PanelCommand::SetHideOnOutsideClick(hide) => {
                hide_on_outside_click = hide;
                continue;
            }
            PanelCommand::SetInputCapture(captured) => {
                parts.native.set_input_capture(captured);
                continue;
            }
            PanelCommand::DismissPopup => {
                parts.emit(PanelEvent::PopupDismissed, cx);
                continue;
            }
            PanelCommand::BeginEditing(trigger) => {
                begin_editing(&parts, trigger, cx);
                continue;
            }
            PanelCommand::EndEditing => {
                end_editing(&parts, cx);
                continue;
            }
            PanelCommand::SetTextScale(text_scale) => {
                parts.native.set_text_scale(text_scale);
                continue;
            }
            PanelCommand::SetMaterial(material) => {
                apply_material(&parts, material, cx);
                continue;
            }
            PanelCommand::CapturePasteTarget {
                token,
                deadline,
                done,
            } => {
                if done.is_closed() {
                    continue;
                }
                let result = paste::capture_if_current(&token, deadline, || {
                    let target = parts.native.begin_paste_handoff()?;
                    let commands = cx.update(|cx| cx.global::<Panel>().commands());
                    Ok(PasteCapture::new(target, commands))
                });
                let _ = done.try_send(result);
                continue;
            }
            PanelCommand::YieldForPaste {
                keep_visible,
                target,
                token,
                deadline,
                done,
            } => {
                if done.is_closed() {
                    continue;
                }
                let result = (|| {
                    if !token.is_current() || Instant::now() >= deadline {
                        anyhow::bail!("paste request was cancelled before input yield");
                    }
                    paste::yield_captured(
                        target,
                        visible,
                        |target| parts.native.validate_paste_handoff(target),
                        || {
                            if visible {
                                end_editing(&parts, cx);
                                if !keep_visible {
                                    hide(&parts, Trigger::now(TriggerSource::Paste), cx);
                                }
                            }
                            parts.native.set_input_capture(false);
                        },
                    )
                })();
                if result.is_err() {
                    parts.native.cancel_paste_handoff();
                }
                let _ = done.try_send(result);
                continue;
            }
            PanelCommand::PasteReady {
                target,
                token,
                deadline,
                done,
            } => {
                if done.is_closed() {
                    continue;
                }
                let result = if token.is_current() && Instant::now() < deadline {
                    parts
                        .native
                        .paste_handoff_ready(target)
                        .map(|ready| ready && !kwikpaste_os::keystroke::modifiers_pressed())
                } else {
                    Err(anyhow::anyhow!("paste readiness request was superseded"))
                };
                if result.is_err() {
                    parts.native.cancel_paste_handoff();
                }
                let _ = done.try_send(result);
                continue;
            }
            PanelCommand::InjectPaste {
                handoff,
                token,
                deadline,
                done,
            } => {
                if done.is_closed() {
                    continue;
                }
                let result = paste::inject_if_current(
                    &token,
                    || {
                        parts
                            .native
                            .paste_handoff_ready(handoff.target)
                            .map(|ready| ready && !kwikpaste_os::keystroke::modifiers_pressed())
                    },
                    || {
                        if done.is_closed() || Instant::now() >= deadline {
                            anyhow::bail!("paste injection acknowledgment expired");
                        }
                        kwikpaste_os::keystroke::simulate_paste_to(handoff.target)?;
                        Ok(InjectReport {
                            panel_was_visible: handoff.panel_was_visible,
                            foreground: handoff.target.window,
                        })
                    },
                );
                parts.native.cancel_paste_handoff();
                let _ = done.try_send(result);
                continue;
            }
            PanelCommand::CancelPaste => {
                parts.native.cancel_paste_handoff();
                continue;
            }
            PanelCommand::CancelCapturedPaste(target) => {
                parts.native.cancel_paste_handoff_if(target);
                continue;
            }
        };

        match (want_visible, visible) {
            (true, false) => show(&parts, trigger, cx),
            (true, true) => parts.native.raise(),
            (false, true) => hide(&parts, trigger, cx),
            (false, false) => {}
        }
    }
}

fn apply_material(parts: &Parts, material: WindowMaterial, cx: &mut AsyncApp) {
    let result = cx.update(|cx| {
        parts.window.update(cx, |_, window, cx| {
            super::material::apply_to_window(window, &material);
            kwikpaste_ui::set_root_translucent(
                window,
                material.effective != kwikpaste_core::settings::Material::Default,
                cx,
            );
        })
    });
    if let Err(err) = result {
        log::debug!("window material not applied now: {err:#}");
    }
    #[cfg(target_os = "macos")]
    if let Err(err) = parts.native.set_material(material.effective) {
        log::debug!("macOS material not applied now: {err:#}");
    }
}

fn show(parts: &Parts, trigger: Trigger, cx: &mut AsyncApp) {
    // vsync 线程死了、或 GPU 设备恢复不了，面板只会出一帧或黑屏：不显示，有序重启。
    if !super::watchdog::render_ok("panel show") {
        return;
    }
    let native = &parts.native;
    let layout = cx.update(|cx| window_state::layout(cx));
    let placement = native
        .place(&layout)
        .inspect_err(|err| log::error!("panel placement failed, showing in place: {err:#}"))
        .ok();

    let prepared = cx.update(|cx| {
        parts.events.update(cx, |_, cx| cx.emit(PanelEvent::Shown));
        parts.window.update(cx, |_, window, cx| {
            if window.focused(cx).is_none() {
                let focus = parts.root.read(cx).focus.clone();
                window.focus(&focus, cx);
            }
            window.refresh();
        })
    });
    if let Err(err) = prepared {
        log::error!("panel could not prepare its first frame: {err:#}");
    }

    let first_frame = arm_first_frame();
    let show_started = clock::now_ticks();
    native.show();
    if crate::selftest::enabled(crate::selftest::PANEL_INVARIANTS)
        && let Err(err) = probe::panel_invariants(native)
    {
        log::error!("panel invariants selftest failed: {err:#}");
        std::process::exit(1);
    }
    SHOWN.store(true, Ordering::SeqCst);
    crate::health::set_phase(crate::health::Phase::Panel);
    let show_returned = clock::now_ticks();
    // 列表自测（跑分、截图、交互脚本）期间不装全局键鼠钩子，免得吞掉本机其它程序（包括同时在跑的
    // 平台探针）的方向键、回车和外部点击；它们的按键由自测自己派发给面板窗口。
    if !crate::selftest::list_selftest() {
        native.start_hooks();
    }
    if let Some(placement) = &placement {
        native.verify(placement);
    }

    let report = ShowReport {
        trigger,
        show_started,
        show_returned,
        native_fields: probe::enabled().then(|| native.probe_fields(placement.as_ref())),
    };
    cx.foreground_executor()
        .spawn(async move {
            if let Ok(rendered) = first_frame.recv().await {
                report.finish(rendered);
            }
        })
        .detach();
}

fn hide(parts: &Parts, trigger: Trigger, cx: &mut AsyncApp) {
    let native = &parts.native;
    let was_editing = native.is_editing();
    // 编辑中隐藏：系统自己把前台交还给原窗口，不再去抢。
    native.end_editing(false);
    native.stop_hooks();
    let geometry = native.hide();
    SHOWN.store(false, Ordering::SeqCst);
    crate::health::set_phase(crate::health::Phase::Idle);

    cx.update(|cx| {
        if let Some(geometry) = geometry {
            window_state::save(cx, geometry);
        }
        // 钩子停了，之后真实的 Ctrl 松开收不到，先把修饰键状态复位，免得快捷键提示残留。
        let _ = parts.window.update(cx, |_, window, cx| {
            window.dispatch_event(
                PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                    modifiers: Modifiers::default(),
                    capslock: Capslock::default(),
                }),
                cx,
            );
        });
        parts.events.update(cx, |_, cx| {
            if was_editing {
                cx.emit(PanelEvent::EditingEnded);
            }
            cx.emit(PanelEvent::Hidden);
        });
    });

    if probe::enabled() {
        if was_editing {
            probe::editing_ended(&native.probe_fields(None));
        }
        probe::hidden(trigger, &native.probe_fields(None));
    }
}

fn begin_editing(parts: &Parts, trigger: EditTrigger, cx: &mut AsyncApp) {
    let native = &parts.native;
    if native.is_editing() {
        parts.emit(PanelEvent::EditingStarted, cx);
        return;
    }

    let result = if native.is_visible() {
        native.begin_editing(trigger)
    } else {
        Err(anyhow::anyhow!("the panel is hidden"))
    };
    let event = match &result {
        Ok(report) => {
            log::debug!(
                "editing started by {} in {:.1} ms (marked Alt swallowed: {})",
                trigger.name(),
                report.elapsed_ms,
                report.marked_alt_swallowed
            );
            PanelEvent::EditingStarted
        }
        Err(err) => {
            log::warn!("editing refused ({}): {err:#}", trigger.name());
            PanelEvent::EditingRefused
        }
    };
    parts.emit(event, cx);

    if probe::enabled() {
        probe::editing(
            trigger,
            result.as_ref().ok().copied().unwrap_or_default(),
            result.as_ref().err().map(|err| format!("{err:#}")),
            &native.probe_fields(None),
        );
    }
}

fn end_editing(parts: &Parts, cx: &mut AsyncApp) {
    if !parts.native.is_editing() {
        return;
    }

    parts.native.end_editing(true);
    parts.emit(PanelEvent::EditingEnded, cx);

    if probe::enabled() {
        probe::editing_ended(&parts.native.probe_fields(None));
    }
}

/// 一次显示从触发到首帧的计时。
struct ShowReport {
    trigger: Trigger,
    show_started: i64,
    show_returned: i64,
    native_fields: Option<String>,
}

impl ShowReport {
    /// `rendered` 是首帧渲染时刻。首帧在 `ShowWindow` 里同步画完时，它返回就已经呈现，
    /// 以返回时刻为准；否则以渲染时刻为准（之后紧跟着呈现，差不到 1 ms）。
    fn finish(self, rendered: i64) {
        let inside_show = rendered <= self.show_returned;
        let frame = if inside_show {
            self.show_returned
        } else {
            rendered
        };
        let latency = clock::ticks_to_ms(frame - self.trigger.ticks);
        log::debug!(
            "panel shown by {} in {latency:.1} ms (first frame {} the native show)",
            self.trigger.source.name(),
            if inside_show { "inside" } else { "after" }
        );

        if let Some(native_fields) = self.native_fields {
            probe::shown(
                self.trigger,
                [self.show_started, self.show_returned, rendered, frame],
                latency,
                inside_show,
                &native_fields,
            );
        }
    }
}

#[cfg(test)]
mod paste_control_tests {
    use super::*;

    #[test]
    fn producer_inside_click_invalidates_an_inject_already_at_the_queue_head() {
        use std::cell::Cell;
        use std::sync::Arc;
        let coordinator = Arc::new(PasteCoordinator::default());
        let lease = coordinator.try_begin(100).unwrap();
        let (commands, receiver) = async_channel::bounded(2);
        let (done, _reply) = async_channel::bounded(1);
        let target = PasteTarget {
            generation: 1,
            window: 100,
            process_id: 20,
        };
        commands
            .try_send(PanelCommand::InjectPaste {
                handoff: PasteHandoff {
                    target,
                    panel_was_visible: true,
                },
                token: lease.token(),
                deadline: Instant::now() + std::time::Duration::from_secs(1),
                done,
            })
            .unwrap();
        // The producer observes a nonactivating inside click, but its control remains behind Inject.
        commands
            .try_send(PanelCommand::SetInputCapture(true).observed_with(150, &coordinator))
            .unwrap();
        let PanelCommand::InjectPaste { token, .. } = receiver.try_recv().unwrap() else {
            panic!("Inject must still be first");
        };
        let injected = Cell::new(false);
        let result = paste::inject_if_current(
            &token,
            || Ok(true),
            || {
                injected.set(true);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!injected.get());
        assert_eq!(receiver.len(), 1);
    }

    #[test]
    fn late_preferences_and_tray_bridge_events_keep_their_original_cancellation_cutoff() {
        use super::super::paste_coordinator::PasteCoordinator;
        use std::sync::Arc;
        let state = Arc::new(PasteCoordinator::default());
        let newer_paste = state.try_begin(200).unwrap();
        let (sender, receiver) = async_channel::bounded(2);
        let trigger = Trigger {
            source: TriggerSource::Tray,
            ticks: 150,
        };
        sender
            .try_send(PanelCommand::Show(trigger).observed_with(150, &state))
            .unwrap();
        sender
            .try_send(PanelCommand::CancelPaste.observed_with(175, &state))
            .unwrap();
        for original_ticks in [150, 175] {
            let PanelCommand::Observed { ticks, .. } = receiver.try_recv().unwrap() else {
                panic!("bridge control lost its observation time");
            };
            assert_eq!(ticks, original_ticks);
            assert!(state.started_after(ticks));
            state.cancel_before(ticks);
            assert!(newer_paste.is_current());
        }
    }

    #[test]
    fn user_controls_are_stamped_once_and_internal_paste_hide_is_preserved() {
        let trigger = Trigger::now(TriggerSource::Ui);
        let controls = [
            PanelCommand::Toggle(trigger),
            PanelCommand::Show(trigger),
            PanelCommand::Hide(trigger),
            PanelCommand::BeginEditing(EditTrigger::Mouse),
            PanelCommand::EndEditing,
            PanelCommand::SetInputCapture(true),
            PanelCommand::SetInputCapture(false),
            PanelCommand::DismissPopup,
            PanelCommand::CancelPaste,
        ];
        let coordinator = PasteCoordinator::default();
        for control in controls {
            assert!(control.cancels_paste());
            let observed = control.observed_with(clock::now_ticks(), &coordinator);
            let PanelCommand::Observed { ticks, .. } = &observed else {
                panic!("user control must be observed by its producer");
            };
            let initial_ticks = *ticks;
            assert!(
                matches!(observed.observed(), PanelCommand::Observed { ticks, .. } if ticks == initial_ticks)
            );
        }
        let internal_hide = PanelCommand::Hide(Trigger::now(TriggerSource::Paste));
        assert!(!internal_hide.cancels_paste());
        assert!(matches!(internal_hide.observed(), PanelCommand::Hide(_)));
    }
}
