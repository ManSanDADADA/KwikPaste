//! Windows 的按键注入（1.x `keystroke/windows.rs`）。

use std::io;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_INSERT, VK_LWIN,
    VK_MENU, VK_RWIN, VK_SHIFT,
};

const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);
/// 没有分配给任何按键的虚拟键码，注入后目标应用没有可见反应。
const VK_UNASSIGNED: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

/// 默认键位里 Ctrl+V 不是粘贴（当作 ^V 发给 shell）、Shift+Insert 才是粘贴的终端，按 exe 文件名
/// （不含扩展名，不区分大小写）匹配。
const SHIFT_INSERT_PASTE_APPS: [&str; 3] = ["WindTerm", "NyaTerm", "putty"];

pub fn ensure_accessibility_trusted() -> io::Result<()> {
    Ok(())
}

/// 前台是 [`SHIFT_INSERT_PASTE_APPS`] 里的终端时发 Shift+Insert，否则发 Ctrl+V。
pub fn simulate_paste() -> io::Result<()> {
    let shift_insert =
        super::apps::frontmost_app().is_some_and(|app| pastes_with_shift_insert(&app.name));
    if shift_insert {
        // 不带扩展键标志的 Insert 是小键盘 0，NumLock 打开时终端会读成数字键。
        return send_keys(&[
            (VK_SHIFT, KEYBD_EVENT_FLAGS(0)),
            (VK_INSERT, KEYEVENTF_EXTENDEDKEY),
            (VK_INSERT, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
            (VK_SHIFT, KEYEVENTF_KEYUP),
        ]);
    }

    send_keys(&[
        (VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        (VK_V, KEYBD_EVENT_FLAGS(0)),
        (VK_V, KEYEVENTF_KEYUP),
        (VK_CONTROL, KEYEVENTF_KEYUP),
    ])
}

fn pastes_with_shift_insert(exe_stem: &str) -> bool {
    SHIFT_INSERT_PASTE_APPS
        .iter()
        .any(|app| app.eq_ignore_ascii_case(exe_stem))
}

pub fn mask_modifier_release() -> io::Result<()> {
    if ![VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(is_key_down) {
        return Ok(());
    }

    send_keys(&[
        (VK_UNASSIGNED, KEYBD_EVENT_FLAGS(0)),
        (VK_UNASSIGNED, KEYEVENTF_KEYUP),
    ])
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

/// 按顺序注入一组键盘事件；系统拒收（例如被更高完整性级别的窗口挡住）时返回错误，
/// 错误的 `kind` 沿用系统错误码（拒绝访问为 `PermissionDenied`）。
fn send_keys(keys: &[(VIRTUAL_KEY, KEYBD_EVENT_FLAGS)]) -> io::Result<()> {
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

    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        let os = io::Error::last_os_error();
        return Err(io::Error::new(
            os.kind(),
            format!("SendInput injected {sent}/{} events: {os}", inputs.len()),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_terminals_paste_with_shift_insert() {
        assert!(pastes_with_shift_insert("WindTerm"));
        assert!(pastes_with_shift_insert("nyaterm"));
        assert!(!pastes_with_shift_insert("WindowsTerminal"));
        assert!(!pastes_with_shift_insert("Code"));
    }
}
