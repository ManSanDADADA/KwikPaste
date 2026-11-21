//! 面板动作与可移植按键契约；不依赖窗口工具包或平台钩子。

use super::Shortcuts;

/// 面板动作的稳定标识；分组动作的键位顺序与执行方向一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    PasteSelected,
    PasteSelectedPlain,
    PasteNth,
    CopySelected,
    FavoriteSelected,
    PinSelected,
    NoteSelected,
    EditContent,
    SplitSelected,
    /// 翻译当前项（翻译扩展启用时）。
    TranslateSelected,
    OpenSelected,
    DeleteSelected,
    SelectAll,
    Navigate,
    SwitchCategory,
    SwitchCustomGroup,
    FocusSearch,
    ToggleRange,
    CreateGroup,
    PreviewSelected,
    /// 查看当前项：图片在独立窗口里看大图，文本打开内容编辑窗。
    ViewImage,
    PinWindow,
    ShowShortcuts,
    OpenPreference,
    ClosePreviewFilterWindow,
}

impl PanelAction {
    pub const ALL: [Self; 25] = [
        Self::PasteSelected,
        Self::PasteSelectedPlain,
        Self::PasteNth,
        Self::CopySelected,
        Self::FavoriteSelected,
        Self::PinSelected,
        Self::NoteSelected,
        Self::EditContent,
        Self::SplitSelected,
        Self::TranslateSelected,
        Self::OpenSelected,
        Self::DeleteSelected,
        Self::SelectAll,
        Self::Navigate,
        Self::SwitchCategory,
        Self::SwitchCustomGroup,
        Self::FocusSearch,
        Self::ToggleRange,
        Self::CreateGroup,
        Self::PreviewSelected,
        Self::ViewImage,
        Self::PinWindow,
        Self::ShowShortcuts,
        Self::OpenPreference,
        Self::ClosePreviewFilterWindow,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::PasteSelected => "pasteSelected",
            Self::PasteSelectedPlain => "pasteSelectedPlain",
            Self::PasteNth => "pasteNth",
            Self::CopySelected => "copySelected",
            Self::FavoriteSelected => "favoriteSelected",
            Self::PinSelected => "pinSelected",
            Self::NoteSelected => "noteSelected",
            Self::EditContent => "editContent",
            Self::SplitSelected => "splitSelected",
            Self::TranslateSelected => "translateSelected",
            Self::OpenSelected => "openSelected",
            Self::DeleteSelected => "deleteSelected",
            Self::SelectAll => "selectAll",
            Self::Navigate => "navigate",
            Self::SwitchCategory => "switchCategory",
            Self::SwitchCustomGroup => "switchCustomGroup",
            Self::FocusSearch => "focusSearch",
            Self::ToggleRange => "toggleRange",
            Self::CreateGroup => "createGroup",
            Self::PreviewSelected => "previewSelected",
            Self::ViewImage => "viewItem",
            Self::PinWindow => "pinWindow",
            Self::ShowShortcuts => "showShortcuts",
            Self::OpenPreference => "openPreference",
            Self::ClosePreviewFilterWindow => "closePreviewFilterWindow",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    pub const fn editable(self) -> bool {
        !matches!(
            self,
            Self::Navigate
                | Self::SwitchCategory
                | Self::SwitchCustomGroup
                | Self::PreviewSelected
                | Self::ClosePreviewFilterWindow
        )
    }

    /// 自定义按键上线之后才新增的动作：默认键是“软”的，别的动作已经占用它时让位，
    /// 免得老用户的设置因为按键重复而保存不了。
    const fn soft_default(self) -> bool {
        matches!(self, Self::TranslateSelected)
    }

    /// 默认 GPUI 按键；第 N 项粘贴只返回修饰键前缀，数字由运行时展开。
    pub const fn default_keys(self) -> &'static [&'static str] {
        match self {
            Self::PasteSelected => &["enter"],
            Self::PasteSelectedPlain => &["secondary-enter"],
            Self::PasteNth => &["secondary"],
            Self::CopySelected => &["secondary-c"],
            Self::FavoriteSelected => &["secondary-d"],
            Self::PinSelected => &["secondary-t"],
            Self::NoteSelected => &["secondary-m"],
            Self::EditContent => &["secondary-e"],
            Self::SplitSelected => &["secondary-s"],
            Self::TranslateSelected => &["secondary-l"],
            Self::OpenSelected => &["secondary-o"],
            Self::DeleteSelected if cfg!(target_os = "macos") => {
                &["secondary-backspace", "delete", "secondary-delete"]
            }
            Self::DeleteSelected => &["delete", "secondary-backspace", "secondary-delete"],
            Self::SelectAll => &["secondary-a"],
            Self::Navigate => &["up", "down"],
            Self::SwitchCategory => &["left", "right"],
            Self::SwitchCustomGroup => &["tab", "shift-tab"],
            Self::FocusSearch => &["secondary-f"],
            Self::ToggleRange => &["secondary-q"],
            Self::CreateGroup => &["secondary-n"],
            Self::PreviewSelected => &["space"],
            // 默认键不在可录入集合里（由列表的按下监听处理），用户覆盖不会与它重复。
            Self::ViewImage => &["shift-space"],
            Self::PinWindow => &["secondary-p"],
            Self::ShowShortcuts => &["secondary-k"],
            Self::OpenPreference => &["secondary-,"],
            Self::ClosePreviewFilterWindow => &["escape"],
        }
    }
}

/// 按数字顺序展开第 N 项粘贴；空前缀表示解绑。
fn expand_nth(prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0']
        .into_iter()
        .map(|key| format!("{prefix}-{key}"))
        .collect()
}

/// 读取有效键位。手改配置的非法覆盖回落默认，固定动作及未知 id 不改变运行时行为。
pub fn effective_panel_keys(shortcuts: &Shortcuts, action: PanelAction) -> Vec<String> {
    let overrides = shortcuts
        .panel
        .get(action.id())
        .filter(|value| action.editable() && override_allowed(action, value));
    if let Some(value) = overrides {
        if action == PanelAction::PasteNth {
            return expand_nth(value);
        }
        return if value.is_empty() {
            Vec::new()
        } else {
            vec![value.clone()]
        };
    }
    if action.soft_default() && default_claimed(shortcuts, action) {
        return Vec::new();
    }
    if action == PanelAction::PasteNth {
        return expand_nth("secondary");
    }
    action
        .default_keys()
        .iter()
        .map(|key| (*key).to_owned())
        .collect()
}

/// 软默认键是否已被别的动作的有效覆盖占用。
fn default_claimed(shortcuts: &Shortcuts, action: PanelAction) -> bool {
    action.default_keys().iter().any(|key| {
        shortcuts.panel.iter().any(|(id, value)| {
            value == key
                && PanelAction::from_id(id).is_some_and(|other| {
                    other != action && other.editable() && override_allowed(other, value)
                })
        })
    })
}

/// 只接收规范小写键位，避免吞掉普通输入或覆盖固定导航键。
pub fn panel_keystroke_allowed(keystroke: &str) -> bool {
    let (secondary, key) = match keystroke.strip_prefix("secondary-") {
        Some(key) => (true, key),
        None => (false, keystroke),
    };
    let key = key.strip_prefix("shift-").unwrap_or(key);
    let special = matches!(key, "enter" | "delete" | "backspace")
        || key
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=24).contains(&n) && key == format!("f{n}"));
    if special {
        return true;
    }
    secondary
        && key.len() == 1
        && key.bytes().all(|key| {
            key.is_ascii_lowercase() || key.is_ascii_digit() || b",./;'[]-=`\\".contains(&key)
        })
}

fn override_allowed(action: PanelAction, value: &str) -> bool {
    value.is_empty()
        || if action == PanelAction::PasteNth {
            matches!(value, "secondary" | "secondary-shift")
        } else {
            panel_keystroke_allowed(value)
        }
}

/// 占用某个物理组合键的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutOwner {
    Panel(PanelAction),
    Global(&'static str),
}

/// 将全局 accelerator 转成当前平台的面板键位；面板无法表达的修饰键返回 None。
pub fn accelerator_to_panel_keystroke(accelerator: &str) -> Option<String> {
    let mut parts = accelerator
        .split('+')
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let key = parts.pop()?;
    let mut secondary = false;
    let mut shift = false;
    for modifier in parts {
        match modifier.as_str() {
            "shift" if !shift => shift = true,
            "commandorcontrol" | "commandorctrl" | "cmdorctrl" | "cmdorcontrol" | "mod"
                if !secondary =>
            {
                secondary = true
            }
            "control" | "ctrl" if !cfg!(target_os = "macos") && !secondary => secondary = true,
            "command" | "cmd" | "super" | "meta" if cfg!(target_os = "macos") && !secondary => {
                secondary = true
            }
            _ => return None,
        }
    }
    let key = match key.as_str() {
        "return" => "enter",
        "esc" => "escape",
        "arrowup" => "up",
        "arrowdown" => "down",
        "arrowleft" => "left",
        "arrowright" => "right",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "semicolon" => ";",
        "quote" => "'",
        "bracketleft" => "[",
        "bracketright" => "]",
        "minus" => "-",
        "equal" => "=",
        "backquote" => "`",
        "backslash" => "\\",
        key => key
            .strip_prefix("key")
            .filter(|key| key.len() == 1)
            .or_else(|| key.strip_prefix("digit").filter(|key| key.len() == 1))
            .unwrap_or(key),
    };
    if key.is_empty() {
        return None;
    }
    Some(format!(
        "{}{}{key}",
        if secondary { "secondary-" } else { "" },
        if shift { "shift-" } else { "" }
    ))
}

/// 查找已占用该键位的面板动作，包含固定键与第 N 项粘贴的十个数字。
pub fn panel_action_using(shortcuts: &Shortcuts, keystroke: &str) -> Option<PanelAction> {
    PanelAction::ALL.into_iter().find(|action| {
        effective_panel_keys(shortcuts, *action)
            .iter()
            .any(|key| key == keystroke)
    })
}

/// 检查候选键位与其它面板动作、全局热键的冲突；第 N 项粘贴传入修饰键前缀。
pub fn panel_key_conflict(
    shortcuts: &Shortcuts,
    action: PanelAction,
    keystroke: &str,
) -> Option<ShortcutOwner> {
    let candidates = if action == PanelAction::PasteNth {
        expand_nth(keystroke)
    } else if keystroke.is_empty() {
        Vec::new()
    } else {
        vec![keystroke.to_owned()]
    };
    for other in PanelAction::ALL {
        let soft = other.soft_default() && !shortcuts.panel.contains_key(other.id());
        if other != action
            && !soft
            && effective_panel_keys(shortcuts, other)
                .iter()
                .any(|key| candidates.contains(key))
        {
            return Some(ShortcutOwner::Panel(other));
        }
    }
    let mut globals = vec![
        ("openClipboard", shortcuts.open_clipboard.clone()),
        ("openPreference", shortcuts.open_preference.clone()),
        ("pastePlain", shortcuts.paste_plain.clone()),
    ];
    if shortcuts.quick_paste.enabled {
        globals.extend(('0'..='9').map(|key| {
            (
                "quickPaste",
                format!("{}+{key}", shortcuts.quick_paste.modifiers.accelerator()),
            )
        }));
    }
    globals.into_iter().find_map(|(owner, value)| {
        accelerator_to_panel_keystroke(&value)
            .filter(|key| candidates.contains(key))
            .map(|_| ShortcutOwner::Global(owner))
    })
}

/// 写入时只校验面板内部约束；存量全局热键与默认面板键重叠不能阻止其它设置保存。
pub(super) fn validate_panel_shortcuts(shortcuts: &Shortcuts) -> anyhow::Result<()> {
    for (id, value) in &shortcuts.panel {
        let Some(action) = PanelAction::from_id(id) else {
            continue;
        };
        anyhow::ensure!(action.editable(), "panel action {id} has fixed keys");
        anyhow::ensure!(
            override_allowed(action, value),
            "invalid panel keystroke for {id}"
        );
    }
    let mut seen = std::collections::HashSet::new();
    for action in PanelAction::ALL {
        for key in effective_panel_keys(shortcuts, action) {
            anyhow::ensure!(seen.insert(key), "panel shortcuts must be unique");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_ids_round_trip() {
        let shortcuts: Shortcuts = serde_json::from_str("{}").unwrap();
        assert!(shortcuts.panel.is_empty());
        let restored: Shortcuts =
            serde_json::from_value(serde_json::to_value(&shortcuts).unwrap()).unwrap();
        assert_eq!(shortcuts, restored);
        for action in PanelAction::ALL {
            assert_eq!(PanelAction::from_id(action.id()), Some(action));
            assert!(!effective_panel_keys(&shortcuts, action).is_empty());
        }
        assert!(validate_panel_shortcuts(&shortcuts).is_ok());
        assert_eq!(
            effective_panel_keys(&shortcuts, PanelAction::PasteNth).len(),
            10
        );
        assert_eq!(
            effective_panel_keys(&shortcuts, PanelAction::DeleteSelected).len(),
            3
        );
    }

    #[test]
    fn overrides_unbind_and_ignore_future_ids() {
        let mut shortcuts = Shortcuts::default();
        shortcuts
            .panel
            .insert("favoriteSelected".into(), "secondary-b".into());
        shortcuts.panel.insert("copySelected".into(), String::new());
        shortcuts
            .panel
            .insert("pasteNth".into(), "secondary-shift".into());
        shortcuts
            .panel
            .insert("futureAction".into(), "future-format".into());
        assert_eq!(
            effective_panel_keys(&shortcuts, PanelAction::FavoriteSelected),
            ["secondary-b"]
        );
        assert!(effective_panel_keys(&shortcuts, PanelAction::CopySelected).is_empty());
        assert_eq!(
            panel_action_using(&shortcuts, "secondary-shift-0"),
            Some(PanelAction::PasteNth)
        );
        assert!(validate_panel_shortcuts(&shortcuts).is_ok());
        shortcuts.panel.insert("navigate".into(), String::new());
        assert!(validate_panel_shortcuts(&shortcuts).is_err());
        assert_eq!(
            effective_panel_keys(&shortcuts, PanelAction::Navigate),
            ["up", "down"]
        );
    }

    #[test]
    fn view_image_can_be_rebound_and_unbound() {
        let mut shortcuts = Shortcuts::default();
        assert_eq!(
            effective_panel_keys(&shortcuts, PanelAction::ViewImage),
            ["shift-space"]
        );
        shortcuts
            .panel
            .insert("viewItem".into(), "secondary-shift-v".into());
        assert_eq!(
            effective_panel_keys(&shortcuts, PanelAction::ViewImage),
            ["secondary-shift-v"]
        );
        assert!(validate_panel_shortcuts(&shortcuts).is_ok());
        shortcuts.panel.insert("viewItem".into(), String::new());
        assert!(effective_panel_keys(&shortcuts, PanelAction::ViewImage).is_empty());
        assert!(validate_panel_shortcuts(&shortcuts).is_ok());
    }

    #[test]
    fn allowed_keys_and_duplicate_rejection() {
        for key in [
            "secondary-shift-b",
            "shift-enter",
            "f24",
            "secondary--",
            "secondary-\\",
        ] {
            assert!(panel_keystroke_allowed(key), "{key}");
        }
        for key in [
            "b",
            "shift-b",
            "secondary-tab",
            "secondary-space",
            "secondary-up",
            "alt-b",
            "shift-secondary-b",
            "secondary-B",
            "f25",
            "f01",
            "secondary",
        ] {
            assert!(!panel_keystroke_allowed(key), "{key}");
        }
        let mut shortcuts = Shortcuts::default();
        shortcuts
            .panel
            .insert("favoriteSelected".into(), "secondary-c".into());
        assert!(validate_panel_shortcuts(&shortcuts).is_err());
        assert_eq!(
            panel_key_conflict(&shortcuts, PanelAction::FavoriteSelected, "secondary-0"),
            Some(ShortcutOwner::Panel(PanelAction::PasteNth))
        );
        shortcuts
            .panel
            .insert("favoriteSelected".into(), "shift-b".into());
        assert!(validate_panel_shortcuts(&shortcuts).is_err());
    }

    #[test]
    fn translate_default_yields_to_an_existing_override() {
        let mut shortcuts = Shortcuts::default();
        assert_eq!(
            effective_panel_keys(&shortcuts, PanelAction::TranslateSelected),
            ["secondary-l"]
        );
        // 默认键不算冲突：别的动作可以直接改成 Mod+L。
        assert_eq!(
            panel_key_conflict(&shortcuts, PanelAction::FavoriteSelected, "secondary-l"),
            None
        );
        // 升级前就把别的动作改成 Mod+L 的用户：翻译让位，设置照常能保存。
        shortcuts
            .panel
            .insert("favoriteSelected".into(), "secondary-l".into());
        assert!(effective_panel_keys(&shortcuts, PanelAction::TranslateSelected).is_empty());
        assert!(validate_panel_shortcuts(&shortcuts).is_ok());
        // 用户给翻译另设的键照常参与冲突检查。
        shortcuts
            .panel
            .insert("translateSelected".into(), "secondary-j".into());
        assert_eq!(
            panel_key_conflict(&shortcuts, PanelAction::NoteSelected, "secondary-j"),
            Some(ShortcutOwner::Panel(PanelAction::TranslateSelected))
        );
    }

    #[test]
    fn globals_map_to_physical_panel_keys_without_blocking_legacy_settings() {
        let mut shortcuts = Shortcuts::default();
        #[cfg(target_os = "windows")]
        {
            shortcuts.open_clipboard = "Control+D".into();
        }
        #[cfg(target_os = "macos")]
        {
            shortcuts.open_clipboard = "Super+D".into();
        }
        assert_eq!(
            accelerator_to_panel_keystroke(&shortcuts.open_clipboard),
            Some("secondary-d".into())
        );
        assert_eq!(
            panel_key_conflict(&shortcuts, PanelAction::FavoriteSelected, "secondary-d"),
            Some(ShortcutOwner::Global("openClipboard"))
        );
        assert!(validate_panel_shortcuts(&shortcuts).is_ok());
        assert_eq!(accelerator_to_panel_keystroke("Alt+D"), None);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(accelerator_to_panel_keystroke("Control+D"), None);
            assert_eq!(
                accelerator_to_panel_keystroke("Command+Shift+B"),
                Some("secondary-shift-b".into())
            );
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(accelerator_to_panel_keystroke("Super+D"), None);
            shortcuts.quick_paste.enabled = true;
            assert_eq!(
                panel_key_conflict(&shortcuts, PanelAction::PasteNth, "secondary-shift"),
                Some(ShortcutOwner::Global("quickPaste"))
            );
        }
    }
}
