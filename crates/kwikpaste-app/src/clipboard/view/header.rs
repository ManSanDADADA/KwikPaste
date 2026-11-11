//! 主窗口头部（1.x `Header.tsx` + `SearchInput.tsx`）：logo、占满剩余宽度的搜索框、固定窗口。
//! 偏好设置入口在页脚。
//!
//! 搜索框走平台层的编辑态：在输入框上按下鼠标请求 `BeginEditing(Mouse)`，`EditingStarted` 之后
//! 才真正能打字（Windows 上面板此时才是前台窗口）。输入 200 ms 防抖、去掉首尾空白后交给列表；
//! 输入法组字期间不触发（[`kwikpaste_ui::TextInput::on_event_in`]）。
//!
//! 搜索框右端的漏斗打开筛选菜单（时间、来源应用、类型），选中的条件以标签排在搜索图标后面，
//! 点 × 或在空输入框里按 Backspace 移除。点漏斗和标签不进入编辑态（见 [`Header::press_search`]）。

use std::{f32::consts::FRAC_PI_4, sync::Arc, time::Duration};

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, ImageSource,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, WeakEntity, Window, div, img, prelude::FluentBuilder as _,
    radians,
};
use kwikpaste_core::settings::PanelAction;
use kwikpaste_ui::{
    Glyph, Icon, IconButtonLook as Look, IconName, Input, KpStyled as _, MenuEntry, MenuItem,
    MenuTrigger, Submenu, TextInput, TextInputEvent, TooltipExt as _, icon_button,
    theme::{self, TextSize, space},
};

use super::{
    SEARCH_CONTEXT,
    card::logo,
    image_cache::{ImageKey, ImageState, KpImageCache, ResizeMode, path_of},
};
use crate::{
    clipboard::model::filter::{SearchFilters, Since, SourceApp, TextKind},
    i18n::{self, t, t_args},
    platform::{EditTrigger, updater, window_drag::WindowDragArea as _},
};

/// 搜索框里最多直接显示几个筛选标签，其余合成一个 `+N`。
const VISIBLE_CHIPS: usize = 2;
/// 菜单里应用图标的边长（设计 px）。
const APP_ICON: f32 = 16.;

/// 1.x `useDebounceFn(..., { wait: 200 })`。
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// 头部发给主窗口的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderEvent {
    /// 防抖后的搜索词（已去掉首尾空白）。
    Keyword(Arc<str>),
    /// 在输入框上按下了鼠标，要进入编辑态。
    RequestEditing(EditTrigger),
    TogglePin,
    /// 筛选条件变了。
    Filters(SearchFilters),
}

/// 搜索框里的一个筛选标签。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Chip {
    Since(Since),
    App(SourceApp),
    Kind(TextKind),
}

impl Chip {
    fn label(&self) -> SharedString {
        match self {
            Self::Since(since) => since_label(*since),
            Self::App(app) => SharedString::from(app.name.to_string()),
            Self::Kind(kind) => kind_label(*kind),
        }
    }

    fn remove_from(&self, filters: &mut SearchFilters) {
        match self {
            Self::Since(_) => filters.since = None,
            Self::App(app) => filters.toggle_app(app.clone()),
            Self::Kind(kind) => filters.toggle_kind(*kind),
        }
    }
}

/// 标签按时间、应用、类型的顺序排列。
fn chips(filters: &SearchFilters) -> Vec<Chip> {
    filters
        .since
        .map(Chip::Since)
        .into_iter()
        .chain(filters.apps.iter().cloned().map(Chip::App))
        .chain(filters.kinds.iter().copied().map(Chip::Kind))
        .collect()
}

fn since_label(since: Since) -> SharedString {
    t(match since {
        Since::Today => "clipboard:filter.since.today",
        Since::Week => "clipboard:filter.since.week",
        Since::Month => "clipboard:filter.since.month",
    })
}

fn kind_label(kind: TextKind) -> SharedString {
    t(match kind {
        TextKind::Link => "clipboard:filter.kinds.link",
        TextKind::Email => "clipboard:filter.kinds.email",
        TextKind::Color => "clipboard:filter.kinds.color",
        TextKind::Path => "clipboard:filter.kinds.path",
        TextKind::Rich => "clipboard:filter.kinds.rich",
    })
}

/// 菜单项：点击时按 `change` 改筛选条件。勾选项不带图标，左侧才显示勾选标记。
fn filter_item(
    header: &WeakEntity<Header>,
    label: SharedString,
    checked: bool,
    change: impl Fn(&mut SearchFilters) + 'static,
) -> MenuItem {
    let header = header.clone();

    MenuItem::new(label, move |_, cx| {
        header
            .update(cx, |header, cx| header.change_filters(&change, cx))
            .ok();
    })
    .checked(checked)
}

/// 筛选菜单：时间（单选）、来源应用和类型（多选）三个子菜单，加清除筛选。
fn filter_menu(header: &WeakEntity<Header>, window: &mut Window, cx: &mut App) -> Vec<MenuEntry> {
    let Some(entity) = header.upgrade() else {
        return Vec::new();
    };
    let (filters, apps, icons) = {
        let header = entity.read(cx);
        (
            header.filters.clone(),
            header.apps.clone(),
            header.icons.clone(),
        )
    };

    let times = Since::ALL
        .iter()
        .map(|&since| {
            filter_item(
                header,
                since_label(since),
                filters.since == Some(since),
                move |filters| {
                    filters.since = (filters.since != Some(since)).then_some(since);
                },
            )
            .into()
        })
        .collect();
    let apps: Vec<MenuEntry> = if apps.is_empty() {
        vec![
            MenuItem::new(t("clipboard:filter.noApps"), |_, _| {})
                .disabled(true)
                .into(),
        ]
    } else {
        apps.into_iter()
            .map(|app| {
                let image = app.icon_path.as_deref().and_then(|path| {
                    let key = Header::icon_key(path, window);
                    match icons.update(cx, |icons, cx| icons.request(key, window, cx)) {
                        ImageState::Ready(image) => Some(image),
                        ImageState::Loading | ImageState::Failed => None,
                    }
                });
                let checked = filters.apps.iter().any(|it| it.ids == app.ids);
                let label = SharedString::from(app.name.to_string());

                filter_item(header, label, checked, move |filters| {
                    filters.toggle_app(app.clone())
                })
                .image(image)
                .into()
            })
            .collect()
    };
    let kinds = TextKind::ALL
        .iter()
        .map(|&kind| {
            filter_item(
                header,
                kind_label(kind),
                filters.kinds.contains(&kind),
                move |filters| filters.toggle_kind(kind),
            )
            .into()
        })
        .collect();
    let clear = filter_item(header, t("clipboard:filter.clear"), false, |filters| {
        *filters = SearchFilters::default()
    })
    .icon(IconName::FilterX)
    .disabled(filters.is_empty());

    vec![
        Submenu::new(t("clipboard:filter.time"), times)
            .icon(IconName::Clock)
            .into(),
        Submenu::new(t("clipboard:filter.apps"), apps)
            .icon(IconName::AppWindow)
            .into(),
        Submenu::new(t("clipboard:filter.type"), kinds)
            .icon(IconName::Shapes)
            .into(),
        MenuEntry::Separator,
        clear.into(),
    ]
}

pub struct Header {
    input: TextInput,
    filters: SearchFilters,
    /// 筛选菜单里可选的来源应用（历史记录里出现过的）。
    apps: Vec<SourceApp>,
    /// 筛选菜单里的应用图标；面板隐藏时释放。
    icons: Entity<KpImageCache>,
    /// 按住修饰键：图标换成快捷键角标。
    hints: bool,
    /// 窗口已固定（1.x 固定按钮的本地状态）。
    pinned: bool,
    /// 搜索框处于编辑态。
    editing: bool,
    /// 这次在搜索框上按下的鼠标要进入编辑态；按在漏斗或标签上时由它们清掉（见 [`Self::press_search`]）。
    edit_press: bool,
    language: i18n::Language,
    /// 还没发出的搜索词；防抖到点或 [`Self::take_pending_keyword`] 取走。
    pending_keyword: Option<Arc<str>>,
    debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HeaderEvent> for Header {}

impl Header {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = TextInput::new(t("clipboard:header.searchPlaceholder"), window, cx);
        let changes = input.on_event_in(window, cx, |header, event, _, cx| {
            if let TextInputEvent::Change(value) = event {
                header.debounce_keyword(value, cx);
            }
        });

        let mut subscriptions = vec![changes];
        if let Some(state) = cx
            .try_global::<updater::UpdateStateHost>()
            .map(|host| host.0.clone())
        {
            subscriptions.push(cx.observe(&state, |_, _, cx| cx.notify()));
        }

        Self {
            input,
            filters: SearchFilters::default(),
            apps: Vec::new(),
            icons: cx.new(|_| KpImageCache::with_capacity(32)),
            hints: false,
            pinned: false,
            editing: false,
            language: i18n::language(),
            pending_keyword: None,
            debounce: None,
            edit_press: false,
            _subscriptions: subscriptions,
        }
    }

    /// 在搜索框上按下鼠标：等这次按下派发完再决定是否请求编辑态。漏斗和标签在同一次捕获阶段里
    /// 随后清掉标记，按在它们上面就不进入编辑态（编辑态会把焦点抢回输入框，刚打开的菜单随之关闭）。
    /// 不能用遮挡命中区域代替：下拉菜单的点击监听在漏斗外层，遮挡会把它也挡住。
    fn press_search(&mut self, cx: &mut Context<Self>) {
        if self.editing {
            return;
        }

        self.edit_press = true;
        let header = cx.entity().downgrade();
        cx.defer(move |cx| {
            header
                .update(cx, |header, cx| {
                    if std::mem::take(&mut header.edit_press) {
                        cx.emit(HeaderEvent::RequestEditing(EditTrigger::Mouse));
                    }
                })
                .ok();
        });
    }

    pub fn pending_update_version(&self, cx: &App) -> Option<String> {
        updater::pending_update(cx).map(|update| update.version.clone())
    }

    pub fn input(&self) -> &TextInput {
        &self.input
    }

    pub fn set_hints(&mut self, hints: bool, cx: &mut Context<Self>) {
        if self.hints != hints {
            self.hints = hints;
            cx.notify();
        }
    }

    pub fn set_pinned(&mut self, pinned: bool, cx: &mut Context<Self>) {
        self.pinned = pinned;
        cx.notify();
    }

    pub fn editing(&self) -> bool {
        self.editing
    }

    pub fn set_editing(&mut self, editing: bool, cx: &mut Context<Self>) {
        self.editing = editing;
        cx.notify();
    }

    /// 清空搜索框和筛选标签，取消还没发出的防抖。由调用方同步列表的筛选条件。
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_keyword = None;
        self.debounce = None;
        self.filters = SearchFilters::default();
        self.input.set_value("", window, cx);
        cx.notify();
    }

    /// 筛选菜单里可选的来源应用。
    pub fn set_apps(&mut self, apps: Vec<SourceApp>, cx: &mut Context<Self>) {
        self.apps = apps;
        cx.notify();
    }

    /// 面板隐藏时释放菜单里的应用图标。
    pub fn release_icons(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.icons
            .update(cx, |icons, cx| icons.clear(Some(window), cx));
    }

    /// 改筛选条件并通知面板。
    pub fn change_filters(
        &mut self,
        change: impl FnOnce(&mut SearchFilters),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.filters);
        cx.emit(HeaderEvent::Filters(self.filters.clone()));
        cx.notify();
    }

    /// 空输入框里的 Backspace：移除最后一个标签。没有标签时返回 `false`，按键照常交给输入框。
    fn pop_chip(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(last) = chips(&self.filters).pop() else {
            return false;
        };

        self.change_filters(|filters| last.remove_from(filters), cx);
        true
    }

    fn icon_key(path: &str, window: &Window) -> ImageKey {
        let side = (space(APP_ICON / 4.).to_pixels(window.rem_size()).as_f32()
            * window.scale_factor())
        .ceil()
        .max(1.) as u32;

        ImageKey {
            modified: None,
            path: path_of(path),
            width: side,
            height: side,
            resize: ResizeMode::Contain,
        }
    }

    /// 指针移到漏斗上时先解码应用图标，菜单打开时多半已经就绪。
    fn load_icons(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for app in &self.apps {
            let Some(path) = app.icon_path.as_deref() else {
                continue;
            };
            let key = Self::icon_key(path, window);
            self.icons
                .update(cx, |icons, cx| icons.request(key, window, cx));
        }
    }

    /// 编辑态开始后聚焦输入框并全选（1.x `focus({ cursor: "all" })`）。
    pub fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.focus(window, cx);
        self.input.select_all(window, cx);
    }

    /// 自测用：当作用户输入了 `value`（与真实输入一样经过防抖）。
    pub fn type_text(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.set_value(value.to_owned(), window, cx);
        self.debounce_keyword(SharedString::from(value.to_owned()), cx);
    }

    /// 搜索框里的筛选标签：前 [`VISIBLE_CHIPS`] 个带 ×，其余合成 `+N`，点它打开筛选菜单。
    fn render_chips(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let all = chips(&self.filters);
        let hidden = all.len().saturating_sub(VISIBLE_CHIPS);
        let mut elements: Vec<AnyElement> = all
            .into_iter()
            .take(VISIBLE_CHIPS)
            .enumerate()
            .map(|(index, chip)| self.render_chip(index, chip, cx))
            .collect();
        if hidden > 0 {
            let header = cx.entity().downgrade();
            elements.push(
                MenuTrigger::new("clipboard-search-filter-more")
                    .capture_any_mouse_down(cx.listener(keep_editing_off))
                    .child(
                        chip_body(cx).px(space(2.)).cursor_pointer().child(
                            div()
                                .kp_text(TextSize::Xs)
                                .child(SharedString::from(format!("+{hidden}"))),
                        ),
                    )
                    .dropdown(move |window, cx| filter_menu(&header, window, cx)),
            );
        }

        elements
    }

    fn render_chip(&self, index: usize, chip: Chip, cx: &mut Context<Self>) -> AnyElement {
        let label = chip.label();
        let remove = t_args("clipboard:filter.remove", &[("name", &label)]);
        let hover = theme::components(cx).icon_button.selected_hover;

        chip_body(cx)
            .id(("clipboard-search-filter-chip", index))
            .capture_any_mouse_down(cx.listener(keep_editing_off))
            .pl(space(2.))
            .pr(space(0.5))
            .child(
                div()
                    .kp_text(TextSize::Xs)
                    .max_w(space(18.))
                    .truncate()
                    .child(label),
            )
            .child(
                div()
                    .id(("clipboard-search-filter-remove", index))
                    .role(gpui::Role::Button)
                    .aria_label(remove)
                    .flex()
                    .flex_none()
                    .size(space(4.))
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(Icon::new(IconName::Close).size(space(3.)))
                    .on_click(cx.listener(move |header, _, _, cx| {
                        header.change_filters(|filters| chip.remove_from(filters), cx);
                    })),
            )
            .into_any_element()
    }

    /// 搜索框右端的漏斗：有筛选条件时高亮。
    fn render_filter_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let header = cx.entity().downgrade();
        let look = if self.filters.is_empty() {
            Look::Text
        } else {
            Look::Selected
        };

        // 图标按钮的悬停已给了提示框，预取图标挂在外层的鼠标移动上（已缓存的图标再请求不会重复解码）。
        MenuTrigger::new("clipboard-search-filter")
            .capture_any_mouse_down(cx.listener(keep_editing_off))
            .on_mouse_move(cx.listener(|header, _: &MouseMoveEvent, window, cx| {
                header.load_icons(window, cx);
            }))
            .child(icon_button(
                cx,
                "clipboard-search-filter-button",
                Glyph::Icon(IconName::ListFilter),
                t("clipboard:filter.title"),
                None,
                look,
            ))
            .dropdown(move |window, cx| filter_menu(&header, window, cx))
    }

    fn debounce_keyword(&mut self, value: SharedString, cx: &mut Context<Self>) {
        let keyword: Arc<str> = Arc::from(value.trim());
        // 只记长度：搜索词是用户内容，不进日志。
        log::debug!("search input changed ({} chars)", keyword.chars().count());
        self.pending_keyword = Some(keyword);
        self.debounce = Some(cx.spawn(async move |header, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            header
                .update(cx, |header, cx| {
                    if let Some(keyword) = header.pending_keyword.take() {
                        cx.emit(HeaderEvent::Keyword(keyword));
                    }
                })
                .ok();
        }));
    }

    /// 取走还没发出的搜索词并取消防抖，由调用方当场应用：防抖期间按 Enter 不能按搜索前的结果执行。
    /// 事件要等这次更新结束才送达，所以直接返回而不是 emit。
    pub fn take_pending_keyword(&mut self) -> Option<Arc<str>> {
        self.debounce = None;
        self.pending_keyword.take()
    }
}

/// 按在漏斗或标签上：这次按下不进入编辑态。
fn keep_editing_off(
    header: &mut Header,
    _: &MouseDownEvent,
    _: &mut Window,
    _: &mut Context<Header>,
) {
    header.edit_press = false;
}

/// 筛选标签的胶囊：淡主色底、主色文字（同固定窗口按钮的选中态）。
fn chip_body(cx: &App) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .h(space(5.))
        .items_center()
        .gap(space(0.5))
        .rounded_full()
        .bg(theme::components(cx).icon_button.selected)
        .text_color(theme::semantic(cx).accent.solid)
}

impl Render for Header {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != i18n::language() {
            self.language = i18n::language();
            self.input
                .set_placeholder(t("clipboard:header.searchPlaceholder"), window, cx);
        }
        let pending_version = self.pending_update_version(cx);
        let chips = self.render_chips(cx);
        let filter_button = self.render_filter_button(cx);
        let tokens = theme::semantic(cx);
        let shortcuts = crate::core_host::core(cx)
            .map(|core| core.settings().shortcuts)
            .unwrap_or_default();
        let hint = |action| {
            self.hints
                .then(|| crate::clipboard::model::shortcut::panel_hint(&shortcuts, action))
                .flatten()
                .map(SharedString::from)
        };
        let pin_label = if self.pinned {
            t("clipboard:header.unpin")
        } else {
            t("clipboard:header.pin")
        };

        div()
            .flex()
            .flex_none()
            .w_full()
            .items_center()
            .gap(space(2.))
            .px(space((12.) / 4.))
            .pt(space((12.) / 4.))
            .pb(space((8.) / 4.))
            .child(
                div()
                    .flex_none()
                    .window_drag_area()
                    .child(img(ImageSource::Image(logo())).size(space(5.))),
            )
            .when_some(pending_version, |element, version| {
                let label = t_args("clipboard:header.updateAvailable", &[("version", &version)]);
                element.child(
                    div()
                        .id("clipboard-header-update")
                        .role(gpui::Role::Button)
                        .aria_label(label.clone())
                        .flex_none()
                        .rounded_full()
                        .px(space(1.5))
                        .py(space(0.5))
                        .kp_text(TextSize::Xs)
                        .bg(tokens.accent.solid)
                        .text_color(tokens.text.on_accent)
                        .cursor_pointer()
                        .hover(|style| style.bg(tokens.accent.hover))
                        .kp_tooltip(label)
                        .child(t("clipboard:header.updateBadge"))
                        .on_click(|_, _, cx| updater::open_pending_update(cx)),
                )
            })
            // 留一段空白拖动窗口，搜索框左缘大致对齐分组栏第一条分隔线。可点徽标不属于拖动区域，
            // macOS 的拖动会吞掉子元素点击。
            .child(
                div()
                    .flex_none()
                    .w(space(14.))
                    .self_stretch()
                    .window_drag_area(),
            )
            .child(
                div()
                    .id("clipboard-search")
                    .flex_1()
                    .min_w_0()
                    .key_context(SEARCH_CONTEXT)
                    .capture_any_mouse_down(cx.listener(|header, event: &MouseDownEvent, _, cx| {
                        if event.button == MouseButton::Left {
                            header.press_search(cx);
                        }
                    }))
                    .capture_key_down(cx.listener(|header, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "backspace"
                            && header.input.value(cx).is_empty()
                            && header.pop_chip(cx)
                        {
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        Input::search(&self.input)
                            .small()
                            .hint_key(hint(PanelAction::FocusSearch))
                            .accessibility_label(t("clipboard:header.searchPlaceholder"))
                            .chips(chips)
                            .suffix(filter_button),
                    ),
            )
            // 固定后图标转正（-45°），和卡片上的置顶图钉一个朝向。
            .child(
                icon_button(
                    cx,
                    "clipboard-header-pin",
                    if self.pinned {
                        Glyph::Rotated(IconName::PinWindow, radians(-FRAC_PI_4))
                    } else {
                        Glyph::Icon(IconName::PinWindow)
                    },
                    pin_label,
                    hint(PanelAction::PinWindow),
                    if self.pinned {
                        Look::Selected
                    } else {
                        Look::Text
                    },
                )
                .on_click(cx.listener(|_, _, _, cx| cx.emit(HeaderEvent::TogglePin))),
            )
    }
}
