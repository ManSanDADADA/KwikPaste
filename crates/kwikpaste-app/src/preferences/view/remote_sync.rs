//! 「同步」页：顶部在局域网同步和远程同步之间切换；这里是远程同步那一块和顶部的切换。
//!
//! 远程同步走官方服务器或用户自部署的服务器（同一个服务端程序）。状态来自 core 的
//! `remote_sync_state`，`RemoteSyncChanged` 时刷新；服务器令牌、组密钥都在 core 的本机状态文件里，
//! 这里只在建组 / 加入时把输入框里的地址和令牌交给 core。

use std::rc::Rc;

use chrono::{DateTime, Local, Utc};
use gpui::{
    AnyElement, App, Context, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, Window, div,
};
use kwikpaste_core::settings::{RemoteImageQuality, Settings, SettingsDelta};
use kwikpaste_core::sync::{
    RemoteDeviceView, RemoteImages, RemoteServer, RemoteServerView, RemoteStatus, RemoteSyncState,
};
use kwikpaste_ui::{
    Button, DialogSpec, Input, KpStyled as _, NumberInput, NumberInputState, TextInput,
    form_dialog,
    theme::{self, SemanticTokens, TextSize, space},
    toast::{self, Toast},
};
use serde_json::json;

use super::{
    CONTROL_WIDTH, Preferences, choices, note_row, platform_label, section_block, setting_row,
};
use crate::{core_host, i18n};

/// 同步页顶部切换的两块。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SyncView {
    #[default]
    Lan,
    Remote,
}

/// 还没加入同步组时选的服务器。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ServerChoice {
    #[default]
    Official,
    SelfHost,
}

/// 远程同步的输入控件与界面状态，随偏好窗口存活。
pub(super) struct RemoteControls {
    pub(super) state: Option<RemoteSyncState>,
    pub(super) view: SyncView,
    choice: ServerChoice,
    url: TextInput,
    token: TextInput,
    max_image: NumberInputState,
    /// 建组、加入、申请配对码这类请求在跑时按钮不可点。
    busy: bool,
}

impl RemoteControls {
    pub(super) fn new(
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Preferences>,
    ) -> (Self, Vec<Subscription>) {
        let state = core_host::core(cx).map(|core| core.remote_sync_state());
        let url = TextInput::new(key("selfHost.urlPlaceholder"), window, cx);
        let token = TextInput::new(key("selfHost.tokenPlaceholder"), window, cx);
        let max_image = NumberInputState::new(
            u64::from(settings.sync.remote.max_image_mb),
            kwikpaste_core::settings::LAN_SYNC_MAX_IMAGE_MB_MIN.into(),
            kwikpaste_core::settings::LAN_SYNC_MAX_IMAGE_MB_MAX.into(),
            window,
            cx,
        );
        let subscriptions = vec![max_image.on_commit(window, cx, |this, value, _, cx| {
            this.update("sync.remote.maxImageMb", json!(value), cx);
        })];
        // 已经加入同步组时打开页面，直接停在远程同步。
        let view = if state.as_ref().is_some_and(|state| state.server.is_some()) {
            SyncView::Remote
        } else {
            SyncView::Lan
        };
        let controls = Self {
            state,
            view,
            choice: ServerChoice::Official,
            url,
            token,
            max_image,
            busy: false,
        };
        (controls, subscriptions)
    }

    pub(super) fn sync(
        &self,
        settings: &Settings,
        delta: &SettingsDelta,
        window: &mut Window,
        cx: &mut App,
    ) {
        if delta.touches("sync.remote.maxImageMb") {
            self.max_image
                .set_value(u64::from(settings.sync.remote.max_image_mb), window, cx);
        }
    }

    /// 关窗前提交正在编辑的图片上限：数字框只在失焦或回车时保存。
    pub(super) fn commit_if_focused(&self, window: &Window, cx: &mut App) {
        self.max_image.commit_if_focused(window, cx);
    }

    pub(super) fn relabel(&self, window: &mut Window, cx: &mut App) {
        self.url
            .set_placeholder(key("selfHost.urlPlaceholder"), window, cx);
        self.token
            .set_placeholder(key("selfHost.tokenPlaceholder"), window, cx);
    }
}

impl Preferences {
    /// 整个同步页：顶部切换，下面是局域网同步或远程同步。
    pub(super) fn render_sync_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let selected = match self.remote.view {
            SyncView::Lan => "lan",
            SyncView::Remote => "remote",
        };
        let switcher = choices::segmented(
            "sync-view",
            i18n::t("preferences:schema.tabs.sync.title"),
            vec![
                ("lan".into(), key("views.lan")),
                ("remote".into(), key("views.remote")),
            ],
            Some(selected),
            false,
            Rc::new(move |value, _, cx| {
                let view = if value == "remote" {
                    SyncView::Remote
                } else {
                    SyncView::Lan
                };
                let _ = entity.update(cx, |this, cx| {
                    if view == SyncView::Lan {
                        this.abandon_remote_code(cx);
                    }
                    this.remote.view = view;
                    cx.notify();
                });
            }),
            cx,
        );
        let body = match self.remote.view {
            SyncView::Lan => self.render_lan_sync(cx),
            SyncView::Remote => self.render_remote_sync(cx),
        };
        div()
            .flex()
            .flex_col()
            .gap(space(5.))
            .child(div().flex().child(switcher))
            .child(body)
            .into_any_element()
    }

    fn render_remote_sync(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let page = div().flex().flex_col().gap(space(7.));
        let Some(state) = self.remote.state.clone() else {
            return page
                .child(section_block(
                    None,
                    None,
                    vec![note_row(true, key("starting"), tokens.text.muted, tokens)],
                    tokens,
                ))
                .into_any_element();
        };
        let Some(server) = state.server.clone() else {
            return self.render_remote_setup(page, cx);
        };

        let self_host = matches!(server, RemoteServerView::SelfHost { .. });
        let page = page.child(section_block(
            None,
            None,
            self.remote_options(&state, self_host, cx),
            tokens,
        ));
        let page = page.child(section_block(
            Some(key("server.title")),
            None,
            self.remote_server_rows(&state, &server, cx),
            tokens,
        ));
        let add = Button::new("remote-add-device", key("devices.add"))
            .small()
            .ghost()
            .disabled(self.remote.busy || state.pairing.is_some())
            .on_click(cx.listener(|this, _, window, cx| {
                this.request_remote_code(window, cx);
            }));
        page.child(section_block(
            Some(key("devices.title")),
            Some(add.into_any_element()),
            self.remote_device_rows(&state, cx),
            tokens,
        ))
        .into_any_element()
    }

    /// 还没加入同步组：选服务器，然后建组或用配对码加入。
    fn render_remote_setup(&self, page: gpui::Div, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let entity = cx.entity().downgrade();
        let selected = match self.remote.choice {
            ServerChoice::Official => "official",
            ServerChoice::SelfHost => "selfHost",
        };
        let choice = choices::segmented(
            "remote-server-choice",
            key("server.title"),
            vec![
                ("official".into(), key("server.official")),
                ("selfHost".into(), key("server.selfHost")),
            ],
            Some(selected),
            self.remote.busy,
            Rc::new(move |value, _, cx| {
                let choice = if value == "selfHost" {
                    ServerChoice::SelfHost
                } else {
                    ServerChoice::Official
                };
                let _ = entity.update(cx, |this, cx| {
                    this.remote.choice = choice;
                    cx.notify();
                });
            }),
            cx,
        );
        let mut server = vec![setting_row(
            true,
            key("server.title"),
            SharedString::default(),
            Some(choice),
            tokens,
        )];
        match self.remote.choice {
            ServerChoice::Official => server.push(note_row(
                false,
                i18n::t_args(
                    "preferences:remoteSync.official.summary",
                    &[("hours", "24")],
                ),
                tokens.text.secondary,
                tokens,
            )),
            ServerChoice::SelfHost => {
                server.push(setting_row(
                    false,
                    key("selfHost.url"),
                    SharedString::default(),
                    Some(
                        Input::new(&self.remote.url)
                            .width(CONTROL_WIDTH)
                            .disabled(self.remote.busy)
                            .accessibility_label(key("selfHost.url"))
                            .into_any_element(),
                    ),
                    tokens,
                ));
                server.push(setting_row(
                    false,
                    key("selfHost.token"),
                    key("selfHost.tokenDescription"),
                    Some(
                        Input::new(&self.remote.token)
                            .width(CONTROL_WIDTH)
                            .disabled(self.remote.busy)
                            .accessibility_label(key("selfHost.token"))
                            .into_any_element(),
                    ),
                    tokens,
                ));
                server.push(note_row(
                    false,
                    key("selfHost.summary"),
                    tokens.text.secondary,
                    tokens,
                ));
            }
        }

        let create = Button::new("remote-create", key("setup.create"))
            .primary()
            .disabled(self.remote.busy)
            .on_click(cx.listener(|this, _, window, cx| this.create_remote_group(window, cx)));
        let join = Button::new("remote-join", key("setup.join"))
            .disabled(self.remote.busy)
            .on_click(cx.listener(|this, _, window, cx| this.open_remote_join(window, cx)));
        let actions = vec![
            setting_row(
                true,
                key("setup.createTitle"),
                key("setup.createDescription"),
                Some(create.into_any_element()),
                tokens,
            ),
            setting_row(
                false,
                key("setup.joinTitle"),
                key("setup.joinDescription"),
                Some(join.into_any_element()),
                tokens,
            ),
        ];
        page.child(section_block(None, None, server, tokens))
            .child(section_block(None, None, actions, tokens))
            .into_any_element()
    }

    /// 已加入同步组时的选项：开关、设备名、同步内容；自部署服务器多出图片质量、上限和全部历史。
    fn remote_options(
        &self,
        state: &RemoteSyncState,
        self_host: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = theme::semantic(cx);
        let remote = &self.settings.sync.remote;
        let policy = state.policy.as_ref();
        let images_allowed =
            policy.is_none_or(|policy| !matches!(policy.images, RemoteImages::Off));
        let mut rows = vec![
            setting_row(
                true,
                schema_title("enabled"),
                if self_host {
                    key("enabledSelfHost")
                } else {
                    key("enabledOfficial")
                },
                Some(self.render_lan_switch(
                    "sync.remote.enabled",
                    remote.enabled,
                    schema_title("enabled"),
                    cx,
                )),
                tokens,
            ),
            setting_row(
                false,
                i18n::t("preferences:schema.settings.sync.lan.deviceName.title"),
                key("deviceNameDescription"),
                Some(
                    Input::new(&self.lan_name)
                        .width(CONTROL_WIDTH)
                        .into_any_element(),
                ),
                tokens,
            ),
            setting_row(
                false,
                schema_title("text"),
                SharedString::default(),
                Some(self.render_lan_switch(
                    "sync.remote.text",
                    remote.text,
                    schema_title("text"),
                    cx,
                )),
                tokens,
            ),
        ];
        if images_allowed {
            rows.push(setting_row(
                false,
                schema_title("image"),
                if self_host {
                    SharedString::default()
                } else {
                    key("official.imageNote")
                },
                Some(self.render_lan_switch(
                    "sync.remote.image",
                    remote.image,
                    schema_title("image"),
                    cx,
                )),
                tokens,
            ));
        }
        if policy.is_none_or(|policy| policy.settings) {
            rows.push(setting_row(
                false,
                schema_title("settings"),
                i18n::t("preferences:schema.settings.sync.remote.settings.description"),
                Some(self.render_lan_switch(
                    "sync.remote.settings",
                    remote.settings,
                    schema_title("settings"),
                    cx,
                )),
                tokens,
            ));
        }
        rows.push(setting_row(
            false,
            schema_title("writeClipboard"),
            i18n::t("preferences:schema.settings.sync.lan.writeClipboard.description"),
            Some(self.render_lan_switch(
                "sync.remote.writeClipboard",
                remote.write_clipboard,
                schema_title("writeClipboard"),
                cx,
            )),
            tokens,
        ));
        if !self_host {
            return rows;
        }

        if images_allowed {
            let entity = cx.entity().downgrade();
            let quality = match remote.image_quality {
                RemoteImageQuality::Original => "original",
                RemoteImageQuality::High => "high",
                RemoteImageQuality::Saver => "saver",
            };
            rows.push(setting_row(
                false,
                schema_title("imageQuality"),
                i18n::t("preferences:schema.settings.sync.remote.imageQuality.description"),
                Some(choices::segmented(
                    "remote-image-quality",
                    schema_title("imageQuality"),
                    ["original", "high", "saver"]
                        .into_iter()
                        .map(|value| {
                            (
                                value.into(),
                                i18n::t(&format!(
                                    "preferences:schema.settings.sync.remote.imageQuality.options.{value}"
                                )),
                            )
                        })
                        .collect(),
                    Some(quality),
                    false,
                    Rc::new(move |value, _, cx| {
                        let value = value.to_owned();
                        let _ = entity.update(cx, |this, cx| {
                            this.update("sync.remote.imageQuality", json!(value), cx);
                        });
                    }),
                    cx,
                )),
                tokens,
            ));
            rows.push(setting_row(
                false,
                schema_title("maxImageMb"),
                i18n::t("preferences:schema.settings.sync.lan.maxImageMb.description"),
                Some(
                    NumberInput::new(&self.remote.max_image)
                        .suffix("MB")
                        .width(CONTROL_WIDTH)
                        .accessibility_label(schema_title("maxImageMb"))
                        .into_any_element(),
                ),
                tokens,
            ));
        }
        if policy.is_none_or(|policy| policy.history) {
            let description = match state.history_upload {
                Some((done, total)) => i18n::t_args(
                    "preferences:remoteSync.historyUploading",
                    &[("done", &done.to_string()), ("total", &total.to_string())],
                ),
                None => i18n::t("preferences:schema.settings.sync.remote.fullHistory.description"),
            };
            rows.push(setting_row(
                false,
                schema_title("fullHistory"),
                description,
                Some(self.render_lan_switch(
                    "sync.remote.fullHistory",
                    remote.full_history,
                    schema_title("fullHistory"),
                    cx,
                )),
                tokens,
            ));
        }
        rows
    }

    /// 服务器那一块：哪台服务器、连接状态、退出同步组。
    fn remote_server_rows(
        &self,
        state: &RemoteSyncState,
        server: &RemoteServerView,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = theme::semantic(cx);
        let (title, description) = match server {
            RemoteServerView::Official => {
                let hours = state
                    .policy
                    .as_ref()
                    .and_then(|policy| policy.text_retention_hours)
                    .unwrap_or(24)
                    .to_string();
                (
                    key("server.official"),
                    // 这一行右边有按钮，放不下完整说明，只写一句短的。
                    i18n::t_args(
                        "preferences:remoteSync.official.short",
                        &[("hours", &hours)],
                    ),
                )
            }
            RemoteServerView::SelfHost { url } => (key("server.selfHost"), url.clone().into()),
        };
        let leave = Button::new("remote-leave", key("server.leave"))
            .disabled(self.remote.busy)
            .on_click(cx.listener(|this, _, window, cx| this.leave_remote_group(window, cx)));
        let mut rows = vec![setting_row(
            true,
            title,
            description,
            Some(leave.into_any_element()),
            tokens,
        )];
        let (status, color) = remote_status(state, tokens);
        rows.push(note_row(false, status, color, tokens));
        if let Some(until) = state.image_quota_until {
            rows.push(note_row(
                false,
                i18n::t_args(
                    "preferences:remoteSync.status.imageQuota",
                    &[("time", &local_time(until))],
                ),
                tokens.status.warning.solid,
                tokens,
            ));
        }
        rows
    }

    /// 设备那一块：正在显示的配对码排在最前，然后是组里的设备（本机不能在这里移除）。
    fn remote_device_rows(
        &self,
        state: &RemoteSyncState,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = theme::semantic(cx);
        let entity = cx.entity().downgrade();
        let mut rows = Vec::new();
        if let Some(pairing) = &state.pairing {
            let cancel = Button::new("remote-code-cancel", i18n::t("common:actions.cancel"))
                .on_click(cx.listener(|this, _, window, cx| this.cancel_remote_code(window, cx)));
            rows.push(setting_row(
                true,
                i18n::t_args(
                    "preferences:remoteSync.pairing.code",
                    &[("code", &spaced_code(&pairing.code))],
                ),
                i18n::t_args(
                    "preferences:remoteSync.pairing.hint",
                    &[("time", &local_time(pairing.expires_at))],
                ),
                Some(cancel.into_any_element()),
                tokens,
            ));
        }
        for device in state.devices.iter().cloned() {
            let presence = if device.is_self {
                key("devices.self")
            } else if device.online {
                i18n::t("preferences:lanSync.devices.online")
            } else {
                device.last_seen_at.map_or_else(
                    || i18n::t("preferences:lanSync.devices.offline"),
                    |time| {
                        i18n::t_args(
                            "preferences:lanSync.devices.lastSeen",
                            &[("time", &local_date_time(time))],
                        )
                    },
                )
            };
            let description = format!("{} · {presence}", platform_label(device.platform));
            let control = (!device.is_self).then(|| {
                let entity = entity.clone();
                let device = device.clone();
                Button::new(
                    format!("remote-remove-{}", device.id),
                    key("devices.remove"),
                )
                .disabled(self.remote.busy)
                .on_click(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.remove_remote_device(device.clone(), window, cx);
                        });
                    }
                })
                .into_any_element()
            });
            rows.push(setting_row(
                rows.is_empty(),
                device.name.clone().into(),
                description.into(),
                control,
                tokens,
            ));
        }
        if state.devices.len() <= 1 && state.pairing.is_none() {
            rows.push(note_row(
                rows.is_empty(),
                key("devices.alone"),
                tokens.text.muted,
                tokens,
            ));
        }
        rows
    }

    /// 输入框里的服务器；自部署时地址必填。
    fn chosen_remote_server(&self, cx: &App) -> Option<RemoteServer> {
        match self.remote.choice {
            ServerChoice::Official => Some(RemoteServer::Official),
            ServerChoice::SelfHost => {
                let url = self.remote.url.value(cx).trim().to_owned();
                (!url.is_empty()).then(|| RemoteServer::SelfHost {
                    url,
                    token: self.remote.token.value(cx).trim().to_owned(),
                })
            }
        }
    }

    /// 离开远程同步（切到局域网、换页、关窗）时作废正在显示的配对码：码只在用户看着它的时候有效。
    pub(super) fn abandon_remote_code(&mut self, cx: &mut Context<Self>) {
        let showing = self
            .remote
            .state
            .as_ref()
            .is_some_and(|state| state.pairing.is_some());
        let Some(core) = core_host::core(cx).cloned().filter(|_| showing) else {
            return;
        };
        if let Some(state) = self.remote.state.as_mut() {
            state.pairing = None;
        }
        cx.spawn(async move |_, _| {
            if let Err(error) = core.cancel_remote_pairing().await {
                log::warn!("cancel remote pairing code failed: {error:#}");
            }
        })
        .detach();
    }

    fn create_remote_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.chosen_remote_server(cx) else {
            toast::show(Toast::error(key("selfHost.urlRequired")), window, cx);
            return;
        };
        self.run_remote(window, cx, key("setup.created"), async move |core| {
            core.create_remote_group(server).await
        });
    }

    fn open_remote_join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.chosen_remote_server(cx) else {
            toast::show(Toast::error(key("selfHost.urlRequired")), window, cx);
            return;
        };
        let code = TextInput::new(key("join.placeholder"), window, cx);
        let content = code.clone();
        let answer = form_dialog(
            DialogSpec::new(key("join.title"))
                .ok_text(key("join.ok"))
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _| {
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(div().kp_text(TextSize::Sm).child(key("join.description")))
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
                let code: String = cx
                    .update(|_, cx| code.value(cx).to_string())
                    .unwrap_or_default()
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect();
                let _ = entity.update_in(cx, |this, window, cx| {
                    this.run_remote(window, cx, key("join.joined"), async move |core| {
                        core.join_remote_group(server, code).await
                    });
                });
            })
            .detach();
    }

    fn request_remote_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_remote(window, cx, SharedString::default(), async move |core| {
            core.request_remote_pairing_code().await.map(|_| ())
        });
    }

    fn cancel_remote_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_remote(window, cx, SharedString::default(), async move |core| {
            core.cancel_remote_pairing().await
        });
    }

    fn remove_remote_device(
        &mut self,
        device: RemoteDeviceView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = i18n::t_args(
            "preferences:remoteSync.devices.removeConfirmTitle",
            &[("name", &device.name)],
        );
        self.confirm_remote(
            title,
            key("devices.removeConfirmContent"),
            key("devices.remove"),
            key("devices.removed"),
            window,
            cx,
            async move |core| core.remove_remote_device(device.id).await,
        );
    }

    fn leave_remote_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_remote(
            key("server.leaveConfirmTitle"),
            key("server.leaveConfirmContent"),
            key("server.leave"),
            key("server.left"),
            window,
            cx,
            async move |core| core.leave_remote_group().await,
        );
    }

    /// 先确认再执行一个远程同步操作。
    #[allow(clippy::too_many_arguments)]
    fn confirm_remote(
        &mut self,
        title: SharedString,
        content: SharedString,
        ok: SharedString,
        success: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
        action: impl AsyncFnOnce(kwikpaste_core::Core) -> kwikpaste_core::Result<()> + 'static,
    ) {
        let answer = form_dialog(
            DialogSpec::new(title)
                .ok_text(ok)
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _| {
                div()
                    .kp_text(TextSize::Sm)
                    .child(content.clone())
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
                let _ = entity.update_in(cx, |this, window, cx| {
                    this.run_remote(window, cx, success, action);
                });
            })
            .detach();
    }

    /// 在后台执行一个远程同步操作：执行期间按钮不可点，成功时（有文案才）提示，失败时提示原因。
    fn run_remote(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        success: SharedString,
        action: impl AsyncFnOnce(kwikpaste_core::Core) -> kwikpaste_core::Result<()> + 'static,
    ) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        self.remote.busy = true;
        cx.notify();
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let result = action(core.clone()).await;
                let _ = entity.update_in(cx, |this, window, cx| {
                    this.remote.busy = false;
                    this.remote.state = Some(core.remote_sync_state());
                    match result {
                        Ok(()) if !success.is_empty() => {
                            toast::show(Toast::success(success), window, cx);
                        }
                        Ok(()) => {}
                        Err(error) => toast::show(Toast::error(error.to_string()), window, cx),
                    }
                    cx.notify();
                });
            })
            .detach();
    }
}

/// 状态行的文字和颜色。
fn remote_status(state: &RemoteSyncState, tokens: &SemanticTokens) -> (SharedString, Hsla) {
    if let Some((done, total)) = state.history_upload {
        return (
            i18n::t_args(
                "preferences:remoteSync.historyUploading",
                &[("done", &done.to_string()), ("total", &total.to_string())],
            ),
            tokens.text.secondary,
        );
    }
    match &state.status {
        RemoteStatus::NotConfigured | RemoteStatus::Paused => {
            (key("status.paused"), tokens.text.muted)
        }
        RemoteStatus::Connecting => (key("status.connecting"), tokens.text.muted),
        RemoteStatus::Connected => (
            state.last_sync_at.map_or_else(
                || key("status.connected"),
                |time| {
                    i18n::t_args(
                        "preferences:remoteSync.status.connectedAt",
                        &[("time", &local_time(time))],
                    )
                },
            ),
            tokens.status.success.solid,
        ),
        RemoteStatus::Offline { message } => (
            i18n::t_args(
                "preferences:remoteSync.status.offline",
                &[("reason", message)],
            ),
            tokens.status.danger.solid,
        ),
        RemoteStatus::RateLimited { until } => (
            i18n::t_args(
                "preferences:remoteSync.status.rateLimited",
                &[("time", &local_time(*until))],
            ),
            tokens.status.warning.solid,
        ),
        RemoteStatus::Revoked => (key("status.revoked"), tokens.status.danger.solid),
        RemoteStatus::ServerTokenInvalid => (key("status.serverToken"), tokens.status.danger.solid),
        RemoteStatus::Incompatible { message } => {
            (message.clone().into(), tokens.status.danger.solid)
        }
    }
}

fn key(name: &str) -> SharedString {
    i18n::t(&format!("preferences:remoteSync.{name}"))
}

fn schema_title(name: &str) -> SharedString {
    i18n::t(&format!(
        "preferences:schema.settings.sync.remote.{name}.title"
    ))
}

fn local_time(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%H:%M").to_string()
}

fn local_date_time(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%m-%d %H:%M").to_string()
}

/// `12345678` → `1234 5678`，读给别人听或照着输都更容易。
fn spaced_code(code: &str) -> String {
    if code.len() == 8 && code.is_ascii() {
        format!("{} {}", &code[..4], &code[4..])
    } else {
        code.to_owned()
    }
}
