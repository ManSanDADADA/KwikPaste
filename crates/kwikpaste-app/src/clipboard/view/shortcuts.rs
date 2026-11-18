//! 主面板快捷键说明。

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
    rems,
};
use kwikpaste_ui::theme::{TextSize, space};
use kwikpaste_ui::{DialogSpec, KpStyled as _, Shortcut, form_dialog, theme};

use super::panel::ClipboardPanel;
use crate::{clipboard::model::shortcut, core_host, i18n::t};
use kwikpaste_core::settings::{PanelAction, Settings, effective_panel_keys};

/// 说明里的分段（小标题 key）与各段的显示顺序。
const SECTIONS: [(&str, &[PanelAction]); 3] = [
    (
        "clipboard:shortcuts.sections.item",
        &[
            PanelAction::PasteSelected,
            PanelAction::PasteSelectedPlain,
            PanelAction::PasteNth,
            PanelAction::PreviewSelected,
            PanelAction::ViewImage,
            PanelAction::CopySelected,
            PanelAction::SplitSelected,
            PanelAction::TranslateSelected,
            PanelAction::OpenSelected,
            PanelAction::NoteSelected,
            PanelAction::EditContent,
            PanelAction::FavoriteSelected,
            PanelAction::PinSelected,
            PanelAction::DeleteSelected,
        ],
    ),
    (
        "clipboard:shortcuts.sections.navigation",
        &[
            PanelAction::SelectAll,
            PanelAction::Navigate,
            PanelAction::FocusSearch,
            PanelAction::ToggleRange,
            PanelAction::SwitchCategory,
            PanelAction::SwitchCustomGroup,
            PanelAction::CreateGroup,
        ],
    ),
    (
        "clipboard:shortcuts.sections.window",
        &[
            PanelAction::PinWindow,
            PanelAction::ShowShortcuts,
            PanelAction::OpenPreference,
            PanelAction::ClosePreviewFilterWindow,
        ],
    ),
];

/// 一个按键（GPUI 写法，如 `secondary-shift-b`）拆成键帽：修饰键各一个，最后是主键。
fn keycaps(keystroke: &str) -> Vec<String> {
    let mut caps = Vec::new();
    let mut rest = keystroke;
    loop {
        if let Some(next) = rest.strip_prefix("secondary-") {
            caps.push(shortcut::display("CmdOrCtrl"));
            rest = next;
        } else if let Some(next) = rest.strip_prefix("shift-") {
            caps.push(shortcut::display("Shift"));
            rest = next;
        } else {
            break;
        }
    }
    let key = match rest {
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "enter" => "Enter",
        "escape" => "Escape",
        "space" => "Space",
        "tab" => "Tab",
        "delete" => "Delete",
        "backspace" => "Backspace",
        other => other,
    };
    caps.push(shortcut::display(key));
    caps
}

/// 动作当前生效的按键：可改的只列第一个（删除默认有三个组合），固定的方向键、Tab 用「/」并列。
/// 「粘贴前 10 项」显示成修饰键加 `1…0`。
fn keys_of(settings: &Settings, action: PanelAction) -> Vec<String> {
    let keys = effective_panel_keys(&settings.shortcuts, action);
    if action == PanelAction::PasteNth {
        let Some(first) = keys.first() else {
            return Vec::new();
        };
        let mut caps = keycaps(first);
        caps.pop();
        caps.push("1…0".to_owned());
        return caps;
    }
    if action.editable() {
        return keys.first().map(|key| keycaps(key)).unwrap_or_default();
    }
    let mut caps = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            caps.push("/".to_owned());
        }
        caps.extend(keycaps(key));
    }
    caps
}

/// 弹框标题、按钮和上下内边距大约占的高度：列表最高为窗口高减去这些，放不下时在框内滚动。
const DIALOG_CHROME: f32 = 200.;

/// 在当前面板窗口打开快捷键列表。只读说明，只有一个确定按钮。
pub fn show(window: &mut Window, cx: &mut Context<ClipboardPanel>) {
    drop(form_dialog(
        DialogSpec::new(t("clipboard:shortcuts.title"))
            .ok_text(t("common:ui.ok"))
            .without_cancel(),
        content,
        window,
        cx,
    ));
}

fn content(window: &mut Window, cx: &mut App) -> AnyElement {
    let tokens = theme::semantic(cx);
    let settings = core_host::core(cx).map_or_else(Settings::default, |core| core.settings());
    let translate = core_host::core(cx).is_some_and(|core| core.translate_enabled());
    let max_height = (window.viewport_size().height - px(DIALOG_CHROME))
        .max(px(160.))
        .min(rems(30.).to_pixels(window.rem_size()));

    div()
        .id("clipboard-shortcuts")
        .flex()
        .flex_col()
        .max_h(max_height)
        .overflow_y_scroll()
        .children(
            SECTIONS
                .iter()
                .enumerate()
                .flat_map(|(section, (title, actions))| {
                    let heading = div()
                        .when(section > 0, |heading| heading.mt(space(3.)))
                        .pb(space(1.))
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.muted)
                        .child(t(title));
                    let listed = actions.iter().filter(move |action| {
                        translate || **action != PanelAction::TranslateSelected
                    });
                    let rows = listed.enumerate().map(|(index, action)| {
                        // 用户改过的按键照设置显示，键名换成平台写法（Windows `Ctrl`、macOS `⌘`），行间细分隔线。
                        let keys = keys_of(&settings, *action);
                        let label = format!("clipboard:shortcuts.{}", action.id());
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space(4.))
                            .min_h(rems(2.25))
                            .py(space(1.))
                            .when(index > 0, |row| {
                                row.border_t_1().border_color(tokens.border.divider)
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(tokens.text.primary)
                                    .kp_text(TextSize::Sm)
                                    .child(t(&label)),
                            )
                            .when(keys.is_empty(), |row| {
                                row.child(
                                    div()
                                        .kp_text(TextSize::Xs)
                                        .text_color(tokens.text.muted)
                                        .child(t("preferences:controls.shortcutNotSet")),
                                )
                            })
                            .when(!keys.is_empty(), |row| row.child(Shortcut::new(keys)))
                    });
                    std::iter::once(heading.into_any_element())
                        .chain(rows.map(|row| row.into_any_element()))
                        .collect::<Vec<_>>()
                }),
        )
        .into_any_element()
}
