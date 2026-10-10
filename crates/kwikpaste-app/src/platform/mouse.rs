//! 设置 `shortcuts.winV`（接管 Win+V，见 `kwikpaste_os::win::win_v`）也在这里跟随：它和鼠标按键唤起
//! 一样是「开启期间常驻的钩子，开合面板」，设置一变立刻生效，关掉即恢复系统行为。
//!
//! Windows 鼠标钩子的两项功能：点击面板外部时隐藏（面板从不激活，收不到失焦通知，只能全局监听
//! 按下），以及鼠标按键唤起（设置 `shortcuts.mouseTrigger`，与 1.x 相同，见
//! `kwikpaste_os::win::mouse`）。
//!
//! macOS：失焦隐藏继续由面板自己的 global monitor 处理；鼠标按键唤起用 `NSEvent` 的 global
//! monitor 观察 `OtherMouseDown`，不吞事件，也不需要 CGEventTap 的辅助功能权限。

use async_channel::Sender;
use gpui::App;
use kwikpaste_core::settings::Shortcuts;

use super::panel::PanelCommand;

/// 接上鼠标钩子的出口（Windows）：窗外按下即请求隐藏面板。钩子随面板显示、隐藏起停。
#[cfg(target_os = "windows")]
pub fn serve(cx: &mut App, commands: Sender<PanelCommand>) {
    use kwikpaste_os::win::mouse::{self, MouseEvent};

    use super::panel::{Trigger, TriggerSource};

    let win_v_commands = commands.clone();
    let installed = mouse::set_sink(move |event| match event {
        MouseEvent::OutsideClick(_) | MouseEvent::ForegroundChanged => {
            let trigger = Trigger::now(TriggerSource::OutsideClick);
            let _ = commands.try_send(PanelCommand::Hide(trigger));
        }
        MouseEvent::InsideClick(_) => {
            let _ = commands.try_send(PanelCommand::SetInputCapture(true));
        }
        MouseEvent::Trigger => {
            let trigger = Trigger::now(TriggerSource::MouseButton);
            let _ = commands.try_send(PanelCommand::Toggle(trigger));
        }
    });
    if let Err(err) = installed {
        log::error!("outside clicks cannot hide the panel: {err}");
    }

    let win_v = kwikpaste_os::win::win_v::set_sink(move || {
        let trigger = Trigger::now(TriggerSource::WinV);
        let _ = win_v_commands.try_send(PanelCommand::Toggle(trigger));
    });
    if let Err(err) = win_v {
        log::error!("Win+V cannot open the panel: {err}");
    }
    if crate::selftest::enabled(crate::selftest::PLATFORM) {
        kwikpaste_os::win::win_v::accept_probe_input(true);
    }
    if let Some(core) = crate::core_host::core(cx) {
        apply(&core.settings().shortcuts);
    }
}

/// 跟随设置：`shortcuts.winV` 起停 Win+V 钩子，`shortcuts.mouseTrigger` 切换唤起按键。
#[cfg(target_os = "windows")]
pub fn apply(shortcuts: &Shortcuts) {
    use kwikpaste_core::settings::MouseTrigger;
    use kwikpaste_os::win::mouse::{self, TriggerButton};

    match kwikpaste_os::win::win_v::set_enabled(shortcuts.win_v) {
        Ok(()) => log::info!(
            "Win+V takeover {}",
            if shortcuts.win_v { "on" } else { "off" }
        ),
        Err(err) => log::error!("Win+V takeover could not be switched: {err}"),
    }
    let button = match shortcuts.mouse_trigger {
        MouseTrigger::Disabled => None,
        MouseTrigger::Middle => Some(TriggerButton::Middle),
        MouseTrigger::Back => Some(TriggerButton::Back),
        MouseTrigger::Forward => Some(TriggerButton::Forward),
    };
    match mouse::set_trigger(button) {
        Ok(()) => log::info!("mouse trigger: {button:?}"),
        Err(err) => log::error!("mouse trigger could not be switched: {err}"),
    }
}

/// macOS：Win+V 不存在；鼠标按键唤起由 NSEvent global monitor 观察 OtherMouseDown。
#[cfg(target_os = "macos")]
pub fn apply(shortcuts: &Shortcuts) {
    use kwikpaste_core::settings::MouseTrigger;
    use kwikpaste_os::mac::mouse::{self, TriggerButton};

    let button = match shortcuts.mouse_trigger {
        MouseTrigger::Disabled => None,
        MouseTrigger::Middle => Some(TriggerButton::Middle),
        MouseTrigger::Back => Some(TriggerButton::Back),
        MouseTrigger::Forward => Some(TriggerButton::Forward),
    };
    mouse::set_trigger(button);
    log::info!("mouse trigger: {button:?}");
}

#[cfg(target_os = "macos")]
pub fn serve(cx: &mut App, commands: Sender<PanelCommand>) {
    let installed = kwikpaste_os::mac::mouse::set_sink(move |event| {
        if matches!(event, kwikpaste_os::mac::mouse::MouseEvent::Trigger) {
            let trigger = super::panel::Trigger::now(super::panel::TriggerSource::MouseButton);
            let _ = commands.try_send(PanelCommand::Toggle(trigger));
        }
    });
    if let Err(err) = installed {
        log::error!("macOS mouse trigger monitor could not be installed: {err}");
    }
    if let Some(core) = crate::core_host::core(cx) {
        apply(&core.settings().shortcuts);
    }
}
