//! 全局热键：global-hotkey（1.x 的 tauri-plugin-global-shortcut 用的同一个 crate）。
//!
//! - 切换面板：设置 `shortcuts.openClipboard`（Tauri accelerator 字面量，如 `Alt+C`，用
//!   `HotKey::try_from` 解析，与 1.x 一致）。
//! - 快速粘贴：`shortcuts.quickPaste` 打开时注册「修饰键 + 1…9、0」，粘贴「全部」视图里的第 1…10 条
//!   （见 [`super::paste::quick_paste`]）。
//! - 偏好设置：`shortcuts.openPreference`（默认 Alt+X），发 [`HostRequest::OpenPreferences`]，由 UI
//!   打开偏好窗（见 [`super::host`]）。
//! - 纯文本粘贴：`shortcuts.pastePlain` 注册后直接处理当前系统剪贴板。
//!
//! 设置变更时重新注册。管理器在主线程创建，`WM_HOTKEY` / Carbon 事件由 GPUI 的消息循环顺带派发；
//! 事件经「专用线程阻塞 `recv()` → `async_channel` → 主线程」送回，不轮询。线程里只转发、不碰 GPUI，
//! 唯一的例外是快速粘贴按下时立刻屏蔽 Alt / Win 的单独松开：这一步必须趁修饰键还按着做。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_channel::Sender;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui::{App, AsyncApp, Global};
use kwikpaste_core::settings::Shortcuts;

use super::host::{self, HostRequest, RequestSource};
use super::panel::{PanelCommand, Trigger, TriggerSource};
use super::paste;
use crate::core_host;

/// 开发构建在设置仍是出厂默认值时改用的热键。出厂默认的 Alt+C 属于本机正在运行的 1.x，
/// 开发版抢先注册会让 1.x 重启后注册失败；Ctrl+Alt+Shift+F9 两边都不用。
const DEVELOPMENT_TOGGLE: &str = "Control+Alt+Shift+F9";
/// 同理，偏好快捷键出厂默认的 Alt+X 在开发构建里换成它。
const DEVELOPMENT_PREFERENCE: &str = "Control+Alt+Shift+F8";

/// 快速粘贴的数字键与条目序号（从 0 起），与 1.x 相同：1…9 是第 1…9 条，0 是第 10 条。
const QUICK_PASTE_KEYS: [(&str, i64); 10] = [
    ("1", 0),
    ("2", 1),
    ("3", 2),
    ("4", 3),
    ("5", 4),
    ("6", 5),
    ("7", 6),
    ("8", 7),
    ("9", 8),
    ("0", 9),
];

/// 热键按下后要做的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    TogglePanel,
    QuickPaste(i64),
    OpenPreference,
    PastePlain,
}

/// 当前注册的热键 id → 动作，事件桥线程据此分发。
type Actions = Arc<Mutex<HashMap<u32, Action>>>;

/// 持有管理器（丢弃时注销全部热键）和当前注册的热键。
struct Hotkeys {
    manager: GlobalHotKeyManager,
    toggle: Option<HotKey>,
    preference: Option<HotKey>,
    paste_plain: Option<HotKey>,
    quick_paste: Vec<HotKey>,
    actions: Actions,
    recording: bool,
    paused: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RegistrationStatus {
    pub open_clipboard_failed: bool,
    pub open_preference_failed: bool,
    pub paste_plain_failed: bool,
}

impl Global for RegistrationStatus {}

impl Global for Hotkeys {}

/// 创建管理器、启动事件桥，按当前设置注册热键。
pub fn register(cx: &mut App, commands: Sender<PanelCommand>) -> anyhow::Result<()> {
    let manager = GlobalHotKeyManager::new()?;
    let actions = Actions::default();
    let (quick_sender, quick_receiver) = async_channel::unbounded();
    let (preference_sender, preference_receiver) = async_channel::unbounded();
    let (plain_sender, plain_receiver) = async_channel::unbounded();

    let bridge_actions = actions.clone();
    std::thread::Builder::new()
        .name("hotkey-bridge".to_owned())
        .spawn(move || {
            let receiver = GlobalHotKeyEvent::receiver();
            while let Ok(event) = receiver.recv() {
                if event.state != HotKeyState::Pressed {
                    continue;
                }
                let action = lock(&bridge_actions).get(&event.id).copied();
                let delivered = match action {
                    Some(Action::TogglePanel) => {
                        let trigger = Trigger::now(TriggerSource::Hotkey);
                        commands
                            .send_blocking(PanelCommand::Toggle(trigger).observed())
                            .is_ok()
                    }
                    Some(Action::QuickPaste(offset)) => {
                        if let Err(err) = kwikpaste_os::keystroke::mask_modifier_release() {
                            log::warn!("modifier release could not be masked: {err}");
                        }
                        quick_sender
                            .send_blocking((offset, kwikpaste_os::clock::now_ticks()))
                            .is_ok()
                    }
                    Some(Action::OpenPreference) => {
                        let ticks = kwikpaste_os::clock::now_ticks();
                        super::paste_coordinator::observe_control(ticks);
                        preference_sender.send_blocking(ticks).is_ok()
                    }
                    Some(Action::PastePlain) => {
                        if let Err(err) = kwikpaste_os::keystroke::mask_modifier_release() {
                            log::warn!("modifier release could not be masked: {err}");
                        }
                        plain_sender
                            .send_blocking(kwikpaste_os::clock::now_ticks())
                            .is_ok()
                    }
                    None => true,
                };
                if !delivered {
                    break;
                }
            }
        })?;

    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok((offset, ticks)) = quick_receiver.recv().await {
            if cx.update(|cx| paste::control_is_stale(cx, ticks)) {
                continue;
            }
            cx.update(|cx| paste::quick_paste(cx, offset)).detach();
        }
    })
    .detach();

    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(ticks) = plain_receiver.recv().await {
            if cx.update(|cx| paste::control_is_stale(cx, ticks)) {
                continue;
            }
            cx.update(paste::paste_plain).detach();
        }
    })
    .detach();

    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(ticks) = preference_receiver.recv().await {
            cx.update(|cx| {
                host::dispatch_observed(
                    cx,
                    HostRequest::OpenPreferences {
                        source: RequestSource::Hotkey,
                    },
                    ticks,
                );
            });
        }
    })
    .detach();

    cx.set_global(Hotkeys {
        manager,
        toggle: None,
        preference: None,
        paste_plain: None,
        quick_paste: Vec::new(),
        actions,
        recording: false,
        paused: false,
    });
    let shortcuts = core_host::core(cx).map(|core| core.settings().shortcuts);
    if let Some(shortcuts) = shortcuts {
        apply(&shortcuts, cx);
    }

    Ok(())
}

/// 按设置重新注册切换面板、偏好设置与快速粘贴的热键；与当前相同时什么也不做。
pub fn apply(shortcuts: &Shortcuts, cx: &mut App) {
    if !cx.has_global::<Hotkeys>() {
        return;
    }
    let toggle = wanted_toggle(shortcuts);
    let preference = wanted_preference(shortcuts);
    let paste_plain = wanted_paste_plain(shortcuts);
    let quick_paste = wanted_quick_paste(shortcuts);

    let status = {
        let hotkeys = cx.global_mut::<Hotkeys>();
        if !registration_allowed(hotkeys.recording, hotkeys.paused) {
            return;
        }
        if hotkeys.toggle != toggle {
            if let Some(previous) = hotkeys.toggle.take() {
                hotkeys.unregister(previous);
            }
            if let Some(hotkey) = toggle
                && hotkeys.register(hotkey, Action::TogglePanel)
            {
                hotkeys.toggle = Some(hotkey);
                log::info!("panel hotkey registered: {hotkey}");
            }
        }
        if hotkeys.preference != preference {
            if let Some(previous) = hotkeys.preference.take() {
                hotkeys.unregister(previous);
            }
            if let Some(hotkey) = preference
                && hotkeys.register(hotkey, Action::OpenPreference)
            {
                hotkeys.preference = Some(hotkey);
                log::info!("preference hotkey registered: {hotkey}");
            }
        }

        if hotkeys.paste_plain != paste_plain {
            if let Some(previous) = hotkeys.paste_plain.take() {
                hotkeys.unregister(previous);
            }
            if let Some(hotkey) = paste_plain
                && hotkeys.register(hotkey, Action::PastePlain)
            {
                hotkeys.paste_plain = Some(hotkey);
                log::info!("plain paste hotkey registered: {hotkey}");
            }
        }

        let current: Vec<HotKey> = hotkeys.quick_paste.clone();
        if current
            != quick_paste
                .iter()
                .map(|(hotkey, _)| *hotkey)
                .collect::<Vec<_>>()
        {
            for previous in current {
                hotkeys.unregister(previous);
            }
            hotkeys.quick_paste = quick_paste
                .into_iter()
                .filter(|&(hotkey, offset)| hotkeys.register(hotkey, Action::QuickPaste(offset)))
                .map(|(hotkey, _)| hotkey)
                .collect();
            if !hotkeys.quick_paste.is_empty() {
                log::info!(
                    "quick paste hotkeys registered: {}",
                    hotkeys.quick_paste.len()
                );
            }
        }
        RegistrationStatus {
            open_clipboard_failed: toggle.is_some() && hotkeys.toggle != toggle,
            open_preference_failed: preference.is_some() && hotkeys.preference != preference,
            paste_plain_failed: paste_plain.is_some() && hotkeys.paste_plain != paste_plain,
        }
    };
    publish_registration_status(cx, status);
}

fn publish_registration_status(cx: &mut App, status: RegistrationStatus) {
    cx.set_global(status);
}

fn registration_allowed(recording: bool, paused: bool) -> bool {
    !recording && !paused
}

/// 注销全部热键（更新交接停输入时用）。
pub fn unregister_all(cx: &mut App) {
    if !cx.has_global::<Hotkeys>() {
        return;
    }
    clear_registered(cx);
}

/// 暂停全局热键，给偏好设置里的物理按键录制让出输入。
pub fn suspend(cx: &mut App) {
    if !cx.has_global::<Hotkeys>() {
        return;
    }
    cx.global_mut::<Hotkeys>().recording = true;
    clear_registered(cx);
}

/// 恢复全局热键，并按最新设置重新注册。
pub fn resume(cx: &mut App) {
    if !cx.has_global::<Hotkeys>() {
        return;
    }
    cx.global_mut::<Hotkeys>().recording = false;
    if cx.global::<Hotkeys>().paused {
        return;
    }
    if let Some(core) = core_host::core(cx) {
        let shortcuts = core.settings().shortcuts;
        apply(&shortcuts, cx);
    }
}

/// 设置「前台应用暂停」这一独立原因；录制按键仍保持自己的暂停状态。
pub fn set_paused(paused: bool, cx: &mut App) {
    if !cx.has_global::<Hotkeys>() {
        return;
    }
    let recording = {
        let hotkeys = cx.global_mut::<Hotkeys>();
        hotkeys.paused = paused;
        hotkeys.recording
    };
    if paused {
        clear_registered(cx);
        return;
    }
    if recording {
        return;
    }
    let shortcuts = core_host::core(cx).map(|core| core.settings().shortcuts);
    if let Some(shortcuts) = shortcuts {
        apply(&shortcuts, cx);
    }
}

fn clear_registered(cx: &mut App) {
    {
        let hotkeys = cx.global_mut::<Hotkeys>();
        let all: Vec<HotKey> = hotkeys
            .toggle
            .take()
            .into_iter()
            .chain(hotkeys.preference.take())
            .chain(hotkeys.paste_plain.take())
            .chain(hotkeys.quick_paste.drain(..))
            .collect();
        for hotkey in all {
            hotkeys.unregister(hotkey);
        }
    }
    publish_registration_status(cx, RegistrationStatus::default());
}

impl Hotkeys {
    fn register(&mut self, hotkey: HotKey, action: Action) -> bool {
        match self.manager.register(hotkey) {
            Ok(()) => {
                lock(&self.actions).insert(hotkey.id(), action);
                true
            }
            Err(err) => {
                log::error!("hotkey {hotkey} could not be registered: {err}");
                false
            }
        }
    }

    fn unregister(&mut self, hotkey: HotKey) {
        lock(&self.actions).remove(&hotkey.id());
        if let Err(err) = self.manager.unregister(hotkey) {
            log::warn!("hotkey {hotkey} could not be unregistered: {err}");
        }
    }
}

fn lock(actions: &Actions) -> MutexGuard<'_, HashMap<u32, Action>> {
    actions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wanted_toggle(shortcuts: &Shortcuts) -> Option<HotKey> {
    let accelerator = effective(
        &shortcuts.open_clipboard,
        &Shortcuts::default().open_clipboard,
        DEVELOPMENT_TOGGLE,
    );
    match parse(&accelerator) {
        Ok(hotkey) => hotkey,
        Err(err) => {
            log::error!("panel hotkey {accelerator:?} is invalid: {err}");
            None
        }
    }
}

fn wanted_preference(shortcuts: &Shortcuts) -> Option<HotKey> {
    let accelerator = effective(
        &shortcuts.open_preference,
        &Shortcuts::default().open_preference,
        DEVELOPMENT_PREFERENCE,
    );
    match parse(&accelerator) {
        Ok(hotkey) => hotkey,
        Err(err) => {
            log::error!("preference hotkey {accelerator:?} is invalid: {err}");
            None
        }
    }
}

fn wanted_paste_plain(shortcuts: &Shortcuts) -> Option<HotKey> {
    match parse(&shortcuts.paste_plain) {
        Ok(hotkey) => hotkey,
        Err(err) => {
            log::error!(
                "plain paste hotkey {:?} is invalid: {err}",
                shortcuts.paste_plain
            );
            None
        }
    }
}

/// 查询热键注册失败状态；禁用、录制或主动暂停不算注册失败。
pub fn registration_status(cx: &App) -> RegistrationStatus {
    cx.try_global::<RegistrationStatus>()
        .copied()
        .unwrap_or_default()
}

pub fn registration_failed(id: &str, cx: &App) -> bool {
    let status = registration_status(cx);
    match id {
        "shortcuts.openClipboard" => status.open_clipboard_failed,
        "shortcuts.openPreference" => status.open_preference_failed,
        "shortcuts.pastePlain" => status.paste_plain_failed,
        _ => false,
    }
}

/// 快速粘贴打开时的十个热键与对应序号；关掉时为空。
fn wanted_quick_paste(shortcuts: &Shortcuts) -> Vec<(HotKey, i64)> {
    if !shortcuts.quick_paste.enabled {
        return Vec::new();
    }
    let modifiers = shortcuts.quick_paste.modifiers.accelerator();

    QUICK_PASTE_KEYS
        .iter()
        .filter_map(|&(key, offset)| {
            let accelerator = format!("{modifiers}+{key}");
            match parse(&accelerator) {
                Ok(hotkey) => hotkey.map(|hotkey| (hotkey, offset)),
                Err(err) => {
                    log::error!("quick paste hotkey {accelerator:?} is invalid: {err}");
                    None
                }
            }
        })
        .collect()
}

/// 空字符串表示用户清空了快捷键。
fn parse(accelerator: &str) -> Result<Option<HotKey>, global_hotkey::hotkey::HotKeyParseError> {
    if accelerator.trim().is_empty() {
        return Ok(None);
    }
    HotKey::try_from(accelerator).map(Some)
}

/// 实际注册的快捷键：正式版就是设置值；开发版在设置仍是出厂默认值时换成开发用的组合
/// （出厂默认值属于本机已安装的应用，开发版抢先注册会让它注册失败）。
fn effective(configured: &str, default: &str, development: &str) -> String {
    if cfg!(feature = "production-identity") || configured != default {
        return configured.to_owned();
    }

    development.to_owned()
}

#[cfg(test)]
mod tests {
    use global_hotkey::hotkey::{Code, Modifiers};
    use kwikpaste_core::settings::QuickPasteModifiers;

    use super::*;

    #[test]
    fn tauri_accelerators_parse_like_1x() {
        let parsed = |text: &str| parse(text).expect("valid").expect("not empty");

        assert_eq!(
            parsed("Alt+C"),
            HotKey::new(Some(Modifiers::ALT), Code::KeyC)
        );
        assert_eq!(
            parsed("Alt+X"),
            HotKey::new(Some(Modifiers::ALT), Code::KeyX)
        );
        assert_eq!(
            parsed(DEVELOPMENT_TOGGLE),
            HotKey::new(
                Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
                Code::F9
            )
        );
        assert_eq!(parse("  ").expect("empty is allowed"), None);
        assert!(parse("Alt+").is_err());
    }

    #[cfg(not(feature = "production-identity"))]
    #[test]
    fn development_builds_never_take_the_shipped_default() {
        let mut shortcuts = Shortcuts::default();
        assert_eq!(
            wanted_toggle(&shortcuts),
            parse(DEVELOPMENT_TOGGLE).expect("valid")
        );
        assert_eq!(
            wanted_preference(&shortcuts),
            parse(DEVELOPMENT_PREFERENCE).expect("valid")
        );

        shortcuts.open_clipboard = "Control+Shift+F7".to_owned();
        shortcuts.open_preference = "Alt+P".to_owned();
        assert_eq!(
            wanted_toggle(&shortcuts),
            parse("Control+Shift+F7").expect("valid")
        );
        assert_eq!(
            wanted_preference(&shortcuts),
            parse("Alt+P").expect("valid")
        );
    }

    #[test]
    fn quick_paste_registers_ten_digits_only_when_enabled() {
        let mut shortcuts = Shortcuts::default();
        assert!(wanted_quick_paste(&shortcuts).is_empty());

        shortcuts.quick_paste.enabled = true;
        shortcuts.quick_paste.modifiers = QuickPasteModifiers::ControlAlt;
        let wanted = wanted_quick_paste(&shortcuts);

        assert_eq!(wanted.len(), 10);
        assert_eq!(
            wanted[0],
            (
                HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Digit1),
                0
            )
        );
        assert_eq!(
            wanted[9],
            (
                HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Digit0),
                9
            )
        );
    }

    #[test]
    fn recording_and_pause_reasons_both_block_registration() {
        assert!(registration_allowed(false, false));
        assert!(!registration_allowed(true, false));
        assert!(!registration_allowed(false, true));
        assert!(!registration_allowed(true, true));
    }
}
