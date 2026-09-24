use anyhow::anyhow;
use std::mem::{size_of, zeroed};
use winapi::um::winuser::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL,
    VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

use crate::core::error::Result;

/// V 键的虚拟键码，winapi 未直接导出。
const VK_V: u16 = 0x56;

/// 未分配给任何按键的虚拟键码，注入后目标应用不会有可见反应。
const VK_UNASSIGNED: u16 = 0xE8;

/// 向系统事件队列投递一次 Ctrl+V，模拟「粘贴」。
///
/// 之所以选 Ctrl+V 而非 Shift+Insert：
/// - Monaco editor（VS Code 聊天输入、所有 Web 嵌入式代码编辑器）把 Insert 解释为
///   「切换插入/改写模式」并吞掉 Shift 修饰，导致「粘贴失效 + 光标变成块状」。
/// - Ctrl+V 是 Windows / Chromium / Electron / 现代 IDE 输入控件普遍约定，
///   与剪贴板内容类型无关，覆盖面最广。
pub fn simulate_paste() -> Result<()> {
    send_keys(&[
        (VK_CONTROL as u16, 0),
        (VK_V, 0),
        (VK_V, KEYEVENTF_KEYUP),
        (VK_CONTROL as u16, KEYEVENTF_KEYUP),
    ])
}

/// 全局快捷键命中时用户还按着修饰键；Alt / Win 在没有其它按键参与时松开，
/// 会激活目标窗口的菜单栏或弹出开始菜单。趁它们仍按着时注入一次无意义按键，
/// 让这次松开不再被当作单独按下。
pub fn mask_modifier_release() -> Result<()> {
    if ![VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(is_key_down) {
        return Ok(());
    }

    send_keys(&[(VK_UNASSIGNED, 0), (VK_UNASSIGNED, KEYEVENTF_KEYUP)])
}

/// 判断用户是否仍按着 Ctrl / Shift / Alt / Win 中的任意一个。
pub fn modifiers_pressed() -> bool {
    [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
        .into_iter()
        .any(is_key_down)
}

fn is_key_down(vk: i32) -> bool {
    (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
}

/// 按顺序注入一组 `(虚拟键码, dwFlags)` 键盘事件。
fn send_keys(keys: &[(u16, u32)]) -> Result<()> {
    let mut inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, flags)| {
            let mut input: INPUT = unsafe { zeroed() };
            input.type_ = INPUT_KEYBOARD;
            unsafe {
                *input.u.ki_mut() = KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                };
            }
            input
        })
        .collect();

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        return Err(anyhow!("SendInput injected {sent}/{} events", inputs.len()).into());
    }
    Ok(())
}
