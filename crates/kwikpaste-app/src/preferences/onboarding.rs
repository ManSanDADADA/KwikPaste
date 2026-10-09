//! 首次启动引导窗：步骤和设置契约与 1.x `src/pages/Onboarding` 保持一致。

use std::collections::HashMap;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Div, Entity, FocusHandle, Global, ImageSource,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, img, prelude::FluentBuilder as _,
    px, rems, size,
};
use kwikpaste_core::app_ids;
use kwikpaste_core::settings::Settings;
use kwikpaste_ui::{
    Button, ButtonSize, Checkbox, KpStyled as _, ScrollArea, Select, SelectOption, SelectState,
    Switch, kinsoku_wrap,
    theme::{self, TextSize, space},
};
use serde_json::json;

use super::{
    icons::PrefIcon,
    schema::{self, Control},
    text, values, view,
};
use crate::clipboard::view::image_cache::{
    ImageKey, ImageState, KpImageCache, ResizeMode, path_of,
};
use crate::{core_host, i18n, platform::hotkey};

const WINDOW_SIZE: gpui::Size<gpui::Pixels> = size(px(880.), px(640.));
const WINDOW_MIN_SIZE: gpui::Size<gpui::Pixels> = size(px(760.), px(560.));
const WELCOME: usize = 0;
const PERMISSIONS: usize = 1;
const SHORTCUTS: usize = 2;
const IGNORE_APPS: usize = 3;
const DONE: usize = 4;

struct OnboardingWindow {
    handle: AnyWindowHandle,
    view: Entity<Onboarding>,
}

impl Global for OnboardingWindow {}

/// 打开首次引导窗。窗口不抢焦点，但按偏好窗相同的方式带到前台。
pub fn open(cx: &mut App) -> anyhow::Result<()> {
    if let Some(handle) = cx.try_global::<OnboardingWindow>().map(|host| host.handle) {
        return handle.update(cx, |_, window, cx| {
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            view::bring_window_to_front(window, cx);
        });
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(WINDOW_SIZE, cx)),
        window_min_size: Some(WINDOW_MIN_SIZE),
        titlebar: Some(TitlebarOptions {
            title: Some(i18n::t("onboarding:welcome.title")),
            ..Default::default()
        }),
        focus: false,
        ..Default::default()
    };
    let (handle, view) = crate::platform::open_window(options, cx, |window, cx| {
        let view = cx.new(|cx| Onboarding::new(window, cx));
        crate::platform::reveal_after_first_frame(window, cx, |window, cx| {
            view::bring_window_to_front(window, cx);
        });
        view
    })?;
    cx.set_global(OnboardingWindow { handle, view });
    cx.on_window_closed(move |cx, closed_id| {
        if closed_id == handle.window_id() {
            hotkey::resume(cx);
            let _ = cx.remove_global::<OnboardingWindow>();
        }
    })
    .detach();
    Ok(())
}

struct Onboarding {
    step: usize,
    settings: Settings,
    has_permissions: bool,
    source_apps: Vec<kwikpaste_core::ops::ClipboardAppView>,
    recording: Option<&'static str>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    language: SelectState,
    /// 快捷键一步里的下拉选项（鼠标按键唤起），按设置 id 存。
    selects: HashMap<&'static str, SelectState>,
    /// 下拉选项文字所用的语言；切换语言后重新填一遍。
    options_language: i18n::Language,
    /// 上一帧的 rem 与内容区宽度，给避头折行预排用。
    rem: Pixels,
    content_width: Pixels,
    _subscriptions: Vec<Subscription>,
    finishing: bool,
    icons: Entity<KpImageCache>,
}

/// 快捷键一步里下拉选项的候选项（值、当前语言的文字）。
fn select_options(setting: &schema::Setting) -> Vec<SelectOption> {
    text::options(setting)
        .into_iter()
        .map(|(value, label)| SelectOption::new(value, label))
        .collect()
}

impl Onboarding {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = core_host::core(cx).map_or_else(Settings::default, |core| core.settings());
        let has_permissions = core_host::core(cx).is_some_and(|core| {
            #[cfg(target_os = "windows")]
            {
                !core.paths().is_portable()
            }
            #[cfg(target_os = "macos")]
            {
                let _ = core;
                true
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                false
            }
        });
        let steps: usize = if has_permissions { 5 } else { 4 };
        let initial_step = if crate::selftest::active() {
            std::env::var("KP_ONBOARDING_STEP")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0)
        } else {
            usize::try_from(settings.onboarding.last_step).unwrap_or(0)
        };
        let entity = cx.entity().downgrade();
        if let Some(core) = core_host::core(cx).cloned() {
            cx.spawn(async move |_, cx| match core.list_all_apps().await {
                Ok(source_apps) => {
                    let _ = entity.update(cx, |this, cx| {
                        this.source_apps = source_apps;
                        cx.notify();
                    });
                }
                Err(error) => log::warn!("could not load onboarding source apps: {error:#}"),
            })
            .detach();
        }
        let language = SelectState::new(
            vec![
                SelectOption::new(
                    "zh-CN",
                    i18n::t("preferences:schema.settings.appearance.language.options.zh-CN"),
                ),
                SelectOption::new(
                    "en-US",
                    i18n::t("preferences:schema.settings.appearance.language.options.en-US"),
                ),
            ],
            Some(match settings.appearance.language {
                kwikpaste_core::settings::Language::ZhCN => "zh-CN",
                kwikpaste_core::settings::Language::EnUS => "en-US",
            }),
            window,
            cx,
        );
        let language_subscription = language.on_change(cx, |this, value, cx| {
            if let Some(value) = value {
                this.update_settings(json!({ "appearance": { "language": value.as_ref() } }), cx);
            }
        });
        let mut subscriptions = vec![language_subscription];
        let mut selects = HashMap::new();
        let settings_json = values::to_json(&settings);
        for setting in schema::shortcut_settings() {
            let (Control::Select(kind), Some(path)) = (setting.control, setting.path) else {
                continue;
            };
            let numeric = matches!(kind, schema::Options::Numbers(_));
            let selected = values::get_choice(&settings_json, path);
            let state = SelectState::new(select_options(&setting), selected.as_deref(), window, cx);
            subscriptions.push(state.on_change(cx, move |this, value, cx| {
                if let Some(value) = value {
                    this.update_settings(values::choice_patch(path, &value, numeric), cx);
                }
            }));
            selects.insert(setting.id, state);
        }
        let icons = cx.new(|_| KpImageCache::with_capacity(128));
        subscriptions.push(cx.observe(&icons, |_, _, cx| cx.notify()));
        Self {
            step: initial_step.min(steps.saturating_sub(1)),
            settings,
            has_permissions,
            source_apps: Vec::new(),
            recording: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            language,
            selects,
            options_language: i18n::language(),
            rem: window.rem_size(),
            content_width: px(0.),
            _subscriptions: subscriptions,
            finishing: false,
            icons,
        }
    }

    /// 切换语言后，下拉选项的文字换成新语言（选中值不变）。
    fn relabel_options(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let language = i18n::language();
        if self.options_language == language {
            return;
        }
        self.options_language = language;
        for setting in schema::shortcut_settings() {
            if let Some(state) = self.selects.get(setting.id) {
                state.set_options(select_options(&setting), window, cx);
            }
        }
    }

    fn steps(&self) -> Vec<usize> {
        if self.has_permissions {
            vec![WELCOME, PERMISSIONS, SHORTCUTS, IGNORE_APPS, DONE]
        } else {
            vec![WELCOME, SHORTCUTS, IGNORE_APPS, DONE]
        }
    }

    fn current_position(&self) -> usize {
        self.step
    }

    fn current_kind(&self) -> usize {
        self.steps().get(self.step).copied().unwrap_or(WELCOME)
    }

    fn update_settings(&mut self, patch: serde_json::Value, cx: &mut Context<Self>) {
        if let Some(core) = core_host::core(cx).cloned() {
            cx.spawn(
                async move |this, cx| match core.update_settings(patch).await {
                    Ok(settings) => {
                        let _ = this.update(cx, |this, cx| {
                            this.settings = settings;
                            cx.notify();
                        });
                    }
                    Err(error) => log::error!("onboarding settings update failed: {error}"),
                },
            )
            .detach();
        } else {
            let merged = values::merge(values::to_json(&self.settings), patch);
            if let Ok(settings) = serde_json::from_value(merged) {
                self.settings = settings;
            }
            cx.notify();
        }
    }

    fn save_step(&mut self, cx: &mut Context<Self>) {
        self.update_settings(json!({ "onboarding": { "lastStep": self.step } }), cx);
    }

    fn move_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.finish_recording(cx);
        let steps = self.steps();
        let position = self.current_position() as isize + delta;
        let position = position.clamp(0, steps.len().saturating_sub(1) as isize) as usize;
        self.step = position;
        self.save_step(cx);
        cx.notify();
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing {
            return;
        }
        let Some(core) = core_host::core(cx).cloned() else {
            log::error!("onboarding cannot finish without the settings core");
            return;
        };
        self.finishing = true;
        self.finish_recording(cx);
        let patch = json!({ "onboarding": { "completed": true, "lastStep": self.step } });
        let handle = window.window_handle();
        cx.spawn(
            async move |this, cx| match core.update_settings(patch).await {
                Ok(_) => {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                }
                Err(error) => {
                    log::error!("onboarding completion could not be saved: {error}");
                    let _ = this.update(cx, |this, cx| {
                        this.finishing = false;
                        cx.notify();
                    });
                }
            },
        )
        .detach();
        cx.notify();
    }

    fn begin_recording(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(id);
        hotkey::suspend(cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn finish_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        hotkey::resume(cx);
        cx.notify();
    }

    fn capture_shortcut(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.recording else { return };
        if event.keystroke.key.eq_ignore_ascii_case("escape") {
            self.finish_recording(cx);
            return;
        }
        if !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.shift
            && !event.keystroke.modifiers.platform
            && matches!(
                event.keystroke.key.to_ascii_lowercase().as_str(),
                "backspace" | "delete"
            )
        {
            self.save_shortcut(id, String::new(), cx);
            return;
        }
        let Some(shortcut) = view::shortcut_from_keystroke(&event.keystroke) else {
            return;
        };
        if view::shortcut_conflicts_with_settings(id, &shortcut, &self.settings) {
            log::warn!(
                "onboarding shortcut {shortcut:?} conflicts with an existing preference shortcut"
            );
            self.finish_recording(cx);
            return;
        }
        self.save_shortcut(id, shortcut, cx);
        let _ = window;
    }

    fn save_shortcut(&mut self, id: &'static str, value: String, cx: &mut Context<Self>) {
        let Some(path) = shortcut_path(id) else {
            self.finish_recording(cx);
            return;
        };
        self.finish_recording(cx);
        self.update_settings(values::patch(path, json!(value)), cx);
    }

    fn set_excluded_app(&mut self, id: String, excluded: bool, cx: &mut Context<Self>) {
        app_ids::set_app_listed(
            &mut self.settings.clipboard.filters.excluded_app_ids,
            &id,
            excluded,
        );
        self.update_settings(
            json!({ "clipboard": { "filters": { "excludedAppIds": self.settings.clipboard.filters.excluded_app_ids } } }),
            cx,
        );
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let steps = self.steps();
        let position = self.current_position();
        // 每一步一段，走过的和当前步是主色，其余是填充色。
        let segments = (0..steps.len()).map(|index| {
            div()
                .flex_1()
                .h(px(4.))
                .rounded_full()
                .bg(if index <= position {
                    tokens.accent.solid
                } else {
                    tokens.fill.default
                })
        });
        div()
            .flex()
            .flex_col()
            .gap(space(3.))
            .px(space(6.))
            .pt(space(4.))
            .child(
                div()
                    .flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space(1.5))
                            .kp_text(TextSize::Sm)
                            .child(div().text_color(tokens.text.muted).child(format!(
                                "{}/{}",
                                position + 1,
                                steps.len()
                            )))
                            .child(div().text_color(tokens.text.secondary).child(i18n::t(
                                match self.current_kind() {
                                    WELCOME => "onboarding:steps.welcome",
                                    PERMISSIONS => "onboarding:steps.permissions",
                                    SHORTCUTS => "onboarding:steps.shortcuts",
                                    IGNORE_APPS => "onboarding:steps.ignoreApps",
                                    _ => "onboarding:steps.done",
                                },
                            ))),
                    )
                    // 下拉框的根元素会撑满剩余宽度，包一层定宽的容器它才会靠右。
                    .child(
                        div().flex_none().w(rems(8.)).child(
                            Select::new(&self.language)
                                .small()
                                .width(rems(8.))
                                .accessibility_label(i18n::t(
                                    "preferences:schema.settings.appearance.language.title",
                                )),
                        ),
                    ),
            )
            .child(div().flex().gap(space(1.5)).children(segments))
    }

    /// 按 `width` 预排正文字号的文字，避免句号、顿号这类标点落在行首（GPUI 的折行不避头）。
    fn wrap(&self, text: SharedString, width: Pixels, cx: &App) -> SharedString {
        kinsoku_wrap(
            &text,
            width,
            TextSize::Sm.font_size().to_pixels(self.rem),
            cx,
        )
    }

    /// 每一步顶部的标题和说明。
    fn step_heading(&self, title: SharedString, description: SharedString, cx: &App) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .mb(space(2.))
            .child(
                div()
                    .kp_text(TextSize::Lg)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .kp_text(TextSize::Sm)
                    .text_color(theme::semantic(cx).text.secondary)
                    .child(self.wrap(description, self.content_width, cx)),
            )
    }

    /// 浅灰底的圆角块，没有描边和阴影（同剪贴板面板的扁平风格）。
    fn surface_card(&self, cx: &App) -> Div {
        div()
            .flex()
            .flex_col()
            .rounded(theme::radius::LG)
            .bg(theme::semantic(cx).fill.subtle)
            .overflow_hidden()
    }

    /// 权限一行：浅灰圆角块，左边标题和说明，右边按钮。
    fn permission_tile(
        &self,
        title: SharedString,
        description: SharedString,
        action: Button,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        self.surface_card(cx)
            .flex_row()
            .items_center()
            .justify_between()
            .gap(space(4.))
            .px(space(4.))
            .py(space(3.5))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space(1.))
                    .flex_1()
                    .min_w_0()
                    .child(div().kp_text(TextSize::Base).child(title))
                    .child(
                        div()
                            .kp_text(TextSize::Sm)
                            .text_color(tokens.text.muted)
                            .child(description),
                    ),
            )
            .child(div().flex_none().child(action))
            .into_any_element()
    }

    fn render_feature_card(
        &self,
        icon: PrefIcon,
        title: SharedString,
        description: SharedString,
        cx: &mut Context<Self>,
    ) -> Div {
        let tokens = theme::semantic(cx);
        // 三列网格（列间距 12 px）里的一格，再减去左右各 16 px 的内边距。
        let text_width = (self.content_width - space(6.).to_pixels(self.rem)) / 3.
            - space(8.).to_pixels(self.rem);
        let description = self.wrap(description, text_width, cx);
        self.surface_card(cx)
            .gap(space(3.))
            .min_w_0()
            .p(space(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(rems(2.25))
                    .rounded(theme::radius::MD)
                    .bg(tokens.accent.subtle)
                    .child(icon.view(rems(1.25), tokens.accent.solid)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space(1.))
                    .child(
                        div()
                            .kp_text(TextSize::Base)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .kp_text(TextSize::Sm)
                            .text_color(tokens.text.muted)
                            .child(description),
                    ),
            )
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(space(2.))
            .pt(space(6.))
            .child(img(ImageSource::Image(view::logo())).size(rems(3.5)))
            .child(
                div()
                    .mt(space(2.))
                    .kp_text(TextSize::Lg)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(i18n::t("onboarding:welcome.title")),
            )
            .child(
                div()
                    .kp_text(TextSize::Sm)
                    .text_color(theme::semantic(cx).text.secondary)
                    .child(i18n::t("onboarding:welcome.description")),
            )
            .child(
                div()
                    .grid()
                    .w_full()
                    .mt(space(6.))
                    .grid_cols(3)
                    .gap(space(3.))
                    .children([
                        self.render_feature_card(
                            PrefIcon::ClipboardPlus,
                            i18n::t("onboarding:welcome.features.capture.title"),
                            i18n::t("onboarding:welcome.features.capture.description"),
                            cx,
                        ),
                        self.render_feature_card(
                            PrefIcon::Search,
                            i18n::t("onboarding:welcome.features.search.title"),
                            i18n::t("onboarding:welcome.features.search.description"),
                            cx,
                        ),
                        self.render_feature_card(
                            PrefIcon::ClipboardPaste,
                            i18n::t("onboarding:welcome.features.reuse.title"),
                            i18n::t("onboarding:welcome.features.reuse.description"),
                            cx,
                        ),
                    ]),
            )
            .into_any_element()
    }

    fn render_permissions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut rows = Vec::new();
        let description_key = if cfg!(target_os = "macos") {
            "onboarding:permissions.description.macos"
        } else {
            "onboarding:permissions.description.windows"
        };
        #[cfg(target_os = "macos")]
        {
            let accessibility =
                Button::new("onboarding-accessibility", i18n::t("common:actions.open"))
                    .accessibility_label(i18n::t(
                        "preferences:schema.settings.permissions.accessibility.title",
                    ))
                    .on_click(|_, _, _| {
                        if let Err(error) =
                            kwikpaste_os::mac::permissions::open_accessibility_settings()
                        {
                            log::warn!("accessibility settings could not be opened: {error}");
                        }
                    });
            rows.push(self.permission_tile(
                i18n::t("preferences:schema.settings.permissions.accessibility.title"),
                i18n::t("preferences:schema.settings.permissions.accessibility.description"),
                accessibility,
                cx,
            ));
            let disk = Button::new("onboarding-full-disk", i18n::t("common:actions.open"))
                .accessibility_label(i18n::t(
                    "preferences:schema.settings.permissions.fullDiskAccess.title",
                ))
                .on_click(|_, _, _| {
                    if let Err(error) =
                        kwikpaste_os::mac::permissions::open_full_disk_access_settings()
                    {
                        log::warn!("full disk access settings could not be opened: {error}");
                    }
                });
            rows.push(self.permission_tile(
                i18n::t("preferences:schema.settings.permissions.fullDiskAccess.title"),
                i18n::t("preferences:schema.settings.permissions.fullDiskAccess.description"),
                disk,
                cx,
            ));
        }
        #[cfg(target_os = "windows")]
        {
            let admin = Button::new("onboarding-admin", i18n::t("common:actions.open"))
                .accessibility_label(i18n::t(
                    "preferences:schema.settings.permissions.runAsAdministrator.title",
                ))
                .on_click(|_, _, cx| {
                    if let Err(error) = crate::platform::autostart::restart_as_admin(cx) {
                        log::warn!("administrator restart was not started: {error:#}");
                    }
                });
            rows.push(self.permission_tile(
                i18n::t("preferences:schema.settings.permissions.runAsAdministrator.title"),
                i18n::t("preferences:schema.settings.permissions.runAsAdministrator.description"),
                admin,
                cx,
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(space(3.))
            .child(self.step_heading(
                i18n::t("onboarding:permissions.title"),
                i18n::t(description_key),
                cx,
            ))
            .children(rows)
            .into_any_element()
    }

    fn render_shortcuts(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let settings_json = values::to_json(&self.settings);
        let mut rows = Vec::new();
        for setting in schema::shortcut_settings() {
            if setting.is_collapsed(&self.settings) {
                continue;
            }
            let Some(path) = setting.path else { continue };
            let value = values::get(&settings_json, path);
            let id = setting.id;
            let control =
                match setting.control {
                    Control::ShortcutRecorder => {
                        let current = value
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        view::shortcut_recorder_button(
                            id,
                            current,
                            &self.settings,
                            self.recording == Some(id),
                            cx,
                        )
                        .accessibility_label(text::setting_title(&setting))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.begin_recording(id, window, cx)
                        }))
                        .into_any_element()
                    }
                    Control::Switch => {
                        let entity = cx.entity().downgrade();
                        Switch::new(id)
                            .accessibility_label(text::setting_title(&setting))
                            .checked(value.and_then(serde_json::Value::as_bool).unwrap_or(false))
                            .on_change(move |checked, _, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    this.update_settings(values::patch(path, json!(checked)), cx);
                                });
                            })
                            .into_any_element()
                    }
                    Control::Select(_) => match self.selects.get(id) {
                        Some(state) => Select::new(state)
                            .width(rems(12.))
                            .accessibility_label(text::setting_title(&setting))
                            .into_any_element(),
                        None => continue,
                    },
                    _ => continue,
                };
            // 与偏好窗的设置行同一个样子：只在行与行之间画细分隔线。
            let tokens = theme::semantic(cx);
            rows.push(
                view::row_frame(rows.is_empty(), tokens)
                    .child(view::row_label(
                        text::setting_title(&setting),
                        text::setting_description(&setting),
                        tokens,
                    ))
                    .child(div().flex().flex_none().child(control))
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .child(self.step_heading(
                i18n::t("onboarding:shortcuts.title"),
                i18n::t("onboarding:shortcuts.description"),
                cx,
            ))
            .child(div().flex().flex_col().children(rows))
            .into_any_element()
    }

    /// 引导窗里的来源应用图标缓存；图标只解码到物理显示尺寸。
    fn cached_app_icon(
        &self,
        path: Option<&str>,
        tokens: &kwikpaste_ui::theme::SemanticTokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let size = rems(1.25);
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            return view::app_icon(None, tokens);
        };
        let physical = (kwikpaste_ui::theme::px_rems(20.)
            .to_pixels(window.rem_size())
            .as_f32()
            * window.scale_factor())
        .ceil()
        .max(1.) as u32;
        let state = self.icons.update(cx, |icons, cx| {
            icons.request(
                ImageKey {
                    path: path_of(path),
                    width: physical,
                    height: physical,
                    resize: ResizeMode::Contain,
                },
                window,
                cx,
            )
        });
        match state {
            ImageState::Ready(image) => img(ImageSource::Render(image))
                .size(size)
                .into_any_element(),
            ImageState::Loading => div().size(size).into_any_element(),
            ImageState::Failed => view::app_icon(None, tokens),
        }
    }

    /// 忽略应用：浅灰列表块里每行一个应用，勾选框、应用图标和应用名，下面一行灰色小字是路径。
    fn render_ignore_apps(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity().downgrade();
        let tokens = theme::semantic(cx);
        let icon_states: Vec<_> = self
            .source_apps
            .iter()
            .map(|app| self.cached_app_icon(app.icon_path.as_deref(), tokens, window, cx))
            .collect();
        let rows =
            self.source_apps
                .iter()
                .zip(icon_states)
                .enumerate()
                .map(|(index, (app, icon))| {
                    let id = app.id.clone();
                    let name = if app.name.is_empty() {
                        id.clone()
                    } else {
                        app.name.clone()
                    };
                    let checked = app_ids::contains_app(
                        &self.settings.clipboard.filters.excluded_app_ids,
                        &id,
                    );
                    let path = (!app.name.is_empty()).then(|| id.clone());
                    div()
                        .flex()
                        .flex_col()
                        .gap(space(0.5))
                        .px(space(4.))
                        .py(space(2.5))
                        .when(index > 0, |row| {
                            row.border_t_1().border_color(tokens.border.divider)
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(space(2.))
                                .child(
                                    Checkbox::new(format!("onboarding-ignore-{id}"))
                                        .accessibility_label(name.clone())
                                        .checked(checked)
                                        .on_change({
                                            let entity = entity.clone();
                                            let id = id.clone();
                                            move |checked, _, cx| {
                                                if let Some(entity) = entity.upgrade() {
                                                    entity.update(cx, |this, cx| {
                                                        this.set_excluded_app(
                                                            id.clone(),
                                                            checked,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }
                                        }),
                                )
                                .child(
                                    div()
                                        .id(format!("onboarding-ignore-label-{index}"))
                                        .flex()
                                        .flex_1()
                                        .min_w_0()
                                        .items_center()
                                        .gap(space(2.))
                                        .cursor_pointer()
                                        .on_click({
                                            let entity = entity.clone();
                                            move |_, _, cx| {
                                                if let Some(entity) = entity.upgrade() {
                                                    entity.update(cx, |this, cx| {
                                                        this.set_excluded_app(
                                                            id.clone(),
                                                            !checked,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }
                                        })
                                        .child(icon)
                                        .child(div().min_w_0().truncate().child(name)),
                                ),
                        )
                        .children(path.map(|path| {
                            // 与应用名左对齐（16 px 方框、20 px 图标及其间距）。
                            div()
                                .pl(space(13.))
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.muted)
                                .truncate()
                                .child(path)
                        }))
                        .into_any_element()
                });
        div()
            .flex()
            .flex_col()
            .gap(space(3.))
            .child(self.step_heading(
                i18n::t("onboarding:ignoreApps.title"),
                i18n::t("onboarding:ignoreApps.description"),
                cx,
            ))
            .child(self.surface_card(cx).children(rows))
            .child(
                div().flex().justify_end().child(
                    Button::new(
                        "onboarding-open-preferences",
                        i18n::t("onboarding:ignoreApps.manage"),
                    )
                    .small()
                    .ghost()
                    .on_click(|_, _, cx| {
                        if let Err(error) = crate::preferences::open(cx) {
                            log::warn!("could not open preferences from onboarding: {error:#}");
                        }
                    }),
                ),
            )
            .into_any_element()
    }

    fn render_step(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.current_kind() {
            WELCOME => self.render_welcome(cx),
            PERMISSIONS => self.render_permissions(cx),
            SHORTCUTS => self.render_shortcuts(cx),
            IGNORE_APPS => self.render_ignore_apps(window, cx),
            DONE => self.render_done(cx),
            _ => div().into_any_element(),
        }
    }
}

impl Onboarding {
    fn render_done(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(space(2.))
            .pt(space(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(rems(3.5))
                    .rounded_full()
                    .bg(theme::components(cx).onboarding.done_background)
                    .child(
                        PrefIcon::CheckCircle
                            .view(rems(1.75), theme::semantic(cx).status.success.solid),
                    ),
            )
            .child(
                div()
                    .mt(space(2.))
                    .kp_text(TextSize::Lg)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(i18n::t("onboarding:done.title")),
            )
            .child(
                div()
                    .kp_text(TextSize::Sm)
                    .text_color(theme::semantic(cx).text.secondary)
                    .child(i18n::t("onboarding:done.description")),
            )
            .child(
                div()
                    .grid()
                    .w_full()
                    .mt(space(6.))
                    .grid_cols(3)
                    .gap(space(3.))
                    .children([
                        self.render_feature_card(
                            PrefIcon::ClipboardPlus,
                            i18n::t("onboarding:done.cards.open.title"),
                            i18n::t("onboarding:done.cards.open.description"),
                            cx,
                        ),
                        self.render_feature_card(
                            PrefIcon::Search,
                            i18n::t("onboarding:done.cards.search.title"),
                            i18n::t("onboarding:done.cards.search.description"),
                            cx,
                        ),
                        self.render_feature_card(
                            PrefIcon::Settings,
                            i18n::t("onboarding:done.cards.preferences.title"),
                            i18n::t("onboarding:done.cards.preferences.description"),
                            cx,
                        ),
                    ]),
            )
            .into_any_element()
    }
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.relabel_options(window, cx);
        // 内容区：窗口宽减去左右各 24 px 的内边距，最宽 48 rem（见下面的滚动区）。
        self.rem = window.rem_size();
        self.content_width = (window.viewport_size().width - space(12.).to_pixels(self.rem))
            .min(rems(48.).to_pixels(self.rem));
        let steps = self.steps();
        let position = self.current_position();
        let is_last = position + 1 == steps.len();
        let is_first = position == 0;
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.capture_shortcut(event, window, cx)
            }))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(crate::platform::material::shell_surface(
                cx,
                theme::semantic(cx).surface.panel,
            ))
            .text_color(theme::semantic(cx).text.primary)
            .child(self.render_header(cx))
            .child(
                ScrollArea::new("onboarding-scroll", &self.scroll)
                    .flex_1()
                    .px(space(6.))
                    .py(space(6.))
                    .child(
                        div()
                            .w_full()
                            .max_w(rems(48.))
                            .mx_auto()
                            .child(self.render_step(window, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(space(6.))
                    .py(space(4.))
                    .border_t_1()
                    .border_color(theme::semantic(cx).border.divider)
                    // 第一步没有“上一步”，留一个占位让右侧按钮仍靠右。
                    .child(if is_first {
                        div().into_any_element()
                    } else {
                        Button::new("onboarding-back", i18n::t("onboarding:actions.previous"))
                            .size(ButtonSize::Medium)
                            .disabled(self.finishing)
                            .on_click(cx.listener(|this, _, _, cx| this.move_step(-1, cx)))
                            .into_any_element()
                    })
                    .child(
                        div().flex().items_center().gap(space(2.)).children([
                            Button::new("onboarding-skip", i18n::t("onboarding:actions.skip"))
                                .ghost()
                                .disabled(self.finishing)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.finish(window, cx)),
                                )
                                .into_any_element(),
                            Button::new(
                                "onboarding-next",
                                if is_last {
                                    i18n::t("onboarding:actions.finish")
                                } else if is_first {
                                    i18n::t("onboarding:actions.start")
                                } else {
                                    i18n::t("onboarding:actions.next")
                                },
                            )
                            .primary()
                            .disabled(self.finishing)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if is_last {
                                    this.finish(window, cx);
                                } else {
                                    this.move_step(1, cx);
                                }
                            }))
                            .into_any_element(),
                        ]),
                    ),
            )
    }
}

fn shortcut_path(id: &str) -> Option<&'static str> {
    match id {
        "shortcuts.openClipboard" => Some("shortcuts.openClipboard"),
        "shortcuts.openPreference" => Some("shortcuts.openPreference"),
        _ => None,
    }
}
