//! 图片文字识别的引擎选择（仅 Windows）：系统识别，或下载一档 PP-OCRv6。
//!
//! 每档是一个独立扩展，同时只装一档；选中未下载的档位就下载它，下载完即使用，
//! 切回系统识别就是卸载已装的档位。

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, Toggled, div,
    prelude::FluentBuilder as _, rems,
};
use kwikpaste_ui::{
    KpStyled as _, Tag, TagColor,
    theme::{self, SemanticTokens, TextSize, space},
};
use kwikpaste_updater::extensions::{ExtensionStatus, OperationState};

use super::extensions::{Action, status_text};
use super::{Preferences, row_frame, text};
use crate::i18n;

/// 从轻到重；顺序就是界面上的顺序。
pub(super) const TIERS: [&str; 3] = ["ocr-tiny", "ocr-small", "ocr-medium"];
/// 第一次选 PP-OCRv6 时推荐的档位。
const RECOMMENDED: &str = "ocr-tiny";
/// 三项实测数据，定宽排开，各行上下对齐方便比较；窗口窄时自动换行。
const STATS: [&str; 3] = ["errors", "speed", "memory"];
const STAT_WIDTH: f32 = 6.;

/// 一个选项此刻的样子；`action` 为空时整行不可点。
struct Choice {
    key: &'static str,
    checked: bool,
    recommended: bool,
    trailing: Option<SharedString>,
    action: Option<(String, Action)>,
}

impl Preferences {
    /// 一整行：标题与说明、四个单选项、实测数据的出处。
    pub(super) fn render_ocr_engines(
        &self,
        statuses: &[ExtensionStatus],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = theme::semantic(cx);
        let tiers: Vec<(&'static str, &ExtensionStatus)> = TIERS
            .iter()
            .filter_map(|id| {
                statuses
                    .iter()
                    .find(|status| status.id == *id)
                    .map(|status| (*id, status))
            })
            .collect();
        let active = tiers
            .iter()
            .find(|(_, status)| status.installed.is_some())
            .map(|(id, _)| *id);
        let any_busy = tiers.iter().any(|(_, status)| busy(status));

        let mut choices = vec![Choice {
            key: "system",
            checked: active.is_none(),
            recommended: false,
            trailing: None,
            action: active
                .filter(|_| !any_busy)
                .map(|id| (id.to_owned(), Action::Remove)),
        }];
        for (id, status) in &tiers {
            let checked = status.installed.is_some();
            let (trailing, action) = if checked {
                (None, None)
            } else if busy(status) {
                (status_text(status), None)
            } else if matches!(status.operation, OperationState::Error { .. }) {
                (
                    Some(i18n::t("preferences:extensions.retry")),
                    Some(Action::Refresh),
                )
            } else if let Some(catalog) = &status.catalog {
                (
                    Some(i18n::t_args(
                        "preferences:ocrEngine.download",
                        &[("size", &text::format_bytes(catalog.size))],
                    )),
                    Some(Action::Install),
                )
            } else {
                (Some(i18n::t("preferences:ocrEngine.unavailable")), None)
            };
            choices.push(Choice {
                key: id.trim_start_matches("ocr-"),
                checked,
                recommended: *id == RECOMMENDED,
                trailing,
                action: action
                    .filter(|_| !any_busy)
                    .map(|action| ((*id).to_owned(), action)),
            });
        }
        let error = tiers
            .iter()
            .find_map(|(_, status)| match &status.operation {
                OperationState::Error { message } => Some(SharedString::from(message.clone())),
                _ => None,
            });

        let options = choices
            .into_iter()
            .map(|choice| self.render_choice(choice, cx))
            .collect::<Vec<_>>();
        row_frame(false, tokens)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(space(3.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space(0.5))
                            .child(
                                div()
                                    .kp_text(TextSize::Sm)
                                    .child(i18n::t("preferences:ocrEngine.title")),
                            )
                            .child(
                                div()
                                    .kp_text(TextSize::Xs)
                                    .text_color(tokens.text.muted)
                                    .child(i18n::t("preferences:ocrEngine.description")),
                            ),
                    )
                    .child(div().flex().flex_col().gap(space(1.)).children(options))
                    .when_some(error, |column, error| {
                        column.child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.status.danger.solid)
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.muted)
                            .child(i18n::t("preferences:ocrEngine.footnote")),
                    ),
            )
            .into_any_element()
    }

    /// 单选行：圆点；名称（可带推荐标签）与下载提示或进度，一句定位，一行实测数据。
    fn render_choice(&self, choice: Choice, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let name = i18n::t(&format!("preferences:ocrEngine.{}.name", choice.key));
        let clickable = choice.action.is_some();
        let entity = cx.entity().downgrade();

        div()
            .id(SharedString::from(format!("ocr-engine-{}", choice.key)))
            .role(Role::RadioButton)
            .aria_label(name.clone())
            .aria_toggled(if choice.checked {
                Toggled::True
            } else {
                Toggled::False
            })
            .flex()
            .items_center()
            .gap(space(3.))
            .px(space(3.))
            .py(space(2.5))
            .rounded(theme::radius::MD)
            .border_1()
            .border_color(if choice.checked {
                tokens.accent.border
            } else {
                tokens.border.subtle
            })
            .when(choice.checked, |row| row.bg(tokens.accent.subtle))
            .when(clickable, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(tokens.item.hover))
            })
            .when_some(choice.action, |row, (id, action)| {
                row.on_click(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.run_extension_action(id.clone(), action, window, cx);
                        });
                    }
                })
            })
            .child(radio(choice.checked, tokens))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(space(1.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space(2.))
                            .child(div().kp_text(TextSize::Sm).child(name))
                            .when(choice.recommended, |line| {
                                line.child(
                                    Tag::new(i18n::t("preferences:ocrEngine.recommended"))
                                        .color(TagColor::Primary),
                                )
                            })
                            .child(div().flex_1())
                            .when_some(choice.trailing, |line, trailing| {
                                line.child(
                                    div()
                                        .flex_none()
                                        .kp_text(TextSize::Xs)
                                        .text_color(if clickable {
                                            tokens.accent.text
                                        } else {
                                            tokens.text.muted
                                        })
                                        .child(trailing),
                                )
                            }),
                    )
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .child(i18n::t(&format!(
                                "preferences:ocrEngine.{}.summary",
                                choice.key
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_y(space(0.5))
                            .children(STATS.map(|stat| {
                                div()
                                    .flex_none()
                                    .w(rems(STAT_WIDTH))
                                    .flex()
                                    .gap(space(1.))
                                    .kp_text(TextSize::Xs)
                                    .child(div().text_color(tokens.text.muted).child(i18n::t(
                                        &format!("preferences:ocrEngine.columns.{stat}"),
                                    )))
                                    .child(div().text_color(tokens.text.primary).child(i18n::t(
                                        &format!("preferences:ocrEngine.{}.{stat}", choice.key),
                                    )))
                            })),
                    ),
            )
            .into_any_element()
    }
}

fn busy(status: &ExtensionStatus) -> bool {
    matches!(
        status.operation,
        OperationState::Downloading { .. } | OperationState::Installing
    )
}

/// 单选圆点：选中时是主色描边加实心圆心。
fn radio(checked: bool, tokens: &SemanticTokens) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(space(4.))
        .rounded_full()
        .border_1()
        .border_color(if checked {
            tokens.accent.solid
        } else {
            tokens.border.strong
        })
        .when(checked, |circle| {
            circle.child(div().size(space(2.)).rounded_full().bg(tokens.accent.solid))
        })
        .into_any_element()
}
