//! 列表里带回调的小部件：卡片的悬停快捷动作与复选框、页脚、多选栏、空态。

use std::sync::Arc;

use kwikpaste_core::settings::PanelAction;

use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _, radians,
};
use kwikpaste_ui::{
    Button, Checkbox, Glyph, Icon, IconButtonLook as Look, IconName, KpStyled as _,
    TooltipExt as _, icon_button,
    theme::{self, TextSize, radius, semantic::SemanticTokens, space},
};

use super::ClipboardList;
use crate::{
    clipboard::model::{
        actions::{QuickAction, is_copy},
        empty_state::{category_key, empty_text},
        item::ListItem,
        shortcut,
    },
    i18n::{t, t_args, t_count},
    platform::window_drag::WindowDragArea as _,
};

/// 快捷动作的常规图标和文案（未激活态），卡片按钮和偏好设置里的快捷动作管理共用。
pub(crate) fn quick_action_glyph(action: QuickAction) -> (IconName, SharedString) {
    let (icon, key) = match action {
        QuickAction::Paste => (IconName::ClipboardPaste, "paste"),
        QuickAction::PastePlain => (IconName::ClipboardType, "pastePlain"),
        QuickAction::PastePath => (IconName::FileSymlink, "pastePath"),
        QuickAction::Copy => (IconName::Copy, "copy"),
        QuickAction::CopyPlain => (IconName::CopyCheck, "copyPlain"),
        QuickAction::SplitWords => (IconName::TextSelect, "splitWords"),
        QuickAction::OpenLink => (IconName::SquareArrowOutUpRight, "openLink"),
        QuickAction::SendEmail => (IconName::Mail, "sendEmail"),
        QuickAction::Reveal => (IconName::FolderOpen, "reveal"),
        QuickAction::Note => (IconName::NotebookPen, "note"),
        QuickAction::Star => (IconName::Star, "star"),
        QuickAction::PinItem => (IconName::PushPin, "pinItem"),
        QuickAction::Delete => (IconName::Trash, "delete"),
    };
    (icon, t(&format!("clipboard:quickActions.{key}")))
}

/// 一个快捷动作按钮的图标、文案和颜色（1.x `resolveItemActionPresentation`）。
fn presentation(
    tokens: &SemanticTokens,
    action: QuickAction,
    item: &ListItem,
    copied: bool,
) -> (IconName, SharedString, gpui::Hsla) {
    let color = match action {
        QuickAction::Copy | QuickAction::CopyPlain if copied => {
            return (
                IconName::CircleCheck,
                t("clipboard:quickActions.copySuccess"),
                tokens.status.success.solid,
            );
        }
        QuickAction::Star if item.is_favorite => {
            return (
                IconName::Star,
                t("clipboard:quickActions.starActive"),
                tokens.status.warning.solid,
            );
        }
        QuickAction::PinItem if item.is_pinned => {
            return (
                IconName::PushPin,
                t("clipboard:quickActions.pinItemActive"),
                tokens.accent.solid,
            );
        }
        QuickAction::Note if item.note.is_some() => tokens.accent.solid,
        QuickAction::Delete => tokens.status.danger.solid,
        _ => tokens.text.secondary,
    };
    let (icon, label) = quick_action_glyph(action);
    (icon, label, color)
}

fn action_name(action: QuickAction) -> &'static str {
    match action {
        QuickAction::Paste => "paste",
        QuickAction::PastePlain => "paste-plain",
        QuickAction::PastePath => "paste-path",
        QuickAction::Copy => "copy",
        QuickAction::CopyPlain => "copy-plain",
        QuickAction::SplitWords => "split-words",
        QuickAction::OpenLink => "open-link",
        QuickAction::SendEmail => "send-email",
        QuickAction::Reveal => "reveal",
        QuickAction::Note => "note",
        QuickAction::Star => "star",
        QuickAction::PinItem => "pin-item",
        QuickAction::Delete => "delete",
    }
}

impl ClipboardList {
    /// 卡片悬停时显示的一排快捷动作（20 px 图标按钮）。按下与点击都不再冒泡给卡片，
    /// 免得触发选中、单击粘贴或双击粘贴。
    pub(super) fn quick_actions(
        &self,
        item: &Arc<ListItem>,
        actions: Vec<QuickAction>,
        can_delete: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = theme::semantic(cx);
        let disabled_opacity = theme::components(cx).quick_action.disabled_opacity;
        let buttons = actions.into_iter().map(|action| {
            // 受删除保护：删除按钮留在原位置灰，提示里说明原因。
            let protected = action == QuickAction::Delete && !can_delete;
            let copied = is_copy(action)
                && self
                    .copied
                    .as_ref()
                    .is_some_and(|(id, copied)| *id == item.id && *copied == action);
            let (icon, label, color) = presentation(tokens, action, item, copied);
            let hover_color = if color == tokens.text.secondary {
                tokens.text.primary
            } else {
                color
            };
            let item = item.clone();

            div()
                .id(ElementId::Name(action_name(action).into()))
                .flex()
                .flex_none()
                .size(space((20.) / 4.))
                .items_center()
                .justify_center()
                .rounded(radius::SM)
                .text_color(color)
                .map(|button| {
                    if protected {
                        button.cursor_default().opacity(disabled_opacity)
                    } else {
                        button.cursor_pointer().hover(move |style| {
                            style.bg(tokens.fill.subtle).text_color(hover_color)
                        })
                    }
                })
                .child(
                    Icon::new(icon)
                        .size(TextSize::Sm.font_size())
                        .when(action == QuickAction::PinItem, |icon| {
                            icon.rotate(radians(-std::f32::consts::FRAC_PI_4))
                        }),
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |list, _, window, cx| {
                    cx.stop_propagation();
                    if !protected {
                        list.run_quick_action(item.clone(), action, window, cx);
                    }
                }))
                .kp_tooltip(if protected {
                    t("clipboard:selection.protected")
                } else {
                    label
                })
        });

        div()
            .flex()
            .items_center()
            .gap(space((2.) / 4.))
            .children(buttons)
            .into_any_element()
    }

    /// 多选时卡片上的复选框；受删除保护的记录不可选（1.x `disabled={!canDelete}`）。
    pub(super) fn checkbox(&self, item: &ListItem, can_delete: bool) -> AnyElement {
        Checkbox::new(ElementId::Name(format!("check-{}", item.id).into()))
            .checked(self.selection.is_checked(&item.id))
            .disabled(!can_delete)
            .accessibility_label(t("clipboard:footer.select"))
            .into_any_element()
    }

    /// 空列表：按筛选条件给出 16 种提示之一。
    pub(super) fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let filter = self.filter();
        let text = empty_text(filter);
        let category = text.category.map(|kind| t(category_key(kind)));
        let group = filter.group_id.as_ref().map(|id| {
            self.groups
                .iter()
                .find(|group| group.id == *id)
                .map(|group| SharedString::from(group.name.to_string()))
                .unwrap_or_else(|| t("clipboard:empty.groupFallback"))
        });
        let mut args: Vec<(&str, &str)> = Vec::new();
        if text.searching {
            args.push(("keyword", &filter.keyword));
        }
        if let Some(category) = &category {
            args.push(("category", category));
        }
        if let Some(group) = &group {
            args.push(("group", group));
        }

        // 图标放在浅灰圆底里；整组略高于正中（底部留白），视觉上才显得居中。
        let icon = if text.searching || text.filtered {
            IconName::Search
        } else {
            IconName::Inbox
        };

        // 右栏的分组是它的页签，不算筛选。
        let clearable = filter.searching()
            || !filter.search.is_empty()
            || filter.category.is_some()
            || (filter.group_id.is_some() && !self.side);

        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(space((12.) / 4.))
            .px(space((24.) / 4.))
            .pb(space((40.) / 4.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .size(space((48.) / 4.))
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(tokens.fill.subtle)
                    .child(
                        Icon::new(icon)
                            .size(space((24.) / 4.))
                            .color(tokens.text.muted),
                    ),
            )
            .child(
                div()
                    .max_w(space((280.) / 4.))
                    .kp_text(TextSize::Sm)
                    .text_color(tokens.text.muted)
                    .text_center()
                    .child(t_args(text.key, &args)),
            )
            .when(clearable, |column| {
                column.child(
                    Button::new("empty-clear-filters", t("clipboard:empty.clearFilters"))
                        .small()
                        .ghost()
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(super::ListIntent::ClearFilters);
                        })),
                )
            })
            .into_any_element()
    }

    /// 页脚：左侧当前项序号与总条数，右侧多选入口、快捷键列表和偏好设置（1.x `Footer.tsx`）；
    /// 多选时换成多选栏。
    pub(super) fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.selection.active() {
            return self.render_selection_bar(cx);
        }

        let tokens = theme::semantic(cx);
        let total = i64::try_from(self.model.total()).unwrap_or(i64::MAX);
        let hint = |action| {
            self.key_hints
                .then(|| shortcut::panel_hint(&self.settings.shortcuts, action))
                .flatten()
                .map(SharedString::from)
        };
        let empty = total == 0;
        let status = if !self.model.loaded_initial() && !self.footer_status.borrow().is_empty() {
            self.footer_status.borrow().clone()
        } else {
            let status = if empty {
                t_count("clipboard:footer.total", total, &[])
            } else {
                let index = (self.controller.active_index(&self.model) + 1).to_string();
                t_count("clipboard:footer.position", total, &[("index", &index)])
            };
            *self.footer_status.borrow_mut() = status.clone();
            status
        };

        div()
            .flex()
            .flex_none()
            .h(space((32.) / 4.))
            .items_center()
            .justify_between()
            .px(space((12.) / 4.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .window_drag_area()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.muted)
                    .child(status),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(space(1.))
                    .child(
                        icon_button(
                            cx,
                            "footer-select",
                            Glyph::Icon(IconName::ListChecks),
                            t("clipboard:footer.select"),
                            hint(PanelAction::SelectAll),
                            Look::Text,
                        )
                        .when(empty, |button| {
                            button
                                .opacity(theme::components(cx).quick_action.disabled_opacity)
                                .cursor_default()
                        })
                        .on_click(cx.listener(move |list, _, _, cx| {
                            if !empty {
                                list.enter_selection(cx);
                            }
                        })),
                    )
                    // 双栏的右栏只留位置和多选，快捷键、偏好设置按钮在左栏的页脚上。
                    .when(!self.side, |tools| {
                        tools
                            .child(
                                icon_button(
                                    cx,
                                    "footer-shortcuts",
                                    Glyph::Icon(IconName::Keyboard),
                                    t("clipboard:footer.shortcuts"),
                                    hint(PanelAction::ShowShortcuts),
                                    Look::Text,
                                )
                                .on_click(cx.listener(
                                    |_, _, _, cx| {
                                        cx.emit(super::ListIntent::ShowShortcuts);
                                    },
                                )),
                            )
                            .child(
                                icon_button(
                                    cx,
                                    "footer-preferences",
                                    Glyph::Icon(IconName::SettingLine),
                                    t("clipboard:footer.preferences"),
                                    hint(PanelAction::OpenPreference),
                                    Look::Text,
                                )
                                .on_click(cx.listener(
                                    |_, _, _, cx| {
                                        cx.emit(super::ListIntent::OpenPreferences);
                                    },
                                )),
                            )
                    }),
            )
            .into_any_element()
    }

    /// 多选栏（1.x `SelectionBar.tsx`）：左侧已选条数或操作提示，右侧全选、粘贴、删除、取消。
    fn render_selection_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let count = self.selection.count();
        let busy = self.selection.busy();
        let exit_label = shortcut::panel_display(
            &self.settings.shortcuts,
            PanelAction::ClosePreviewFilterWindow,
        )
        .map_or_else(
            || t("clipboard:selection.exit"),
            |key| t_args("clipboard:selection.exitWithKey", &[("key", &key)]),
        );
        let status = if count > 0 {
            t_count(
                "clipboard:selection.selected",
                i64::try_from(count).unwrap_or(i64::MAX),
                &[],
            )
        } else {
            t("clipboard:selection.hint")
        };
        let toggle_label = if self.selection.all_checked() {
            t("clipboard:selection.clear")
        } else {
            t("clipboard:selection.selectAll")
        };

        div()
            .flex()
            .flex_none()
            .h(space((32.) / 4.))
            .items_center()
            .justify_between()
            .gap(space((8.) / 4.))
            .px(space((12.) / 4.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.secondary)
                    .child(status),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(space(1.))
                    .child(
                        Button::new("selection-toggle-all", toggle_label)
                            .small()
                            .ghost()
                            .disabled(busy)
                            .when_some(
                                shortcut::panel_display(
                                    &self.settings.shortcuts,
                                    PanelAction::SelectAll,
                                ),
                                |button, hint| button.tooltip(hint),
                            )
                            .on_click(cx.listener(|list, _, window, cx| {
                                list.toggle_all(window, cx);
                            })),
                    )
                    .child(
                        Button::new("selection-paste", t("clipboard:selection.paste"))
                            .small()
                            .primary()
                            .disabled(busy || count == 0)
                            .on_click(cx.listener(|list, _, _, cx| list.paste_checked(cx))),
                    )
                    .child(
                        Button::new("selection-delete", t("clipboard:selection.delete"))
                            .small()
                            .danger_outline()
                            .disabled(busy || count == 0)
                            .when_some(
                                shortcut::panel_display(
                                    &self.settings.shortcuts,
                                    PanelAction::DeleteSelected,
                                ),
                                |button, hint| button.tooltip(hint),
                            )
                            .on_click(cx.listener(|list, _, window, cx| {
                                list.delete_checked(window, cx);
                            })),
                    )
                    .child(
                        Button::icon("selection-exit", IconName::Close, exit_label)
                            .small()
                            .on_click(cx.listener(|list, _, _, cx| list.exit_selection(cx))),
                    ),
            )
            .into_any_element()
    }
}
