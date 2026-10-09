use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, FocusHandle, Global, Hsla, Image, ImageSource,
    InteractiveElement as _, IntoElement, KeyDownEvent, Keystroke, ParentElement as _, Rems,
    Render, Role, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Subscription,
    TitlebarOptions, WeakEntity, Window, WindowBounds, WindowOptions, div, img,
    prelude::FluentBuilder as _, px, rems, size,
};
use kwikpaste_core::{
    CoreEvent, StorageLocation, app_ids,
    backup::{self, BackupContainerMode, BackupExportMode, BackupImportStrategy, BackupScope},
    db::overview::{ClearScope, ContentCategory},
    ops::{PreferenceDirectory, StorageOverview},
    readable_export::{ExportFormat, ExportOptions, ExportPreview},
    settings::Settings,
    settings::{CaptureKind, Content, ItemAction, RetentionRule, RetentionUnit},
    sync::{LanDeviceView, LanNearbyView, LanSyncState, PairTarget},
};
use kwikpaste_ui::{
    Button, Checkbox, ConfirmSpec, DialogSpec, Icon, IconName, Input, KpStyled as _, NumberInput,
    NumberInputState, ScrollArea, Select, SelectOption, SelectState, Slider, SliderState, Switch,
    TextInput, form_dialog,
    theme::{self, SemanticTokens, TextSize, px_rems, space},
    toast::{self, Toast},
};
use serde_json::json;
use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use super::{
    icons::PrefIcon,
    schema::{self, Control, PermissionKind, Setting, TabId},
    sortable, text, values,
};
use crate::{
    clipboard::{
        self,
        source::ClipboardSource,
        view::{
            group_dialogs,
            image_cache::{ImageKey, ImageState, KpImageCache, ResizeMode, path_of},
        },
    },
    core_host, i18n,
    platform::{core_events, hotkey},
};

mod image_text;
mod overview;

const WINDOW_MIN_SIZE: gpui::Size<gpui::Pixels> = size(px(960.), px(600.));

struct PreferencesWindow {
    handle: AnyWindowHandle,
}

/// 偏好窗口的真实 AppKit 状态，只供双门控自测读取。
#[cfg(target_os = "macos")]
pub(crate) fn foreground_ready(cx: &mut App) -> bool {
    use kwikpaste_os::mac::window::OrdinaryWindow;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Some(handle) = cx
        .try_global::<PreferencesWindow>()
        .map(|window| window.handle)
    else {
        return false;
    };
    handle
        .update(cx, |_, window, _| {
            let Ok(handle) = HasWindowHandle::window_handle(window) else {
                return false;
            };
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                return false;
            };
            // SAFETY: GPUI 的主线程窗口借用保证这里的 NSView 有效。
            unsafe { OrdinaryWindow::from_raw(handle.ns_view) }
                .is_ok_and(|native| native.is_key_and_visible())
        })
        .unwrap_or(false)
}

impl Global for PreferencesWindow {}

pub(crate) fn logo() -> Arc<Image> {
    static LOGO: std::sync::LazyLock<Arc<Image>> = std::sync::LazyLock::new(|| {
        let bytes: &[u8] = if cfg!(target_os = "macos") {
            include_bytes!("../../assets/logo-mac.png")
        } else {
            include_bytes!("../../assets/logo.png")
        };
        Arc::new(Image::from_bytes(gpui::ImageFormat::Png, bytes.to_vec()))
    });
    LOGO.clone()
}

fn window_size(cx: &App) -> gpui::Size<gpui::Pixels> {
    let scale = cx
        .try_global::<crate::platform::SystemSignals>()
        .map_or(1.0, |signals| signals.text_scale as f32);
    size(px(980. * scale), px(700. * scale))
}

fn initial_tab() -> TabId {
    if crate::selftest::active() {
        match std::env::var("KP_PREFERENCES_TAB").ok().as_deref() {
            Some("shortcuts") => TabId::Shortcuts,
            Some("appearance") => TabId::Appearance,
            Some("capture") => TabId::Capture,
            Some("window") => TabId::Window,
            Some("paste") => TabId::Paste,
            Some("items") => TabId::Items,
            Some("sync") => TabId::Sync,
            Some("overview") => TabId::Overview,
            Some("data") => TabId::Data,
            Some("about") => TabId::About,
            _ => TabId::General,
        }
    } else {
        TabId::General
    }
}

static STORAGE_WARNING_SHOWN: AtomicBool = AtomicBool::new(false);

pub(super) fn open(cx: &mut App) -> anyhow::Result<()> {
    if let Some(handle) = cx
        .try_global::<PreferencesWindow>()
        .map(|window| window.handle)
    {
        let _ = handle.update(cx, |_, window, cx| bring_window_to_front(window, cx));
        return Ok(());
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(window_size(cx), cx)),
        window_min_size: Some(WINDOW_MIN_SIZE),
        titlebar: Some(TitlebarOptions {
            title: Some(i18n::t("preferences:windowTitle")),
            ..Default::default()
        }),
        focus: false,
        ..Default::default()
    };
    let (handle, _) = crate::platform::open_window(options, cx, |window, cx| {
        let view = cx.new(|cx| Preferences::new(window, cx));
        view.update(cx, |this, cx| {
            this.refresh_storage_overview(cx);
            this.refresh_image_text(cx);
        });
        if view
            .read(cx)
            .storage_location
            .as_ref()
            .is_some_and(|location| location.unavailable_custom_path.is_some())
            && !STORAGE_WARNING_SHOWN.swap(true, Ordering::Relaxed)
        {
            toast::show(
                Toast::warning(i18n::t("preferences:storageLocation.unavailableToast")),
                window,
                cx,
            );
        }
        crate::platform::reveal_after_first_frame(window, cx, |window, cx| {
            bring_window_to_front(window, cx);
        });
        view
    })?;
    cx.set_global(PreferencesWindow { handle });
    let window_id = handle.window_id();
    cx.on_window_closed(move |cx, closed_id| {
        if closed_id == window_id {
            let _ = cx.remove_global::<PreferencesWindow>();
        }
    })
    .detach();
    Ok(())
}

pub(super) fn open_import(path: PathBuf, cx: &mut App) -> anyhow::Result<()> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(window_size(cx), cx)),
        window_min_size: Some(WINDOW_MIN_SIZE),
        titlebar: Some(TitlebarOptions {
            title: Some(i18n::t("preferences:windowTitle")),
            ..Default::default()
        }),
        focus: false,
        ..Default::default()
    };
    let (handle, _) = crate::platform::open_window(options, cx, |window, cx| {
        let view = cx.new(|cx| Preferences::new(window, cx));
        view.update(cx, |this, cx| {
            this.refresh_storage_overview(cx);
            this.refresh_image_text(cx);
        });
        crate::platform::reveal_after_first_frame(window, cx, |window, cx| {
            bring_window_to_front(window, cx);
        });
        Preferences::show_import_confirmation(path.clone(), window, cx);
        view
    })?;
    cx.set_global(PreferencesWindow { handle });
    let window_id = handle.window_id();
    cx.on_window_closed(move |cx, closed_id| {
        if closed_id == window_id {
            let _ = cx.remove_global::<PreferencesWindow>();
        }
    })
    .detach();
    Ok(())
}

#[cfg(target_os = "windows")]
pub(super) fn bring_window_to_front(window: &Window, _: &App) {
    use kwikpaste_os::win::foreground::bring_to_front;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let _ = bring_to_front(handle.hwnd.get());
}

#[cfg(target_os = "macos")]
pub(super) fn bring_window_to_front(window: &Window, cx: &App) {
    use kwikpaste_os::mac::window::OrdinaryWindow;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: GPUI 的主线程窗口借用保证 NSView 在此处有效；helper 持有 NSWindow。
    let native = match unsafe { OrdinaryWindow::from_raw(handle.ns_view) } {
        Ok(native) => native,
        Err(error) => {
            log::warn!("could not capture preferences window: {error}");
            return;
        }
    };
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        // 主线程任务在当前 GPUI 借用结束后执行；关闭后的窗口不得重新显示。
        if handle.update(cx, |_, _, _| ()).is_err() {
            return;
        }
        if let Err(error) = native.bring_to_front() {
            log::warn!("could not bring preferences window to front: {error}");
        }
    })
    .detach();
}

struct Preferences {
    tab: TabId,
    settings: Settings,
    search: TextInput,
    _subscriptions: Vec<Subscription>,
    appearance: SelectState,
    language: SelectState,
    selects: std::collections::HashMap<&'static str, SelectState>,
    number_inputs: std::collections::HashMap<&'static str, NumberInputState>,
    sliders: std::collections::HashMap<&'static str, SliderState>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    recording: Option<&'static str>,
    storage_overview: Option<StorageOverview>,
    storage_location: Option<StorageLocation>,
    storage_migrating: bool,
    lan_state: Option<LanSyncState>,
    lan_code_hidden: bool,
    /// 图片文字识别的计数（采集页状态行），打开窗口和收到 `OcrChanged` 时刷新。
    ocr_status: Option<kwikpaste_core::OcrStatus>,
    /// 系统的识别能力；只在开着识别时探测。
    ocr_support: Option<kwikpaste_core::OcrSupport>,
    lan_name: TextInput,
    lan_max_image: NumberInputState,
    icons: gpui::Entity<KpImageCache>,
}

impl Preferences {
    /// 偏好窗口里的来源应用图标缓存；图标只保留物理显示尺寸的位图。
    pub(super) fn cached_app_icon(
        &self,
        path: Option<&str>,
        tokens: &kwikpaste_ui::theme::SemanticTokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let size = rems(1.25);
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            return div()
                .size(size)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Monitor)
                        .size(rems(1.1))
                        .color(tokens.text.muted),
                )
                .into_any_element();
        };
        let physical = (px_rems(20.).to_pixels(window.rem_size()).as_f32() * window.scale_factor())
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
                .flex_none()
                .into_any_element(),
            ImageState::Loading => div().size(size).flex_none().into_any_element(),
            ImageState::Failed => div()
                .size(size)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Monitor)
                        .size(rems(1.1))
                        .color(tokens.text.muted),
                )
                .into_any_element(),
        }
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = core_host::core(cx).map_or_else(Settings::default, |core| core.settings());
        let icons = cx.new(|_| KpImageCache::with_capacity(128));
        let search = TextInput::new(i18n::t("preferences:search.placeholder"), window, cx);
        let search_subscription = search.on_change(cx, |_, _, cx| cx.notify());
        let theme = settings.appearance.theme;
        let language_value = settings.appearance.language;
        let appearance = SelectState::new(
            vec![
                SelectOption::new(
                    "auto",
                    i18n::t("preferences:schema.settings.appearance.theme.options.auto"),
                ),
                SelectOption::new(
                    "light",
                    i18n::t("preferences:schema.settings.appearance.theme.options.light"),
                ),
                SelectOption::new(
                    "dark",
                    i18n::t("preferences:schema.settings.appearance.theme.options.dark"),
                ),
            ],
            Some(match theme {
                kwikpaste_core::settings::Theme::Auto => "auto",
                kwikpaste_core::settings::Theme::Light => "light",
                kwikpaste_core::settings::Theme::Dark => "dark",
            }),
            window,
            cx,
        );
        let language = SelectState::new(
            vec![
                SelectOption::new(
                    "zh-CN",
                    i18n::t("preferences:schema.settings.appearance.language.options.zh-CN"),
                ),
                SelectOption::new("en-US", "English"),
            ],
            Some(match language_value {
                kwikpaste_core::settings::Language::ZhCN => "zh-CN",
                kwikpaste_core::settings::Language::EnUS => "en-US",
            }),
            window,
            cx,
        );
        let appearance_sub = appearance.on_change(cx, |this, value, cx| {
            if let Some(value) = value {
                this.update("appearance.theme", json!(value.as_ref()), cx);
            }
        });
        let language_sub = language.on_change(cx, |this, value, cx| {
            if let Some(value) = value {
                this.update("appearance.language", json!(value.as_ref()), cx);
            }
        });
        let lan_name = TextInput::new(
            i18n::t("preferences:schema.settings.sync.lan.deviceName.placeholder"),
            window,
            cx,
        );
        lan_name.set_value(settings.sync.lan.device_name.clone(), window, cx);
        let lan_name_sub = lan_name.on_change(cx, |this, value, cx| {
            this.update("sync.lan.deviceName", json!(value.to_string()), cx);
        });
        let lan_max_image = NumberInputState::new(
            u64::from(settings.sync.lan.max_image_mb),
            kwikpaste_core::settings::LAN_SYNC_MAX_IMAGE_MB_MIN.into(),
            kwikpaste_core::settings::LAN_SYNC_MAX_IMAGE_MB_MAX.into(),
            window,
            cx,
        );
        let lan_max_image_sub = lan_max_image.on_commit(window, cx, |this, value, _, cx| {
            this.update("sync.lan.maxImageMb", json!(value), cx);
        });
        let portable = core_host::core(cx).is_some_and(|core| core.paths().is_portable());
        let storage_location = core_host::core(cx).and_then(|core| core.storage_location().ok());
        let mut selects = std::collections::HashMap::new();
        let mut number_inputs = std::collections::HashMap::new();
        let mut sliders = std::collections::HashMap::new();
        let mut setting_subscriptions = Vec::new();
        let settings_json = values::to_json(&settings);
        for tab in schema::tabs(portable) {
            for section in tab.sections {
                for setting in section.settings {
                    match setting.control {
                        Control::Select(kind) => {
                            let numeric = matches!(kind, schema::Options::Numbers(_));
                            let options = text::options(&setting)
                                .into_iter()
                                .map(|(value, label)| SelectOption::new(value, label))
                                .collect();
                            let selected =
                                values::get_choice(&settings_json, setting.path.unwrap_or(""));
                            let state = SelectState::new(options, selected.as_deref(), window, cx);
                            let path = setting.path;
                            let subscription = state.on_change(cx, move |this, value, cx| {
                                if let (Some(path), Some(value)) = (path, value) {
                                    this.apply_patch(
                                        values::choice_patch(path, &value, numeric),
                                        cx,
                                    );
                                }
                            });
                            setting_subscriptions.push(subscription);
                            selects.insert(setting.id, state);
                        }
                        Control::Tiles(kind) => {
                            let options = kind
                                .values()
                                .iter()
                                .map(|value| {
                                    SelectOption::new(
                                        *value,
                                        i18n::t(&format!(
                                            "preferences:schema.settings.{}.options.{}",
                                            setting.id, value
                                        )),
                                    )
                                })
                                .collect::<Vec<_>>();
                            let selected =
                                values::get_choice(&settings_json, setting.path.unwrap_or(""));
                            let state = SelectState::new(options, selected.as_deref(), window, cx);
                            let path = setting.path;
                            let subscription = state.on_change(cx, move |this, value, cx| {
                                if let (Some(path), Some(value)) = (path, value) {
                                    this.update(path, json!(value.as_ref()), cx);
                                }
                            });
                            setting_subscriptions.push(subscription);
                            selects.insert(setting.id, state);
                        }
                        Control::GroupSelect => {
                            let options = ["all", "preserve", "missingGroup"]
                                .into_iter()
                                .map(|value| {
                                    SelectOption::new(
                                        value,
                                        i18n::t(&format!(
                                            "preferences:schema.settings.{}.options.{}",
                                            setting.id, value
                                        )),
                                    )
                                })
                                .collect::<Vec<_>>();
                            let selected = values::get(&settings_json, setting.path.unwrap_or(""))
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("all")
                                .to_owned();
                            let state = SelectState::new(options, Some(&selected), window, cx);
                            let path = setting.path;
                            let subscription = state.on_change(cx, move |this, value, cx| {
                                if let (Some(path), Some(value)) = (path, value) {
                                    this.update(path, json!(value.as_ref()), cx);
                                }
                            });
                            setting_subscriptions.push(subscription);
                            selects.insert(setting.id, state);
                        }
                        Control::Slider { min, max } => {
                            let value = values::get_u64(&settings_json, setting.path.unwrap_or(""))
                                .clamp(u64::from(min), u64::from(max))
                                as u8;
                            let state = SliderState::new(value, min, max, cx);
                            let path = setting.path;
                            setting_subscriptions.push(state.on_change(cx, |_, cx| cx.notify()));
                            setting_subscriptions.push(state.on_commit(
                                cx,
                                move |this, value, cx| {
                                    if let Some(path) = path {
                                        this.update(path, json!(value), cx);
                                    }
                                },
                            ));
                            sliders.insert(setting.id, state);
                        }
                        Control::Number { min, max, .. } => {
                            let value = values::get(&settings_json, setting.path.unwrap_or(""))
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(min);
                            let state = NumberInputState::new(value, min, max, window, cx);
                            let path = setting.path;
                            let subscription =
                                state.on_commit(window, cx, move |this, value, _, cx| {
                                    if let Some(path) = path {
                                        this.update(path, json!(value), cx);
                                    }
                                });
                            setting_subscriptions.push(subscription);
                            number_inputs.insert(setting.id, state);
                        }
                        _ => {}
                    }
                }
            }
        }
        let retention_value = NumberInputState::new(
            u64::from(settings.clipboard.history.retention.value),
            0,
            u64::from(u32::MAX),
            window,
            cx,
        );
        let retention_value_sub = retention_value.on_commit(window, cx, |this, value, _, cx| {
            this.update("clipboard.history.retention.value", json!(value), cx);
        });
        number_inputs.insert("history.retention.value", retention_value);
        let retention_unit = match settings.clipboard.history.retention.unit {
            kwikpaste_core::settings::RetentionUnit::Minutes => "minutes",
            kwikpaste_core::settings::RetentionUnit::Hours => "hours",
            kwikpaste_core::settings::RetentionUnit::Days => "days",
            kwikpaste_core::settings::RetentionUnit::Weeks => "weeks",
            kwikpaste_core::settings::RetentionUnit::Months => "months",
            kwikpaste_core::settings::RetentionUnit::Forever => "forever",
        };
        let retention_unit_state = SelectState::new(
            ["minutes", "hours", "days", "weeks", "months", "forever"]
                .into_iter()
                .map(|value| {
                    SelectOption::new(
                        value,
                        i18n::t(&format!("preferences:schema.retentionUnits.{value}")),
                    )
                })
                .collect(),
            Some(retention_unit),
            window,
            cx,
        );
        let retention_unit_sub = retention_unit_state.on_change(cx, |this, value, cx| {
            if let Some(value) = value {
                this.update(
                    "clipboard.history.retention.unit",
                    json!(value.as_ref()),
                    cx,
                );
            }
        });
        selects.insert("history.retention.unit", retention_unit_state);
        // 材质切换后窗口重套背板，分层底色也要跟着重画；选项显示的仍是用户的设置值（同 1.x）。
        let material_subscription =
            cx.observe_global::<crate::platform::material::WindowMaterial>(|_, cx| cx.notify());
        let hotkey_status_subscription =
            cx.observe_global::<hotkey::RegistrationStatus>(|_, cx| cx.notify());
        // 截图验收（只在自测里）：`KP_PREFERENCES_DEMO` 为 rule / export / export-readable / apps
        // 时打开对应的弹框。
        // 弹框挂在窗口的 Root 上，要等 `open_window` 装好 Root 之后才能弹。
        if crate::selftest::active()
            && let Ok(demo) = std::env::var("KP_PREFERENCES_DEMO")
        {
            cx.spawn_in(window, async move |this, cx| {
                let _ = this.update_in(cx, |this, window, cx| match demo.as_str() {
                    "rule" => this.add_retention_rule(window, cx),
                    "export" => this.open_export(ExportKind::Backup, window, cx),
                    "export-readable" => this.open_export(ExportKind::Readable, window, cx),
                    "apps" => this.open_source_apps(window, cx),
                    other => log::warn!("unknown preferences demo {other}"),
                });
            })
            .detach();
        }
        let lan_state = core_host::core(cx).map(|core| core.lan_sync_state());
        let subscriptions: Vec<Subscription> = core_events(cx)
            .map(|events| {
                cx.subscribe(&events, |this, _, event: &CoreEvent, cx| {
                    if matches!(
                        event,
                        CoreEvent::LanSyncChanged | CoreEvent::LanDevicePaired { .. }
                    ) && let Some(core) = core_host::core(cx)
                    {
                        this.lan_state = Some(core.lan_sync_state());
                        cx.notify();
                    }
                    // 识别进度和开关变化：采集页的状态行跟着刷新。
                    if this.tab == TabId::Capture
                        && (matches!(event, CoreEvent::OcrChanged)
                            || matches!(
                                event,
                                CoreEvent::SettingsUpdated { delta, .. }
                                    if delta.touches("clipboard.ocr")
                            ))
                    {
                        this.refresh_image_text(cx);
                    }
                    // 停在数据概览页时，新采集、清理和分组变化都实时反映到统计上。
                    if matches!(event, CoreEvent::ClipboardReloaded) {
                        this.storage_location =
                            core_host::core(cx).and_then(|core| core.storage_location().ok());
                        this.icons.update(cx, |icons, cx| icons.clear(None, cx));
                        cx.notify();
                    }
                    if this.tab == TabId::Overview
                        && matches!(
                            event,
                            CoreEvent::ClipboardUpserted { .. }
                                | CoreEvent::ClipboardCleaned { .. }
                                | CoreEvent::ClipboardReloaded
                                | CoreEvent::GroupsUpdated
                        )
                    {
                        this.refresh_storage_overview(cx);
                    }
                })
            })
            .into_iter()
            .chain([
                search_subscription,
                appearance_sub,
                language_sub,
                lan_name_sub,
                lan_max_image_sub,
                retention_value_sub,
                retention_unit_sub,
                material_subscription,
                hotkey_status_subscription,
            ])
            .chain(setting_subscriptions)
            .collect();
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.observe(&icons, |_, _, cx| cx.notify()));
        Self {
            tab: initial_tab(),
            settings,
            search,
            _subscriptions: subscriptions,
            appearance,
            language,
            selects,
            number_inputs,
            sliders,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            recording: None,
            storage_overview: None,
            storage_location,
            storage_migrating: false,
            lan_state,
            lan_code_hidden: false,
            ocr_status: None,
            ocr_support: None,
            lan_name,
            lan_max_image,
            icons,
        }
    }

    fn update(&mut self, path: &'static str, value: serde_json::Value, cx: &mut Context<Self>) {
        let mut patch = values::patch(path, value.clone());
        if path == "clipboard.display.density"
            && value == "custom"
            && let Some(seed) = values::custom_density_seed(&self.settings)
        {
            patch = values::merge(patch, seed);
        }
        self.apply_patch(patch, cx);
    }

    /// 所有控件共用同一条落盘路径，不把结构化设置误写成字符串。
    fn apply_patch(&mut self, patch: serde_json::Value, cx: &mut Context<Self>) {
        if let Some(core) = core_host::core(cx).cloned() {
            let patch_for_task = patch.clone();
            cx.spawn(
                async move |this, cx| match core.update_settings(patch_for_task).await {
                    Ok(settings) => {
                        let _ = this.update(cx, |this, cx| {
                            this.settings = settings;
                            cx.notify();
                        });
                    }
                    Err(error) => log::error!("preferences update failed: {error}"),
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
        let Some(id) = self.recording else {
            return;
        };
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
            self.save_recorded_shortcut(id, String::new(), window, cx);
            return;
        }
        let Some(shortcut) = shortcut_from_keystroke(&event.keystroke) else {
            return;
        };
        if shortcut_conflicts_with_settings(id, &shortcut, &self.settings) {
            log::warn!("shortcut {shortcut:?} conflicts with an existing preference shortcut");
            toast::show(
                Toast::error(i18n::t("preferences:controls.shortcutConflict")),
                window,
                cx,
            );
            self.finish_recording(cx);
            return;
        }
        self.save_recorded_shortcut(id, shortcut, window, cx);
    }

    fn save_recorded_shortcut(
        &mut self,
        path_id: &'static str,
        value: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = shortcut_path(path_id) else {
            self.finish_recording(cx);
            return;
        };
        let patch = values::patch(path, json!(value));
        if let Some(core) = core_host::core(cx).cloned() {
            cx.spawn(async move |this, cx| {
                let result = core.update_settings(patch).await;
                let _ = this.update(cx, |this, cx| {
                    this.recording = None;
                    hotkey::resume(cx);
                    match result {
                        Ok(settings) => this.settings = settings,
                        Err(error) => log::error!("shortcut update failed: {error}"),
                    }
                    cx.notify();
                });
            })
            .detach();
        } else {
            let merged = values::merge(values::to_json(&self.settings), patch);
            if let Ok(settings) = serde_json::from_value(merged) {
                self.settings = settings;
            }
            self.finish_recording(cx);
        }
    }

    fn refresh_storage_overview(&mut self, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = core.storage_overview().await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(overview) => this.storage_overview = Some(overview),
                    Err(error) => log::warn!("storage overview failed: {error:#}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn clean_resource_cache(&mut self, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            match core.clean_resource_cache().await {
                Ok(result) => log::info!(
                    "preferences cleaned {} cache files ({} bytes)",
                    result.removed_files,
                    result.removed_bytes
                ),
                Err(error) => log::warn!("preferences cache cleanup failed: {error:#}"),
            }
            if let Ok(overview) = core.storage_overview().await {
                let _ = this.update(cx, |this, cx| {
                    this.storage_overview = Some(overview);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn run_history_cleanup(&mut self, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        cx.spawn(async move |_this, _cx| match core.run_cleanup_now().await {
            Ok(result) => log::info!(
                "preferences history cleanup removed {} items",
                result.removed
            ),
            Err(error) => log::warn!("preferences history cleanup failed: {error:#}"),
        })
        .detach();
    }

    /// 打开确认框后按内容类别或来源应用清理普通记录；收藏与置顶由 core 保留。
    fn clear_storage_scope(
        &self,
        scope: ClearScope,
        title: gpui::SharedString,
        content: gpui::SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let entity = cx.entity().downgrade();
        let answer = form_dialog(
            DialogSpec::new(title)
                .ok_text(i18n::t("preferences:overview.clear.confirm"))
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
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                match core.clear_items_in_scope(scope).await {
                    Ok(removed) => {
                        let _ = entity.update_in(cx, |this, window, cx| {
                            toast::show(
                                Toast::success(i18n::t_args(
                                    "preferences:overview.clear.done",
                                    &[("count", &removed.to_string())],
                                )),
                                window,
                                cx,
                            );
                            this.refresh_storage_overview(cx);
                        });
                    }
                    Err(error) => log::warn!("scoped storage cleanup failed: {error:#}"),
                }
            })
            .detach();
    }

    /// 清空记录（1.x `confirmClearClipboardItems`）：收藏和置顶默认保留，勾选后连带删除。
    fn clear_history(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let delete_favorites = Rc::new(Cell::new(false));
        let delete_pinned = Rc::new(Cell::new(false));
        let answer = form_dialog(
            DialogSpec::new(i18n::t("commands:clearConfirm.title"))
                .ok_text(i18n::t("common:actions.clear"))
                .cancel_text(i18n::t("common:actions.cancel"))
                .danger(),
            {
                let delete_favorites = delete_favorites.clone();
                let delete_pinned = delete_pinned.clone();
                move |_, _| {
                    let toggle = |id: &'static str, key: &str, flag: &Rc<Cell<bool>>| {
                        let flag = flag.clone();
                        Checkbox::new(id)
                            .label(i18n::t(key))
                            .checked(flag.get())
                            .on_change(move |checked, window, _| {
                                flag.set(checked);
                                window.refresh();
                            })
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap(space(3.))
                        .kp_text(TextSize::Sm)
                        .child(i18n::t("commands:clearConfirm.content"))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_x(space(5.))
                                .gap_y(space(2.))
                                .child(toggle(
                                    "clear-history-favorites",
                                    "commands:clearConfirm.deleteFavorites",
                                    &delete_favorites,
                                ))
                                .child(toggle(
                                    "clear-history-pinned",
                                    "commands:clearConfirm.deletePinned",
                                    &delete_pinned,
                                )),
                        )
                        .into_any_element()
                }
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
                let result = core
                    .clear_items(delete_favorites.get(), delete_pinned.get())
                    .await;
                let _ = entity.update_in(cx, |this, window, cx| match result {
                    Ok(removed) => {
                        toast::show(
                            Toast::success(i18n::t_args(
                                "preferences:overview.clear.done",
                                &[("count", &removed.to_string())],
                            )),
                            window,
                            cx,
                        );
                        this.refresh_storage_overview(cx);
                    }
                    Err(error) => {
                        log::warn!("clearing history failed: {error:#}");
                        let message = i18n::t_args(
                            "commands:error",
                            &[
                                ("label", &i18n::t("commands:labels.clearClipboardItems")),
                                ("message", &error.to_string()),
                            ],
                        );
                        toast::show(Toast::error(message), window, cx);
                    }
                });
            })
            .detach();
    }

    /// 确认后恢复默认偏好（历史记录不动）。各控件的状态只在构造时按设置初始化，
    /// 所以重置后整个视图按新设置重建，只保留当前页和滚动位置。
    fn reset_preferences(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let answer = kwikpaste_ui::confirm(
            ConfirmSpec::new(i18n::t(
                "preferences:schema.settings.diagnostics.resetPreferences.confirmTitle",
            ))
            .content(i18n::t(
                "preferences:schema.settings.diagnostics.resetPreferences.confirmContent",
            ))
            .ok_text(i18n::t("common:actions.reset"))
            .cancel_text(i18n::t("common:actions.cancel"))
            .danger(),
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let result = core.reset_settings().await;
                let _ = entity.update_in(cx, |this, window, cx| match result {
                    Ok(_) => {
                        crate::platform::apply_language(cx);
                        let (tab, scroll) = (this.tab, this.scroll.clone());
                        *this = Self::new(window, cx);
                        this.tab = tab;
                        this.scroll = scroll;
                        this.refresh_storage_overview(cx);
                        toast::show(
                            Toast::success(i18n::t("commands:messages.settingsReset")),
                            window,
                            cx,
                        );
                        cx.notify();
                    }
                    Err(error) => {
                        log::warn!("resetting preferences failed: {error:#}");
                        let message = i18n::t_args(
                            "commands:error",
                            &[
                                ("label", &i18n::t("commands:labels.resetSettings")),
                                ("message", &error.to_string()),
                            ],
                        );
                        toast::show(Toast::error(message), window, cx);
                    }
                });
            })
            .detach();
    }

    /// 数据目录行右侧的打开 / 更改 / 还原；迁移中或自定义目录不可用时只能打开。
    fn render_storage_actions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let location = self.storage_location.as_ref();
        let fallback = location.and_then(|location| location.unavailable_custom_path.as_deref());
        let disabled = self.storage_migrating || fallback.is_some();
        let portable = core_host::core(cx).is_some_and(|core| core.paths().is_portable());
        let open = Button::new("storage-open", i18n::t("preferences:storageLocation.open"))
            .disabled(self.storage_migrating)
            .on_click(
                cx.listener(|this, _, _, cx| this.open_directory(PreferenceDirectory::Data, cx)),
            );
        let change = Button::new(
            "storage-change",
            i18n::t("preferences:storageLocation.change"),
        )
        .disabled(disabled)
        .loading(self.storage_migrating)
        .on_click(cx.listener(|this, _, window, cx| this.change_storage_directory(window, cx)));
        let reset = Button::new(
            "storage-reset",
            i18n::t("preferences:storageLocation.reset"),
        )
        .disabled(disabled || !location.is_some_and(|location| location.is_custom))
        .on_click(cx.listener(|this, _, window, cx| this.reset_storage_directory(window, cx)));
        let mut actions = div().flex().items_center().gap(space(2.)).child(open);
        if !portable {
            actions = actions.child(change);
        }
        if !portable
            && location.is_some_and(|location| {
                location.is_custom || location.unavailable_custom_path.is_some()
            })
        {
            actions = actions.child(reset);
        }
        actions.into_any_element()
    }

    fn change_storage_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.storage_migrating
            || self
                .storage_location
                .as_ref()
                .is_some_and(|location| location.unavailable_custom_path.is_some())
        {
            return;
        }
        let task = crate::clipboard::view::pin::prompt_for_paths(
            gpui::PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some(i18n::t("preferences:storageLocation.pickTitle")),
            },
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let Some(paths) = task.await else {
                    return;
                };
                let Some(parent) = paths.into_iter().next() else {
                    return;
                };
                let _ = entity.update_in(cx, |this, window, cx| {
                    if kwikpaste_core::cloud_sync_provider(&parent).is_some() {
                        let answer = kwikpaste_ui::confirm(
                            ConfirmSpec::new(i18n::t(
                                "preferences:storageLocation.cloudConfirmTitle",
                            ))
                            .content(i18n::t("preferences:storageLocation.cloudConfirmContent"))
                            .ok_text(i18n::t("common:actions.continue"))
                            .cancel_text(i18n::t("common:actions.cancel")),
                            window,
                            cx,
                        );
                        let entity = cx.entity().downgrade();
                        window
                            .spawn(cx, async move |cx| {
                                if answer.await.unwrap_or(false) {
                                    let _ = entity.update(cx, |this, cx| {
                                        this.start_storage_change(parent, cx)
                                    });
                                }
                            })
                            .detach();
                    } else {
                        this.start_storage_change(parent, cx);
                    }
                });
            })
            .detach();
    }

    fn start_storage_change(&mut self, parent: PathBuf, cx: &mut Context<Self>) {
        if self.storage_migrating {
            return;
        }
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        self.storage_migrating = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = core.change_storage_location(parent).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.storage_migrating = false;
                match result {
                    Ok(result) => {
                        this.storage_location = Some(result.location);
                        this.storage_overview = None;
                        this.refresh_storage_overview(cx);
                        toast::show(
                            Toast::success(i18n::t("preferences:storageLocation.changed")),
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        log::warn!("storage location switch failed: {error:#}");
                        let message = i18n::t_args(
                            "commands:error",
                            &[
                                ("label", &i18n::t("commands:labels.changeStorageLocation")),
                                ("message", &error.to_string()),
                            ],
                        );
                        toast::show(Toast::error(message), window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reset_storage_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.storage_migrating {
            return;
        }
        let answer = kwikpaste_ui::confirm(
            ConfirmSpec::new(i18n::t("preferences:storageLocation.resetConfirmTitle"))
                .content(i18n::t("preferences:storageLocation.resetConfirmContent"))
                .ok_text(i18n::t("preferences:storageLocation.reset"))
                .cancel_text(i18n::t("common:actions.cancel")),
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if answer.await.unwrap_or(false) {
                    let _ = entity.update(cx, |this, cx| this.start_storage_reset(cx));
                }
            })
            .detach();
    }

    fn start_storage_reset(&mut self, cx: &mut Context<Self>) {
        if self.storage_migrating {
            return;
        }
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        self.storage_migrating = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = core.reset_storage_location().await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.storage_migrating = false;
                match result {
                    Ok(result) => {
                        this.storage_location = Some(result.location);
                        this.storage_overview = None;
                        this.refresh_storage_overview(cx);
                        toast::show(
                            Toast::success(i18n::t("preferences:storageLocation.restored")),
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        log::warn!("storage location switch failed: {error:#}");
                        let message = i18n::t_args(
                            "commands:error",
                            &[
                                ("label", &i18n::t("commands:labels.resetStorageLocation")),
                                ("message", &error.to_string()),
                            ],
                        );
                        toast::show(Toast::error(message), window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_directory(&self, target: PreferenceDirectory, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx) else {
            return;
        };
        match core.preference_directory(target) {
            Ok(path) => cx.reveal_path(&path),
            Err(error) => log::warn!("could not open preference directory: {error:#}"),
        }
    }

    /// 操作按钮的文字取设置项的 `controlLabel`，没有时显示“打开”；危险操作用红色描边按钮。
    fn render_action(
        &self,
        id: &'static str,
        danger: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let label = match id {
            "capture.order" | "history.rules" => {
                i18n::t(&format!("preferences:schema.settings.{id}.title"))
            }
            _ => {
                let label =
                    text::optional(&format!("preferences:schema.settings.{id}.controlLabel"));
                if label.is_empty() {
                    i18n::t("common:actions.open")
                } else {
                    label
                }
            }
        };
        let entity = cx.entity().downgrade();
        Button::new(format!("action-{id}"), label)
            .when(danger, |button| button.danger_outline())
            .on_click(move |_, window, cx| {
                let Some(entity) = entity.upgrade() else {
                    return;
                };
                entity.update(cx, |this, cx| match id {
                    "copy.sound.preview" => this.preview_copy_sound(cx),
                    "backup.export" => this.open_export(ExportKind::Backup, window, cx),
                    "backup.importHistory" => this.import_backup(window, cx),
                    "localData.cleanCache" => this.clean_resource_cache(cx),
                    "history.cleanupStatus" => this.run_history_cleanup(cx),
                    "localData.clearHistory" => this.clear_history(window, cx),
                    "localData.dataDirectory" => this.open_directory(PreferenceDirectory::Data, cx),
                    "localData.logDirectory" => this.open_directory(PreferenceDirectory::Logs, cx),
                    "organizing.customGroups" => this.open_group_manager(window, cx),
                    "source.excludedApps" => {
                        this.open_source_apps_for("source.excludedApps", window, cx)
                    }
                    "shortcuts.pauseApps" => {
                        this.open_source_apps_for("shortcuts.pauseApps", window, cx)
                    }
                    "actions.visible" => this.open_action_visibility(window, cx),
                    "control.reopenOnboarding" => {
                        if let Err(error) = super::open_onboarding(cx) {
                            log::warn!("could not reopen onboarding: {error:#}");
                        }
                    }
                    "about.checkUpdates" => crate::platform::updater::check_now(cx),
                    "about.website" => cx.open_url(text::WEBSITE_URL),
                    "about.github" => cx.open_url(text::REPOSITORY_URL),
                    "diagnostics.resetPreferences" => this.reset_preferences(window, cx),
                    _ => log::info!("preferences action requested: {id}"),
                });
            })
            .into_any_element()
    }

    /// 试听取滑块当前值，不等待异步落盘；平台层在工作线程播放，不阻塞偏好窗。
    fn preview_copy_sound(&self, cx: &App) {
        let volume_percent = self.sliders.get("copy.sound.volume").map_or(
            self.settings.clipboard.feedback.copy_sound_volume.min(100),
            |state| state.value(cx),
        );
        if let Some(core) = core_host::core(cx) {
            core.play_copy_sound(volume_percent);
        }
    }

    /// 拖放按当前位置移入目标行；未启用的格式仍保留在顺序中，避免开关采集类型时丢失位置。
    fn move_capture_kind_to(&mut self, kind: CaptureKind, target: usize, cx: &mut Context<Self>) {
        let mut order = self.settings.clipboard.capture.ordered_kinds();
        if !reorder_capture_kinds(&mut order, kind, target) {
            return;
        }
        match serde_json::to_value(order) {
            Ok(value) => self.update("clipboard.capture.order", value, cx),
            Err(error) => log::warn!("could not encode capture order: {error}"),
        }
    }

    /// 打开快捷动作排序弹框；所有改动先留在弹框实体里，保存时一次写回顺序和勾选项。
    fn open_action_visibility(&self, window: &mut Window, cx: &mut Context<Self>) {
        let order =
            normalized_item_action_order(&self.settings.clipboard.content.item_action_order);
        let dialog = cx.new(|_| ActionVisibilityDialog {
            enabled: self.settings.clipboard.content.item_actions.clone(),
            order,
        });
        let content = dialog.clone();
        let answer = form_dialog(
            DialogSpec::new(i18n::t("preferences:schema.settings.actions.visible.title"))
                .ok_text(i18n::t("common:actions.save"))
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _| content.clone().into_any_element(),
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let Some((order, enabled)) = cx
                    .update(|_, cx| {
                        let dialog = dialog.read(cx);
                        let enabled = dialog
                            .order
                            .iter()
                            .copied()
                            .filter(|action| dialog.enabled.contains(action))
                            .collect::<Vec<_>>();
                        (dialog.order.clone(), enabled)
                    })
                    .ok()
                else {
                    return;
                };
                let patch = values::merge(
                    values::patch("clipboard.content.itemActionOrder", json!(order)),
                    values::patch("clipboard.content.itemActions", json!(enabled)),
                );
                let _ = entity.update(cx, |this, cx| this.apply_patch(patch, cx));
            })
            .detach();
    }

    fn add_retention_rule(&self, window: &mut Window, cx: &mut Context<Self>) {
        let rules = &self.settings.clipboard.history.rules;
        let id = (1..=rules.len() + 1)
            .map(|index| format!("custom-{index}"))
            .find(|candidate| !rules.iter().any(|rule| rule.id == *candidate))
            .unwrap_or_else(|| format!("custom-{}", rules.len() + 1));
        let mut rule = RetentionRule {
            id,
            enabled: true,
            keep: kwikpaste_core::settings::Retention {
                value: 7,
                unit: RetentionUnit::Days,
            },
            ..RetentionRule::default()
        };
        rule.categories = Vec::new();
        self.open_retention_rule_editor(None, rule, window, cx);
    }

    fn update_retention_rules(&mut self, rules: Vec<RetentionRule>, cx: &mut Context<Self>) {
        match serde_json::to_value(rules) {
            Ok(value) => self.update("clipboard.history.rules", value, cx),
            Err(error) => log::warn!("could not encode retention rules: {error}"),
        }
    }

    fn edit_retention_rule(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rule) = self.settings.clipboard.history.rules.get(index).cloned() else {
            return;
        };
        self.open_retention_rule_editor(Some(index), rule, window, cx);
    }

    fn open_retention_rule_editor(
        &self,
        index: Option<usize>,
        base: RetentionRule,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| RetentionRuleEditor::new(&base, window, cx));
        let title_key = if index.is_some() {
            "preferences:retentionRules.form.titleEdit"
        } else {
            "preferences:retentionRules.form.titleCreate"
        };
        let content = editor.clone();
        let answer = form_dialog(
            DialogSpec::new(i18n::t(title_key))
                .ok_text(i18n::t("common:actions.save"))
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _| content.clone().into_any_element(),
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let Some(rule) = cx.update(|_, cx| editor.read(cx).to_rule(&base, cx)).ok() else {
                    return;
                };
                let _ = entity.update(cx, |this, cx| {
                    let mut rules = this.settings.clipboard.history.rules.clone();
                    if let Some(index) = index {
                        if let Some(current) = rules.get_mut(index) {
                            *current = rule;
                        }
                    } else {
                        rules.insert(0, rule);
                    }
                    this.update_retention_rules(rules, cx);
                });
            })
            .detach();
    }

    fn render_capture_order(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let order = self.settings.clipboard.capture.ordered_kinds();
        let rows = order.iter().enumerate().map(|(index, kind)| {
            let kind = *kind;
            let label = capture_kind_label(kind);
            let icon = capture_kind_icon(kind);
            sortable::row(format!("capture-order-{index}"), tokens)
                .when(index > 0, |row| {
                    row.border_t_1().border_color(tokens.border.divider)
                })
                .on_drag(CaptureOrderDrag { kind }, |dragged, _, _, cx| {
                    cx.new(|_| *dragged)
                })
                .drag_over::<CaptureOrderDrag>(move |style, _, _, _| {
                    sortable::drop_target(style, tokens)
                })
                .on_drop(cx.listener(move |this, dragged: &CaptureOrderDrag, _, cx| {
                    this.move_capture_kind_to(dragged.kind, index, cx);
                }))
                .child(
                    div()
                        .flex_none()
                        .cursor_grab()
                        .child(PrefIcon::Grip.view(rems(1.), tokens.text.faint)),
                )
                .child(icon.view(rems(1.), tokens.text.secondary))
                .child(div().flex_1().kp_text(TextSize::Sm).child(label))
                .into_any_element()
        });
        list_tile(tokens).children(rows).into_any_element()
    }

    fn render_retention_rules(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity().downgrade();
        let tokens = theme::semantic(cx);
        let rules = &self.settings.clipboard.history.rules;
        let rows = rules.iter().enumerate().map(|(index, rule)| {
            let rule_id = rule.id.clone();
            let up = entity.clone();
            let down = entity.clone();
            let remove = entity.clone();
            let enabled = rule.enabled;
            let summary = retention_rule_summary(rule);
            let keep = retention_label(rule.keep.value, rule.keep.unit);
            div()
                .flex()
                .items_center()
                .gap(space(2.))
                .px(space(3.))
                .py(space(2.))
                .when(index > 0, |row| {
                    row.border_t_1().border_color(tokens.border.divider)
                })
                .child(
                    Switch::new(format!("retention-enabled-{index}"))
                        .small()
                        .checked(enabled)
                        .accessibility_label(summary.clone())
                        .on_change({
                            let entity = entity.clone();
                            let id = rule_id.clone();
                            move |checked, _, cx| {
                                if let Some(entity) = entity.upgrade() {
                                    entity.update(cx, |this, cx| {
                                        let mut next =
                                            this.settings.clipboard.history.rules.clone();
                                        if let Some(rule) =
                                            next.iter_mut().find(|rule| rule.id == id)
                                        {
                                            rule.enabled = checked;
                                        }
                                        this.update_retention_rules(next, cx);
                                    });
                                }
                            }
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space(0.5))
                        .flex_1()
                        .child(div().kp_text(TextSize::Sm).child(summary))
                        .child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.muted)
                                .child(if enabled {
                                    keep
                                } else {
                                    i18n::t("preferences:retentionRules.disabled")
                                }),
                        ),
                )
                .child(
                    Button::new(
                        format!("retention-up-{index}"),
                        i18n::t("preferences:retentionRules.moveUp"),
                    )
                    .small()
                    .ghost()
                    .disabled(index == 0)
                    .on_click(move |_, _, cx| {
                        if let Some(entity) = up.upgrade() {
                            entity.update(cx, |this, cx| {
                                let mut next = this.settings.clipboard.history.rules.clone();
                                if index > 0 {
                                    next.swap(index, index - 1);
                                    this.update_retention_rules(next, cx);
                                }
                            });
                        }
                    }),
                )
                .child(
                    Button::new(
                        format!("retention-down-{index}"),
                        i18n::t("preferences:retentionRules.moveDown"),
                    )
                    .small()
                    .ghost()
                    .disabled(index + 1 >= rules.len())
                    .on_click(move |_, _, cx| {
                        if let Some(entity) = down.upgrade() {
                            entity.update(cx, |this, cx| {
                                let mut next = this.settings.clipboard.history.rules.clone();
                                if index + 1 < next.len() {
                                    next.swap(index, index + 1);
                                    this.update_retention_rules(next, cx);
                                }
                            });
                        }
                    }),
                )
                .child(
                    Button::new(
                        format!("retention-edit-{index}"),
                        i18n::t("preferences:retentionRules.edit"),
                    )
                    .small()
                    .ghost()
                    .on_click({
                        let entity = entity.clone();
                        move |_, window, cx| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(cx, |this, cx| {
                                    this.edit_retention_rule(index, window, cx);
                                });
                            }
                        }
                    }),
                )
                .child(
                    Button::new(
                        format!("retention-delete-{index}"),
                        i18n::t("preferences:retentionRules.delete"),
                    )
                    .small()
                    .danger_outline()
                    .on_click(move |_, _, cx| {
                        if let Some(entity) = remove.upgrade() {
                            entity.update(cx, |this, cx| {
                                let next = this
                                    .settings
                                    .clipboard
                                    .history
                                    .rules
                                    .iter()
                                    .filter(|rule| rule.id != rule_id)
                                    .cloned()
                                    .collect();
                                this.update_retention_rules(next, cx);
                            });
                        }
                    }),
                )
                .into_any_element()
        });
        let add = entity.clone();
        // 规则列表与采集顺序同样是浅灰底的列表块；“添加规则”是列表下方按内容宽度的小按钮。
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(space(2.))
            .child(if rules.is_empty() {
                list_tile(tokens)
                    .px(space(3.))
                    .py(space(3.))
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.muted)
                    .child(i18n::t("preferences:retentionRules.empty"))
                    .into_any_element()
            } else {
                list_tile(tokens).children(rows).into_any_element()
            })
            .child(
                div().flex().child(
                    Button::new("retention-add", i18n::t("preferences:retentionRules.add"))
                        .small()
                        .with_icon(IconName::Plus)
                        .on_click(move |_, window, cx| {
                            if let Some(entity) = add.upgrade() {
                                entity.update(cx, |this, cx| this.add_retention_rule(window, cx));
                            }
                        }),
                ),
            )
            .into_any_element()
    }

    fn render_app_exclusion(
        &self,
        setting_id: &'static str,
        value: Option<serde_json::Value>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let count = value
            .and_then(|value| {
                value.as_array().map(|items| {
                    app_ids::count_apps(items.iter().filter_map(serde_json::Value::as_str))
                })
            })
            .unwrap_or_default();
        let entity = cx.entity().downgrade();
        Button::new(
            setting_id,
            i18n::t_args(
                "preferences:schema.settings.source.excludedApps.count",
                &[("count", &count.to_string())],
            ),
        )
        .on_click(move |_, window, cx| {
            if let Some(entity) = entity.upgrade() {
                entity.update(cx, |this, cx| {
                    this.open_source_apps_for(setting_id, window, cx)
                });
            }
        })
        .into_any_element()
    }

    /// 采集类型使用与 1.x 相同的五项多选控件，并一次性提交对象补丁。
    fn render_capture_kinds(
        &self,
        value: Option<serde_json::Value>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let capture = value.unwrap_or_default();
        let entity = cx.entity().downgrade();
        div()
            .flex()
            .flex_wrap()
            .gap(space(2.))
            .children(values::CAPTURE_KINDS.into_iter().map(|kind| {
                let checked = capture
                    .get(kind)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let label = i18n::t(&format!("preferences:schema.captureKinds.{kind}"));
                let callback_entity = entity.clone();
                Checkbox::new(format!("capture-kind-{kind}"))
                    .label(label.clone())
                    .accessibility_label(label)
                    .checked(checked)
                    .on_change(move |checked, _, cx| {
                        if let Some(entity) = callback_entity.upgrade() {
                            entity.update(cx, |this, cx| {
                                let current = values::to_json(&this.settings);
                                let selected: Vec<&str> = values::CAPTURE_KINDS
                                    .into_iter()
                                    .filter(|candidate| {
                                        if *candidate == kind {
                                            checked
                                        } else {
                                            values::get(
                                                &current,
                                                &format!("clipboard.capture.{candidate}"),
                                            )
                                            .and_then(serde_json::Value::as_bool)
                                            .unwrap_or(false)
                                        }
                                    })
                                    .collect();
                                this.apply_patch(values::capture_kinds_patch(&selected), cx);
                            });
                        }
                    })
            }))
            .into_any_element()
    }

    fn render_retention(&self, cx: &App) -> gpui::AnyElement {
        let Some(value_state) = self.number_inputs.get("history.retention.value") else {
            return div().into_any_element();
        };
        let Some(unit_state) = self.selects.get("history.retention.unit") else {
            return div().into_any_element();
        };
        let keep_forever = unit_state
            .selected_value(cx)
            .is_some_and(|value| value.as_ref() == "forever");
        // 数值和单位合起来与其他控件同宽；“永久保留”时只剩单位，仍占满这一列。
        div()
            .flex()
            .items_center()
            .gap(space(2.))
            .when(!keep_forever, |row| {
                row.child(NumberInput::new(value_state).width(rems(4.5)))
            })
            .child(
                Select::new(unit_state)
                    .width(if keep_forever {
                        CONTROL_WIDTH
                    } else {
                        rems(7.)
                    })
                    .accessibility_label(i18n::t(
                        "preferences:schema.settings.history.retention.title",
                    )),
            )
            .into_any_element()
    }

    fn refresh_lan_code(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                match core.refresh_lan_pairing_code().await {
                    Ok(state) => {
                        let _ = entity.update(cx, |this, cx| {
                            this.lan_state = Some(state);
                            cx.notify();
                        });
                    }
                    Err(error) => log::warn!("refresh LAN pairing code failed: {error:#}"),
                }
            })
            .detach();
    }

    /// 设备行的配对对话框；附近设备用 id，手动添加时改用地址。
    fn open_lan_pair_dialog(
        &self,
        nearby: Option<LanNearbyView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let address = TextInput::new(
            i18n::t("preferences:lanSync.pairModal.addressPlaceholder"),
            window,
            cx,
        );
        let code = TextInput::new(i18n::t("preferences:lanSync.pairModal.code"), window, cx);
        let address_content = address.clone();
        let code_content = code.clone();
        let nearby_for_content = nearby.clone();
        let title = nearby.as_ref().map_or_else(
            || i18n::t("preferences:lanSync.pairModal.manualTitle"),
            |device| {
                i18n::t_args(
                    "preferences:lanSync.pairModal.title",
                    &[("name", &device.name)],
                )
            },
        );
        let answer = form_dialog(
            DialogSpec::new(title)
                .ok_text(i18n::t("preferences:lanSync.pairModal.ok"))
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _| {
                let mut body = div().flex().flex_col().gap(space(2.));
                if nearby_for_content.is_none() {
                    body = body.child(Input::new(&address_content));
                }
                body.child(Input::new(&code_content)).into_any_element()
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
                let values = cx
                    .update(|_, cx| (address.value(cx).to_string(), code.value(cx).to_string()))
                    .unwrap_or_default();
                let target = nearby.map_or_else(
                    || PairTarget::Address(values.0.trim().to_owned()),
                    |device| PairTarget::Device(device.id),
                );
                match core.pair_lan_device(target, values.1).await {
                    Ok(name) => {
                        let _ = entity.update_in(cx, |_, window, cx| {
                            toast::show(
                                Toast::success(i18n::t_args(
                                    "preferences:lanSync.pairedByOther",
                                    &[("name", &name)],
                                )),
                                window,
                                cx,
                            );
                        });
                    }
                    Err(error) => {
                        let _ = entity.update_in(cx, |_, window, cx| {
                            toast::show(Toast::error(error.to_string()), window, cx);
                        });
                    }
                }
            })
            .detach();
    }

    fn open_lan_connect_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let address = TextInput::new(
            i18n::t("preferences:lanSync.connect.addressPlaceholder"),
            window,
            cx,
        );
        let content = address.clone();
        let answer = form_dialog(
            DialogSpec::new(i18n::t("preferences:lanSync.connect.title"))
                .ok_text(i18n::t("preferences:lanSync.connect.ok"))
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _| div().child(Input::new(&content)).into_any_element(),
            window,
            cx,
        );
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let address = cx
                    .update(|_, cx| address.value(cx).to_string())
                    .unwrap_or_default();
                match core.connect_lan_device(address.trim().to_owned()).await {
                    Ok(name) => {
                        let _ = entity.update_in(cx, |_, window, cx| {
                            toast::show(
                                Toast::success(i18n::t_args(
                                    "preferences:lanSync.connect.success",
                                    &[("name", &name)],
                                )),
                                window,
                                cx,
                            );
                        });
                    }
                    Err(error) => {
                        let _ = entity.update_in(cx, |_, window, cx| {
                            toast::show(Toast::error(error.to_string()), window, cx);
                        });
                    }
                }
            })
            .detach();
    }

    fn remove_lan_device(
        &self,
        device: LanDeviceView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let entity = cx.entity().downgrade();
        let title = i18n::t_args(
            "preferences:lanSync.devices.removeConfirmTitle",
            &[("name", &device.name)],
        );
        let answer = form_dialog(
            DialogSpec::new(title)
                .ok_text(i18n::t("preferences:lanSync.devices.remove"))
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _| {
                div()
                    .kp_text(TextSize::Sm)
                    .child(i18n::t("preferences:lanSync.devices.removeConfirmContent"))
                    .into_any_element()
            },
            window,
            cx,
        );
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                match core.remove_lan_device(device.id).await {
                    Ok(()) => {
                        let _ = entity.update_in(cx, |_, window, cx| {
                            toast::show(
                                Toast::success(i18n::t("preferences:lanSync.devices.removed")),
                                window,
                                cx,
                            );
                        });
                    }
                    Err(error) => {
                        let _ = entity.update_in(cx, |_, window, cx| {
                            toast::show(Toast::error(error.to_string()), window, cx);
                        });
                    }
                }
            })
            .detach();
    }

    /// 同步页：上面是同步开关与选项，下面按“本机”“已配对设备”“附近设备”分组，
    /// 全部用与其他页相同的扁平设置行。
    fn render_lan_sync(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity().downgrade();
        let tokens = theme::semantic(cx);
        let lan = &self.settings.sync.lan;
        let enabled = Switch::new("lan-sync-enabled")
            .accessibility_label(i18n::t(
                "preferences:schema.settings.sync.lan.enabled.title",
            ))
            .checked(lan.enabled)
            .on_change({
                let entity = entity.clone();
                move |checked, _, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.update("sync.lan.enabled", json!(checked), cx);
                        });
                    }
                }
            });
        let options = vec![
            setting_row(
                true,
                i18n::t("preferences:schema.settings.sync.lan.enabled.title"),
                i18n::t("preferences:schema.settings.sync.lan.enabled.description"),
                Some(enabled.into_any_element()),
                tokens,
            ),
            setting_row(
                false,
                i18n::t("preferences:schema.settings.sync.lan.deviceName.title"),
                i18n::t("preferences:schema.settings.sync.lan.deviceName.description"),
                Some(
                    Input::new(&self.lan_name)
                        .width(CONTROL_WIDTH)
                        .into_any_element(),
                ),
                tokens,
            ),
            setting_row(
                false,
                i18n::t("preferences:schema.settings.sync.lan.text.title"),
                gpui::SharedString::default(),
                Some(self.render_lan_switch(
                    "sync.lan.text",
                    lan.text,
                    i18n::t("preferences:schema.settings.sync.lan.text.title"),
                    cx,
                )),
                tokens,
            ),
            setting_row(
                false,
                i18n::t("preferences:schema.settings.sync.lan.image.title"),
                gpui::SharedString::default(),
                Some(self.render_lan_switch(
                    "sync.lan.image",
                    lan.image,
                    i18n::t("preferences:schema.settings.sync.lan.image.title"),
                    cx,
                )),
                tokens,
            ),
            setting_row(
                false,
                i18n::t("preferences:schema.settings.sync.lan.writeClipboard.title"),
                i18n::t("preferences:schema.settings.sync.lan.writeClipboard.description"),
                Some(self.render_lan_switch(
                    "sync.lan.writeClipboard",
                    lan.write_clipboard,
                    i18n::t("preferences:schema.settings.sync.lan.writeClipboard.title"),
                    cx,
                )),
                tokens,
            ),
            setting_row(
                false,
                i18n::t("preferences:schema.settings.sync.lan.maxImageMb.title"),
                i18n::t("preferences:schema.settings.sync.lan.maxImageMb.description"),
                Some(
                    NumberInput::new(&self.lan_max_image)
                        .suffix("MB")
                        .width(CONTROL_WIDTH)
                        .accessibility_label(i18n::t(
                            "preferences:schema.settings.sync.lan.maxImageMb.title",
                        ))
                        .into_any_element(),
                ),
                tokens,
            ),
        ];
        let page = div()
            .flex()
            .flex_col()
            .gap(space(7.))
            .child(section_block(None, None, options, tokens));

        // 没开、正在启动或启动失败时，下面只有一句状态说明。
        let status = if !lan.enabled {
            Some((i18n::t("preferences:lanSync.off"), tokens.text.muted))
        } else {
            match self.lan_state.as_ref() {
                None => Some((i18n::t("preferences:lanSync.starting"), tokens.text.muted)),
                Some(state) if !state.running => Some((
                    state
                        .error
                        .clone()
                        .map_or_else(|| i18n::t("preferences:lanSync.starting"), Into::into),
                    tokens.status.danger.solid,
                )),
                Some(_) => None,
            }
        };
        let Some(state) = self.lan_state.as_ref().filter(|_| status.is_none()) else {
            let (text, color) =
                status.unwrap_or_else(|| (i18n::t("preferences:lanSync.off"), tokens.text.muted));
            return page
                .child(section_block(
                    Some(i18n::t("preferences:lanSync.devices.title")),
                    None,
                    vec![note_row(true, text, color, tokens)],
                    tokens,
                ))
                .into_any_element();
        };

        let port = state.port.unwrap_or(kwikpaste_core::sync::DEFAULT_PORT);
        let addresses = if state.addresses.is_empty() {
            i18n::t("preferences:lanSync.thisDevice.noAddress").to_string()
        } else {
            state
                .addresses
                .iter()
                .map(|address| format_socket_address(address, port))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        let code = state
            .pairing_code
            .as_deref()
            .map_or_else(String::new, ToOwned::to_owned);
        let code_label = if code.is_empty() {
            i18n::t("preferences:lanSync.pairingCode.exhausted")
        } else {
            i18n::t_args(
                "preferences:lanSync.pairingCode.hint",
                &[("count", &state.pairing_attempts_left.to_string())],
            )
        };
        let code_toggle = Button::new(
            "lan-code-toggle",
            if self.lan_code_hidden || code.is_empty() {
                "••••••".to_owned()
            } else {
                code.clone()
            },
        )
        .tooltip(if self.lan_code_hidden {
            i18n::t("preferences:lanSync.pairingCode.show")
        } else {
            i18n::t("preferences:lanSync.pairingCode.hide")
        })
        .disabled(code.is_empty())
        .on_click(cx.listener(|this, _, _, cx| {
            this.lan_code_hidden = !this.lan_code_hidden;
            cx.notify();
        }));
        let code_refresh = Button::new(
            "lan-code-refresh",
            i18n::t("preferences:lanSync.pairingCode.refresh"),
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.refresh_lan_code(window, cx);
        }));
        let this_device = vec![
            setting_row(
                true,
                state.device_name.clone().into(),
                addresses.into(),
                None,
                tokens,
            ),
            setting_row(
                false,
                i18n::t("preferences:lanSync.pairingCode.title"),
                code_label,
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(space(2.))
                        .child(code_toggle)
                        .child(code_refresh)
                        .into_any_element(),
                ),
                tokens,
            ),
        ];

        let mut paired = Vec::new();
        for device in state.devices.iter().cloned() {
            let entity = entity.clone();
            let address = device.address.as_deref().map_or_else(
                || i18n::t("preferences:lanSync.devices.offline").to_string(),
                ToOwned::to_owned,
            );
            let presence = if device.online {
                i18n::t("preferences:lanSync.devices.online")
            } else {
                i18n::t("preferences:lanSync.devices.offline")
            };
            let name = device.name.clone();
            let remove = Button::new(
                format!("lan-remove-{}", device.id),
                i18n::t("preferences:lanSync.devices.remove"),
            )
            .on_click(move |_, window, cx| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |this, cx| {
                        this.remove_lan_device(device.clone(), window, cx);
                    });
                }
            });
            paired.push(setting_row(
                paired.is_empty(),
                name.into(),
                format!("{address} · {presence}").into(),
                Some(remove.into_any_element()),
                tokens,
            ));
        }
        if paired.is_empty() {
            paired.push(note_row(
                true,
                i18n::t("preferences:lanSync.devices.empty"),
                tokens.text.muted,
                tokens,
            ));
        }

        let mut nearby = Vec::new();
        for device in state.nearby.iter().cloned() {
            let entity = entity.clone();
            let label = if device.compatible {
                i18n::t("preferences:lanSync.nearby.pair")
            } else {
                i18n::t("preferences:lanSync.nearby.incompatible")
            };
            let description = format!("{} · {}", device.address, platform_label(device.platform));
            let name = device.name.clone();
            let pair = Button::new(format!("lan-pair-{}", device.id), label)
                .primary()
                .disabled(!device.compatible)
                .on_click(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.open_lan_pair_dialog(Some(device.clone()), window, cx);
                        });
                    }
                });
            nearby.push(setting_row(
                nearby.is_empty(),
                name.into(),
                description.into(),
                Some(pair.into_any_element()),
                tokens,
            ));
        }
        if nearby.is_empty() {
            nearby.push(note_row(
                true,
                i18n::t("preferences:lanSync.nearby.empty"),
                tokens.text.muted,
                tokens,
            ));
        }

        let connect = Button::new("lan-connect", i18n::t("preferences:lanSync.connect.manual"))
            .small()
            .ghost()
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_lan_connect_dialog(window, cx);
            }));
        let manual_pair = Button::new(
            "lan-manual-pair",
            i18n::t("preferences:lanSync.nearby.manual"),
        )
        .small()
        .ghost()
        .on_click(cx.listener(|this, _, window, cx| {
            this.open_lan_pair_dialog(None, window, cx);
        }));

        let page = page
            .child(section_block(
                Some(i18n::t("preferences:lanSync.thisDevice.title")),
                None,
                this_device,
                tokens,
            ))
            .child(section_block(
                Some(i18n::t("preferences:lanSync.devices.title")),
                Some(connect.into_any_element()),
                paired,
                tokens,
            ))
            .child(section_block(
                Some(i18n::t("preferences:lanSync.nearby.title")),
                Some(manual_pair.into_any_element()),
                nearby,
                tokens,
            ));
        #[cfg(target_os = "windows")]
        let page = page.child(
            div()
                .px(space(1.))
                .kp_text(TextSize::Xs)
                .text_color(tokens.text.muted)
                .child(i18n::t("preferences:lanSync.firewallHint")),
        );
        page.into_any_element()
    }

    fn render_lan_switch(
        &self,
        path: &'static str,
        checked: bool,
        label: gpui::SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let entity = cx.entity().downgrade();
        Switch::new(path)
            .accessibility_label(label)
            .checked(checked)
            .on_change(move |checked, _, cx| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |this, cx| this.update(path, json!(checked), cx));
                }
            })
            .into_any_element()
    }

    /// 「导出」弹框：备份包和 Excel / Markdown 共用一个弹框，`kind` 决定先显示哪一种。
    fn open_export(&self, kind: ExportKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let dialog = cx.new(|cx| ExportDialog::new(kind, core.clone(), window, cx));
        let readable = dialog.read(cx).readable.downgrade();
        let backup_dialog = dialog.downgrade();
        let groups_core = core.clone();
        window
            .spawn(cx, async move |cx| match groups_core.list_groups().await {
                Ok(groups) => {
                    let _ = backup_dialog.update(cx, |dialog, cx| {
                        dialog.groups = groups.clone();
                        cx.notify();
                    });
                    let _ = readable.update(cx, |dialog, cx| {
                        dialog.groups = groups;
                        dialog.groups_ready = true;
                        cx.notify();
                    });
                }
                Err(error) => log::warn!("load readable export groups failed: {error:#}"),
            })
            .detach();
        let content = dialog.clone();
        let validation_dialog = dialog.downgrade();
        let answer = form_dialog(
            DialogSpec::new(i18n::t("preferences:backup.export.dialogTitle"))
                .ok_text(i18n::t("preferences:readableExport.export"))
                .cancel_text(i18n::t("common:actions.cancel"))
                .validate(move |window, cx| {
                    let Some(dialog) = validation_dialog.upgrade() else {
                        return false;
                    };
                    let Some(error) = dialog.read(cx).validation_error(cx) else {
                        return true;
                    };
                    toast::show(Toast::error(error), window, cx);
                    false
                }),
            move |_, _| content.clone().into_any_element(),
            window,
            cx,
        );
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let Ok(kind) = cx.update(|_, cx| dialog.read(cx).kind) else {
                    return;
                };
                match kind {
                    ExportKind::Backup => export_backup_file(core, dialog, cx).await,
                    ExportKind::Readable => export_readable_files(core, dialog, cx).await,
                }
            })
            .detach();
    }

    fn import_backup(&self, _window: &mut Window, cx: &mut Context<Self>) {
        let prompt = clipboard::view::pin::prompt_for_paths(
            gpui::PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let Some(path) = prompt.await.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            let _ = entity.update_in(cx, |_this, window, cx| {
                Preferences::show_import_confirmation(path, window, cx);
            });
        })
        .detach();
    }

    fn show_import_confirmation(path: PathBuf, window: &mut Window, cx: &mut App) {
        let Ok(mode) = backup::inspect_backup_file(&path) else {
            log::warn!("invalid backup file: {}", path.display());
            return;
        };
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let password = TextInput::new(i18n::t("preferences:backup.import.password"), window, cx);
        let strategy = SelectState::new(
            vec![
                SelectOption::new("merge", i18n::t("preferences:backup.import.strategyMerge")),
                SelectOption::new(
                    "overwrite",
                    i18n::t("preferences:backup.import.strategyOverwrite"),
                ),
            ],
            Some("merge"),
            window,
            cx,
        );
        let import_settings = Rc::new(Cell::new(true));
        let password_for_content = password.clone();
        let strategy_for_content = strategy.clone();
        let settings_for_content = import_settings.clone();
        let answer = form_dialog(
            DialogSpec::new(i18n::t("preferences:backup.import.title"))
                .ok_text(i18n::t("preferences:backup.import.ok"))
                .cancel_text(i18n::t("common:actions.cancel")),
            move |_, _cx| {
                let settings_flag = settings_for_content.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(div().kp_text(TextSize::Sm).child(match mode {
                        BackupContainerMode::Encrypted => {
                            i18n::t("preferences:backup.import.typeEncrypted")
                        }
                        BackupContainerMode::Plain => {
                            i18n::t("preferences:backup.import.typePlain")
                        }
                    }))
                    .child(Input::new(&password_for_content))
                    .child(
                        Select::new(&strategy_for_content)
                            .width(rems(16.))
                            .accessibility_label(i18n::t("preferences:backup.import.strategy")),
                    )
                    .child(
                        Checkbox::new("backup-import-settings")
                            .label(i18n::t("preferences:backup.import.importSettings"))
                            .checked(settings_flag.get())
                            .on_change(move |checked, window, _| {
                                settings_flag.set(checked);
                                window.refresh();
                            }),
                    )
                    .into_any_element()
            },
            window,
            cx,
        );
        window
            .spawn(cx, async move |cx| {
                if !answer.await.unwrap_or(false) {
                    return;
                }
                let (password, strategy) = cx
                    .update(|_, cx| {
                        (
                            password.value(cx).to_string(),
                            strategy
                                .selected_value(cx)
                                .unwrap_or_else(|| "merge".into()),
                        )
                    })
                    .unwrap_or_default();
                let strategy = if strategy == "overwrite" {
                    BackupImportStrategy::Overwrite
                } else {
                    BackupImportStrategy::Merge
                };
                let result = core
                    .import_history_backup(
                        backup::ImportHistoryBackupInput {
                            path: path.clone(),
                            password: (!password.is_empty()).then_some(password),
                            import_settings: import_settings.get(),
                        },
                        strategy,
                    )
                    .await;
                let message = match result {
                    Ok(result) => {
                        log::info!(
                            "history backup imported: {} items, {} skipped, restart_required={}",
                            result.imported_items,
                            result.skipped_items,
                            result.requires_restart
                        );
                        Toast::success(match result.strategy {
                            BackupImportStrategy::Merge => i18n::t_args(
                                "commands:messages.backupImported",
                                &[
                                    ("imported", &result.imported_items.to_string()),
                                    ("skipped", &result.skipped_items.to_string()),
                                ],
                            ),
                            BackupImportStrategy::Overwrite => {
                                i18n::t("commands:messages.backupOverwriteImported")
                            }
                        })
                    }
                    Err(error) => {
                        log::warn!("history backup import failed: {error:#}");
                        Toast::error(i18n::t_args(
                            "commands:error",
                            &[
                                ("label", &i18n::t("commands:labels.importBackup")),
                                ("message", &error.to_string()),
                            ],
                        ))
                    }
                };
                let _ = cx.update(|window, cx| toast::show(message, window, cx));
            })
            .detach();
    }

    fn open_group_manager(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let source: Arc<dyn ClipboardSource> = Arc::new(
            clipboard::source::core_source::CoreSource::new(core.clone()),
        );
        window
            .spawn(cx, async move |cx| {
                let Ok(groups) = source.groups().await else {
                    log::warn!("could not load groups for preferences");
                    return;
                };
                let _ = cx.update(|window, cx| {
                    group_dialogs::manage_groups(source, groups, |_, _| {}, window, cx);
                });
            })
            .detach();
    }

    fn open_source_apps(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_source_apps_for("source.excludedApps", window, cx);
    }

    fn open_source_apps_for(
        &self,
        setting_id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(core) = core_host::core(cx).cloned() else {
            return;
        };
        let (title_key, settings_path, excluded) = if setting_id == "shortcuts.pauseApps" {
            (
                "preferences:schema.settings.shortcuts.pauseApps.title",
                "shortcuts.pauseAppIds",
                self.settings.shortcuts.pause_app_ids.clone(),
            )
        } else {
            (
                "preferences:schema.settings.source.excludedApps.title",
                "clipboard.filters.excludedAppIds",
                self.settings.clipboard.filters.excluded_app_ids.clone(),
            )
        };
        let entity = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let apps = match core.list_all_apps().await {
                    Ok(apps) => apps,
                    Err(error) => {
                        log::warn!("could not load source apps: {error:#}");
                        return;
                    }
                };
                let _ = cx.update(|window, cx| {
                    open_source_apps_dialog(
                        apps,
                        excluded,
                        core,
                        entity,
                        title_key,
                        settings_path,
                        window,
                        cx,
                    );
                });
            })
            .detach();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let portable = core_host::core(cx).is_some_and(|core| core.paths().is_portable());
        let tabs = schema::tabs(portable);
        let tokens = theme::semantic(cx);
        let component_tokens = theme::components(cx).preferences;
        let mut nav = div().flex().flex_col().gap(space(0.5));
        let mut group = None;
        for tab in tabs {
            if group != Some(tab.group) && group.is_some() {
                nav = nav.child(div().h(px(1.)).my(space(1.)).bg(tokens.border.divider));
            }
            group = Some(tab.group);
            let selected = tab.id == self.tab;
            let id = tab.id;
            let title = text::tab_title_of(&tab);
            nav = nav.child(
                div()
                    .id(id.key())
                    .role(Role::Tab)
                    .aria_label(title.clone())
                    .flex()
                    .items_center()
                    .gap(space(2.))
                    .w_full()
                    .h(rems(2.25))
                    .px(space(2.5))
                    .rounded(theme::radius::MD)
                    .kp_text(TextSize::Sm)
                    // 选中项是中性灰底（同剪贴板面板的当前项），悬停浅一档。
                    .when(selected, |item| {
                        item.bg(component_tokens.nav_selected)
                            .text_color(tokens.text.primary)
                    })
                    .when(!selected, |item| {
                        item.text_color(tokens.text.secondary)
                            .hover(|style| style.bg(component_tokens.nav_hover))
                    })
                    .cursor_pointer()
                    .child(tab.icon.view(
                        rems(1.),
                        if selected {
                            tokens.text.primary
                        } else {
                            tokens.text.secondary
                        },
                    ))
                    .child(title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = id;
                        if id == TabId::Overview {
                            this.refresh_storage_overview(cx);
                        }
                        if id == TabId::Capture {
                            this.refresh_image_text(cx);
                        }
                        cx.notify();
                    })),
            );
        }
        let usage = self
            .storage_overview
            .as_ref()
            .map(|overview| overview.usage.total_bytes)
            .unwrap_or(0);
        let limit = self.settings.clipboard.history.storage_limit_mb as u64 * 1024 * 1024;
        let ratio = if limit == 0 {
            0.0
        } else {
            (usage as f32 / limit as f32).clamp(0.0, 1.0)
        };
        let storage_card = div()
            .flex()
            .flex_col()
            .gap(space(1.5))
            .rounded(theme::radius::LG)
            .bg(tokens.fill.subtle)
            .p(space(3.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(1.))
                    .kp_text(TextSize::Sm)
                    .child(PrefIcon::HardDrive.view(rems(1.), tokens.status.success.solid))
                    .child(i18n::t("preferences:storage.title")),
            )
            .child(
                div()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.muted)
                    .child(format!(
                        "{} / {}",
                        text::format_bytes(usage),
                        text::format_bytes(limit)
                    )),
            )
            .child(
                div()
                    .h(px(4.))
                    .w_full()
                    .rounded(theme::radius::SM)
                    .bg(tokens.fill.default)
                    .child(
                        div()
                            .h_full()
                            .rounded(theme::radius::SM)
                            .w(rems(10. * ratio.max(0.12)))
                            .bg(tokens.status.success.solid),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .justify_between()
            .h_full()
            .w(rems(14.))
            .p(space(3.))
            .border_r_1()
            .border_color(tokens.border.divider)
            .bg(crate::platform::material::chrome_surface(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space(2.))
                            .px(space(1.))
                            .child(img(ImageSource::Image(logo())).size(rems(2.5)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(div().kp_text(TextSize::Base).child("KwikPaste"))
                                    .child(
                                        div()
                                            .kp_text(TextSize::Xs)
                                            .text_color(tokens.text.secondary)
                                            .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                                    ),
                            ),
                    )
                    .child(nav),
            )
            .child(storage_card)
    }

    /// 这一项现在要不要显示：折叠的子项不显示；搜索时只显示匹配的。
    fn setting_visible(&self, setting: &Setting, cx: &App) -> bool {
        if setting.is_collapsed(&self.settings) {
            return false;
        }
        if setting.id == "ocr.status" && !self.image_text_row_visible() {
            return false;
        }
        let query = self.search.value(cx);
        if query.is_empty() {
            return true;
        }

        search_matches(
            &query.to_lowercase(),
            &text::setting_title(setting),
            setting.keywords,
        )
    }

    fn render_setting(
        &self,
        setting: &Setting,
        first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if setting.id == "ocr.status" {
            return self.render_image_text_status(first, cx);
        }
        let title = text::setting_title(setting);
        if setting.id == "localData.dataDirectory" {
            let tokens = theme::semantic(cx);
            let location = self.storage_location.as_ref();
            let description = match location {
                _ if self.storage_migrating => {
                    i18n::t("preferences:storageLocation.migrating").to_string()
                }
                Some(location) if location.is_custom => format!(
                    "{} · {}",
                    i18n::t("preferences:storageLocation.custom"),
                    location.current_path
                ),
                Some(location) => location.current_path.clone(),
                None => text::setting_description(setting).to_string(),
            };
            let warning = location
                .and_then(|location| location.unavailable_custom_path.as_deref())
                .map(|path| {
                    div()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.status.warning.solid)
                        .child(i18n::t_args(
                            "preferences:storageLocation.unavailable",
                            &[("path", path)],
                        ))
                });
            return row_frame(first, tokens)
                .child(
                    row_label(title, description.into(), tokens)
                        .when_some(warning, |label, warning| label.child(warning)),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_end()
                        .child(self.render_storage_actions(cx)),
                )
                .into_any_element();
        }
        let description = text::setting_description(setting);
        let path = setting.path;
        let settings_json = values::to_json(&self.settings);
        let value = path.and_then(|path| values::get(&settings_json, path).cloned());
        let control = match setting.control {
            Control::Switch => {
                let configured = value
                    .as_ref()
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let checked = if setting.id == "control.autoStart" {
                    crate::platform::autostart::autostart_registered().unwrap_or(configured)
                } else {
                    configured
                };
                let id = setting.id;
                let entity = cx.entity().downgrade();
                Switch::new(id)
                    .accessibility_label(title.clone())
                    .checked(checked)
                    .on_change(move |checked, _, cx| {
                        if let Some(path) = path {
                            let _ = entity.update(cx, |this, cx| {
                                this.update(path, json!(checked), cx);
                            });
                        }
                    })
                    .into_any_element()
            }
            Control::Select(_) | Control::Tiles(_) | Control::GroupSelect => {
                if let Some(state) = self.selects.get(setting.id) {
                    Select::new(state)
                        .width(CONTROL_WIDTH)
                        .disabled(setting.is_disabled(&self.settings))
                        .accessibility_label(title.clone())
                        .into_any_element()
                } else {
                    div().into_any_element()
                }
            }
            Control::Slider { .. } => {
                if let Some(state) = self.sliders.get(setting.id) {
                    div()
                        .flex()
                        .items_center()
                        .gap(space(3.))
                        .child(
                            Slider::new(state)
                                .width(CONTROL_WIDTH)
                                .disabled(setting.is_disabled(&self.settings))
                                .accessibility_label(title.clone()),
                        )
                        .child(
                            div()
                                .w(rems(3.))
                                .text_right()
                                .child(format!("{}%", state.value(cx))),
                        )
                        .into_any_element()
                } else {
                    div().into_any_element()
                }
            }
            Control::Number { suffix, .. } => {
                if let Some(state) = self.number_inputs.get(setting.id) {
                    NumberInput::new(state)
                        .width(CONTROL_WIDTH)
                        .when_some(suffix, |input, suffix| {
                            input.suffix(text::number_suffix(suffix))
                        })
                        .disabled(setting.is_disabled(&self.settings))
                        .accessibility_label(title.clone())
                        .into_any_element()
                } else {
                    div().into_any_element()
                }
            }
            Control::ShortcutRecorder => {
                let current = value
                    .as_ref()
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let recording = self.recording == Some(setting.id);
                let id = setting.id;
                shortcut_recorder_button(id, current, &self.settings, recording, cx)
                    .accessibility_label(title.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.begin_recording(id, window, cx);
                    }))
                    .into_any_element()
            }
            Control::Permission(PermissionKind::RunAsAdministrator) => {
                let status = crate::platform::autostart::admin_status(cx);
                let label = if status.running_as_admin || status.task_ready {
                    i18n::t("preferences:schema.settings.permissions.runAsAdministrator.title")
                } else {
                    i18n::t("common:actions.open")
                };
                Button::new("restart-as-admin", label)
                    .accessibility_label(title.clone())
                    .on_click(move |_, _, cx| {
                        if let Err(error) = crate::platform::autostart::restart_as_admin(cx) {
                            log::warn!("administrator restart was not started: {error:#}");
                        }
                    })
                    .into_any_element()
            }
            #[cfg(target_os = "macos")]
            Control::Permission(
                kind @ (PermissionKind::Accessibility | PermissionKind::FullDiskAccess),
            ) => Button::new(
                format!("permission-{}", setting.id),
                i18n::t("common:actions.open"),
            )
            .accessibility_label(title.clone())
            .on_click(move |_, _, _| {
                let opened = if kind == PermissionKind::FullDiskAccess {
                    kwikpaste_os::mac::permissions::open_full_disk_access_settings()
                } else {
                    kwikpaste_os::mac::permissions::open_accessibility_settings()
                };
                if let Err(error) = opened {
                    log::warn!("macOS privacy settings could not be opened: {error}");
                }
            })
            .into_any_element(),
            Control::StorageOverview => self.render_storage_overview(window, cx),
            Control::CaptureKinds => self.render_capture_kinds(value, cx),
            Control::CaptureOrder => self.render_capture_order(cx),
            Control::Retention => self.render_retention(cx),
            Control::RetentionRules => self.render_retention_rules(cx),
            Control::AppExclusion => self.render_app_exclusion(setting.id, value, cx),
            Control::Action { danger } => self.render_action(setting.id, danger, cx),
            _ => self.render_action(setting.id, false, cx),
        };
        let tokens = theme::semantic(cx);
        let full_width = setting.control.full_width();
        row_frame(first, tokens)
            .when(full_width, |row| {
                row.flex_col().items_start().gap(space(3.))
            })
            .when(setting.parent.is_some(), |row| row.pl(space(5.)))
            .child(row_label(title, description, tokens).when(full_width, |label| label.w_full()))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .when(full_width, |wrapper| wrapper.w_full().justify_start())
                    .child(control),
            )
            .into_any_element()
    }

    fn render_page(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let portable = core_host::core(cx).is_some_and(|core| core.paths().is_portable());
        let tabs = schema::tabs(portable);
        let tab = tabs.into_iter().find(|tab| tab.id == self.tab);
        let sections = tab.map(|tab| tab.sections).unwrap_or_default();
        let show_titles = sections.len() > 1;
        let tokens = theme::semantic(cx);
        // 分组不套卡片框：小号灰字标题下面直接是扁平的设置行，只在行与行之间画细分隔线。
        // 搜索时没有匹配项的分组整个不显示，一项都没有时给一句空状态。
        let mut blocks: Vec<gpui::AnyElement> = Vec::new();
        for section in sections {
            if self.tab == TabId::Overview {
                blocks.push(self.render_storage_overview(window, cx));
                continue;
            }
            let visible: Vec<&Setting> = section
                .settings
                .iter()
                .filter(|setting| self.setting_visible(setting, cx))
                .collect();
            if visible.is_empty() {
                continue;
            }
            if let [setting] = visible.as_slice()
                && matches!(setting.control, Control::LanSync)
            {
                blocks.push(self.render_lan_sync(cx));
                continue;
            }
            let mut rows = Vec::with_capacity(visible.len());
            for (index, setting) in visible.into_iter().enumerate() {
                rows.push(self.render_setting(setting, index == 0, window, cx));
            }
            blocks.push(section_block(
                show_titles.then(|| text::section_title(&section)),
                None,
                rows,
                tokens,
            ));
        }
        if blocks.is_empty() {
            blocks.push(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(space(3.))
                    .pt(space(16.))
                    .child(
                        div()
                            .flex()
                            .size(rems(3.))
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(tokens.fill.subtle)
                            .child(PrefIcon::Search.view(rems(1.5), tokens.text.muted)),
                    )
                    .child(
                        div()
                            .kp_text(TextSize::Sm)
                            .text_color(tokens.text.muted)
                            .child(i18n::t("preferences:search.empty")),
                    )
                    .into_any_element(),
            );
        }
        let content = ScrollArea::new("preferences-scroll", &self.scroll)
            .flex()
            .flex_col()
            .px(space(7.))
            .py(space(6.))
            .gap(space(7.))
            .children(blocks);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .bg(crate::platform::material::shell_surface(
                cx,
                tokens.surface.panel,
            ))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(rems(4.))
                    .px(space(7.))
                    .border_b_1()
                    .border_color(tokens.border.divider)
                    .child(
                        div()
                            .kp_text(TextSize::Lg)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::tab_title(self.tab)),
                    )
                    .child(Input::search(&self.search).small().width(rems(16.))),
            )
            .child(div().flex_1().min_h_0().overflow_hidden().child(content))
    }
}

/// 导出弹框的两种产物：可完整恢复的 `.kwikpastebak` 备份包，或给人看的 Excel / Markdown。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportKind {
    Backup,
    Readable,
}

/// 备份包里放哪些记录：全部、只要收藏，或指定分组。
#[derive(Clone, Copy, PartialEq, Eq)]
enum BackupRange {
    All,
    Favorites,
    Groups,
}

/// 备份密码的最短长度，与 core 导出时的校验一致。
const BACKUP_PASSWORD_MIN_CHARS: usize = 8;

/// 「导出」弹框的内容：顶部分段切换导出类型，下面是对应的表单；可读导出沿用 [`ReadableExportDialog`]。
struct ExportDialog {
    kind: ExportKind,
    backup_mode: SelectState,
    password: TextInput,
    backup_range: BackupRange,
    backup_group_ids: Vec<String>,
    backup_include_ungrouped: bool,
    groups: Vec<kwikpaste_core::db::models::ClipboardGroup>,
    readable: gpui::Entity<ReadableExportDialog>,
    _subscriptions: Vec<Subscription>,
}

impl ExportDialog {
    fn new(
        kind: ExportKind,
        core: kwikpaste_core::Core,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let backup_mode = SelectState::new(
            vec![
                SelectOption::new(
                    "encrypted",
                    i18n::t("preferences:backup.export.modeEncrypted"),
                ),
                SelectOption::new("plain", i18n::t("preferences:backup.export.modePlain")),
            ],
            Some("encrypted"),
            window,
            cx,
        );
        let mode_subscription = backup_mode.on_change(cx, |_, _, cx| cx.notify());

        Self {
            kind,
            backup_mode,
            password: TextInput::new(i18n::t("preferences:backup.export.passwordMin"), window, cx),
            backup_range: BackupRange::All,
            backup_group_ids: Vec::new(),
            backup_include_ungrouped: false,
            groups: Vec::new(),
            readable: cx.new(|cx| ReadableExportDialog::new(core, window, cx)),
            _subscriptions: vec![mode_subscription],
        }
    }

    fn backup_encrypted(&self, cx: &App) -> bool {
        self.backup_mode
            .selected_value(cx)
            .is_none_or(|mode| mode != "plain")
    }

    fn backup_scope(&self) -> BackupScope {
        match self.backup_range {
            BackupRange::All => BackupScope::default(),
            BackupRange::Favorites => BackupScope {
                favorites_only: true,
                ..BackupScope::default()
            },
            BackupRange::Groups => BackupScope {
                favorites_only: false,
                group_ids: Some(self.backup_group_ids.clone()),
                include_ungrouped: self.backup_include_ungrouped,
            },
        }
    }

    /// 点导出前的校验：返回要提示的错误，`None` 表示可以导出。
    fn validation_error(&self, cx: &App) -> Option<gpui::SharedString> {
        match self.kind {
            ExportKind::Backup => {
                if self.backup_range == BackupRange::Groups
                    && self.backup_group_ids.is_empty()
                    && !self.backup_include_ungrouped
                {
                    return Some(i18n::t("preferences:readableExport.noGroups"));
                }
                if !self.backup_encrypted(cx) {
                    return None;
                }
                let length = self.password.value(cx).chars().count();
                if length == 0 {
                    Some(i18n::t("preferences:backup.export.passwordRequired"))
                } else if length < BACKUP_PASSWORD_MIN_CHARS {
                    Some(i18n::t("preferences:backup.export.passwordMin"))
                } else {
                    None
                }
            }
            ExportKind::Readable => {
                let readable = self.readable.read(cx);
                let valid = readable.preview.as_ref().is_some_and(|preview| {
                    preview.item_count > 0
                        && (!readable.include_sensitive || readable.sensitive_confirmed)
                });
                (!valid).then(|| i18n::t("preferences:readableExport.previewRequired"))
            }
        }
    }

    /// 备份范围：分段切换全部 / 仅收藏 / 指定分组，选指定分组时下面列出分组勾选框。
    fn render_backup_range(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let tokens = theme::semantic(cx);
        let entity = cx.entity().downgrade();
        let ranges = div()
            .flex()
            .flex_none()
            .gap(space(0.5))
            .p(space(0.5))
            .rounded(theme::radius::MD)
            .bg(tokens.fill.subtle)
            .children(
                [
                    (
                        BackupRange::All,
                        "backup-range-all",
                        "preferences:readableExport.allRecords",
                    ),
                    (
                        BackupRange::Favorites,
                        "backup-range-favorites",
                        "preferences:readableExport.favorites",
                    ),
                    (
                        BackupRange::Groups,
                        "backup-range-groups",
                        "preferences:readableExport.selectedGroups",
                    ),
                ]
                .map(|(range, id, key)| {
                    segment(id, i18n::t(key), self.backup_range == range, cx).on_click(cx.listener(
                        move |dialog, _, _, cx| {
                            if dialog.backup_range != range {
                                dialog.backup_range = range;
                                cx.notify();
                            }
                        },
                    ))
                }),
            );
        let groups = (self.backup_range == BackupRange::Groups).then(|| {
            let rows = self.groups.iter().map(|group| {
                let id = group.id.clone();
                let entity = entity.clone();
                Checkbox::new(format!("backup-group-{id}"))
                    .label(group.name.clone())
                    .checked(self.backup_group_ids.contains(&id))
                    .on_change(move |checked, _, cx| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(cx, |dialog, cx| {
                                dialog.backup_group_ids.retain(|value| value != &id);
                                if checked {
                                    dialog.backup_group_ids.push(id.clone());
                                }
                                cx.notify();
                            });
                        }
                    })
            });
            div().flex().flex_col().gap(space(1.)).children(rows).child(
                Checkbox::new("backup-ungrouped")
                    .label(i18n::t("preferences:readableExport.ungrouped"))
                    .checked(self.backup_include_ungrouped)
                    .on_change(move |checked, _, cx| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(cx, |dialog, cx| {
                                dialog.backup_include_ungrouped = checked;
                                cx.notify();
                            });
                        }
                    }),
            )
        });

        div()
            .flex()
            .flex_col()
            .gap(space(2.))
            .child(div().flex().child(ranges))
            .when_some(groups, |field, rows| {
                field.child(list_tile(tokens).px(space(3.)).py(space(2.5)).child(rows))
            })
            .into_any_element()
    }
}

impl Render for ExportDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let kinds = div()
            .flex()
            .flex_none()
            .gap(space(0.5))
            .p(space(0.5))
            .rounded(theme::radius::MD)
            .bg(tokens.fill.subtle)
            .children([ExportKind::Backup, ExportKind::Readable].map(|kind| {
                let (id, label) = match kind {
                    ExportKind::Backup => (
                        "export-kind-backup",
                        i18n::t("preferences:backup.export.kindBackup"),
                    ),
                    ExportKind::Readable => (
                        "export-kind-readable",
                        i18n::t("preferences:backup.export.kindReadable"),
                    ),
                };
                segment(id, label, self.kind == kind, cx).on_click(cx.listener(
                    move |dialog, _, _, cx| {
                        if dialog.kind != kind {
                            dialog.kind = kind;
                            cx.notify();
                        }
                    },
                ))
            }));
        let hint = match (self.kind, self.backup_range) {
            (ExportKind::Backup, BackupRange::All) => {
                i18n::t("preferences:backup.export.kindBackupHint")
            }
            (ExportKind::Backup, _) => i18n::t("preferences:backup.export.partialHint"),
            (ExportKind::Readable, _) => i18n::t("preferences:readableExport.notBackup"),
        };
        let body = match self.kind {
            ExportKind::Backup => div()
                .flex()
                .flex_col()
                .gap(space(4.))
                .pb(space(1.))
                .child(form_field(
                    i18n::t("preferences:backup.export.scope"),
                    self.render_backup_range(cx),
                    tokens,
                ))
                .child(form_field(
                    i18n::t("preferences:backup.export.mode"),
                    div()
                        .flex()
                        .child(
                            Select::new(&self.backup_mode)
                                .width(CONTROL_WIDTH)
                                .accessibility_label(i18n::t("preferences:backup.export.mode")),
                        )
                        .into_any_element(),
                    tokens,
                ))
                .map(|form| {
                    if self.backup_encrypted(cx) {
                        form.child(form_field(
                            i18n::t("preferences:backup.export.password"),
                            div()
                                .flex()
                                .flex_col()
                                .gap(space(1.5))
                                .child(Input::new(&self.password))
                                .child(
                                    div()
                                        .kp_text(TextSize::Xs)
                                        .text_color(tokens.text.secondary)
                                        .child(i18n::t("preferences:backup.export.encryptedHint")),
                                )
                                .into_any_element(),
                            tokens,
                        ))
                    } else {
                        form.child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.status.warning.solid)
                                .child(i18n::t("preferences:backup.export.plainWarning")),
                        )
                    }
                })
                .into_any_element(),
            ExportKind::Readable => self.readable.clone().into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .gap(space(4.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(div().flex().child(kinds))
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .child(hint),
                    ),
            )
            .child(body)
    }
}

/// 按弹框里的选项导出备份包：先选保存位置，再交给 core 打包（加密时带密码）。
async fn export_backup_file(
    core: kwikpaste_core::Core,
    dialog: gpui::Entity<ExportDialog>,
    cx: &mut gpui::AsyncWindowContext,
) {
    let Ok((encrypted, password, scope)) = cx.update(|_, cx| {
        let dialog = dialog.read(cx);
        (
            dialog.backup_encrypted(cx),
            dialog.password.value(cx).to_string(),
            dialog.backup_scope(),
        )
    }) else {
        return;
    };
    let name = if scope.favorites_only {
        "KwikPaste-favorites.kwikpastebak"
    } else {
        "KwikPaste-history.kwikpastebak"
    };
    let Ok(prompt) = cx.update(|_, cx| {
        let directory = core
            .preference_directory(PreferenceDirectory::Data)
            .unwrap_or_else(|_| std::env::temp_dir());
        clipboard::view::pin::prompt_for_new_path(&directory, name, cx)
    }) else {
        log::warn!("could not open backup save dialog");
        return;
    };
    let Some(path) = prompt.await else {
        return;
    };
    // 明文备份不能带密码（core 会拒绝），切到明文前输过的密码在这里丢掉。
    let options = backup::ExportHistoryBackupOptions {
        mode: if encrypted {
            BackupExportMode::Encrypted
        } else {
            BackupExportMode::Plain
        },
        password: encrypted.then_some(password),
        scope,
    };
    let message = match core.export_history_backup(path, options).await {
        Ok(result) => {
            log::info!("history backup exported: {}", result.path);
            Toast::success(i18n::t_args(
                "commands:messages.backupExported",
                &[
                    ("count", &result.item_count.to_string()),
                    ("size", &text::format_bytes(result.total_bytes)),
                ],
            ))
        }
        Err(error) => {
            log::warn!("history backup export failed: {error:#}");
            Toast::error(i18n::t_args(
                "commands:error",
                &[
                    ("label", &i18n::t("commands:labels.exportBackup")),
                    ("message", &error.to_string()),
                ],
            ))
        }
    };
    let _ = cx.update(|window, cx| toast::show(message, window, cx));
}

/// 按弹框里预览过的选项导出 Excel / Markdown：按分组拆分时选目录，否则选文件。
async fn export_readable_files(
    core: kwikpaste_core::Core,
    dialog: gpui::Entity<ExportDialog>,
    cx: &mut gpui::AsyncWindowContext,
) {
    let Ok((options, fingerprint)) = cx.update(|_, cx| {
        let readable = dialog.read(cx).readable.read(cx);
        (
            readable.export_options(cx),
            readable
                .preview
                .as_ref()
                .map(|preview| preview.fingerprint.clone()),
        )
    }) else {
        return;
    };
    let Some(fingerprint) = fingerprint else {
        log::warn!("readable export confirmed without a preview");
        return;
    };
    let path = if options.split_by_group {
        let Ok(prompt) = cx.update(|_, cx| {
            clipboard::view::pin::prompt_for_paths(
                gpui::PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: None,
                },
                cx,
            )
        }) else {
            log::warn!("could not open readable export directory dialog");
            return;
        };
        prompt.await.and_then(|paths| paths.into_iter().next())
    } else {
        let Ok(prompt) = cx.update(|_, cx| {
            let directory = core
                .preference_directory(PreferenceDirectory::Data)
                .unwrap_or_else(|_| std::env::temp_dir());
            let name = if options.format == ExportFormat::Markdown {
                "KwikPaste-readable.md"
            } else {
                "KwikPaste-readable.xlsx"
            };
            clipboard::view::pin::prompt_for_new_path(&directory, name, cx)
        }) else {
            log::warn!("could not open readable export save dialog");
            return;
        };
        prompt.await
    };
    let Some(path) = path else {
        return;
    };
    match core.export_readable_data(options, fingerprint, path).await {
        Ok(result) => log::info!("readable export written: {}", result.path),
        Err(error) => log::warn!("readable export failed: {error:#}"),
    }
}

struct ReadableExportDialog {
    core: kwikpaste_core::Core,
    format: SelectState,
    _subscriptions: Vec<Subscription>,
    favorites_only: bool,
    include_sensitive: bool,
    sensitive_confirmed: bool,
    groups: Vec<kwikpaste_core::db::models::ClipboardGroup>,
    groups_ready: bool,
    group_ids: Option<Vec<String>>,
    include_ungrouped: bool,
    split_by_group: bool,
    preview: Option<ExportPreview>,
    preview_failed: bool,
}

impl ReadableExportDialog {
    fn new(core: kwikpaste_core::Core, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let format = SelectState::new(
            vec![
                SelectOption::new("xlsx", i18n::t("preferences:readableExport.formatXlsx")),
                SelectOption::new(
                    "markdown",
                    i18n::t("preferences:readableExport.formatMarkdown"),
                ),
            ],
            Some("xlsx"),
            window,
            cx,
        );
        let format_subscription = format.on_change(cx, |this, _, cx| {
            this.invalidate_preview(cx);
        });
        Self {
            core,
            format,
            _subscriptions: vec![format_subscription],
            favorites_only: false,
            include_sensitive: false,
            sensitive_confirmed: false,
            groups: Vec::new(),
            groups_ready: false,
            group_ids: None,
            include_ungrouped: true,
            split_by_group: false,
            preview: None,
            preview_failed: false,
        }
    }

    fn export_options(&self, cx: &App) -> ExportOptions {
        ExportOptions {
            format: if self
                .format
                .selected_value(cx)
                .is_some_and(|format| format == "markdown")
            {
                ExportFormat::Markdown
            } else {
                ExportFormat::Xlsx
            },
            favorites_only: self.favorites_only,
            group_ids: self.group_ids.clone(),
            include_ungrouped: self.include_ungrouped,
            split_by_group: self.split_by_group,
            include_sensitive: self.include_sensitive,
        }
    }

    fn invalidate_preview(&mut self, cx: &mut Context<Self>) {
        self.preview = None;
        self.preview_failed = false;
        cx.notify();
    }

    fn prepare_preview(&mut self, cx: &mut Context<Self>) {
        let options = self.export_options(cx);
        let core = self.core.clone();
        self.preview = None;
        self.preview_failed = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = core.preview_readable_export(options).await;
            let _ = this.update(cx, |dialog, cx| {
                match result {
                    Ok(preview) => dialog.preview = Some(preview),
                    Err(error) => {
                        dialog.preview_failed = true;
                        log::warn!("readable export preview failed: {error:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ReadableExportDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let selected_groups = self.group_ids.clone();
        let group_rows = selected_groups.map(|selected| {
            let rows = self.groups.iter().map(|group| {
                let id = group.id.clone();
                let checked = selected.contains(&id);
                let entity = entity.clone();
                Checkbox::new(format!("readable-group-{id}"))
                    .label(group.name.clone())
                    .checked(checked)
                    .disabled(!self.groups_ready)
                    .on_change(move |checked, _, cx| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(cx, |dialog, cx| {
                                let ids = dialog.group_ids.get_or_insert_with(Vec::new);
                                if checked {
                                    if !ids.contains(&id) {
                                        ids.push(id.clone());
                                    }
                                } else {
                                    ids.retain(|value| value != &id);
                                }
                                dialog.invalidate_preview(cx);
                            });
                        }
                    })
            });
            div().flex().flex_col().gap(space(1.)).children(rows).child(
                Checkbox::new("readable-ungrouped")
                    .label(i18n::t("preferences:readableExport.ungrouped"))
                    .checked(self.include_ungrouped)
                    .on_change({
                        let entity = entity.clone();
                        move |checked, _, cx| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(cx, |dialog, cx| {
                                    dialog.include_ungrouped = checked;
                                    dialog.invalidate_preview(cx);
                                });
                            }
                        }
                    }),
            )
        });
        let tokens = theme::semantic(cx);
        let preview_summary = self.preview.as_ref().map(|preview| {
            let summary = i18n::t_args(
                "preferences:readableExport.summary",
                &[
                    ("groups", &preview.groups.len().to_string()),
                    ("items", &preview.item_count.to_string()),
                    ("files", &preview.file_count.to_string()),
                ],
            );
            div()
                .flex()
                .flex_col()
                .gap(space(1.))
                .p(space(3.))
                .rounded(theme::radius::MD)
                .bg(tokens.fill.faint)
                .kp_text(TextSize::Xs)
                .text_color(tokens.text.secondary)
                .child(
                    div()
                        .kp_text(TextSize::Sm)
                        .text_color(tokens.text.primary)
                        .child(summary),
                )
                .child(i18n::t_args(
                    "preferences:readableExport.excluded",
                    &[("count", &preview.excluded_sensitive.to_string())],
                ))
                .children(
                    preview
                        .groups
                        .iter()
                        .map(|group| format!("{} · {}", group.name, group.count)),
                )
                .when(preview.item_count == 0, |element| {
                    element.child(
                        div()
                            .text_color(tokens.status.warning.solid)
                            .child(i18n::t("preferences:readableExport.empty")),
                    )
                })
        });
        let selecting_groups = self.group_ids.is_some();
        let groups_mode = div()
            .flex()
            .flex_none()
            .gap(space(0.5))
            .p(space(0.5))
            .rounded(theme::radius::MD)
            .bg(tokens.fill.subtle)
            .children([false, true].map(|selected_mode| {
                let label = if selected_mode {
                    i18n::t("preferences:readableExport.selectedGroups")
                } else {
                    i18n::t("preferences:readableExport.allGroups")
                };
                segment(
                    if selected_mode {
                        "readable-groups-selected"
                    } else {
                        "readable-groups-all"
                    },
                    label,
                    selecting_groups == selected_mode,
                    cx,
                )
                .on_click(cx.listener(move |dialog, _, _, cx| {
                    if dialog.group_ids.is_some() != selected_mode {
                        dialog.group_ids = selected_mode.then(Vec::new);
                        dialog.invalidate_preview(cx);
                    }
                }))
            }));
        // 竖排的表单项：文件格式、分组范围、输出方式、内容范围，最后是预览按钮和预览结果。
        div()
            .flex()
            .flex_col()
            .gap(space(4.))
            .pb(space(1.))
            .child(form_field(
                i18n::t("preferences:readableExport.format"),
                div()
                    .flex()
                    .child(
                        Select::new(&self.format)
                            .width(CONTROL_WIDTH)
                            .accessibility_label(i18n::t("preferences:readableExport.format")),
                    )
                    .into_any_element(),
                tokens,
            ))
            .child(form_field(
                i18n::t("preferences:readableExport.groups"),
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(div().flex().child(groups_mode))
                    .when_some(group_rows, |field, rows| {
                        field.child(list_tile(tokens).px(space(3.)).py(space(2.5)).child(rows))
                    })
                    .into_any_element(),
                tokens,
            ))
            .child(form_field(
                i18n::t("preferences:readableExport.output"),
                Checkbox::new("readable-split")
                    .label(i18n::t("preferences:readableExport.split"))
                    .checked(self.split_by_group)
                    .on_change({
                        let entity = entity.clone();
                        move |checked, _, cx| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(cx, |dialog, cx| {
                                    dialog.split_by_group = checked;
                                    dialog.invalidate_preview(cx);
                                });
                            }
                        }
                    })
                    .into_any_element(),
                tokens,
            ))
            .child(form_field(
                i18n::t("preferences:readableExport.range"),
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(
                        Checkbox::new("readable-favorites")
                            .label(i18n::t("preferences:readableExport.favorites"))
                            .checked(self.favorites_only)
                            .on_change({
                                let entity = entity.clone();
                                move |checked, _, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |dialog, cx| {
                                            dialog.favorites_only = checked;
                                            dialog.invalidate_preview(cx);
                                        });
                                    }
                                }
                            }),
                    )
                    .child(
                        Checkbox::new("readable-sensitive")
                            .label(i18n::t("preferences:readableExport.includeSensitive"))
                            .checked(self.include_sensitive)
                            .on_change({
                                let entity = entity.clone();
                                move |checked, _, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |dialog, cx| {
                                            dialog.include_sensitive = checked;
                                            dialog.sensitive_confirmed = false;
                                            dialog.invalidate_preview(cx);
                                        });
                                    }
                                }
                            }),
                    )
                    .when(self.include_sensitive, |element| {
                        let entity = entity.clone();
                        element.child(
                            Checkbox::new("readable-sensitive-confirm")
                                .label(i18n::t("preferences:readableExport.confirmSensitive"))
                                .checked(self.sensitive_confirmed)
                                .on_change(move |checked, _, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |dialog, cx| {
                                            dialog.sensitive_confirmed = checked;
                                            cx.notify();
                                        });
                                    }
                                }),
                        )
                    })
                    .into_any_element(),
                tokens,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(
                        div().flex().child(
                            Button::new(
                                "readable-preview",
                                i18n::t("preferences:readableExport.preview"),
                            )
                            .on_click(cx.listener(
                                |dialog, _, _, cx| {
                                    dialog.prepare_preview(cx);
                                },
                            )),
                        ),
                    )
                    .when(self.preview_failed, |element| {
                        element.child(
                            div()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.status.warning.solid)
                                .child(i18n::t("preferences:readableExport.retryPreview")),
                        )
                    })
                    .when_some(preview_summary, |element, summary| element.child(summary)),
            )
    }
}

/// 分段切换里的一段（同预览窗的文本方式切换）：选中段在亮色里是浮起的白块，暗色里亮一档。
fn segment(
    id: &'static str,
    label: gpui::SharedString,
    selected: bool,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let tokens = theme::semantic(cx);
    let thumb = match theme::appearance(cx) {
        theme::Appearance::Light => tokens.surface.panel,
        theme::Appearance::Dark => tokens.fill.default,
    };

    div()
        .id(id)
        .role(Role::Tab)
        .aria_selected(selected)
        .flex()
        .items_center()
        .h(rems(1.5))
        .px(space(3.))
        .rounded(theme::radius::SM)
        .kp_text(TextSize::Sm)
        .cursor_pointer()
        .map(|segment| {
            if selected {
                segment
                    .bg(thumb)
                    .shadow(tokens.shadow.card.to_vec())
                    .text_color(tokens.text.primary)
            } else {
                segment
                    .text_color(tokens.text.secondary)
                    .hover(|style| style.text_color(tokens.text.primary))
            }
        })
        .child(label)
}

/// 每个应用一行（同一应用的多个安装版本已在 core 合并）。设置里存的 id 原样保留，可能是旧版本的
/// 路径；勾选状态与增删都按应用比较，见 [`app_ids`]。
struct SourceAppsDialog {
    apps: Vec<kwikpaste_core::ops::ClipboardAppView>,
    selected: Vec<String>,
    icons: gpui::Entity<KpImageCache>,
}

impl SourceAppsDialog {
    fn set_selected(&mut self, id: &str, checked: bool, cx: &mut Context<Self>) {
        app_ids::set_app_listed(&mut self.selected, id, checked);
        cx.notify();
    }
}

/// 显示来源应用缓存图标；抽取失败时用固定尺寸的中性窗口图标占位。
impl SourceAppsDialog {
    fn cached_app_icon(
        &mut self,
        path: Option<&str>,
        tokens: &kwikpaste_ui::theme::SemanticTokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let size = rems(1.25);
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            return div()
                .size(size)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Monitor)
                        .size(rems(1.1))
                        .color(tokens.text.muted),
                )
                .into_any_element();
        };
        let physical = (px_rems(20.).to_pixels(window.rem_size()).as_f32() * window.scale_factor())
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
                .flex_none()
                .into_any_element(),
            ImageState::Loading => div().size(size).flex_none().into_any_element(),
            ImageState::Failed => div()
                .size(size)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Monitor)
                        .size(rems(1.1))
                        .color(tokens.text.muted),
                )
                .into_any_element(),
        }
    }
}

/// 在不带缓存上下文的引导/概览行里显示来源应用图标。
pub(super) fn app_icon(
    _path: Option<&str>,
    tokens: &kwikpaste_ui::theme::SemanticTokens,
) -> gpui::AnyElement {
    div()
        .size(rems(1.25))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            Icon::new(IconName::Monitor)
                .size(rems(1.1))
                .color(tokens.text.muted),
        )
        .into_any_element()
}

/// 设置里的顺序在前（去重），缺的动作按默认顺序补在末尾，弹框始终列出全部动作。
fn normalized_item_action_order(current: &[ItemAction]) -> Vec<ItemAction> {
    let mut order = Vec::new();
    for action in current.iter().chain(&Content::default().item_action_order) {
        if !order.contains(action) {
            order.push(*action);
        }
    }
    order
}

#[derive(Clone, Copy)]
struct ActionVisibilityDrag {
    action: ItemAction,
}

struct ActionVisibilityPreview {
    action: ItemAction,
}

impl Render for ActionVisibilityPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let (icon, label) = clipboard::view::quick_action_glyph(self.action);
        div()
            .flex()
            .items_center()
            .gap(space(2.))
            .px(space(3.))
            .py(space(1.5))
            .rounded(theme::radius::SM)
            .border_1()
            .border_color(tokens.accent.solid)
            .bg(tokens.surface.raised)
            .kp_text(TextSize::Sm)
            .child(PrefIcon::Grip.view(rems(1.), tokens.text.faint))
            .child(Icon::new(icon).size(rems(1.)).color(tokens.text.secondary))
            .child(label)
    }
}

struct ActionVisibilityDialog {
    order: Vec<ItemAction>,
    enabled: Vec<ItemAction>,
}

impl ActionVisibilityDialog {
    fn toggle(&mut self, action: ItemAction, checked: bool, cx: &mut Context<Self>) {
        if checked {
            if !self.enabled.contains(&action) {
                self.enabled.push(action);
            }
        } else {
            self.enabled.retain(|candidate| *candidate != action);
        }
        cx.notify();
    }

    fn move_to(&mut self, action: ItemAction, target: usize, cx: &mut Context<Self>) {
        if target >= self.order.len() {
            return;
        }
        let Some(index) = self.order.iter().position(|candidate| *candidate == action) else {
            return;
        };
        if index == target {
            return;
        }
        let action = self.order.remove(index);
        self.order.insert(target, action);
        cx.notify();
    }
}

impl Render for ActionVisibilityDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let rows = self.order.iter().enumerate().map(|(index, action)| {
            let action = *action;
            let checked = self.enabled.contains(&action);
            let (icon, label) = clipboard::view::quick_action_glyph(action);
            let entity = cx.entity().downgrade();
            let checkbox_entity = entity.clone();
            let label_entity = entity.clone();
            sortable::row(format!("visible-action-{index}"), tokens)
                .when(index > 0, |row| {
                    row.border_t_1().border_color(tokens.border.divider)
                })
                .on_drag(ActionVisibilityDrag { action }, |dragged, _, _, cx| {
                    cx.new(|_| ActionVisibilityPreview {
                        action: dragged.action,
                    })
                })
                .drag_over::<ActionVisibilityDrag>(move |style, _, _, _| {
                    sortable::drop_target(style, tokens)
                })
                .on_drop(
                    cx.listener(move |this, dragged: &ActionVisibilityDrag, _, cx| {
                        this.move_to(dragged.action, index, cx);
                    }),
                )
                .child(
                    div()
                        .flex_none()
                        .cursor_grab()
                        .child(PrefIcon::Grip.view(rems(1.), tokens.text.faint)),
                )
                .child(
                    Checkbox::new(format!("visible-action-check-{index}"))
                        .checked(checked)
                        .accessibility_label(label.clone())
                        .on_change(move |checked, _, cx| {
                            if let Some(entity) = checkbox_entity.upgrade() {
                                entity.update(cx, |dialog, cx| dialog.toggle(action, checked, cx));
                            }
                        }),
                )
                .child(Icon::new(icon).size(rems(1.)).color(tokens.text.secondary))
                .child(
                    div()
                        .id(format!("visible-action-label-{index}"))
                        .flex_1()
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            if let Some(entity) = label_entity.upgrade() {
                                entity.update(cx, |dialog, cx| {
                                    let checked = !dialog.enabled.contains(&action);
                                    dialog.toggle(action, checked, cx);
                                });
                            }
                        })
                        .child(label),
                )
                .into_any_element()
        });
        div()
            .id("visible-actions-list")
            .max_h(rems(28.))
            .overflow_y_scroll()
            .child(list_tile(tokens).children(rows))
    }
}

impl Render for SourceAppsDialog {
    /// 浅灰列表块里每行一个应用：勾选框、应用图标和应用名，下面一行灰色小字是路径（同引导的忽略应用）。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let apps = self.apps.clone();
        let selected = self.selected.clone();
        let rows = apps.iter().enumerate().map(|(index, app)| {
            let id = app.id.clone();
            let checked = app_ids::contains_app(&selected, &id);
            let name = if app.name.is_empty() {
                id.clone()
            } else {
                app.name.clone()
            };
            let path = (!app.name.is_empty()).then(|| id.clone());
            let entity = cx.entity().downgrade();
            div()
                .flex()
                .flex_col()
                .gap(space(0.5))
                .px(space(3.))
                .py(space(2.))
                .when(index > 0, |row| {
                    row.border_t_1().border_color(tokens.border.divider)
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space(2.))
                        .child(
                            Checkbox::new(format!("excluded-app-{id}"))
                                .accessibility_label(name.clone())
                                .checked(checked)
                                .on_change({
                                    let entity = entity.clone();
                                    let id = id.clone();
                                    move |checked, _, cx| {
                                        if let Some(entity) = entity.upgrade() {
                                            entity.update(cx, |this, cx| {
                                                this.set_selected(&id, checked, cx)
                                            });
                                        }
                                    }
                                }),
                        )
                        .child(
                            div()
                                .id(format!("excluded-app-label-{index}"))
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .items_center()
                                .gap(space(2.))
                                .cursor_pointer()
                                .on_click(move |_, _, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |this, cx| {
                                            this.set_selected(&id, !checked, cx)
                                        });
                                    }
                                })
                                .child(self.cached_app_icon(
                                    app.icon_path.as_deref(),
                                    tokens,
                                    window,
                                    cx,
                                ))
                                .child(div().min_w_0().truncate().child(name)),
                        ),
                )
                .children(path.map(|path| {
                    div()
                        .pl(space(13.))
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.muted)
                        .truncate()
                        .child(path)
                }))
        });
        div()
            .id("excluded-apps-list")
            .max_h(rems(24.))
            .overflow_y_scroll()
            .child(list_tile(tokens).children(rows))
    }
}

#[allow(clippy::too_many_arguments)]
fn open_source_apps_dialog(
    apps: Vec<kwikpaste_core::ops::ClipboardAppView>,
    excluded: Vec<String>,
    core: kwikpaste_core::Core,
    parent: WeakEntity<Preferences>,
    title_key: &'static str,
    settings_path: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    let icons = cx.new(|_| KpImageCache::with_capacity(128));
    let dialog = cx.new(|cx| {
        cx.observe(&icons, |_, _, cx| cx.notify()).detach();
        SourceAppsDialog {
            apps,
            selected: excluded,
            icons,
        }
    });
    let content = dialog.clone();
    let adder = dialog.downgrade();
    let answer = form_dialog(
        DialogSpec::new(i18n::t(title_key))
            .ok_text(i18n::t("common:actions.save"))
            .cancel_text(i18n::t("common:actions.cancel"))
            .footer_extra(move |_, _cx| {
                let adder = adder.clone();
                let core = core.clone();
                Button::new(
                    "source-app-add",
                    i18n::t("preferences:schema.settings.source.appTransfer.addApp"),
                )
                .ghost()
                .on_click(move |_, _, cx| {
                    let prompt = clipboard::view::pin::prompt_for_paths(
                        gpui::PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: None,
                        },
                        cx,
                    );
                    let adder = adder.clone();
                    let core = core.clone();
                    cx.spawn(async move |cx| {
                        let Some(path) = prompt.await.and_then(|paths| paths.into_iter().next())
                        else {
                            return;
                        };
                        match core.add_app_from_path(path).await {
                            Ok(app) => {
                                let _ = adder.update(cx, |dialog, cx| {
                                    if !dialog
                                        .apps
                                        .iter()
                                        .any(|known| app_ids::same_app(&known.id, &app.id))
                                    {
                                        dialog.apps.push(app);
                                    }
                                    cx.notify();
                                });
                            }
                            Err(error) => log::warn!("could not add source app: {error:#}"),
                        }
                    })
                    .detach();
                })
                .into_any_element()
            }),
        move |_, _| content.clone().into_any_element(),
        window,
        cx,
    );
    window
        .spawn(cx, async move |cx| {
            if !answer.await.unwrap_or(false) {
                return;
            }
            let selected = cx
                .update(|_, cx| dialog.read(cx).selected.clone())
                .unwrap_or_default();
            let _ = parent.update(cx, |preferences, cx| {
                preferences.update(settings_path, json!(selected), cx);
            });
        })
        .detach();
}

#[derive(Clone, Copy)]
struct CaptureOrderDrag {
    kind: CaptureKind,
}

impl Render for CaptureOrderDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        div()
            .flex()
            .items_center()
            .gap(space(2.))
            .px(space(3.))
            .py(space(1.5))
            .rounded(theme::radius::SM)
            .border_1()
            .border_color(tokens.accent.solid)
            .bg(tokens.surface.raised)
            .kp_text(TextSize::Sm)
            .child(PrefIcon::Grip.view(rems(1.), tokens.text.faint))
            .child(capture_kind_icon(self.kind).view(rems(1.), tokens.accent.solid))
            .child(capture_kind_label(self.kind))
    }
}

/// 移动而不是交换两项，保证被跨过的格式保持原来的相对顺序。
fn reorder_capture_kinds(order: &mut Vec<CaptureKind>, kind: CaptureKind, target: usize) -> bool {
    let Some(index) = order.iter().position(|candidate| *candidate == kind) else {
        return false;
    };
    if target >= order.len() || index == target {
        return false;
    }
    let kind = order.remove(index);
    order.insert(target, kind);
    true
}

struct RetentionRuleEditor {
    categories: Vec<ContentCategory>,
    min_size: NumberInputState,
    sensitive_only: bool,
    unused_only: bool,
    keep_value: NumberInputState,
    keep_unit: SelectState,
}

impl RetentionRuleEditor {
    fn new(rule: &RetentionRule, window: &mut Window, cx: &mut App) -> Self {
        let keep_value = NumberInputState::new(
            u64::from(rule.keep.value),
            0,
            u64::from(u32::MAX),
            window,
            cx,
        );
        let keep_unit = SelectState::new(
            ["minutes", "hours", "days", "weeks", "months", "forever"]
                .into_iter()
                .map(|key| {
                    SelectOption::new(
                        key,
                        i18n::t(&format!("preferences:schema.retentionUnits.{key}")),
                    )
                })
                .collect(),
            Some(retention_unit_key(rule.keep.unit)),
            window,
            cx,
        );
        Self {
            categories: rule.categories.clone(),
            min_size: NumberInputState::new(
                u64::from(rule.min_size_kb),
                0,
                u64::from(u32::MAX),
                window,
                cx,
            ),
            sensitive_only: rule.sensitive_only,
            unused_only: rule.unused_only,
            keep_value,
            keep_unit,
        }
    }

    fn to_rule(&self, base: &RetentionRule, cx: &App) -> RetentionRule {
        let mut rule = base.clone();
        rule.categories = self.categories.clone();
        rule.min_size_kb = self.min_size.clamped(cx) as u32;
        rule.sensitive_only = self.sensitive_only;
        rule.unused_only = self.unused_only;
        rule.keep.value = self.keep_value.clamped(cx) as u32;
        rule.keep.unit = self
            .keep_unit
            .selected_value(cx)
            .map(|value| retention_unit_from_key(value.as_ref()))
            .unwrap_or(RetentionUnit::Forever);
        rule
    }
}

impl Render for RetentionRuleEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let entity = cx.entity().downgrade();
        let keep_forever = self
            .keep_unit
            .selected_value(cx)
            .is_some_and(|value| value.as_ref() == "forever");
        let categories = ContentCategory::ALL.into_iter().map(|category| {
            let checked = self.categories.contains(&category);
            let entity = entity.clone();
            Checkbox::new(format!("retention-category-{category:?}"))
                .label(category_label(category))
                .checked(checked)
                .on_change(move |checked, _, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |editor, cx| {
                            if checked {
                                if !editor.categories.contains(&category) {
                                    editor.categories.push(category);
                                }
                            } else {
                                editor.categories.retain(|item| *item != category);
                            }
                            cx.notify();
                        });
                    }
                })
        });
        let condition_entity = cx.entity().downgrade();
        let sensitive = Checkbox::new("retention-sensitive")
            .label(i18n::t("preferences:retentionRules.form.sensitiveOnly"))
            .checked(self.sensitive_only)
            .on_change(move |checked, _, cx| {
                if let Some(entity) = condition_entity.upgrade() {
                    entity.update(cx, |editor, cx| {
                        editor.sensitive_only = checked;
                        cx.notify();
                    });
                }
            });
        let unused_entity = cx.entity().downgrade();
        let unused = Checkbox::new("retention-unused")
            .label(i18n::t("preferences:retentionRules.form.unusedOnly"))
            .checked(self.unused_only)
            .on_change(move |checked, _, cx| {
                if let Some(entity) = unused_entity.upgrade() {
                    entity.update(cx, |editor, cx| {
                        editor.unused_only = checked;
                        cx.notify();
                    });
                }
            });
        // 竖排的表单项（同新增分组弹框）：标题在上、控件在下，项与项之间 16 px。
        div()
            .flex()
            .flex_col()
            .gap(space(4.))
            .pb(space(1.))
            .child(form_field(
                i18n::t("preferences:retentionRules.form.categories"),
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_2()
                    .children(categories)
                    .into_any_element(),
                tokens,
            ))
            .child(form_field(
                i18n::t("preferences:retentionRules.form.minSize"),
                div()
                    .flex()
                    .child(
                        NumberInput::new(&self.min_size)
                            .width(CONTROL_WIDTH)
                            .suffix("KB"),
                    )
                    .into_any_element(),
                tokens,
            ))
            .child(form_field(
                i18n::t("preferences:retentionRules.form.conditions"),
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(sensitive)
                    .child(unused)
                    .into_any_element(),
                tokens,
            ))
            .child(form_field(
                i18n::t("preferences:retentionRules.form.keep"),
                div()
                    .flex()
                    .flex_col()
                    .gap(space(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space(2.))
                            .when(!keep_forever, |row| {
                                row.child(NumberInput::new(&self.keep_value).width(rems(6.)))
                            })
                            .child(Select::new(&self.keep_unit).width(rems(8.))),
                    )
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.muted)
                            .child(i18n::t("preferences:retentionRules.form.keepHint")),
                    )
                    .into_any_element(),
                tokens,
            ))
    }
}

/// 弹框里竖排的一个表单项：正文字号的灰色标题，下面是控件。
fn form_field(
    label: gpui::SharedString,
    control: gpui::AnyElement,
    tokens: &SemanticTokens,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(space(2.))
        .child(
            div()
                .kp_text(TextSize::Sm)
                .text_color(tokens.text.secondary)
                .child(label),
        )
        .child(control)
}

fn capture_kind_label(kind: CaptureKind) -> gpui::SharedString {
    let key = match kind {
        CaptureKind::Text => "text",
        CaptureKind::Html => "html",
        CaptureKind::Rtf => "rtf",
        CaptureKind::Image => "image",
        CaptureKind::Files => "files",
    };
    i18n::t(&format!("preferences:schema.captureKinds.{key}"))
}

fn capture_kind_icon(kind: CaptureKind) -> PrefIcon {
    match kind {
        CaptureKind::Text => PrefIcon::ClipboardType,
        CaptureKind::Html => PrefIcon::FileCode,
        CaptureKind::Rtf => PrefIcon::FileType,
        CaptureKind::Image => PrefIcon::FileImage,
        CaptureKind::Files => PrefIcon::Files,
    }
}

fn retention_label(value: u32, unit: RetentionUnit) -> gpui::SharedString {
    if matches!(unit, RetentionUnit::Forever) || value == 0 {
        return i18n::t("preferences:retentionRules.summary.keepForever");
    }
    let key = match unit {
        RetentionUnit::Minutes => "minutes",
        RetentionUnit::Hours => "hours",
        RetentionUnit::Days => "days",
        RetentionUnit::Weeks => "weeks",
        RetentionUnit::Months => "months",
        RetentionUnit::Forever => "forever",
    };
    i18n::t_args(
        &format!("preferences:retentionRules.durations.{key}"),
        &[("count", &value.to_string())],
    )
}

fn retention_unit_key(unit: RetentionUnit) -> &'static str {
    match unit {
        RetentionUnit::Minutes => "minutes",
        RetentionUnit::Hours => "hours",
        RetentionUnit::Days => "days",
        RetentionUnit::Weeks => "weeks",
        RetentionUnit::Months => "months",
        RetentionUnit::Forever => "forever",
    }
}

fn retention_unit_from_key(key: &str) -> RetentionUnit {
    match key {
        "minutes" => RetentionUnit::Minutes,
        "hours" => RetentionUnit::Hours,
        "days" => RetentionUnit::Days,
        "weeks" => RetentionUnit::Weeks,
        "months" => RetentionUnit::Months,
        _ => RetentionUnit::Forever,
    }
}

fn retention_rule_summary(rule: &RetentionRule) -> gpui::SharedString {
    let mut parts = Vec::new();
    if rule.categories.is_empty() {
        parts.push(i18n::t("preferences:retentionRules.summary.all").to_string());
    } else {
        parts.push(
            rule.categories
                .iter()
                .map(|category| category_label(*category).to_string())
                .collect::<Vec<_>>()
                .join("、"),
        );
    }
    if rule.min_size_kb > 0 {
        parts.push(
            i18n::t_args(
                "preferences:retentionRules.summary.larger",
                &[("size", &format!("{} KB", rule.min_size_kb))],
            )
            .to_string(),
        );
    }
    if rule.sensitive_only {
        parts.push(i18n::t("preferences:retentionRules.summary.sensitive").to_string());
    }
    if rule.unused_only {
        parts.push(i18n::t("preferences:retentionRules.summary.unused").to_string());
    }
    parts.join(" · ").into()
}

/// 设置行里的列表块（采集顺序、清理规则）：嵌在分组卡片里，用内容底色加细边框和小圆角，
/// 行间细分隔线由调用方画。
fn list_tile(tokens: &SemanticTokens) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .overflow_hidden()
        .rounded(theme::radius::LG)
        .bg(tokens.surface.panel)
        .border_1()
        .border_color(tokens.border.subtle)
}

/// 设置分组的卡片：很淡的填充底、细边框、小圆角，行与行之间的分隔线留在卡片内。
fn section_card(tokens: &SemanticTokens) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .px(space(3.))
        .rounded(theme::radius::LG)
        .bg(tokens.fill.faint)
        .border_1()
        .border_color(tokens.border.subtle)
}

/// 设置行右侧下拉框、数字框、文本框的统一宽度，右边缘和左边缘都对齐。
const CONTROL_WIDTH: Rems = Rems(12.);

/// 设置行的外框：左右内边距、最小高度一致，行与行之间一条细分隔线（分组第一行上方不画）。
pub(super) fn row_frame(first: bool, tokens: &SemanticTokens) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(space(4.))
        .px(space(1.))
        .py(space(2.5))
        .min_h(rems(3.5))
        .when(!first, |row| {
            row.border_t_1().border_color(tokens.border.divider)
        })
}

/// 设置行左侧的标题和说明。
pub(super) fn row_label(
    title: gpui::SharedString,
    description: gpui::SharedString,
    tokens: &SemanticTokens,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(space(0.5))
        .flex_1()
        .min_w_0()
        .child(div().kp_text(TextSize::Sm).child(title))
        .when(!description.is_empty(), |label| {
            label.child(
                div()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.muted)
                    .child(description),
            )
        })
}

/// 一整行设置：标题、说明和右侧控件。
fn setting_row(
    first: bool,
    title: gpui::SharedString,
    description: gpui::SharedString,
    control: Option<gpui::AnyElement>,
    tokens: &SemanticTokens,
) -> gpui::AnyElement {
    row_frame(first, tokens)
        .child(row_label(title, description, tokens))
        .children(control.map(|control| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(space(2.))
                .child(control)
        }))
        .into_any_element()
}

/// 分组里的一句说明（空状态、提示、出错）：没有控件，字号与说明一致。
fn note_row(
    first: bool,
    text: gpui::SharedString,
    color: Hsla,
    tokens: &SemanticTokens,
) -> gpui::AnyElement {
    row_frame(first, tokens)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .kp_text(TextSize::Sm)
                .text_color(color)
                .child(text),
        )
        .into_any_element()
}

/// 一个设置分组：小号灰字标题（可带右侧的操作按钮），下面是装在分组卡片里的设置行。
fn section_block(
    title: Option<gpui::SharedString>,
    action: Option<gpui::AnyElement>,
    rows: Vec<gpui::AnyElement>,
    tokens: &SemanticTokens,
) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(space(1.))
        .when_some(title, |section, title| {
            section.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space(2.))
                    .min_h(rems(1.5))
                    .px(space(1.))
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.muted)
                            .child(title),
                    )
                    .children(action),
            )
        })
        .child(section_card(tokens).children(rows))
        .into_any_element()
}

fn category_label(category: ContentCategory) -> gpui::SharedString {
    let key = match category {
        ContentCategory::Text => "text",
        ContentCategory::Html => "html",
        ContentCategory::Rtf => "rtf",
        ContentCategory::Url => "url",
        ContentCategory::Email => "email",
        ContentCategory::Color => "color",
        ContentCategory::Path => "path",
        ContentCategory::Image => "image",
        ContentCategory::Files => "files",
    };
    i18n::t(&format!("preferences:overview.categories.names.{key}"))
}

fn format_socket_address(address: &str, port: u16) -> String {
    if address.starts_with('[') || !address.contains(':') {
        format!("{address}:{port}")
    } else {
        format!("[{address}]:{port}")
    }
}

fn platform_label(platform: kwikpaste_core::db::models::Platform) -> &'static str {
    match platform {
        kwikpaste_core::db::models::Platform::Windows => "Windows",
        kwikpaste_core::db::models::Platform::Macos => "macOS",
    }
}

impl Render for Preferences {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.capture_shortcut(event, window, cx);
            }))
            .flex()
            .overflow_hidden()
            .when(
                !crate::platform::material::current(cx).is_translucent(),
                |root| root.bg(theme::semantic(cx).surface.window),
            )
            .text_color(theme::semantic(cx).text.primary)
            .child(self.render_sidebar(cx))
            .child(self.render_page(window, cx))
    }
}

pub(crate) fn shortcut_conflicts(left: &str, right: &str) -> bool {
    fn normalized(value: &str) -> Vec<String> {
        let mut keys: Vec<String> = value
            .split('+')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(|key| key.to_ascii_lowercase())
            .collect();
        keys.sort_unstable();
        keys
    }

    !left.trim().is_empty() && normalized(left) == normalized(right)
}

pub(crate) fn shortcut_conflicts_with_settings(
    setting_id: &str,
    candidate: &str,
    settings: &Settings,
) -> bool {
    if candidate.trim().is_empty() {
        return false;
    }
    let other = match setting_id {
        "shortcuts.openClipboard" => Some([
            settings.shortcuts.open_preference.as_str(),
            settings.shortcuts.paste_plain.as_str(),
        ]),
        "shortcuts.openPreference" => Some([
            settings.shortcuts.open_clipboard.as_str(),
            settings.shortcuts.paste_plain.as_str(),
        ]),
        "shortcuts.pastePlain" => Some([
            settings.shortcuts.open_clipboard.as_str(),
            settings.shortcuts.open_preference.as_str(),
        ]),
        _ => None,
    };
    if other.is_some_and(|others| {
        others
            .iter()
            .any(|other| shortcut_conflicts(candidate, other))
    }) {
        return true;
    }
    settings.shortcuts.quick_paste.enabled
        && (0..10).any(|index| {
            let key = if index == 9 {
                "0".to_owned()
            } else {
                (index + 1).to_string()
            };
            let quick = format!(
                "{}+{key}",
                settings.shortcuts.quick_paste.modifiers.accelerator()
            );
            shortcut_conflicts(candidate, &quick)
        })
}

pub(crate) fn backup_confirmation_required(_: BackupContainerMode) -> bool {
    true
}

pub(crate) fn shortcut_from_keystroke(keystroke: &Keystroke) -> Option<String> {
    let mut modifiers = Vec::new();
    if keystroke.modifiers.platform {
        #[cfg(target_os = "macos")]
        modifiers.push("Command");
        #[cfg(not(target_os = "macos"))]
        modifiers.push("Control");
    }
    if keystroke.modifiers.control {
        modifiers.push("Control");
    }
    if keystroke.modifiers.alt {
        modifiers.push("Alt");
    }
    if keystroke.modifiers.shift {
        modifiers.push("Shift");
    }
    let key = normalize_recorded_key(keystroke.key.as_str())?;
    if modifiers.is_empty() && !key.starts_with('F') {
        return None;
    }
    modifiers.push(key.as_str());
    Some(modifiers.join("+"))
}

fn shortcut_path(id: &str) -> Option<&'static str> {
    match id {
        "shortcuts.openClipboard" => Some("shortcuts.openClipboard"),
        "shortcuts.openPreference" => Some("shortcuts.openPreference"),
        "shortcuts.pastePlain" => Some("shortcuts.pastePlain"),
        _ => None,
    }
}

fn normalize_recorded_key(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    if lower.len() == 1 && lower.as_bytes()[0].is_ascii_alphanumeric() {
        return Some(lower.to_ascii_uppercase());
    }
    if let Some(number) = lower.strip_prefix('f')
        && let Ok(number) = number.parse::<u8>()
        && (1..=24).contains(&number)
    {
        return Some(format!("F{number}"));
    }
    let name = match lower.as_str() {
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "enter" | "return" => "Enter",
        "escape" | "esc" => "Esc",
        "home" => "Home",
        "end" => "End",
        "insert" => "Insert",
        "pageup" | "page_up" => "PageUp",
        "pagedown" | "page_down" => "PageDown",
        "arrowup" => "ArrowUp",
        "arrowdown" => "ArrowDown",
        "arrowleft" => "ArrowLeft",
        "arrowright" => "ArrowRight",
        "space" => "Space",
        "tab" => "Tab",
        "backquote" | "grave" => "Backquote",
        "backslash" => "Backslash",
        "bracketleft" => "BracketLeft",
        "bracketright" => "BracketRight",
        "comma" => "Comma",
        "equal" => "Equal",
        "minus" => "Minus",
        "period" => "Period",
        "quote" => "Quote",
        "semicolon" => "Semicolon",
        "slash" => "Slash",
        _ => return None,
    };
    Some(name.to_owned())
}

/// 偏好设置与首次引导共用的快捷键录制控件外观。
pub(crate) fn shortcut_recorder_button(
    id: &'static str,
    current: &str,
    settings: &Settings,
    recording: bool,
    cx: &App,
) -> Button {
    let conflict = shortcut_conflicts_with_settings(id, current, settings);
    let occupied = !conflict && !current.trim().is_empty() && hotkey::registration_failed(id, cx);
    let label: gpui::SharedString = if recording {
        i18n::t("preferences:controls.recordShortcut")
    } else if conflict || occupied {
        format!("{} ⚠", text::format_shortcut(current)).into()
    } else if current.trim().is_empty() {
        i18n::t("preferences:controls.shortcutNotSet")
    } else {
        text::format_shortcut(current).into()
    };
    Button::new(format!("shortcut-{id}"), label)
        .when((conflict || occupied) && !recording, |button| {
            button.danger()
        })
        .tooltip(if recording {
            i18n::t("preferences:controls.recordShortcut")
        } else if conflict {
            i18n::t("preferences:controls.shortcutConflict")
        } else if occupied {
            i18n::t("preferences:controls.shortcutInUse")
        } else {
            i18n::t("preferences:controls.recordShortcut")
        })
}

pub(crate) fn search_matches(query: &str, title: &str, keywords: &[&str]) -> bool {
    let query = query.trim().to_ascii_lowercase();
    query.is_empty()
        || title.to_ascii_lowercase().contains(&query)
        || keywords
            .iter()
            .any(|keyword| keyword.to_ascii_lowercase().contains(&query))
}

#[cfg(test)]
mod tests {
    use gpui::{Keystroke, Modifiers};

    use kwikpaste_core::backup::BackupContainerMode;
    use kwikpaste_core::settings::CaptureKind;
    use kwikpaste_core::settings::Settings;

    use super::{
        backup_confirmation_required, format_socket_address, reorder_capture_kinds, search_matches,
        shortcut_conflicts, shortcut_conflicts_with_settings, shortcut_from_keystroke,
    };

    #[test]
    fn shortcut_conflicts_ignore_modifier_order() {
        assert!(shortcut_conflicts("Alt+X", "X+Alt"));
        assert!(!shortcut_conflicts("Alt+X", "Alt+C"));
        assert!(!shortcut_conflicts("", "Alt+X"));
    }

    #[test]
    fn shortcut_conflicts_include_plain_paste_and_quick_paste() {
        let mut settings = Settings::default();
        settings.shortcuts.paste_plain = "Alt+P".to_owned();
        assert!(shortcut_conflicts_with_settings(
            "shortcuts.openClipboard",
            "P+Alt",
            &settings
        ));

        settings.shortcuts.quick_paste.enabled = true;
        settings.shortcuts.quick_paste.modifiers =
            kwikpaste_core::settings::QuickPasteModifiers::ControlAlt;
        assert!(shortcut_conflicts_with_settings(
            "shortcuts.pastePlain",
            "Alt+Control+1",
            &settings
        ));
    }

    #[test]
    fn search_matches_titles_and_keywords() {
        assert!(search_matches(
            "tray",
            "System startup",
            &["tray", "system"]
        ));
        assert!(!search_matches(
            "missing",
            "System startup",
            &["tray", "system"]
        ));
    }

    #[test]
    fn socket_addresses_keep_ipv6_literals_parseable() {
        assert_eq!(format_socket_address("127.0.0.1", 41573), "127.0.0.1:41573");
        assert_eq!(
            format_socket_address("fe80::1%12", 41573),
            "[fe80::1%12]:41573"
        );
        assert_eq!(
            format_socket_address("[fe80::1%12]", 41573),
            "[fe80::1%12]:41573"
        );
    }

    #[test]
    fn recorded_shortcut_uses_1x_modifier_order_and_physical_key() {
        let keystroke = Keystroke {
            modifiers: Modifiers {
                alt: true,
                shift: true,
                ..Modifiers::default()
            },
            key: "x".to_owned(),
            key_char: Some("x".to_owned()),
        };
        assert_eq!(
            shortcut_from_keystroke(&keystroke).as_deref(),
            Some("Alt+Shift+X")
        );
    }

    #[test]
    fn every_backup_mode_requires_import_confirmation() {
        assert!(backup_confirmation_required(BackupContainerMode::Plain));
        assert!(backup_confirmation_required(BackupContainerMode::Encrypted));
    }

    #[test]
    fn capture_order_drag_reorders_without_dropping_kinds() {
        let mut order = CaptureKind::default_order();
        let last = order.len() - 1;
        assert!(reorder_capture_kinds(&mut order, CaptureKind::Files, last));
        assert_eq!(order.len(), CaptureKind::default_order().len());
        assert_eq!(order.last(), Some(&CaptureKind::Files));
        assert!(!reorder_capture_kinds(&mut order, CaptureKind::Files, last));
    }
}
