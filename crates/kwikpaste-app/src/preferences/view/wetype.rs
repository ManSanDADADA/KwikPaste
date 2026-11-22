//! 微信输入法同步那一页的设置：配对状态、同组设备和三个收发开关。
//!
//! 状态来自 core 的 `wetype_state`（扩展进程上报的快照），`WeTypeChanged` 时整页重绘。

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
};
use kwikpaste_core::WeTypeState;
use kwikpaste_ui::{
    Button, DialogSpec, Input, KpStyled as _, TextInput, form_dialog,
    theme::{self, TextSize, space},
    toast::{self, Toast},
};

use super::{Preferences, note_row, setting_row};
use crate::{core_host, i18n};

impl Preferences {
    /// 启用中的微信输入法扩展的设置行，是它那一页的第二块；第一行不画分隔线。
    pub(super) fn render_wetype(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let tokens = theme::semantic(cx);
        let state = core_host::core(cx)
            .map(|core| core.wetype_state())
            .unwrap_or_default();
        let entity = cx.entity().downgrade();
        let mut rows = Vec::new();

        if !state.running {
            rows.push(note_row(
                true,
                i18n::t("preferences:extensions.wetype.starting"),
                tokens.text.muted,
                tokens,
            ));
        } else if state.paired {
            let unpair = {
                let entity = entity.clone();
                Button::new(
                    "wetype-unpair",
                    i18n::t("preferences:extensions.wetype.unpair"),
                )
                .on_click(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| this.unpair_wetype(window, cx));
                    }
                })
            };
            rows.push(setting_row(
                true,
                i18n::t("preferences:extensions.wetype.paired"),
                i18n::t_args(
                    "preferences:extensions.wetype.pairedDescription",
                    &[("name", &state.self_name)],
                ),
                Some(unpair.into_any_element()),
                tokens,
            ));
            // 设备组里也有本机自己，列表只显示其它设备。
            let names: Vec<&str> = state
                .devices
                .iter()
                .map(|d| d.name.as_str())
                .filter(|name| *name != state.self_name)
                .collect();
            if !names.is_empty() {
                rows.push(setting_row(
                    false,
                    i18n::t("preferences:extensions.wetype.devices"),
                    names
                        .join(&i18n::t("preferences:extensions.wetype.deviceSeparator"))
                        .into(),
                    None,
                    tokens,
                ));
            }
        } else {
            let pair = Button::new("wetype-pair", i18n::t("preferences:extensions.wetype.pair"))
                .on_click(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| this.open_wetype_pair_dialog(window, cx));
                    }
                });
            rows.push(setting_row(
                true,
                i18n::t("preferences:extensions.wetype.notPaired"),
                i18n::t("preferences:extensions.wetype.notPairedDescription"),
                Some(pair.into_any_element()),
                tokens,
            ));
        }
        if state.paired
            && core_host::core(cx).is_some_and(|core| core.wetype_official_client_running())
        {
            rows.push(note_row(
                false,
                i18n::t("preferences:extensions.wetype.coexist"),
                tokens.text.muted,
                tokens,
            ));
        }
        if let Some(error) = state.error.clone().filter(|_| state.running) {
            rows.push(note_row(
                false,
                error.into(),
                tokens.status.danger.solid,
                tokens,
            ));
        }

        let options = &self.settings.extensions.wetype;
        for (path, key, checked) in [
            ("extensions.wetype.sendText", "sendText", options.send_text),
            (
                "extensions.wetype.sendImage",
                "sendImage",
                options.send_image,
            ),
            (
                "extensions.wetype.writeClipboard",
                "writeClipboard",
                options.write_clipboard,
            ),
        ] {
            let title = i18n::t(&format!("preferences:extensions.wetype.{key}"));
            rows.push(setting_row(
                false,
                title.clone(),
                SharedString::default(),
                Some(self.render_lan_switch(path, checked, title, cx)),
                tokens,
            ));
        }
        rows
    }

    /// 输入手机上显示的 6 位配对码；配对最多等 90 秒，结果用 toast 告知。
    fn open_wetype_pair_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let code = TextInput::new(
            i18n::t("preferences:extensions.wetype.codePlaceholder"),
            window,
            cx,
        );
        let content = code.clone();
        let answer = form_dialog(
            DialogSpec::new(i18n::t("preferences:extensions.wetype.pairTitle"))
                .ok_text(i18n::t("preferences:extensions.wetype.pair"))
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _| {
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(div().kp_text(TextSize::Sm).child(i18n::t(
                        "preferences:extensions.wetype.notPairedDescription",
                    )))
                    .child(Input::new(&content))
                    .into_any_element()
            },
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let value = cx
                    .update(|_, cx| code.value(cx).trim().to_owned())
                    .unwrap_or_default();
                let result = core.wetype_pair(value).await;
                let _ = entity.update_in(cx, |_, window, cx| {
                    let toast = match result {
                        Ok(()) => {
                            Toast::success(i18n::t("preferences:extensions.wetype.pairedToast"))
                        }
                        Err(error) => Toast::error(error.to_string()),
                    };
                    toast::show(toast, window, cx);
                });
            })
            .detach();
    }

    /// 只清除本机身份；SDK 退不出设备组，提示用户到手机上移除这台电脑。
    fn unpair_wetype(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let WeTypeState { self_name, .. } = core.wetype_state();
        let answer = form_dialog(
            DialogSpec::new(i18n::t("preferences:extensions.wetype.unpairTitle"))
                .ok_text(i18n::t("preferences:extensions.wetype.unpair"))
                .cancel_text(i18n::t("common:actions.cancel"))
                .danger(),
            move |_, _| {
                div()
                    .kp_text(TextSize::Sm)
                    .child(i18n::t_args(
                        "preferences:extensions.wetype.unpairContent",
                        &[("name", &self_name)],
                    ))
                    .into_any_element()
            },
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                if let Err(error) = core.wetype_unpair().await {
                    let _ = entity.update_in(cx, |_, window, cx| {
                        toast::show(Toast::error(error.to_string()), window, cx);
                    });
                }
            })
            .detach();
    }
}
