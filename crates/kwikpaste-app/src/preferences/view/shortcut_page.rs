//! 快捷键页：每个全局功能一行，点进二级页增删触发方式（组合键、双击按键、鼠标按键、Win+V）；
//! 剪贴板窗口内的按键表点按键重新录制。触发方式仍存在原来的设置字段里，每种最多一个。
use super::*;
use kwikpaste_core::settings::{
    DoubleTapKey, KeySide, MouseTrigger, PanelAction, QuickPasteModifiers, ShortcutOwner,
    accelerator_to_panel_keystroke, effective_panel_keys, panel_action_using, panel_key_conflict,
    panel_keystroke_allowed,
};
use kwikpaste_ui::{MenuEntry, MenuItem, MenuTrigger};

fn words(key: &str) -> gpui::SharedString {
    i18n::t(&format!("preferences:shortcutsPage.{key}"))
}

fn words_args(key: &str, args: &[(&str, &str)]) -> gpui::SharedString {
    i18n::t_args(&format!("preferences:shortcutsPage.{key}"), args)
}

/// 一个全局功能可以挂的触发方式。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Combo,
    DoubleTap,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Mouse,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    WinV,
}

impl Trigger {
    fn key(self) -> &'static str {
        match self {
            Self::Combo => "combo",
            Self::DoubleTap => "doubleTap",
            Self::Mouse => "mouse",
            Self::WinV => "winV",
        }
    }

    fn icon(self) -> PrefIcon {
        match self {
            Self::Combo => PrefIcon::Keyboard,
            Self::DoubleTap => PrefIcon::Repeat,
            Self::Mouse => PrefIcon::Mouse,
            Self::WinV => PrefIcon::AppWindow,
        }
    }
}

/// 只有「打开剪贴板窗口」能用双击按键、鼠标按键和 Win+V；鼠标和 Win+V 只在 Windows 实现。
fn allowed_triggers(id: &str) -> &'static [Trigger] {
    if id != "shortcuts.openClipboard" {
        return &[Trigger::Combo];
    }
    #[cfg(target_os = "windows")]
    {
        &[
            Trigger::Combo,
            Trigger::DoubleTap,
            Trigger::Mouse,
            Trigger::WinV,
        ]
    }
    #[cfg(not(target_os = "windows"))]
    {
        &[Trigger::Combo, Trigger::DoubleTap]
    }
}

fn combo_value<'a>(settings: &'a Settings, id: &str) -> &'a str {
    match id {
        "shortcuts.openClipboard" => &settings.shortcuts.open_clipboard,
        "shortcuts.openPreference" => &settings.shortcuts.open_preference,
        "shortcuts.pastePlain" => &settings.shortcuts.paste_plain,
        _ => "",
    }
}

/// 正在设置的一方。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ShortcutTarget {
    Global(&'static str),
    Panel(PanelAction),
}

/// 已经占用这个按键的一方。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ShortcutUser {
    Global(&'static str),
    QuickPaste,
    Panel(PanelAction),
}

impl ShortcutUser {
    fn name(self) -> gpui::SharedString {
        match self {
            Self::Global(id) => i18n::t(&format!("preferences:schema.settings.{id}.title")),
            Self::QuickPaste => i18n::t("preferences:schema.settings.shortcuts.quickPaste.title"),
            Self::Panel(action) => panel_action_title(action),
        }
    }

    /// 让出按键的补丁；快速粘贴、窗口内的固定键和「粘贴前 10 项」不能单独让出一个组合。
    fn release_patch(self) -> Option<serde_json::Value> {
        match self {
            Self::Global(id) => shortcut_path(id).map(|path| values::patch(path, json!(""))),
            Self::Panel(action) if action.editable() && action != PanelAction::PasteNth => {
                Some(panel_patch(action, Some(String::new())))
            }
            _ => None,
        }
    }
}

/// 录到的按键已被别处使用：在录制的那一行下面提示，可以改用到这里。
pub(super) struct ShortcutConflict {
    target: ShortcutTarget,
    value: String,
    user: ShortcutUser,
}

/// core 报告的占用方换成设置项 id。
fn user_of(owner: ShortcutOwner) -> ShortcutUser {
    match owner {
        ShortcutOwner::Panel(action) => ShortcutUser::Panel(action),
        ShortcutOwner::Global("openPreference") => ShortcutUser::Global("shortcuts.openPreference"),
        ShortcutOwner::Global("pastePlain") => ShortcutUser::Global("shortcuts.pastePlain"),
        ShortcutOwner::Global("openClipboard") => ShortcutUser::Global("shortcuts.openClipboard"),
        ShortcutOwner::Global(_) => ShortcutUser::QuickPaste,
    }
}

fn global_user(id: &'static str) -> ShortcutUser {
    if id == "shortcuts.quickPaste" {
        ShortcutUser::QuickPaste
    } else {
        ShortcutUser::Global(id)
    }
}

/// 全局组合键与别的全局快捷键、快速粘贴或剪贴板窗口内的按键是否撞车。
pub(super) fn global_shortcut_user(
    id: &'static str,
    shortcut: &str,
    settings: &Settings,
) -> Option<ShortcutUser> {
    if let Some(other) = shortcut_conflicts_with_settings(id, shortcut, settings) {
        return Some(global_user(other));
    }
    accelerator_to_panel_keystroke(shortcut)
        .and_then(|keystroke| panel_action_using(&settings.shortcuts, &keystroke))
        .map(ShortcutUser::Panel)
}

/// `shortcuts.panel.<id>` 的补丁；`None` 删掉覆盖、恢复默认。
fn panel_patch(action: PanelAction, value: Option<String>) -> serde_json::Value {
    let value = value.map_or(serde_json::Value::Null, serde_json::Value::String);
    json!({ "shortcuts": { "panel": { action.id(): value } } })
}

fn panel_action_title(action: PanelAction) -> gpui::SharedString {
    i18n::t(&format!("clipboard:shortcuts.{}", action.id()))
}

/// 窗口内按键（GPUI 写法，如 `secondary-shift-b`）的显示文字，平台写法与全局快捷键一致。
fn panel_key_label(keystroke: &str) -> String {
    let mut parts = Vec::new();
    let mut rest = keystroke;
    loop {
        if let Some(next) = rest.strip_prefix("secondary-") {
            parts.push("CmdOrCtrl".to_owned());
            rest = next;
        } else if let Some(next) = rest.strip_prefix("shift-") {
            parts.push("Shift".to_owned());
            rest = next;
        } else {
            break;
        }
    }
    let key = match rest {
        "up" => "Up".to_owned(),
        "down" => "Down".to_owned(),
        "left" => "Left".to_owned(),
        "right" => "Right".to_owned(),
        "enter" => "Enter".to_owned(),
        "escape" => "Esc".to_owned(),
        "space" => "Space".to_owned(),
        "tab" => "Tab".to_owned(),
        "delete" => "Delete".to_owned(),
        "backspace" => "Backspace".to_owned(),
        other => other.to_uppercase(),
    };
    parts.push(key);
    text::format_shortcut(&parts.join("+"))
}

/// 录到的按键换成窗口内按键的写法；用了 Alt、Win（macOS 的 Control）这类面板收不到的修饰键时为
/// `None`。单按修饰键也是 `None`。
fn panel_keystroke_from(keystroke: &Keystroke) -> Option<String> {
    let modifiers = &keystroke.modifiers;
    let (secondary, foreign) = if cfg!(target_os = "macos") {
        (modifiers.platform, modifiers.control)
    } else {
        (modifiers.control, modifiers.platform)
    };
    if foreign || modifiers.alt || modifiers.function {
        return None;
    }
    let key = keystroke.key.to_ascii_lowercase();
    if key.is_empty() || is_modifier_key(&key) {
        return None;
    }
    let mut parts = Vec::new();
    if secondary {
        parts.push("secondary");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    parts.push(key.as_str());
    Some(parts.join("-"))
}

/// 单按修饰键时也会收到按下事件，录制时忽略。
fn is_modifier_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "shift" | "control" | "ctrl" | "alt" | "platform" | "cmd" | "function" | "fn"
    )
}

/// 窗口内按键表的分组，两列各两组。
const PANEL_GROUPS: [(&str, &[PanelAction]); 4] = [
    (
        "paste",
        &[
            PanelAction::PasteSelected,
            PanelAction::PasteSelectedPlain,
            PanelAction::PasteNth,
            PanelAction::CopySelected,
        ],
    ),
    (
        "organize",
        &[
            PanelAction::FavoriteSelected,
            PanelAction::PinSelected,
            PanelAction::NoteSelected,
            PanelAction::EditContent,
            PanelAction::SplitSelected,
            PanelAction::OpenSelected,
            PanelAction::DeleteSelected,
            PanelAction::SelectAll,
        ],
    ),
    (
        "navigate",
        &[
            PanelAction::Navigate,
            PanelAction::SwitchCategory,
            PanelAction::SwitchCustomGroup,
            PanelAction::FocusSearch,
            PanelAction::ToggleRange,
            PanelAction::CreateGroup,
        ],
    ),
    (
        "window",
        &[
            PanelAction::PreviewSelected,
            PanelAction::ViewImage,
            PanelAction::PinWindow,
            PanelAction::ShowShortcuts,
            PanelAction::OpenPreference,
            PanelAction::ClosePreviewFilterWindow,
        ],
    ),
];

/// 「粘贴前 10 项」可选的修饰键。
const PASTE_NTH_PREFIXES: [&str; 2] = ["secondary", "secondary-shift"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChipLook {
    Normal,
    Unset,
    Modified,
    Fixed,
    Recording,
    Danger,
}

/// 按键标签：小号键帽样式，不同状态换底色和字色。
fn chip(
    label: impl Into<gpui::SharedString>,
    look: ChipLook,
    tokens: &SemanticTokens,
) -> gpui::Div {
    let (bg, border, color) = match look {
        ChipLook::Normal => (
            tokens.fill.default,
            tokens.border.default,
            tokens.text.primary,
        ),
        ChipLook::Unset | ChipLook::Fixed => (
            theme::transparent(),
            tokens.border.default,
            tokens.text.muted,
        ),
        ChipLook::Modified | ChipLook::Recording => (
            tokens.accent.subtle,
            tokens.accent.border,
            tokens.accent.text,
        ),
        ChipLook::Danger => (
            tokens.status.danger.subtle,
            tokens.status.danger.border,
            tokens.status.danger.solid,
        ),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(rems(1.5))
        .px(space(2.))
        .rounded(theme::radius::SM)
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(color)
        .kp_text(TextSize::Xs)
        .whitespace_nowrap()
        .child(label.into())
}

/// 下拉按钮的外观：当前值加向下箭头，点开是一列可选值。
fn dropdown_face(label: gpui::SharedString, tokens: &SemanticTokens) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(space(1.5))
        .h(rems(1.75))
        .pl(space(2.5))
        .pr(space(2.))
        .rounded(theme::radius::MD)
        .border_1()
        .border_color(tokens.border.default)
        .bg(tokens.surface.panel)
        .kp_text(TextSize::Sm)
        .cursor_pointer()
        .hover(|style| style.bg(tokens.fill.subtle))
        .child(label)
        .child(
            Icon::new(IconName::ChevronDown)
                .size(rems(0.875))
                .color(tokens.text.muted),
        )
}

impl Preferences {
    pub(super) fn open_shortcut_detail(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.shortcut_detail = Some(id);
        self.shortcut_conflict = None;
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    pub(super) fn close_shortcut_detail(&mut self, cx: &mut Context<Self>) {
        if self.recording.is_some() {
            self.finish_recording(cx);
        }
        self.shortcut_detail = None;
        self.shortcut_conflict = None;
        cx.notify();
    }

    /// 一个下拉：选中项打勾，选别的值写回 `path`。
    fn choice_dropdown(
        &self,
        id: &'static str,
        path: &'static str,
        current: &str,
        options: Vec<(&'static str, gpui::SharedString)>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let label = options
            .iter()
            .find(|(value, _)| *value == current)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let current = current.to_owned();
        let entity = cx.entity().downgrade();
        MenuTrigger::new(id)
            .child(dropdown_face(label, tokens))
            .dropdown(move |_, _| {
                options
                    .iter()
                    .map(|(value, label)| {
                        let entity = entity.clone();
                        let value = *value;
                        MenuEntry::from(
                            MenuItem::new(label.clone(), move |_, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    this.update(path, json!(value), cx);
                                });
                            })
                            .checked(value == current),
                        )
                    })
                    .collect()
            })
    }

    fn trigger_present(&self, id: &'static str, trigger: Trigger) -> bool {
        let shortcuts = &self.settings.shortcuts;
        match trigger {
            Trigger::Combo => {
                !combo_value(&self.settings, id).trim().is_empty() || self.recording == Some(id)
            }
            Trigger::DoubleTap => shortcuts.double_tap.key != DoubleTapKey::Disabled,
            Trigger::Mouse => shortcuts.mouse_trigger != MouseTrigger::Disabled,
            Trigger::WinV => shortcuts.win_v,
        }
    }

    fn add_trigger(
        &mut self,
        id: &'static str,
        trigger: Trigger,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match trigger {
            Trigger::Combo => self.begin_recording(id, window, cx),
            Trigger::DoubleTap => self.update("shortcuts.doubleTap.key", json!("control"), cx),
            Trigger::Mouse => self.update("shortcuts.mouseTrigger", json!("middle"), cx),
            Trigger::WinV => self.update("shortcuts.winV", json!(true), cx),
        }
    }

    fn remove_trigger(&mut self, id: &'static str, trigger: Trigger, cx: &mut Context<Self>) {
        match trigger {
            Trigger::Combo => {
                if let Some(path) = shortcut_path(id) {
                    self.update(path, json!(""), cx);
                }
            }
            Trigger::DoubleTap => self.update("shortcuts.doubleTap.key", json!("disabled"), cx),
            Trigger::Mouse => self.update("shortcuts.mouseTrigger", json!("disabled"), cx),
            Trigger::WinV => self.update("shortcuts.winV", json!(false), cx),
        }
    }

    /// 主列表右侧的标签：已有的触发方式各一个，没有时显示「未设置」。
    fn entry_chips(&self, id: &'static str, cx: &App) -> Vec<gpui::AnyElement> {
        let tokens = theme::semantic(cx);
        let shortcuts = &self.settings.shortcuts;
        if id == "shortcuts.quickPaste" {
            let chip = if shortcuts.quick_paste.enabled {
                let mods = text::format_shortcut(&format!(
                    "{}+1",
                    shortcuts.quick_paste.modifiers.accelerator()
                ));
                let look = if shortcut_setting_problem(
                    "shortcuts.quickPasteModifiers",
                    &self.settings,
                    None,
                    cx,
                )
                .is_some()
                {
                    ChipLook::Danger
                } else {
                    ChipLook::Normal
                };
                chip(format!("{mods}…0"), look, tokens)
            } else {
                chip(words("chips.quickPasteOff"), ChipLook::Unset, tokens)
            };
            return vec![chip.into_any_element()];
        }
        let mut chips = Vec::new();
        for trigger in allowed_triggers(id) {
            if !self.trigger_present(id, *trigger) {
                continue;
            }
            let element = match trigger {
                Trigger::Combo => {
                    let value = combo_value(&self.settings, id);
                    let look = if shortcut_setting_problem(id, &self.settings, self.recording, cx)
                        .is_some()
                    {
                        ChipLook::Danger
                    } else {
                        ChipLook::Normal
                    };
                    chip(text::format_shortcut(value), look, tokens)
                }
                Trigger::DoubleTap => chip(
                    words_args(
                        "chips.doubleTap",
                        &[("key", &double_tap_key_label(shortcuts.double_tap.key))],
                    ),
                    ChipLook::Normal,
                    tokens,
                ),
                Trigger::Mouse => chip(
                    words_args(
                        "chips.mouse",
                        &[("button", &mouse_label(shortcuts.mouse_trigger))],
                    ),
                    ChipLook::Normal,
                    tokens,
                ),
                Trigger::WinV => chip("Win+V", ChipLook::Normal, tokens),
            };
            chips.push(element.into_any_element());
        }
        if chips.is_empty() {
            chips.push(
                chip(
                    i18n::t("preferences:controls.shortcutNotSet"),
                    ChipLook::Unset,
                    tokens,
                )
                .into_any_element(),
            );
        }
        chips
    }

    /// 主列表的一行：标题、说明、已有的触发方式和进入箭头，整行可点。
    pub(super) fn render_shortcut_entry(
        &self,
        setting: &Setting,
        first: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let id = setting.id;
        let title = text::setting_title(setting);
        let problem_id = if id == "shortcuts.quickPaste" {
            "shortcuts.quickPasteModifiers"
        } else {
            id
        };
        let problem = shortcut_setting_problem(problem_id, &self.settings, self.recording, cx);
        let row = row_frame(first, tokens)
            .child(row_label(
                title.clone(),
                text::setting_description(setting),
                tokens,
            ))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(space(1.5))
                    .children(self.entry_chips(id, cx))
                    .child(PrefIcon::ChevronRight.view(rems(1.), tokens.text.faint)),
            );
        div()
            .id(format!("shortcut-entry-{id}"))
            .role(Role::Button)
            .aria_label(title)
            .tab_index(0)
            .flex()
            .flex_col()
            .rounded(theme::radius::MD)
            .cursor_pointer()
            .hover(|style| style.bg(tokens.fill.faint))
            .focus_visible(|style| style.bg(tokens.fill.subtle))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.navigate_to(TabId::Shortcuts, None, window, cx);
                this.open_shortcut_detail(id, cx);
            }))
            .child(row)
            .when_some(problem, |entry, problem| {
                entry.child(
                    div()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.status.danger.solid)
                        .px(space(1.))
                        .pb(space(2.5))
                        .child(problem),
                )
            })
            .into_any_element()
    }

    /// 二级页：返回、标题，然后是触发方式（快速粘贴是开关和修饰键）。
    pub(super) fn render_shortcut_detail(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let setting_key = format!("preferences:schema.settings.{id}");
        let title = i18n::t(&format!("{setting_key}.title"));
        let mut lead = text::optional(&format!("{setting_key}.description")).to_string();
        if allowed_triggers(id).len() > 1 {
            lead = format!("{lead}{}", words("anyTrigger"));
        }
        let back = div()
            .id("shortcut-detail-back")
            .role(Role::Button)
            .aria_label(text::tab_title(TabId::Shortcuts))
            .tab_index(0)
            .flex()
            .items_center()
            .gap(space(1.))
            .self_start()
            .px(space(1.5))
            .py(space(1.))
            .rounded(theme::radius::MD)
            .kp_text(TextSize::Sm)
            .text_color(tokens.text.secondary)
            .cursor_pointer()
            .hover(|style| style.bg(tokens.fill.subtle).text_color(tokens.text.primary))
            .focus_visible(|style| style.bg(tokens.fill.subtle))
            .child(PrefIcon::ArrowLeft.view(rems(1.), tokens.text.secondary))
            .child(text::tab_title(TabId::Shortcuts))
            .on_click(cx.listener(|this, _, _, cx| this.close_shortcut_detail(cx)));
        let head = div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .child(
                div()
                    .kp_text(TextSize::Lg)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when(!lead.is_empty(), |head| {
                head.child(
                    div()
                        .kp_text(TextSize::Sm)
                        .text_color(tokens.text.muted)
                        .child(lead),
                )
            });
        let body = if id == "shortcuts.quickPaste" {
            self.render_quick_paste_detail(cx)
        } else {
            self.render_trigger_list(id, cx)
        };
        div()
            .flex()
            .flex_col()
            .gap(space(4.))
            .child(back)
            .child(head)
            .child(body)
            .into_any_element()
    }

    fn render_trigger_list(&self, id: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let allowed = allowed_triggers(id);
        let mut rows = Vec::new();
        for trigger in allowed {
            if self.trigger_present(id, *trigger) {
                rows.push(self.render_trigger_row(id, *trigger, rows.is_empty(), cx));
            }
        }
        if rows.is_empty() {
            rows.push(note_row(
                true,
                i18n::t("preferences:controls.shortcutNotSet"),
                tokens.text.muted,
                tokens,
            ));
        }
        let missing: Vec<Trigger> = allowed
            .iter()
            .copied()
            .filter(|trigger| !self.trigger_present(id, *trigger))
            .collect();
        let add = (!missing.is_empty()).then(|| {
            let button = Button::new("shortcut-add-trigger-button", words("addTrigger"))
                .with_icon(IconName::Plus)
                .small();
            if allowed.len() == 1 {
                return button
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.add_trigger(id, Trigger::Combo, window, cx);
                    }))
                    .into_any_element();
            }
            let entity = cx.entity().downgrade();
            MenuTrigger::new("shortcut-add-trigger")
                .flex_none()
                .child(button)
                .dropdown(move |_, _| {
                    allowed
                        .iter()
                        .map(|trigger| {
                            let entity = entity.clone();
                            let trigger = *trigger;
                            let added = !missing.contains(&trigger);
                            let label = words(&format!("kinds.{}.title", trigger.key()));
                            MenuEntry::from(
                                MenuItem::new(label, move |window, cx| {
                                    let _ = entity.update(cx, |this, cx| {
                                        this.add_trigger(id, trigger, window, cx);
                                    });
                                })
                                .checked(added)
                                .disabled(added),
                            )
                        })
                        .collect()
                })
        });
        div()
            .flex()
            .flex_col()
            .gap(space(3.))
            .child(section_block(Some(words("triggers")), None, rows, tokens))
            .children(add.map(|add| div().flex().child(add)))
            .into_any_element()
    }

    fn render_trigger_row(
        &self,
        id: &'static str,
        trigger: Trigger,
        first: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let shortcuts = &self.settings.shortcuts;
        let recording = trigger == Trigger::Combo && self.recording == Some(id);
        let icon = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(rems(2.))
            .rounded(theme::radius::MD)
            .bg(tokens.accent.subtle)
            .child(trigger.icon().view(rems(1.125), tokens.accent.text));
        let remove = Button::icon(
            format!("shortcut-remove-{}", trigger.key()),
            IconName::Close,
            words("remove"),
        )
        .ghost()
        .small()
        .on_click(cx.listener(move |this, _, _, cx| this.remove_trigger(id, trigger, cx)));
        let mut controls: Vec<gpui::AnyElement> = Vec::new();
        let mut notes: Vec<gpui::AnyElement> = Vec::new();
        match trigger {
            Trigger::Combo if recording => {
                controls
                    .push(chip(words("recording"), ChipLook::Recording, tokens).into_any_element());
                controls.push(
                    Button::new("shortcut-record-cancel", i18n::t("common:actions.cancel"))
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.finish_recording(cx)))
                        .into_any_element(),
                );
                notes.push(
                    div()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.muted)
                        .child(words("recordingHelp"))
                        .into_any_element(),
                );
            }
            Trigger::Combo => {
                let value = combo_value(&self.settings, id);
                let problem = shortcut_recorder_problem(id, value, &self.settings, false, cx);
                let look = if problem.is_some() {
                    ChipLook::Danger
                } else {
                    ChipLook::Normal
                };
                controls.push(chip(text::format_shortcut(value), look, tokens).into_any_element());
                controls.push(
                    Button::new("shortcut-record-again", words("rerecord"))
                        .small()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.begin_recording(id, window, cx);
                        }))
                        .into_any_element(),
                );
                controls.push(remove.into_any_element());
                if let Some(problem) = problem {
                    notes.push(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.status.danger.solid)
                            .child(problem)
                            .into_any_element(),
                    );
                }
            }
            Trigger::DoubleTap => {
                controls.push(self.choice_dropdown(
                    "shortcut-double-tap-key",
                    "shortcuts.doubleTap.key",
                    double_tap_key_value(shortcuts.double_tap.key),
                    double_tap_key_options(),
                    cx,
                ));
                controls.push(self.choice_dropdown(
                    "shortcut-double-tap-side",
                    "shortcuts.doubleTap.side",
                    key_side_value(shortcuts.double_tap.side),
                    ["any", "left", "right"]
                        .into_iter()
                        .map(|side| {
                            (
                                side,
                                i18n::t(&format!(
                                    "preferences:schema.settings.shortcuts.doubleTapSide.options.{side}"
                                )),
                            )
                        })
                        .collect(),
                    cx,
                ));
                controls.push(remove.into_any_element());
            }
            Trigger::Mouse => {
                controls.push(self.choice_dropdown(
                    "shortcut-mouse-button",
                    "shortcuts.mouseTrigger",
                    mouse_value(shortcuts.mouse_trigger),
                    ["middle", "back", "forward"]
                        .into_iter()
                        .map(|button| {
                            (
                                button,
                                i18n::t(&format!(
                                    "preferences:schema.settings.shortcuts.mouseTrigger.options.{button}"
                                )),
                            )
                        })
                        .collect(),
                    cx,
                ));
                controls.push(remove.into_any_element());
            }
            Trigger::WinV => controls.push(remove.into_any_element()),
        }
        if let Some(conflict) = self.shortcut_conflict.as_ref().filter(|conflict| {
            trigger == Trigger::Combo && conflict.target == ShortcutTarget::Global(id)
        }) {
            notes.push(self.render_conflict(conflict, cx));
        }
        let row = row_frame(first, tokens)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(space(3.))
                    .child(icon)
                    .child(row_label(
                        words(&format!("kinds.{}.title", trigger.key())),
                        words(&format!("kinds.{}.description", trigger.key())),
                        tokens,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(space(2.))
                    .children(controls),
            );
        div()
            .flex()
            .flex_col()
            .child(row)
            .when(!notes.is_empty(), |block| {
                block.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space(2.))
                        .pl(space(12.))
                        .pr(space(1.))
                        .pb(space(2.5))
                        .children(notes),
                )
            })
            .into_any_element()
    }

    /// 冲突提示：说明被谁占用；对方能让出时给「改用到这里」。
    fn render_conflict(
        &self,
        conflict: &ShortcutConflict,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let key = match conflict.target {
            ShortcutTarget::Global(_) => text::format_shortcut(&conflict.value),
            ShortcutTarget::Panel(PanelAction::PasteNth) => {
                format!("{}…0", panel_key_label(&format!("{}-1", conflict.value)))
            }
            ShortcutTarget::Panel(_) => panel_key_label(&conflict.value),
        };
        let name = conflict.user.name();
        let message = if matches!(conflict.user, ShortcutUser::Panel(_)) {
            words_args("conflictPanel", &[("key", &key), ("name", &name)])
        } else {
            words_args("conflict", &[("key", &key), ("name", &name)])
        };
        let can_take_over = conflict.user.release_patch().is_some();
        div()
            .flex()
            .items_center()
            .gap(space(2.5))
            .px(space(3.))
            .py(space(2.))
            .rounded(theme::radius::MD)
            .border_1()
            .border_color(tokens.status.danger.border)
            .bg(tokens.status.danger.subtle)
            .child(PrefIcon::Alert.view(rems(1.), tokens.status.danger.solid))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .kp_text(TextSize::Sm)
                    .child(message),
            )
            .child(
                Button::new("shortcut-conflict-cancel", i18n::t("common:actions.cancel"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shortcut_conflict = None;
                        cx.notify();
                    })),
            )
            .when(can_take_over, |bar| {
                bar.child(
                    Button::new("shortcut-conflict-take-over", words("takeOver"))
                        .small()
                        .primary()
                        .on_click(cx.listener(|this, _, _, cx| this.take_over_conflict(cx))),
                )
            })
            .into_any_element()
    }

    /// 录制时撞车：停止录制，留下提示等用户选择。
    pub(super) fn hold_conflict(
        &mut self,
        target: ShortcutTarget,
        value: String,
        user: ShortcutUser,
        cx: &mut Context<Self>,
    ) {
        self.shortcut_conflict = Some(ShortcutConflict {
            target,
            value,
            user,
        });
        match target {
            ShortcutTarget::Global(_) => self.finish_recording(cx),
            ShortcutTarget::Panel(_) => self.finish_panel_recording(cx),
        }
    }

    /// 对方让出按键、这里用上，一个补丁写完，校验不会看到中间态。
    fn take_over_conflict(&mut self, cx: &mut Context<Self>) {
        let Some(conflict) = self.shortcut_conflict.take() else {
            return;
        };
        let Some(release) = conflict.user.release_patch() else {
            cx.notify();
            return;
        };
        let assign = match conflict.target {
            ShortcutTarget::Global(id) => match shortcut_path(id) {
                Some(path) => values::patch(path, json!(conflict.value)),
                None => return,
            },
            ShortcutTarget::Panel(action) => {
                panel_patch(action, panel_override(action, conflict.value))
            }
        };
        self.apply_patch(values::merge(release, assign), cx);
    }

    fn render_quick_paste_detail(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let quick_paste = self.settings.shortcuts.quick_paste;
        let entity = cx.entity().downgrade();
        let enable = setting_row(
            true,
            words("quickPaste.enable"),
            gpui::SharedString::default(),
            Some(
                Switch::new("shortcut-quick-paste-enabled")
                    .accessibility_label(words("quickPaste.enable"))
                    .checked(quick_paste.enabled)
                    .on_change(move |checked, _, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.update("shortcuts.quickPaste.enabled", json!(checked), cx);
                        });
                    })
                    .into_any_element(),
            ),
            tokens,
        );
        let mut rows = vec![enable];
        if quick_paste.enabled {
            let segments = schema::QUICK_PASTE_MODIFIERS
                .iter()
                .map(|(value, accelerator)| {
                    let value = *value;
                    let selected = modifiers_value(quick_paste.modifiers) == value;
                    segment(
                        match value {
                            "controlShift" => "quick-paste-mod-control-shift",
                            "controlAlt" => "quick-paste-mod-control-alt",
                            "altShift" => "quick-paste-mod-alt-shift",
                            "alt" => "quick-paste-mod-alt",
                            _ => "quick-paste-mod-control",
                        },
                        text::format_shortcut(accelerator).into(),
                        selected,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update("shortcuts.quickPaste.modifiers", json!(value), cx);
                    }))
                });
            let problem =
                shortcut_setting_problem("shortcuts.quickPasteModifiers", &self.settings, None, cx);
            rows.push(
                row_frame(false, tokens)
                    .flex_col()
                    .items_start()
                    .gap(space(3.))
                    .child(row_label(
                        i18n::t("preferences:schema.settings.shortcuts.quickPasteModifiers.title"),
                        i18n::t(
                            "preferences:schema.settings.shortcuts.quickPasteModifiers.description",
                        ),
                        tokens,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(space(1.))
                            .p(space(0.75))
                            .rounded(theme::radius::MD)
                            .bg(tokens.fill.default)
                            .children(segments),
                    )
                    .when_some(problem, |row, problem| {
                        row.child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.status.danger.solid)
                                .child(problem),
                        )
                    })
                    .into_any_element(),
            );
            let mods = text::format_shortcut(quick_paste.modifiers.accelerator());
            let caps = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"]
                .into_iter()
                .enumerate()
                .map(|(index, digit)| {
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap(space(1.))
                        .child(
                            chip(digit, ChipLook::Normal, tokens)
                                .w_full()
                                .justify_center()
                                .h(rems(1.875))
                                .font_weight(gpui::FontWeight::SEMIBOLD),
                        )
                        .child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.muted)
                                .whitespace_nowrap()
                                .child(words_args(
                                    "quickPaste.nth",
                                    &[("n", &(index + 1).to_string())],
                                )),
                        )
                });
            rows.push(
                row_frame(false, tokens)
                    .flex_col()
                    .items_start()
                    .gap(space(3.))
                    .child(
                        div()
                            .kp_text(TextSize::Sm)
                            .text_color(tokens.text.secondary)
                            .child(words_args("quickPaste.hold", &[("mods", &mods)])),
                    )
                    .child(div().flex().w_full().gap(space(1.5)).children(caps))
                    .into_any_element(),
            );
        }
        section_block(None, None, rows, tokens)
    }

    pub(super) fn begin_panel_recording(
        &mut self,
        action: PanelAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.recording.is_some() {
            self.finish_recording(cx);
        }
        self.recording_panel = Some(action);
        self.shortcut_conflict = None;
        hotkey::suspend(cx.entity_id(), cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn finish_panel_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording_panel.take().is_some() {
            hotkey::resume(cx.entity_id(), cx);
        }
        cx.notify();
    }

    /// 录制窗口内按键：Esc 取消；不能用的组合提示后继续录；撞车时留下提示。
    pub(super) fn capture_panel_shortcut(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(action) = self.recording_panel else {
            return;
        };
        if event.keystroke.key.eq_ignore_ascii_case("escape")
            && !event.keystroke.modifiers.modified()
        {
            self.finish_panel_recording(cx);
            return;
        }
        if is_modifier_key(&event.keystroke.key) {
            return;
        }
        let Some(keystroke) = panel_keystroke_from(&event.keystroke) else {
            self.warn_invalid_panel_key(window, cx);
            return;
        };
        if !panel_keystroke_allowed(&keystroke) {
            self.warn_invalid_panel_key(window, cx);
            return;
        }
        if let Some(owner) = panel_key_conflict(&self.settings.shortcuts, action, &keystroke) {
            self.hold_conflict(ShortcutTarget::Panel(action), keystroke, user_of(owner), cx);
            return;
        }
        let value = panel_override(action, keystroke);
        self.finish_panel_recording(cx);
        self.apply_patch(panel_patch(action, value), cx);
    }

    fn warn_invalid_panel_key(&self, window: &mut Window, cx: &mut Context<Self>) {
        let modifier = text::format_shortcut("CmdOrCtrl");
        toast::show(
            Toast::warning(words_args("panel.invalid", &[("mod", &modifier)])),
            window,
            cx,
        );
    }

    /// 剪贴板窗口内的按键表：两列四组，可改的按键点开录制，改过的可恢复默认，固定键只显示。
    pub(super) fn render_panel_shortcuts(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let columns = [&PANEL_GROUPS[..2], &PANEL_GROUPS[2..]].map(|groups| {
            let mut items: Vec<gpui::AnyElement> = Vec::new();
            for (group, actions) in groups {
                items.push(
                    div()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.muted)
                        .pt(space(3.))
                        .pb(space(1.))
                        .child(words(&format!("panel.groups.{group}")))
                        .into_any_element(),
                );
                for (index, action) in actions.iter().enumerate() {
                    items.push(self.render_panel_item(*action, index == 0, cx));
                }
            }
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(rems(16.))
                .children(items)
        });
        let conflict = self
            .shortcut_conflict
            .as_ref()
            .filter(|conflict| matches!(conflict.target, ShortcutTarget::Panel(_)))
            .map(|conflict| self.render_conflict(conflict, cx));
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(space(2.))
            .pb(space(2.))
            .child(
                div()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.muted)
                    .pt(space(2.))
                    .px(space(1.))
                    .child(words("panel.hint")),
            )
            .children(conflict)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(space(7.))
                    .px(space(1.))
                    .children(columns),
            )
            .child(
                div()
                    .id("shortcut-card-click-link")
                    .role(Role::Link)
                    .tab_index(0)
                    .self_start()
                    .mx(space(1.))
                    .mt(space(1.))
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.link)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(words_args(
                        "panel.cardClick",
                        &[("tab", &text::tab_title(TabId::Paste))],
                    ))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.navigate_to(TabId::Paste, Some("paste.autoPaste"), window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_panel_item(
        &self,
        action: PanelAction,
        first: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let keys = effective_panel_keys(&self.settings.shortcuts, action);
        let modified = self.settings.shortcuts.panel.contains_key(action.id());
        let control: gpui::AnyElement = if !action.editable() {
            let label = keys
                .iter()
                .map(|key| panel_key_label(key))
                .collect::<Vec<_>>()
                .join(" / ");
            chip(label, ChipLook::Fixed, tokens)
                .id(format!("panel-key-{}", action.id()))
                .kp_tooltip(words("panel.fixed"))
                .into_any_element()
        } else if self.recording_panel == Some(action) {
            chip(words("panel.recording"), ChipLook::Recording, tokens).into_any_element()
        } else if action == PanelAction::PasteNth {
            let current = self
                .settings
                .shortcuts
                .panel
                .get(action.id())
                .map_or("secondary", String::as_str);
            let options = PASTE_NTH_PREFIXES
                .into_iter()
                .map(|prefix| {
                    (
                        prefix,
                        format!("{}…0", panel_key_label(&format!("{prefix}-1"))).into(),
                    )
                })
                .collect();
            self.panel_prefix_dropdown(current, options, cx)
        } else {
            let label = keys.first().map_or_else(
                || i18n::t("preferences:controls.shortcutNotSet").to_string(),
                |key| panel_key_label(key),
            );
            let look = if keys.is_empty() {
                ChipLook::Unset
            } else if modified {
                ChipLook::Modified
            } else {
                ChipLook::Normal
            };
            chip(label, look, tokens)
                .id(format!("panel-key-{}", action.id()))
                .role(Role::Button)
                .aria_label(panel_action_title(action))
                .tab_index(0)
                .cursor_pointer()
                .hover(|style| style.border_color(tokens.accent.border))
                .focus_visible(|style| style.border_color(tokens.accent.border))
                .kp_tooltip(i18n::t("preferences:controls.recordShortcut"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.begin_panel_recording(action, window, cx);
                }))
                .into_any_element()
        };
        let reset = (modified && action.editable()).then(|| {
            Button::icon(
                format!("panel-key-reset-{}", action.id()),
                IconName::Close,
                words("panel.reset"),
            )
            .ghost()
            .xsmall()
            .tooltip(words("panel.reset"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.apply_patch(panel_patch(action, None), cx);
            }))
        });
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(space(3.))
            .min_h(rems(2.25))
            .when(!first, |item| {
                item.border_t_1().border_color(tokens.border.divider)
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .kp_text(TextSize::Sm)
                    .text_color(tokens.text.secondary)
                    .truncate()
                    .child(panel_action_title(action)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(space(1.))
                    .children(reset)
                    .child(control),
            )
            .into_any_element()
    }

    /// 「粘贴前 10 项」的修饰键下拉，选默认值时删掉覆盖。
    fn panel_prefix_dropdown(
        &self,
        current: &str,
        options: Vec<(&'static str, gpui::SharedString)>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let label = options
            .iter()
            .find(|(value, _)| *value == current)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let current = current.to_owned();
        let entity = cx.entity().downgrade();
        MenuTrigger::new("panel-key-pasteNth")
            .child(
                dropdown_face(label, tokens)
                    .h(rems(1.5))
                    .kp_text(TextSize::Xs),
            )
            .dropdown(move |_, _| {
                options
                    .iter()
                    .map(|(value, label)| {
                        let entity = entity.clone();
                        let value = *value;
                        MenuEntry::from(
                            MenuItem::new(label.clone(), move |_, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    let action = PanelAction::PasteNth;
                                    // 新前缀的数字组合可能已被别的按键占用，先查再写，免得校验拒绝。
                                    if let Some(owner) =
                                        panel_key_conflict(&this.settings.shortcuts, action, value)
                                    {
                                        this.hold_conflict(
                                            ShortcutTarget::Panel(action),
                                            value.to_owned(),
                                            user_of(owner),
                                            cx,
                                        );
                                        return;
                                    }
                                    let value =
                                        (value != PASTE_NTH_PREFIXES[0]).then(|| value.to_owned());
                                    this.apply_patch(panel_patch(action, value), cx);
                                });
                            })
                            .checked(value == current),
                        )
                    })
                    .collect()
            })
    }

    /// 「全部恢复默认」：分组标题右侧，只在有改动时显示。
    pub(super) fn render_panel_reset_all(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.settings.shortcuts.panel.is_empty() {
            return None;
        }
        Some(
            Button::new("panel-keys-reset-all", words("panel.resetAll"))
                .link()
                .xsmall()
                .on_click(cx.listener(|this, _, _, cx| {
                    let reset: serde_json::Map<String, serde_json::Value> = this
                        .settings
                        .shortcuts
                        .panel
                        .keys()
                        .map(|id| (id.clone(), serde_json::Value::Null))
                        .collect();
                    this.shortcut_conflict = None;
                    this.apply_patch(json!({ "shortcuts": { "panel": reset } }), cx);
                }))
                .into_any_element(),
        )
    }
}

/// 录到的值等于默认键时删掉覆盖，否则存下来。
fn panel_override(action: PanelAction, value: String) -> Option<String> {
    let defaults = effective_panel_keys(&kwikpaste_core::settings::Shortcuts::default(), action);
    let is_default = if action == PanelAction::PasteNth {
        value == PASTE_NTH_PREFIXES[0]
    } else {
        defaults.len() == 1 && defaults.first() == Some(&value)
    };
    (!is_default).then_some(value)
}

fn double_tap_key_value(key: DoubleTapKey) -> &'static str {
    match key {
        DoubleTapKey::Disabled => "disabled",
        DoubleTapKey::Control => "control",
        DoubleTapKey::Shift => "shift",
        DoubleTapKey::Alt => "alt",
        DoubleTapKey::Command => "command",
    }
}

fn double_tap_key_label(key: DoubleTapKey) -> gpui::SharedString {
    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    };
    i18n::t(&format!(
        "preferences:schema.settings.shortcuts.doubleTap.options.{platform}.{}",
        double_tap_key_value(key)
    ))
}

fn double_tap_key_options() -> Vec<(&'static str, gpui::SharedString)> {
    let keys: &[DoubleTapKey] = if cfg!(target_os = "macos") {
        &[
            DoubleTapKey::Control,
            DoubleTapKey::Shift,
            DoubleTapKey::Alt,
            DoubleTapKey::Command,
        ]
    } else {
        &[
            DoubleTapKey::Control,
            DoubleTapKey::Shift,
            DoubleTapKey::Alt,
        ]
    };
    keys.iter()
        .map(|key| (double_tap_key_value(*key), double_tap_key_label(*key)))
        .collect()
}

fn key_side_value(side: KeySide) -> &'static str {
    match side {
        KeySide::Any => "any",
        KeySide::Left => "left",
        KeySide::Right => "right",
    }
}

fn mouse_value(button: MouseTrigger) -> &'static str {
    match button {
        MouseTrigger::Disabled => "disabled",
        MouseTrigger::Middle => "middle",
        MouseTrigger::Back => "back",
        MouseTrigger::Forward => "forward",
    }
}

fn mouse_label(button: MouseTrigger) -> gpui::SharedString {
    i18n::t(&format!(
        "preferences:schema.settings.shortcuts.mouseTrigger.options.{}",
        mouse_value(button)
    ))
}

fn modifiers_value(modifiers: QuickPasteModifiers) -> &'static str {
    match modifiers {
        QuickPasteModifiers::ControlShift => "controlShift",
        QuickPasteModifiers::ControlAlt => "controlAlt",
        QuickPasteModifiers::AltShift => "altShift",
        QuickPasteModifiers::Alt => "alt",
        QuickPasteModifiers::Control => "control",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_key_labels_follow_the_platform() {
        if cfg!(target_os = "macos") {
            assert_eq!(panel_key_label("secondary-shift-b"), "⌘⇧B");
        } else {
            assert_eq!(panel_key_label("secondary-shift-b"), "Ctrl+Shift+B");
            assert_eq!(panel_key_label("delete"), "Del");
        }
    }
}
