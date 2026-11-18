//! 官方扩展页：沿用同步页的设置行与分组，状态由更新器宿主推送。

use gpui::{
    App, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
    radians, rems,
};
use kwikpaste_ui::{
    Button, DialogSpec, Icon, IconName, KpStyled as _, Switch, form_dialog,
    theme::{self, SemanticTokens, TextSize, space},
    toast::{self, Toast},
};
use kwikpaste_updater::extensions::{ExtensionStatus, OperationState};

use super::ocr_engine::TIERS;
use super::{Preferences, row_frame, section_block, setting_row, text};
use crate::{i18n, platform::updater};

#[derive(Clone, Copy)]
pub(super) enum Action {
    Refresh,
    Install,
    Uninstall,
    /// 不确认直接卸载：切回系统识别时移除已装的 PP-OCRv6 档位。
    Remove,
    Enable(bool),
}

/// 读取可观察宿主的快照；宿主不可用时仍显示已知扩展的不可用卡片。
fn statuses(cx: &App) -> Vec<ExtensionStatus> {
    if let Some(host) = cx.try_global::<updater::ExtensionStateHost>() {
        return host.0.read(cx).0.clone();
    }
    updater::extensions(cx).map_or_else(
        || {
            vec![ExtensionStatus {
                id: "ocr".to_owned(),
                catalog: None,
                installed: None,
                operation: OperationState::Idle,
                update_available: false,
            }]
        },
        |manager| manager.status(),
    )
}

pub(super) fn ocr_installed(cx: &App) -> bool {
    statuses(cx)
        .iter()
        .any(|status| status.id == "ocr" && status.installed.is_some())
}

impl Preferences {
    pub(super) fn refresh_extensions(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_extension_action(String::new(), Action::Refresh, window, cx);
    }

    /// 卸载先确认；所有管理器操作都在窗口任务里执行，失败显示原始根因。
    pub(super) fn run_extension_action(
        &self,
        id: String,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(manager) = updater::extensions(cx).cloned() else {
            toast::show(
                Toast::error(i18n::t("preferences:extensions.unavailable")),
                window,
                cx,
            );
            return;
        };
        // 卸载都要确认；微信输入法同步和翻译在安装前先说明它们用的是非官方接口。
        let confirm = match action {
            Action::Uninstall => Some((
                "uninstallTitle".to_owned(),
                "uninstallContent",
                "uninstall",
                true,
            )),
            Action::Install if matches!(id.as_str(), "wetype" | "translate") => Some((
                format!("{id}.installTitle"),
                "installContent",
                "install",
                false,
            )),
            _ => None,
        };
        let answer = confirm.map(|(title, content, ok, danger)| {
            let content = i18n::t(&format!("preferences:extensions.{id}.{content}"));
            let spec = DialogSpec::new(i18n::t(&format!("preferences:extensions.{title}")))
                .ok_text(i18n::t(&format!("preferences:extensions.{ok}")))
                .cancel_text(i18n::t("common:actions.cancel"));
            form_dialog(
                if danger { spec.danger() } else { spec },
                move |_, _| {
                    div()
                        .kp_text(TextSize::Sm)
                        .child(content.clone())
                        .into_any_element()
                },
                window,
                cx,
            )
        });
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if let Some(answer) = answer
                    && !answer.await.unwrap_or(false)
                {
                    return;
                }
                let result = match action {
                    Action::Refresh => manager.refresh().await.map(|_| ()),
                    Action::Install => manager.install(id).await,
                    Action::Uninstall | Action::Remove => manager.uninstall(id).await,
                    Action::Enable(enabled) => manager.set_enabled(id, enabled).await,
                };
                if let Err(error) = result {
                    let _ = entity.update_in(cx, |_, window, cx| {
                        toast::show(Toast::error(error.to_string()), window, cx);
                    });
                }
            })
            .detach();
    }

    /// 每个已知扩展一张卡片，操作中的按钮和开关不可用。
    pub(super) fn render_extensions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let entity = cx.entity().downgrade();
        let mut cards = Vec::new();
        let all = statuses(cx);
        // PP-OCRv6 各档不单独成卡，在图片文字识别的设置里选。
        for status in all
            .iter()
            .filter(|status| !TIERS.contains(&status.id.as_str()))
        {
            let status = status.clone();
            let busy = matches!(
                status.operation,
                OperationState::Downloading { .. } | OperationState::Installing
            );
            let mut controls = div().flex().items_center().gap(space(2.));
            let button = |key: &'static str, action: Action| {
                let entity = entity.clone();
                let id = status.id.clone();
                Button::new(
                    format!("extension-{}-{key}", status.id),
                    i18n::t(&format!("preferences:extensions.{key}")),
                )
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| {
                            // 装好后直接展开，显示这个扩展的设置。
                            if matches!(action, Action::Install) {
                                this.extensions_expanded.insert(id.clone());
                            }
                            this.run_extension_action(id.clone(), action, window, cx);
                        });
                    }
                })
            };
            if !matches!(status.operation, OperationState::Error { .. })
                && status.installed.is_none()
                && (status.catalog.is_some() || busy)
            {
                controls = controls.child(button("install", Action::Install));
            }
            if let Some(installed) = &status.installed {
                let entity = entity.clone();
                let id = status.id.clone();
                controls = controls
                    .child(
                        Switch::new(format!("extension-{}-enabled", status.id))
                            .accessibility_label(i18n::t_args(
                                "preferences:extensions.enable",
                                &[(
                                    "name",
                                    &i18n::t(&format!("preferences:extensions.{}.name", status.id)),
                                )],
                            ))
                            .checked(installed.enabled)
                            .disabled(busy)
                            .on_change(move |enabled, window, cx| {
                                if let Some(entity) = entity.upgrade() {
                                    entity.update(cx, |this, cx| {
                                        this.run_extension_action(
                                            id.clone(),
                                            Action::Enable(enabled),
                                            window,
                                            cx,
                                        );
                                    });
                                }
                            }),
                    )
                    .child(button("uninstall", Action::Uninstall));
            }
            let title = div()
                .flex()
                .items_baseline()
                .gap(space(2.))
                .child(div().kp_text(TextSize::Sm).child(i18n::t(&format!(
                    "preferences:extensions.{}.name",
                    status.id
                ))))
                .children(metadata(&status).map(|metadata| {
                    div()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.secondary)
                        .child(metadata)
                }));
            let expanded = self.extensions_expanded.contains(&status.id);
            let toggle = {
                let id = status.id.clone();
                cx.listener(move |this, _, _, cx| {
                    if !this.extensions_expanded.remove(&id) {
                        this.extensions_expanded.insert(id.clone());
                    }
                    cx.notify();
                })
            };
            let label = div()
                .id(SharedString::from(format!(
                    "extension-{}-toggle",
                    status.id
                )))
                .flex()
                .items_center()
                .gap(space(2.))
                .flex_1()
                .min_w_0()
                .cursor_pointer()
                .on_click(toggle)
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(rems(1.))
                        .color(tokens.text.muted)
                        .when(!expanded, |icon| {
                            icon.rotate(radians(-std::f32::consts::FRAC_PI_2))
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(space(0.5))
                        .child(title)
                        .child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.muted)
                                .child(i18n::t(&format!(
                                    "preferences:extensions.{}.description",
                                    status.id
                                ))),
                        ),
                );
            let mut rows = vec![
                row_frame(true, tokens)
                    .child(label)
                    .child(controls.flex_none())
                    .into_any_element(),
            ];
            if let Some(note) = status_text(&status) {
                let action = match status.operation {
                    OperationState::Error { .. } => Some(button("retry", Action::Refresh)),
                    OperationState::Idle if status.update_available => {
                        Some(button("update", Action::Install))
                    }
                    _ => None,
                };
                rows.push(setting_row(
                    false,
                    note,
                    SharedString::default(),
                    action.map(IntoElement::into_any_element),
                    tokens,
                ));
            }
            // 展开后：启用中的扩展显示它的设置，没装或停用时显示功能介绍。
            if expanded {
                let enabled = status
                    .installed
                    .as_ref()
                    .is_some_and(|installed| installed.enabled);
                match status.id.as_str() {
                    "ocr" => {
                        if !enabled {
                            rows.push(details("ocr", tokens));
                        }
                        if enabled || self.image_text_row_visible() {
                            rows.push(self.render_image_text_status(false, cx));
                        }
                        if enabled && cfg!(target_os = "windows") {
                            rows.push(self.render_ocr_engines(&all, cx));
                        }
                    }
                    "wetype" if enabled => rows.extend(self.render_wetype(cx)),
                    "translate" if enabled => rows.extend(self.render_translate(cx)),
                    id => rows.push(details(id, tokens)),
                }
            }
            cards.push(section_block(None, None, rows, tokens));
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(5.))
            .children(cards)
            .into_any_element()
    }
}

/// 自动更新始终开启；状态行只展示操作进度、错误或不可用状态。
pub(super) fn status_text(status: &ExtensionStatus) -> Option<SharedString> {
    match &status.operation {
        OperationState::Downloading { progress } => {
            let percent = (progress.progress.unwrap_or(0.0).clamp(0.0, 1.0) * 100.0)
                .round()
                .to_string();
            return Some(i18n::t_args(
                "preferences:extensions.downloading",
                &[("percent", &percent)],
            ));
        }
        OperationState::Installing => return Some(i18n::t("preferences:extensions.installing")),
        OperationState::Error { message } => return Some(message.clone().into()),
        OperationState::Idle => {}
    }
    if status.installed.is_some() {
        return None;
    }
    status
        .catalog
        .is_none()
        .then(|| i18n::t("preferences:extensions.unavailable"))
}

/// 展开的介绍：功能、实现方式、隐私三段，文案按扩展 id 取。
fn details(id: &str, tokens: &SemanticTokens) -> gpui::AnyElement {
    let parts = ["feature", "how", "privacy"].map(|part| {
        div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .child(
                div()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.muted)
                    .child(i18n::t(&format!("preferences:extensions.details.{part}"))),
            )
            .child(
                div()
                    .kp_text(TextSize::Sm)
                    .text_color(tokens.text.secondary)
                    .child(i18n::t(&format!("preferences:extensions.{id}.{part}"))),
            )
    });
    row_frame(false, tokens)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(space(3.))
                .children(parts),
        )
        .into_any_element()
}

/// 版本或下载大小紧跟卡片标题，不再占用独立设置行。
fn metadata(status: &ExtensionStatus) -> Option<SharedString> {
    status
        .installed
        .as_ref()
        .map(|installed| installed.version.clone().into())
        .or_else(|| {
            status
                .catalog
                .as_ref()
                .map(|catalog| text::format_bytes(catalog.size).into())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kwikpaste_core::extensions::InstalledExtension;
    use kwikpaste_updater::DownloadProgress;

    #[test]
    fn card_status_follows_availability_version_and_operation() {
        let mut status = ExtensionStatus {
            id: "ocr".to_owned(),
            catalog: None,
            installed: None,
            operation: OperationState::Idle,
            update_available: false,
        };
        assert_eq!(
            status_text(&status),
            Some(i18n::t("preferences:extensions.unavailable"))
        );
        assert_eq!(metadata(&status), None);
        status.catalog = Some(
            serde_json::from_value(serde_json::json!({
                "id": "ocr", "version": "1.2.3", "protocol": 1, "size": 1258291,
                "signature": "", "url": "https://example.invalid/ocr",
                "notesZh": "", "notesEn": ""
            }))
            .expect("有效目录夹具"),
        );
        assert_eq!(status_text(&status), None);
        assert_eq!(metadata(&status).as_deref(), Some("1.2 MB"));
        status.installed = Some(InstalledExtension {
            version: "1.0.0".to_owned(),
            protocol: 1,
            enabled: true,
        });
        assert_eq!(metadata(&status).as_deref(), Some("1.0.0"));
        assert_eq!(status_text(&status), None);
        status.update_available = true;
        assert_eq!(status_text(&status), None);
        status.operation = OperationState::Downloading {
            progress: DownloadProgress {
                downloaded: 51,
                total: Some(100),
                progress: Some(0.51),
            },
        };
        assert_eq!(
            status_text(&status),
            Some(i18n::t_args(
                "preferences:extensions.downloading",
                &[("percent", "51")],
            ))
        );
        status.operation = OperationState::Installing;
        assert_eq!(
            status_text(&status),
            Some(i18n::t("preferences:extensions.installing"))
        );
        status.operation = OperationState::Error {
            message: "test failure".to_owned(),
        };
        assert_eq!(status_text(&status).as_deref(), Some("test failure"));
        status.operation = OperationState::Idle;
        status.catalog = None;
        status.update_available = false;
        assert_eq!(status_text(&status), None);
        assert_eq!(metadata(&status).as_deref(), Some("1.0.0"));
    }
}
