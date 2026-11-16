//! 快捷键的显示文案，移植自 1.x `utils/shortcut.ts` 的 `formatShortcutDisplay`：按 `+` 拆开，
//! 每个键换成平台写法，Windows 用 `+` 连起来、macOS 符号直接拼接（`CmdOrCtrl+Backspace` → Windows `Ctrl+⌫`、macOS `⌘⌫`）。

use kwikpaste_core::settings::{PanelAction, Shortcuts, effective_panel_keys};

/// 面板的主显示键位；解绑时不显示提示，macOS 删除键的优先顺序由 core 决定。
pub fn panel_display(shortcuts: &Shortcuts, action: PanelAction) -> Option<String> {
    effective_panel_keys(shortcuts, action)
        .first()
        .map(|key| display_keystroke(key))
}

/// 按住平台修饰键时的提示，只省略已经按住的 secondary，保留 Shift 等剩余键。
pub fn panel_hint(shortcuts: &Shortcuts, action: PanelAction) -> Option<String> {
    effective_panel_keys(shortcuts, action)
        .first()
        .map(|key| display_keystroke(key.strip_prefix("secondary-").unwrap_or(key)))
}

/// 可移植 GPUI 单键显示；逐个取修饰键前缀，不能把减号键也按分隔符拆掉。
pub fn display_keystroke(key: &str) -> String {
    let mut parts = Vec::new();
    let mut key = key;
    if let Some(rest) = key.strip_prefix("secondary-") {
        parts.push("CmdOrCtrl");
        key = rest;
    }
    if let Some(rest) = key.strip_prefix("shift-") {
        parts.push("Shift");
        key = rest;
    }
    parts.push(key);
    display(&parts.join("+"))
}

/// 当前平台的显示文案。
pub fn display(pattern: &str) -> String {
    display_for(pattern, cfg!(target_os = "macos"))
}

fn display_for(pattern: &str, mac: bool) -> String {
    pattern
        .split('+')
        .map(|key| key_display(key.trim(), mac))
        .collect::<Vec<_>>()
        .join(if mac { "" } else { "+" })
}

/// 1.x `KEY_DISPLAY`；不在表里的单个字符转大写，其余原样。
fn key_display(key: &str, mac: bool) -> String {
    let pick = |macos: &str, other: &str| (if mac { macos } else { other }).to_owned();

    match key {
        "Alt" | "Option" => pick("⌥", "Alt"),
        "Cmd" | "CmdOrCtrl" | "CommandOrControl" | "Mod" => pick("⌘", "Ctrl"),
        "Command" | "Meta" | "Super" => pick("⌘", "Win"),
        "Control" | "Ctrl" => pick("⌃", "Ctrl"),
        "Shift" => pick("⇧", "Shift"),
        "Enter" | "Return" | "enter" => pick("⏎", "Enter"),
        "Esc" | "Escape" | "escape" => pick("⎋", "Esc"),
        "Delete" | "delete" => pick("⌦", "Del"),
        "Space" | "space" => pick("␣", "Space"),
        "Tab" | "tab" => pick("⇥", "Tab"),
        "Backspace" | "backspace" => "⌫".to_owned(),
        "ArrowDown" | "Down" | "down" => "↓".to_owned(),
        "ArrowLeft" | "Left" | "left" => "←".to_owned(),
        "ArrowRight" | "Right" | "right" => "→".to_owned(),
        "ArrowUp" | "Up" | "up" => "↑".to_owned(),
        key if key.starts_with('f')
            && key
                .get(1..)
                .is_some_and(|number| number.parse::<u8>().is_ok()) =>
        {
            key.to_ascii_uppercase()
        }
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => c.to_uppercase().collect(),
                _ => key.to_owned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_spells_modifiers_out() {
        assert_eq!(display_for("CmdOrCtrl+Backspace", false), "Ctrl+⌫");
        assert_eq!(display_for("CmdOrCtrl+Enter", false), "Ctrl+Enter");
        assert_eq!(display_for("CmdOrCtrl+a", false), "Ctrl+A");
        assert_eq!(display_for("Enter", false), "Enter");
        assert_eq!(display_for("Escape", false), "Esc");
    }

    #[test]
    fn macos_uses_symbols() {
        assert_eq!(display_for("CmdOrCtrl+Backspace", true), "⌘⌫");
        assert_eq!(display_for("CmdOrCtrl+Enter", true), "⌘⏎");
        assert_eq!(display_for("Escape", true), "⎋");
    }
}
