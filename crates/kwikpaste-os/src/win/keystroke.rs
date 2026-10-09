//! Windows 的按键注入（1.x `keystroke/windows.rs`）。

use std::io;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);
const PASTE_KEYS: [(VIRTUAL_KEY, KEYBD_EVENT_FLAGS); 4] = [
    (VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
    (VK_V, KEYBD_EVENT_FLAGS(0)),
    (VK_V, KEYEVENTF_KEYUP),
    (VK_CONTROL, KEYEVENTF_KEYUP),
];
/// 没有分配给任何按键的虚拟键码，注入后目标应用没有可见反应。
const VK_UNASSIGNED: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

pub fn ensure_accessibility_trusted() -> io::Result<()> {
    Ok(())
}

pub fn simulate_paste() -> io::Result<()> {
    send_keys(&PASTE_KEYS, None)
}

/// Recheck destination ownership and actual foreground immediately before SendInput.
pub fn simulate_paste_to(target: crate::paste_target::PasteTarget) -> io::Result<()> {
    send_keys(&PASTE_KEYS, Some(target))
}

pub fn mask_modifier_release() -> io::Result<()> {
    if ![VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(is_key_down) {
        return Ok(());
    }

    send_keys(
        &[
            (VK_UNASSIGNED, KEYBD_EVENT_FLAGS(0)),
            (VK_UNASSIGNED, KEYEVENTF_KEYUP),
        ],
        None,
    )
}

pub fn modifiers_pressed() -> bool {
    [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
        .into_iter()
        .any(is_key_down)
}

fn is_key_down(vk: VIRTUAL_KEY) -> bool {
    let state = unsafe { GetAsyncKeyState(i32::from(vk.0)) };
    state < 0
}

/// 按顺序注入一组键盘事件；系统拒收（例如被更高完整性级别的窗口挡住）时返回错误。
fn send_keys(
    keys: &[(VIRTUAL_KEY, KEYBD_EVENT_FLAGS)],
    target: Option<crate::paste_target::PasteTarget>,
) -> io::Result<()> {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, flags)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();

    if let Some(target) = target {
        let destination = super::paste_target::WindowTarget {
            window: target.window,
            process_id: target.process_id,
        };
        if !super::is_live_paste_target(destination)
            || super::foreground_window() != target.window
            || super::keyboard::is_captured()
            || modifiers_pressed()
        {
            return Err(io::Error::other(
                "paste destination or input handoff changed before SendInput",
            ));
        }
    }

    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(io::Error::other(format!(
            "SendInput injected {sent}/{} events: {}",
            inputs.len(),
            io::Error::last_os_error()
        )));
    }

    Ok(())
}
