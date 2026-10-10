//! 采集页的图片文字识别状态：识别进度、完成情况、缺少语言包、系统不支持，
//! 以及关闭识别后还留在本机的识别文字。
//!
//! 数字来自 core 的 `ocr_status`（一条聚合查询）。系统能力只在开着识别时探测：探测会短暂
//! 起一个识别进程，结果由 core 在本次运行里缓存。

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, relative,
};
use kwikpaste_core::{OcrStatus, OcrSupport};
use kwikpaste_ui::{
    Button, Icon, IconName, KpStyled as _,
    theme::{self, SemanticTokens, TextSize, space},
    toast::{self, Toast},
};

use super::{Preferences, row_frame};
use crate::{core_host, i18n};

/// 状态行此刻要说的事。
#[derive(Debug, PartialEq)]
enum Row {
    /// 开着识别，还没拿到状态或系统能力。
    Checking,
    MissingLanguage,
    Unsupported,
    Running {
        done: u64,
        total: u64,
    },
    Queued {
        count: u64,
    },
    Done(OcrStatus),
    Empty,
    /// 关着识别，但本机还留着识别出的文字。
    Saved {
        count: u64,
    },
}

impl Preferences {
    /// 重新拉取识别状态；开着识别且还不知道系统能力时顺带探测一次。
    pub(super) fn refresh_image_text(&mut self, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        // 以扩展实时状态作为门控，并丢弃较早生命周期事件的状态与探测结果。
        self.ocr_refresh = self.ocr_refresh.wrapping_add(1);
        let refresh = self.ocr_refresh;
        let probe = core.ocr_enabled() && self.ocr_support.is_none();

        cx.spawn(async move |this, cx| {
            let status = core.ocr_status().await;
            let _ = this.update(cx, |this, cx| {
                if this.ocr_refresh != refresh {
                    return;
                }
                match status {
                    Ok(status) => this.ocr_status = Some(status),
                    Err(error) => log::warn!("image text status failed: {error:#}"),
                }
                cx.notify();
            });
            if !probe {
                return;
            }
            let support = core.ocr_support().await;
            let _ = this.update(cx, |this, cx| {
                if this.ocr_refresh != refresh {
                    return;
                }
                match support {
                    Ok(support) => this.ocr_support = Some(support),
                    Err(error) => log::warn!("image text support probe failed: {error:#}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关着识别又没有留下文字时整行不显示。
    pub(super) fn image_text_row_visible(&self) -> bool {
        self.image_text_row() != Row::Empty
            || self
                .ocr_status
                .as_ref()
                .is_some_and(|status| status.enabled)
    }

    fn image_text_row(&self) -> Row {
        let status = self.ocr_status.as_ref();
        if !self
            .ocr_status
            .as_ref()
            .is_some_and(|status| status.enabled)
        {
            return match status {
                Some(status) if status.with_text > 0 => Row::Saved {
                    count: status.with_text,
                },
                _ => Row::Empty,
            };
        }
        match self.ocr_support {
            Some(OcrSupport::MissingLanguage) => return Row::MissingLanguage,
            Some(OcrSupport::Unsupported | OcrSupport::NotInstalled | OcrSupport::Disabled) => {
                return Row::Unsupported;
            }
            Some(OcrSupport::Available { .. }) => {}
            None => return Row::Checking,
        }
        let Some(status) = status else {
            return Row::Checking;
        };
        if status.total_images == 0 {
            return Row::Empty;
        }
        if status.running {
            return Row::Running {
                done: status.total_images.saturating_sub(status.pending),
                total: status.total_images,
            };
        }
        if status.pending > 0 {
            return Row::Queued {
                count: status.pending,
            };
        }
        Row::Done(status.clone())
    }

    /// 状态行：标题、说明随状态变，右侧按需给按钮，识别中在说明下面画进度条。
    pub(super) fn render_image_text_status(
        &self,
        first: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = theme::semantic(cx);
        let row = self.image_text_row();
        let (title, description) = self.image_text_copy(&row);
        let icon = match row {
            Row::MissingLanguage | Row::Unsupported => {
                Some((IconName::TriangleAlert, tokens.status.warning.solid))
            }
            Row::Done(_) => Some((IconName::CircleCheck, tokens.status.success.solid)),
            _ => None,
        };
        let title = div()
            .flex()
            .items_center()
            .gap(space(1.5))
            .when_some(icon, |line, (name, color)| {
                line.child(Icon::new(name).size(space(3.5)).color(color))
            })
            .child(title);
        let progress = match row {
            Row::Running { done, total } if total > 0 => {
                Some(progress_bar(done as f32 / total as f32, tokens))
            }
            _ => None,
        };
        let action = match row {
            Row::MissingLanguage if cfg!(target_os = "windows") => Some(
                Button::new(
                    "image-text-language",
                    i18n::t("preferences:imageText.missingLanguage.action"),
                )
                .on_click(|_, _, _| {
                    if let Err(error) =
                        kwikpaste_os::dialogs::open_url("ms-settings:regionlanguage")
                    {
                        log::warn!("language settings could not be opened: {error}");
                    }
                })
                .into_any_element(),
            ),
            Row::Saved { .. } => {
                let entity = cx.entity().downgrade();
                Some(
                    Button::new(
                        "image-text-delete",
                        i18n::t("preferences:imageText.saved.action"),
                    )
                    .danger_outline()
                    .on_click(move |_, window, cx| {
                        let Some(entity) = entity.upgrade() else {
                            return;
                        };
                        entity.update(cx, |this, cx| this.delete_image_text(window, cx));
                    })
                    .into_any_element(),
                )
            }
            _ => None,
        };

        row_frame(first, tokens)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space(0.5))
                    .flex_1()
                    .min_w_0()
                    .child(title.kp_text(TextSize::Sm))
                    .when(!description.is_empty(), |label| {
                        label.child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.muted)
                                .child(description),
                        )
                    })
                    .when_some(progress, |label, bar| label.child(bar)),
            )
            .when_some(action, |row, action| {
                row.child(div().flex().flex_none().items_center().child(action))
            })
            .into_any_element()
    }

    fn image_text_copy(&self, row: &Row) -> (SharedString, SharedString) {
        let count = |n: u64| n.to_string();
        match row {
            Row::Checking => (i18n::t("preferences:imageText.checking"), "".into()),
            Row::MissingLanguage => (
                i18n::t("preferences:imageText.missingLanguage.title"),
                i18n::t("preferences:imageText.missingLanguage.description"),
            ),
            Row::Unsupported => (
                i18n::t("preferences:imageText.unsupported.title"),
                i18n::t(if cfg!(target_os = "macos") {
                    "preferences:imageText.unsupported.descriptionMac"
                } else {
                    "preferences:imageText.unsupported.descriptionWindows"
                }),
            ),
            Row::Running { done, total } => (
                i18n::t_args(
                    "preferences:imageText.running.title",
                    &[("done", &count(*done)), ("total", &count(*total))],
                ),
                i18n::t("preferences:imageText.running.description"),
            ),
            Row::Queued { count: n } => (
                i18n::t_args(
                    "preferences:imageText.queued.title",
                    &[("count", &count(*n))],
                ),
                self.language_line().unwrap_or_default(),
            ),
            Row::Done(status) => {
                let mut parts = vec![i18n::t_args(
                    "preferences:imageText.withText",
                    &[("count", &count(status.with_text))],
                )];
                if status.failed > 0 {
                    parts.push(i18n::t_args(
                        "preferences:imageText.failed",
                        &[("count", &count(status.failed))],
                    ));
                }
                parts.extend(self.language_line());
                (
                    i18n::t_args(
                        "preferences:imageText.done.title",
                        &[("count", &count(status.total_images))],
                    ),
                    parts.join(" · ").into(),
                )
            }
            Row::Empty => (
                i18n::t("preferences:imageText.empty.title"),
                i18n::t("preferences:imageText.empty.description"),
            ),
            Row::Saved { count: n } => (
                i18n::t("preferences:imageText.saved.title"),
                i18n::t_args(
                    "preferences:imageText.saved.description",
                    &[("count", &count(*n))],
                ),
            ),
        }
    }

    /// 「识别语言：中文（简体）、英文」；未知的语言标记原样显示。
    fn language_line(&self) -> Option<SharedString> {
        let Some(OcrSupport::Available { languages }) = &self.ocr_support else {
            return None;
        };
        if languages.is_empty() {
            return None;
        }
        let names: Vec<String> = languages.iter().map(|tag| language_name(tag)).collect();
        let separator = i18n::t("preferences:imageText.languageSeparator");
        Some(i18n::t_args(
            "preferences:imageText.language",
            &[("languages", &names.join(&separator))],
        ))
    }

    /// 删掉关闭识别后留下的文字；图片本身不受影响，重新开启时会再识别。
    fn delete_image_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };

        cx.spawn_in(window, async move |this, cx| {
            let result = core.clear_ocr_data().await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => toast::show(
                        Toast::success(i18n::t("preferences:imageText.deleted")),
                        window,
                        cx,
                    ),
                    Err(error) => {
                        log::warn!("image text could not be deleted: {error:#}");
                        let message = i18n::t_args(
                            "commands:error",
                            &[
                                ("label", &i18n::t("preferences:imageText.deleteFailed")),
                                ("message", &error.to_string()),
                            ],
                        );
                        toast::show(Toast::error(message), window, cx);
                    }
                }
                this.refresh_image_text(cx);
            });
        })
        .detach();
    }
}

/// 识别语言的显示名：`zh-Hans-CN` 取 `zh-Hans`，`en-US` 取 `en`。
fn language_name(tag: &str) -> String {
    let lower = tag.to_ascii_lowercase();
    let key = if lower.starts_with("zh-hant") || lower == "zh-tw" || lower == "zh-hk" {
        "zh-Hant"
    } else if lower.starts_with("zh") {
        "zh-Hans"
    } else {
        lower.split('-').next().unwrap_or_default()
    };
    let name =
        crate::preferences::text::optional(&format!("preferences:imageText.languages.{key}"));
    if name.is_empty() {
        tag.to_owned()
    } else {
        name.to_string()
    }
}

/// 细进度条：底色是弱填充，已完成的部分用主色。
fn progress_bar(ratio: f32, tokens: &SemanticTokens) -> AnyElement {
    div()
        .mt(space(1.5))
        .h(space(1.))
        .w_full()
        .rounded(theme::radius::SM)
        .bg(tokens.fill.default)
        .child(
            div()
                .h_full()
                .rounded(theme::radius::SM)
                .w(relative(ratio.clamp(0.02, 1.)))
                .bg(tokens.accent.solid),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_names_fold_region_and_script() {
        assert_eq!(language_name("xx-YY"), "xx-YY");
        assert_eq!(
            language_name("zh-Hans-CN"),
            language_name("zh-CN"),
            "both are Simplified Chinese"
        );
    }
}
