//! 平台层的 GPUI 接线：创建平台、单实例、core、剪贴板面板、键盘与鼠标钩子、编辑态、
//! 粘贴链路、全局热键、托盘和系统设置信号。
//!
//! 启动顺序（附录 C §4.3 的子集）：`main` 先装日志和 panic hook（`crate::health`）；[`launch`] 在
//! 创建 GPU 设备之前判重（第二实例把参数转交给主实例后直接退出；崩溃重启的子进程改为等前一个
//! 实例退出后接管）并启动 core；[`create`] 创建 GPUI 平台；[`start`] 在 `Application::run`
//! 回调里设全局行为、开看门狗、预创建隐藏的面板、注册热键和托盘、接上 core 的设置事件。
//!
//! 只需要原生句柄的部分在 `kwikpaste-os`，这里只放要碰 `gpui::*` 的胶水。所有原生窗口调用
//! 都在 `cx.spawn` 的任务体里、GPUI 借用之外进行。
//!
//! # UI 怎么接
//! - 面板显示 / 隐藏、编辑态进出：订阅 [`Panel::events`] 发出的 [`PanelEvent`]（面板窗口打开之前
//!   就已挂上，UI 在 `build_panel` 里构造视图时即可订阅）。收到 `Shown` 时把 GPUI 焦点放到列表
//!   （带 key context 的元素）上，钩子转来的按键才会命中列表的绑定。
//! - 非编辑态的键盘（Windows）：表 `kwikpaste_os::hook_keys::HOOK_KEYS` 里的键由钩子截下，
//!   以普通 GPUI 按键派发给面板（`window.dispatch_keystroke`），UI 照常 `bind_keys` 即可；
//!   空格另有 `KeyUp`，Ctrl 的按下松开以 `ModifiersChanged` 送达。[`hook_keystrokes`] 列出全部组合。
//! - 编辑态：输入框外层 `capture_any_mouse_down` 里 [`request`] `BeginEditing(EditTrigger::Mouse)`，
//!   Ctrl+F 之类键盘触发用 `EditTrigger::Keyboard`；收到 `EditingStarted` 后再聚焦输入框，
//!   `EditingRefused` 表示没拿到前台（留在列表）。退出时 [`request`] `EndEditing`，
//!   收到 `EditingEnded` 把焦点还给列表。面板隐藏会自动结束编辑态。
//! - 系统信号：[`SystemSignals`] 全局（文本大小、高对比度、减少动画），`cx.observe_global` 订阅；
//!   文本大小已经同步给 `kwikpaste_ui::theme::set_text_scale`，减少动画已写进 `cx.reduce_motion()`。
//! - 粘贴、复制：列表的意图交给 [`paste`] 模块（[`paste::paste`]、[`paste::paste_fragment`]、
//!   [`paste::copy`]），流程与时序见该模块文档；全局快速粘贴由热键直接走 [`paste::quick_paste`]。
//! - 拖出：卡片按下记 [`drag_out::DragTracker`]，越过阈值后 [`drag_out::start_item`]，接法见
//!   [`drag_out`] 模块文档。
//! - core：`crate::core_host::core(cx)` 取 `Core`；[`CoreEvents`] 转发 core 的全部事件。
//! - 窗口材质：根元素底色按 [`material::current`] 选，接法见 [`material`] 模块文档。
//! - 偏好设置、引导、备份导入：第二次启动、托盘、偏好快捷键和带 `.kwikpastebak` 的启动都变成
//!   [`host::HostRequest`]，UI 用 [`host::set_handler`] 接手，见 [`host`] 模块文档。

pub(crate) mod autostart;
pub mod drag_out;
mod editing;
pub mod host;
pub(crate) mod hotkey;
mod instance;
mod keyboard;
pub mod material;
mod mouse;
mod panel;
pub mod paste;
mod paste_coordinator;
mod probe;
mod probe_view;
mod seed;
mod settings;
mod system;
mod tray;
mod trigger_pause;
pub(crate) mod updater;
mod watchdog;
pub mod window_drag;
mod window_state;

#[cfg(target_os = "macos")]
#[path = "native_macos.rs"]
mod native;
#[cfg(target_os = "windows")]
#[path = "native_windows.rs"]
mod native;

use std::{path::PathBuf, rc::Rc};

#[cfg(target_os = "windows")]
use std::{
    cell::{Cell, RefCell},
    time::Duration,
};

use anyhow::Context as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, CursorHideMode, Entity, Platform, QuitMode, Render,
    Window, WindowOptions,
};
use kwikpaste_os::single_instance::{self, Claim, PrimaryInstance};

#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::core_host::{self, StartedCore};
use crate::{health, i18n::t, selftest};

#[allow(unused_imports, reason = "UI 接线用的接口，见本模块文档")]
pub use editing::EditTrigger;
pub use panel::{Panel, PanelCommand, PanelEvent, Trigger, TriggerSource, rendered_frames};
#[allow(unused_imports, reason = "UI 接线用的接口，见本模块文档")]
pub use settings::{CoreEvents, apply_language, core_events};
#[allow(unused_imports, reason = "UI 接线用的接口，见本模块文档")]
pub use system::SystemSignals;

/// 打开应用窗口并加入材质同步集合。
///
/// 所有 GPUI 窗口都走这一层，保证用户在偏好设置里切换材质时，已打开的预览、偏好、引导、
/// 更新和展示窗口会一起切换，而不是只有剪贴板面板刷新。
pub fn open_window<V: Render>(
    options: WindowOptions,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> gpui::Result<(AnyWindowHandle, Entity<V>)> {
    let (handle, view) = kwikpaste_ui::open_window(options, cx, build)?;
    material::register_window(handle, cx);
    Ok((handle, view))
}

#[cfg(target_os = "windows")]
type RevealCallback = Box<dyn FnOnce(&mut Window, &mut App)>;

#[cfg(target_os = "windows")]
struct RevealState {
    revealed: Cell<bool>,
    callback: RefCell<Option<RevealCallback>>,
}

#[cfg(target_os = "windows")]
fn reveal_once(window: &mut Window, cx: &mut App, state: &RevealState) {
    if state.revealed.replace(true) {
        return;
    }
    if let Some(hwnd) = window_hwnd(window)
        && let Err(error) = kwikpaste_os::win::set_window_cloaked(hwnd, false)
    {
        log::warn!("could not uncloak window: {error}");
    }
    if let Some(callback) = state.callback.borrow_mut().take() {
        callback(window, cx);
    }
}

#[cfg(target_os = "windows")]
fn window_hwnd(window: &Window) -> Option<isize> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.hwnd.get())
}

/// 新建的窗口先用 DWM 隐藏，画完第一帧再显示并执行 `on_revealed`（调到前台）。
///
/// GPUI 在 Windows 上建窗时就把窗口显示出来，第一帧没画好之前用户看到的是一块空的圆角窗框；
/// 在建窗回调里调用本函数即可把这段遮住。被隐藏的窗口仍是 `IsWindowVisible`，GPUI 照常绘制。
/// 第一个 `on_next_frame` 在首帧绘制前触发，第二个在首帧之后；500 ms 没等到就直接显示，
/// 窗口不会一直隐藏。macOS 直接执行 `on_revealed`。
pub fn reveal_after_first_frame(
    window: &mut Window,
    cx: &mut App,
    on_revealed: impl FnOnce(&mut Window, &mut App) + 'static,
) {
    #[cfg(target_os = "macos")]
    on_revealed(window, cx);

    #[cfg(target_os = "windows")]
    {
        let Some(hwnd) = window_hwnd(window) else {
            on_revealed(window, cx);
            return;
        };
        if let Err(error) = kwikpaste_os::win::set_window_cloaked(hwnd, true) {
            log::warn!("could not cloak window: {error}");
        }
        let state = Rc::new(RevealState {
            revealed: Cell::new(false),
            callback: RefCell::new(Some(Box::new(on_revealed))),
        });
        let first_state = state.clone();
        window.on_next_frame(move |window, _| {
            let second_state = first_state.clone();
            window.on_next_frame(move |window, cx| reveal_once(window, cx, &second_state));
        });

        let timeout_state = state;
        window
            .spawn(cx, async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let _ = cx.update(|window, cx| reveal_once(window, cx, &timeout_state));
            })
            .detach();
    }
}

/// 崩溃重启的子进程最多等前一个实例退出这么久。
const TAKE_OVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 退出码：有序重启时为 70，更新交接要求的退出码次之，否则为 0。
pub fn exit_code() -> i32 {
    match health::exit_code() {
        0 => updater::exit_code(),
        code => code,
    }
}

/// 判重通过后带进 GPUI 的启动状态。
pub struct Launch {
    instance: PrimaryInstance,
    invocations: (
        async_channel::Sender<instance::ObservedInvocation>,
        async_channel::Receiver<instance::ObservedInvocation>,
    ),
    core: StartedCore,
}

/// 单实例判重并启动 core。本进程是第二实例时把参数转交给主实例并返回 `None`，调用方应直接退出。
///
/// 必须在 [`create`] 之前调用：第二实例不初始化 GPU，也不碰数据库。
pub fn launch() -> anyhow::Result<Option<Launch>> {
    let identifier = crate::identity::identifier();
    // 设置要求以管理员运行而当前没提权：拉起提权的进程后退出（与 1.x 相同，在判重之前）。
    if autostart::elevate_if_configured() {
        return Ok(None);
    }
    let (sender, invocations) = async_channel::unbounded();
    let forward = sender.clone();
    let on_invocation = move |invocation| {
        let _ = forward.try_send(instance::ObservedInvocation::received(invocation));
    };
    // 崩溃重启的子进程不把参数转交给正在死去的前一个实例，而是等它退出后接管。
    let claim = if health::relaunch_count() > 0 {
        match single_instance::take_over(identifier, on_invocation, TAKE_OVER_TIMEOUT) {
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {
                log::error!("the crashed instance did not exit in time ({err}); giving up");
                return Ok(None);
            }
            claim => claim,
        }
    } else {
        single_instance::claim(identifier, on_invocation)
    }
    .with_context(|| format!("single instance check for {identifier}"))?;

    let instance = match claim {
        Claim::Primary(instance) => instance,
        Claim::Forwarded => {
            log::info!("{identifier} is already running; handed the arguments over");
            return Ok(None);
        }
    };
    // The installer uses this same executable to ask an existing instance to quit. If there is
    // no instance to receive it, do not turn the installer probe into a normal app launch.
    if std::env::args().skip(1).any(|arg| arg == instance::QUIT) {
        drop(instance);
        return Ok(None);
    }
    health::after_claim();
    // 崩溃重启自测：模拟「一启动就崩」的毒输入（子进程继承环境变量，每次启动都崩）。
    if selftest::enabled(selftest::CRASH_RESTART)
        && std::env::var_os("KWIKPASTE_SELFTEST_PANIC_AT_STARTUP").is_some_and(|value| value == "1")
    {
        panic!("selftest panic during startup");
    }
    let core = core_host::start()?;

    Ok(Some(Launch {
        instance,
        invocations: (sender, invocations),
        core,
    }))
}

/// 创建 GPUI 平台。
///
/// Windows 上不走 `gpui_platform::application()`：它对 `WindowsPlatform::new` 的错误直接 panic，
/// 这里自己调用、拿到 `Result`，初始化失败时才有机会告诉用户原因。
#[cfg(target_os = "windows")]
pub fn create() -> anyhow::Result<Rc<dyn Platform>> {
    let platform = gpui_windows::WindowsPlatform::new(false)?;

    Ok(Rc::new(platform))
}

#[cfg(target_os = "macos")]
pub fn create() -> anyhow::Result<Rc<dyn Platform>> {
    Ok(gpui_platform::current_platform(false))
}

/// macOS 点 Dock 图标重新打开应用：没有可见窗口时打开偏好设置（未完成引导时改开引导窗），与 1.x 相同。
#[cfg(target_os = "macos")]
pub fn reopen_from_dock(cx: &mut App) {
    let any_visible = cx.windows().iter().any(|handle| {
        handle
            .update(cx, |_, window, _| window.is_visible())
            .unwrap_or(false)
    });
    if any_visible {
        return;
    }

    host::dispatch(
        cx,
        host::HostRequest::OpenPreferences {
            source: host::RequestSource::Dock,
        },
    );
}

/// 在 `Application::run` 回调里调用：设全局行为，接上 core，预创建隐藏的面板，注册热键和托盘。
///
/// `--selftest-platform` 下面板放平台自测视图，不用 `build_panel`。只有面板创建失败才返回错误；
/// 热键、托盘失败只记日志，应用照常运行。
pub fn start<V: Render>(
    cx: &mut App,
    launch: Launch,
    build_panel: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    if let Err(err) = kwikpaste_os::mac::panel::set_dock_icon_visible(
        launch.core.host.core().settings().general.dock_icon,
    ) {
        log::warn!("could not set the application activation policy: {err}");
    }
    cx.set_quit_mode(QuitMode::Explicit);
    // 钩子派发的按键会命中 action，默认模式会因此隐藏停在面板上的鼠标指针。
    cx.set_cursor_hide_mode(CursorHideMode::Never);
    paste::init(cx);
    probe::init();
    watchdog::serve(cx);
    if selftest::enabled(selftest::PLATFORM) {
        drag_out::guard_selftest_drops();
    }

    let StartedCore { host, events } = launch.core;
    cx.set_global(host);
    settings::serve(cx, events);
    settings::apply_language(cx);
    let text_scale = system::init(cx);
    if let Some(core) = core_host::core(cx) {
        window_state::migrate_legacy(core);
    }

    if selftest::enabled(selftest::PLATFORM) && !selftest::enabled(selftest::UI_PANEL) {
        panel::open(cx, text_scale, |window, cx| {
            cx.new(|cx| probe_view::ProbeView::new(window, cx))
        })?;
    } else {
        panel::open(cx, text_scale, build_panel)?;
    }
    material::apply(cx);
    if !selftest::active()
        && let Some(log_dir) = health::take_gave_up_notice()
    {
        show_crash_notice(log_dir, cx);
    }
    let commands = cx.global::<Panel>().commands();
    keyboard::serve(cx);
    mouse::serve(cx, commands.clone());
    system::serve(cx, commands.clone());

    // 列表自测不碰全局热键：热键是系统范围独占的，会抢走同时在跑的平台探针
    // 或手动开着的开发实例的热键。
    if !crate::selftest::list_selftest()
        && let Err(err) = hotkey::register(cx, commands.clone())
    {
        log::error!("global hotkey is unavailable: {err:#}");
    }
    trigger_pause::serve(cx);
    // 演示实例保留托盘，便于核对原生菜单；跑分和自动交互自测仍不创建托盘。
    if (!crate::selftest::list_selftest() || selftest::enabled(selftest::LIST_DEMO))
        && let Err(err) = tray::create(cx, commands.clone())
    {
        log::error!("tray icon is unavailable: {err:#}");
    }
    settings::follow(cx);
    autostart::sync_at_startup(cx);

    // Host 请求（托盘、快捷键、第二次启动和备份文件）统一交给偏好窗口。
    // 注册必须先于 `queue_launch_arguments`，这样冷启动携带的备份文件不会退回面板。
    host::set_handler(cx, |request, cx| {
        let result = if matches!(request, host::HostRequest::OpenPreferences { .. })
            && core_host::core(cx).is_some_and(|core| !core.settings().onboarding.completed)
        {
            // 与 1.x 相同：首次启动尚未完成引导时，偏好请求先打开引导窗。
            crate::preferences::open_onboarding(cx)
        } else {
            crate::preferences::open_request(cx, request.clone())
        };
        if let Err(err) = result {
            log::error!("could not handle host request {request:?}: {err:#}");
        }
    });
    host::queue_launch_arguments(cx);
    let should_open_onboarding = (!selftest::active()
        && core_host::core(cx).is_some_and(|core| !core.settings().onboarding.completed))
        || selftest::onboarding_requested();
    if should_open_onboarding && let Err(err) = crate::preferences::open_onboarding(cx) {
        log::error!("could not open first-run onboarding: {err:#}");
    }
    probe::follow_clipboard(cx);
    instance::serve(cx, launch.instance, launch.invocations, commands);
    updater::start(cx);
    if selftest::enabled(selftest::UPDATER_UI) {
        updater::selftest_update_window(cx);
    }
    if selftest::enabled(selftest::ANNOUNCEMENT) {
        updater::selftest_announcement(cx);
    }
    if selftest::enabled(selftest::CRASH_GAVE_UP)
        && let Some(core) = core_host::core(cx)
    {
        show_crash_notice(core.paths().log_dir(), cx);
    }
    // 面板起来了才算正常启动：这时再在后台清掉 1.x 的 WebView2 数据（只清一次，不挡启动）。
    if !selftest::active()
        && let Some(core) = core_host::core(cx)
    {
        core.remove_legacy_webview_data();
    }
    health::set_phase(health::Phase::Idle);

    Ok(())
}

fn show_crash_notice(log_dir: PathBuf, cx: &App) {
    let title = t("common:health.gaveUpTitle");
    let body = t("common:health.gaveUpBody");
    let open_logs = t("common:health.openLogs");
    let ok = t("common:health.ok");
    let buttons = [
        kwikpaste_os::dialogs::DialogButton::new(open_logs),
        kwikpaste_os::dialogs::DialogButton::new(ok),
    ];
    // 原生模态框会泵消息，不能在 GPUI 借用 App 的启动回调中阻塞。
    cx.background_executor()
        .spawn(async move {
            let selected = kwikpaste_os::dialogs::show(&title, &body, &buttons);
            if crate::selftest::enabled(crate::selftest::CRASH_GAVE_UP) {
                log::info!(
                    "crash notice self-test result: {selected:?}; log path: {}",
                    log_dir.display()
                );
                return;
            }
            if selected == Some(0)
                && let Err(err) = kwikpaste_os::dialogs::open_path(&log_dir)
            {
                log::warn!("crash log directory could not be opened: {err}");
            }
        })
        .detach();
}

/// 进入一个短暂的阶段（粘贴、拖出），守卫丢弃时按面板是否显示回到 `Panel` 或 `Idle`。
pub(crate) fn enter_phase(phase: health::Phase) -> PhaseGuard {
    health::set_phase(phase);
    PhaseGuard
}

pub(crate) struct PhaseGuard;

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        health::set_phase(if panel::is_shown() {
            health::Phase::Panel
        } else {
            health::Phase::Idle
        });
    }
}

/// 请求显示、隐藏、切换面板或进出编辑态。
pub fn request(cx: &App, command: PanelCommand) {
    let command = command.observed();
    if let PanelCommand::Observed { ticks, .. } = &command {
        paste::cancel_pending_before(cx, *ticks);
    }
    if let Some(panel) = cx.try_global::<Panel>() {
        panel.request(command);
    }
}

/// 钩子会派发给面板的全部按键组合（GPUI keystroke 字符串），供 UI 核对绑定。
pub fn hook_keystrokes() -> Vec<String> {
    kwikpaste_os::hook_keys::keystrokes()
}
