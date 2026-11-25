//! 偏好窗的文案：key 由稳定的分类、分组、设置项 id 推导（1.x `preferenceI18n.ts`），以及字节数、
//! 快捷键的显示格式。

use gpui::SharedString;

use super::schema::{Control, Options, Section, Setting, Tab, TabId};
use crate::i18n::{t, t_args};

/// 官网与源码仓库：关于页两行的说明直接显示网址。
pub const WEBSITE_URL: &str = "https://paste.fastthree.com";
pub const REPOSITORY_URL: &str = "https://github.com/ManSanDADADA/KwikPaste";
/// 同步服务端仓库，README 开头就是自部署教程。
pub const SYNC_SERVER_URL: &str = "https://github.com/ManSanDADADA/KwikPaste-sync-server";

/// 取文案；key 不存在时返回空串（i18n 找不到时原样返回 key）。
pub fn optional(key: &str) -> SharedString {
    let text = t(key);
    if text.as_ref() == key {
        return SharedString::default();
    }

    text
}

pub fn tab_title(id: TabId) -> SharedString {
    t(&format!("preferences:schema.tabs.{}.title", id.key()))
}

pub fn tab_title_of(tab: &Tab) -> SharedString {
    tab_title(tab.id)
}

pub fn section_title(section: &Section) -> SharedString {
    t(&format!("preferences:schema.sections.{}.title", section.id))
}

/// 开机启动、托盘图标、任务栏图标的标题与说明分平台。
fn platform_field(setting: &Setting, field: &str) -> Option<String> {
    if !matches!(
        setting.id,
        "control.autoStart" | "control.trayIcon" | "control.dockIcon"
    ) {
        return None;
    }
    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    };

    Some(format!(
        "preferences:schema.settings.{}.{platform}.{field}",
        setting.id
    ))
}

pub fn setting_title(setting: &Setting) -> SharedString {
    let key = platform_field(setting, "title")
        .unwrap_or_else(|| format!("preferences:schema.settings.{}.title", setting.id));
    t(&key)
}

/// 说明是可选的：标题已经说清楚的设置不写说明。官网、源码两行显示网址。
pub fn setting_description(setting: &Setting) -> SharedString {
    match setting.id {
        "about.website" => return WEBSITE_URL.into(),
        "about.github" => return REPOSITORY_URL.into(),
        _ => {}
    }
    let key = platform_field(setting, "description")
        .unwrap_or_else(|| format!("preferences:schema.settings.{}.description", setting.id));
    optional(&key)
}

pub fn setting_key(setting: &Setting, field: &str) -> String {
    format!("preferences:schema.settings.{}.{field}", setting.id)
}

/// 下拉或分段选项的 `(值, 文案)`。
pub fn options(setting: &Setting) -> Vec<(SharedString, SharedString)> {
    let (Control::Select(options) | Control::Segmented(options)) = setting.control else {
        return Vec::new();
    };
    let label = |value: &str| {
        let field = if setting.id == "shortcuts.doubleTap" {
            let platform = if cfg!(target_os = "macos") {
                "macos"
            } else {
                "windows"
            };
            format!("options.{platform}.{value}")
        } else {
            format!("options.{value}")
        };
        t(&setting_key(setting, &field))
    };

    match options {
        Options::Values(values) => values
            .iter()
            .map(|value| (SharedString::from(*value), label(value)))
            .collect(),
        Options::Numbers(values) => values
            .iter()
            .map(|value| {
                let value = value.to_string();
                let text = label(&value);
                (SharedString::from(value), text)
            })
            .collect(),
        Options::Shortcuts(values) => values
            .iter()
            .map(|(value, shortcut)| {
                (
                    SharedString::from(*value),
                    SharedString::from(format_shortcut(shortcut)),
                )
            })
            .collect(),
    }
}

pub fn number_suffix(key: &str) -> SharedString {
    t(&format!("preferences:schema.numberSuffixes.{key}"))
}

pub fn capture_kind_label(kind: &str) -> SharedString {
    t(&format!("preferences:schema.captureKinds.{kind}"))
}

/// 字节数：1024 进制，B / KB / MB / GB / TB；B 或 ≥ 10 时不带小数，否则一位小数（1.x `formatBytes`）。
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    let name = UNITS.get(unit).copied().unwrap_or("TB");
    if unit == 0 || value >= 10. {
        format!("{value:.0} {name}")
    } else {
        format!("{value:.1} {name}")
    }
}

/// 快捷键字面量（`Control+Shift+V`）按平台显示：Windows 用 `+` 连接 `Ctrl` `Win` 等，macOS 用符号不加分隔。
pub fn format_shortcut(value: &str) -> String {
    let keys: Vec<String> = value
        .split('+')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(key_display)
        .collect();
    if cfg!(target_os = "macos") {
        keys.concat()
    } else {
        keys.join("+")
    }
}

fn key_display(key: &str) -> String {
    let mac = cfg!(target_os = "macos");
    let symbol = match key {
        "Alt" | "Option" => Some(if mac { "⌥" } else { "Alt" }),
        "Command" | "Meta" | "Super" => Some(if mac { "⌘" } else { "Win" }),
        "Cmd" | "CmdOrCtrl" | "CommandOrControl" | "Mod" => Some(if mac { "⌘" } else { "Ctrl" }),
        "Control" | "Ctrl" => Some(if mac { "⌃" } else { "Ctrl" }),
        "Shift" => Some(if mac { "⇧" } else { "Shift" }),
        "Delete" => Some(if mac { "⌦" } else { "Del" }),
        "Esc" | "Escape" => Some(if mac { "⎋" } else { "Esc" }),
        "Enter" | "Return" => Some(if mac { "⏎" } else { "Enter" }),
        "Space" => Some(if mac { "␣" } else { "Space" }),
        "Tab" => Some(if mac { "⇥" } else { "Tab" }),
        "Backspace" => Some("⌫"),
        "ArrowUp" | "Up" => Some("↑"),
        "ArrowDown" | "Down" => Some("↓"),
        "ArrowLeft" | "Left" => Some("←"),
        "ArrowRight" | "Right" => Some("→"),
        _ => None,
    };
    if let Some(symbol) = symbol {
        return symbol.to_owned();
    }
    if let Some(letter) = key.strip_prefix("Key").filter(|rest| rest.len() == 1) {
        return letter.to_owned();
    }
    if let Some(digit) = key.strip_prefix("Digit").filter(|rest| rest.len() == 1) {
        return digit.to_owned();
    }
    if key.chars().count() == 1 {
        return key.to_uppercase();
    }

    key.to_owned()
}

/// `{{label}}` 一类插值的便捷写法。
pub fn with_args(key: &str, args: &[(&str, &str)]) -> SharedString {
    t_args(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_follow_the_1x_format() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(10 * 1024), "10 KB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(format_bytes(1_073_741_824 * 1024 * 5000), "5000 TB");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn shortcuts_display_like_1x_on_windows() {
        assert_eq!(format_shortcut("Control+Shift"), "Ctrl+Shift");
        assert_eq!(format_shortcut("Command+Alt+KeyV"), "Win+Alt+V");
        assert_eq!(format_shortcut("Alt+Escape"), "Alt+Esc");
        assert_eq!(format_shortcut("F9"), "F9");
        assert_eq!(format_shortcut(""), "");
    }
}
