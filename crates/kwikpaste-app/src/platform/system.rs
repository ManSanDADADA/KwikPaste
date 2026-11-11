//! UI 要跟随的系统设置：文本大小、高对比度、减少动画。
//!
//! 读到的值存成 [`SystemSignals`] 全局（UI 用 `cx.observe_global::<SystemSignals>` 订阅），并直接应用：
//! - 文本大小 → `kwikpaste_ui::theme::set_text_scale`（rem 基准），面板的最小、默认尺寸同步补偿；
//! - 减少动画 → `cx.set_reduce_motion`（gpui-base 只在启动时读一次，这里跟随运行中的变化）；
//! - 高对比度 → 只放进全局，配色由 UI 决定；
//! - 透明效果、系统深色 → 窗口材质跟随（见 [`super::material`]）。
//!
//! Windows 上 `UISettings` 的文本大小事件和面板收到的 `WM_SETTINGCHANGE` 都会触发重读。
//! 自测进程可用 `KP_TEXT_SCALE` 模拟文本大小（与组件展示窗相同），不改系统设置。

use async_channel::Sender;
use gpui::{App, Global};

use super::panel::PanelCommand;
use crate::selftest;

/// 当前的系统设置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemSignals {
    /// Windows「文本大小」，1.0–2.25；macOS 恒为 1。
    pub text_scale: f64,
    pub high_contrast: bool,
    pub reduce_motion: bool,
    /// 系统「透明效果」打开（关掉时材质收敛为 default）。
    pub transparency: bool,
    /// 系统用深色（`appearance.theme = auto` 时材质的深浅跟它走）。
    pub dark: bool,
}

impl Global for SystemSignals {}

/// 读取并应用一次，返回文本大小系数（面板建窗要用）。
pub fn init(cx: &mut App) -> f64 {
    let signals = read();
    apply(signals, cx);
    log::info!("system signals: {signals:?}");

    signals.text_scale
}

fn read() -> SystemSignals {
    #[cfg(target_os = "windows")]
    let settings = kwikpaste_os::win::system::read();
    #[cfg(target_os = "macos")]
    let settings = kwikpaste_os::mac::system::read();

    let override_scale = selftest::active()
        .then(|| std::env::var("KP_TEXT_SCALE").ok()?.parse::<f64>().ok())
        .flatten();

    SystemSignals {
        text_scale: override_scale
            .unwrap_or(settings.text_scale)
            .clamp(1.0, 2.25),
        high_contrast: settings.high_contrast,
        // 崩溃重启的降级模式关掉动画。
        reduce_motion: settings.reduce_motion || crate::health::degraded(),
        transparency: settings.transparency,
        dark: settings.dark,
    }
}

fn apply(signals: SystemSignals, cx: &mut App) {
    kwikpaste_ui::theme::set_text_scale(signals.text_scale as f32, cx);
    if cx.reduce_motion() != signals.reduce_motion {
        cx.set_reduce_motion(signals.reduce_motion);
        cx.refresh_windows();
    }
    super::probe::signals(&signals);
    cx.set_global(signals);
    super::material::apply(cx);
}

/// 订阅变化（Windows）：重读，有变化就应用并通知面板补偿尺寸。
#[cfg(target_os = "windows")]
pub fn serve(cx: &mut App, commands: Sender<PanelCommand>) {
    let (sender, receiver) = async_channel::unbounded();
    let watch = match kwikpaste_os::win::system::watch(move || {
        let _ = sender.try_send(());
    }) {
        Ok(watch) => watch,
        Err(err) => {
            log::error!("system setting changes are not followed: {err}");
            return;
        }
    };

    cx.spawn(async move |cx| {
        // 订阅句柄跟着任务活到进程结束。
        let _watch = watch;
        while receiver.recv().await.is_ok() {
            // 广播成串到达：积压的通知合并成一次重读。
            while receiver.try_recv().is_ok() {}
            let signals = read();
            let changed = cx.update(|cx| {
                let previous = cx.try_global::<SystemSignals>().copied();
                if previous == Some(signals) {
                    return None;
                }
                apply(signals, cx);
                Some(previous)
            });
            let Some(previous) = changed else {
                continue;
            };
            log::info!("system signals changed: {signals:?}");
            if previous.is_none_or(|previous| previous.text_scale != signals.text_scale) {
                let _ = commands
                    .send(PanelCommand::SetTextScale(signals.text_scale))
                    .await;
            }
        }
    })
    .detach();
}

#[cfg(target_os = "macos")]
pub fn serve(cx: &mut App, _commands: Sender<PanelCommand>) {
    let (sender, receiver) = async_channel::unbounded();
    if let Err(err) = kwikpaste_os::mac::system::watch(move || {
        let _ = sender.try_send(());
    }) {
        log::error!("macOS display options are not followed: {err}");
        return;
    }

    cx.spawn(async move |cx| {
        while receiver.recv().await.is_ok() {
            let changed = cx.update(|cx| {
                let signals = read();
                let previous = cx.try_global::<SystemSignals>().copied();
                if previous == Some(signals) {
                    return false;
                }
                apply(signals, cx);
                true
            });
            if changed {
                log::info!("macOS display options changed");
            }
        }
    })
    .detach();
}
