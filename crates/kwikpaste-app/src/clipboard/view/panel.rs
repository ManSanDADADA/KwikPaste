//! 主窗口（附录 D §3）：头部、分组栏、列表与页脚，以及整窗的按键。
//!
//! 按键都绑在 [`KEY_CONTEXT`] 上，焦点在列表或搜索框里都会命中；搜索框聚焦时由
//! [`super::SEARCH_CONTEXT`] 的绑定把 ↑/↓/Enter/Esc/Tab 交给列表。

use std::{rc::Rc, sync::Arc};

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, InteractiveElement as _,
    IntoElement, ModifiersChangedEvent, ParentElement as _, Render, Styled as _, Subscription,
    Window, div,
};
use kwikpaste_core::{
    CoreEvent,
    settings::{
        ListLayout, Settings, WINDOW_OPEN_GROUP_PREFIX, WINDOW_OPEN_SELECTION_ALL,
        WindowOpenCategorySelection, WindowOpenRangeSelection,
    },
};
use kwikpaste_ui::{
    ConfirmSpec, close_dialog, confirm, has_dialog, menu_open, theme,
    toast::{self, Toast},
};

use super::{
    CopySelected, DeleteSelected, Dismiss, EditContent, EditNote, FocusSearch, KEY_CONTEXT,
    NewGroup, NextCategory, NextGroup, OpenPreferences, OpenSelected, PasteSelected,
    PasteSelectedPlain, PinWindow, PreviousCategory, PreviousGroup, QuickPaste, SelectAll,
    SelectNext, SelectPrevious, ShowShortcuts, SplitSelected, ToggleFavorite, TogglePinned,
    ToggleRange,
    editing::{self, EditTarget},
    group_bar::{BarMode, GroupBar, GroupBarEvent},
    group_dialogs,
    header::{Header, HeaderEvent},
    host::ItemHost,
    list::{ClipboardList, ListIntent},
    pin, request_panel, shortcuts,
};
use crate::{
    clipboard::{
        model::{
            filter::{ListFilter, Range, SearchFilters, adjacent_group, bar_categories},
            item::{ItemKind, ListItem},
        },
        source::{ClipboardSource, Group},
    },
    i18n::{t, t_args},
    platform::{CoreEvents, EditTrigger, Panel, PanelCommand, PanelEvent, Trigger, TriggerSource},
};

/// 菜单打开时主窗口的按键动作都不响应：菜单不处理的键（例如 ←/→ 落在没有子菜单的项上、Mod+D）
/// 会冒泡到主窗口的绑定，而按键只该作用于菜单（1.x 的菜单是独立窗口，按键到不了列表）。
fn unless_menu<A: 'static>(
    handler: impl Fn(&mut ClipboardPanel, &A, &mut Window, &mut Context<ClipboardPanel>) + 'static,
) -> impl Fn(&mut ClipboardPanel, &A, &mut Window, &mut Context<ClipboardPanel>) + 'static {
    move |panel, action, window, cx| {
        if menu_open(window) {
            return;
        }
        handler(panel, action, window, cx);
    }
}

/// 主窗口发给宿主的意图：宿主一侧还没有的能力（偏好设置窗口）先只发事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelIntent {
    OpenPreferences,
}

pub struct ClipboardPanel {
    source: Arc<dyn ClipboardSource>,
    host: Rc<dyn ItemHost>,
    list: Entity<ClipboardList>,
    header: Entity<Header>,
    groups: Entity<GroupBar>,
    group_list: Vec<Group>,
    groups_loaded: bool,
    key_hints: bool,
    /// 双栏布局的右栏；单栏时为 `None`。
    side: Option<SidePane>,
    /// 焦点在右栏：按键作用于右栏的列表。
    side_active: bool,
    /// 已经排了一次切换布局，等它执行。
    layout_pending: bool,
    core_events: Option<Entity<CoreEvents>>,
    _subscriptions: Vec<Subscription>,
}

/// 双栏的右栏：收藏与自定义分组的页签和列表，随布局切回单栏一起释放。
struct SidePane {
    list: Entity<ClipboardList>,
    bar: Entity<GroupBar>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelIntent> for ClipboardPanel {}

impl ClipboardPanel {
    pub fn new(
        source: Arc<dyn ClipboardSource>,
        host: Rc<dyn ItemHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 列表先建：面板事件按订阅顺序送达，列表先处理 Shown（回到顶部、拿焦点），主窗口再按设置
        // 调整筛选条件。
        let list = cx.new(|cx| ClipboardList::new(source.clone(), host.clone(), window, cx));
        let header = cx.new(|cx| Header::new(window, cx));
        let entries = list.read(cx).settings().clipboard.window.group_bar.clone();
        let groups = cx.new(|_| GroupBar::new(BarMode::Full, entries));
        let list_focus = list.read(cx).focus_handle(cx);

        let mut subscriptions = vec![
            cx.subscribe_in(&header, window, Self::on_header_event),
            cx.subscribe_in(&groups, window, Self::on_group_event),
            cx.subscribe_in(&list, window, Self::on_list_intent),
            cx.on_focus_in(&list_focus, window, |panel, _, cx| {
                panel.set_side_active(false, cx);
            }),
        ];
        if let Some(panel) = cx.try_global::<Panel>() {
            let events = panel.events().clone();
            subscriptions.push(cx.subscribe_in(
                &events,
                window,
                |panel, _, event: &PanelEvent, window, cx| {
                    panel.on_panel_event(*event, window, cx);
                },
            ));
        }

        let mut panel = Self {
            source,
            host,
            list,
            header,
            groups,
            group_list: Vec::new(),
            groups_loaded: false,
            key_hints: false,
            side: None,
            side_active: false,
            layout_pending: false,
            core_events: None,
            _subscriptions: subscriptions,
        };
        panel.reload_groups(cx);
        panel.reload_filter_apps(cx);
        panel
    }

    pub fn list(&self) -> &Entity<ClipboardList> {
        &self.list
    }

    pub fn header(&self) -> &Entity<Header> {
        &self.header
    }

    fn on_list_intent(
        &mut self,
        list: &Entity<ClipboardList>,
        intent: &ListIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            ListIntent::SplitWords { id } => {
                list.update(cx, |list, cx| list.split_words(id.clone(), cx));
            }
            ListIntent::Changed => {
                let Some(side) = &self.side else {
                    return;
                };
                let other = if *list == self.list {
                    &side.list
                } else {
                    &self.list
                };
                other.update(cx, |list, cx| list.refresh(cx));
            }
            ListIntent::ShowShortcuts => shortcuts::show(window, cx),
            ListIntent::OpenPreferences => self.emit_intent(PanelIntent::OpenPreferences, cx),
            ListIntent::Paste { id, plain } => {
                log::debug!("paste intent handled by panel host: {id} plain={plain}");
            }
            ListIntent::PasteSnippet { id, .. } => {
                log::debug!("paste snippet intent handled by panel host: {id}");
            }
            ListIntent::DragOut { id } => {
                log::debug!("drag-out intent handled by panel host: {id}");
            }
        }
    }

    /// 全部自定义分组（含隐藏的），按排序。
    pub fn group_list(&self) -> &[Group] {
        &self.group_list
    }

    /// 跟随 core 的事件：记录变化、设置变化交给列表，分组变化重读分组。
    pub fn follow_core_events(&mut self, events: &Entity<CoreEvents>, cx: &mut Context<Self>) {
        self.list
            .update(cx, |list, cx| list.follow_core_events(events, cx));
        if let Some(side) = &self.side {
            side.list
                .update(cx, |list, cx| list.follow_core_events(events, cx));
        }
        self.core_events = Some(events.clone());
        self._subscriptions.push(cx.subscribe(
            events,
            |panel, _, event: &CoreEvent, cx| match event {
                CoreEvent::GroupsUpdated => panel.reload_groups(cx),
                CoreEvent::SettingsUpdated { settings, .. } => panel.apply_settings(settings, cx),
                _ => {}
            },
        ));
    }

    /// 分组栏按钮变了：换按钮，隐藏的按钮不再选中。布局在渲染时对齐（见 `sync_layout`），这里只要求重画。
    fn apply_settings(&mut self, settings: &Settings, cx: &mut Context<Self>) {
        let entries = &settings.clipboard.window.group_bar;
        if self.groups.read(cx).entries() != entries.as_slice() {
            let entries = entries.clone();
            self.groups
                .update(cx, |bar, cx| bar.set_entries(entries, cx));
            self.update_filter(|_| {}, cx);
        }
        if (settings.clipboard.display.layout == ListLayout::Split) != self.side.is_some() {
            cx.notify();
        }
    }

    // ------------------------------------------------------------ 双栏

    fn split(&self, cx: &App) -> bool {
        self.list.read(cx).settings().clipboard.display.layout == ListLayout::Split
    }

    /// 按键作用的列表：焦点在右栏时是右栏，否则是左栏（单栏时就是唯一的列表）。
    fn current(&self) -> Entity<ClipboardList> {
        match &self.side {
            Some(side) if self.side_active => side.list.clone(),
            _ => self.list.clone(),
        }
    }

    fn set_side_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.side_active == active {
            return;
        }

        self.side_active = active;
        // 数字角标只标在按键作用的那一栏上。
        if self.key_hints {
            self.list
                .update(cx, |list, cx| list.set_key_hints(!active, cx));
            if let Some(side) = &self.side {
                side.list
                    .update(cx, |list, cx| list.set_key_hints(active, cx));
            }
        }
    }

    /// 布局与设置不一致时，在渲染之外切换一次。
    fn sync_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout_pending || self.split(cx) == self.side.is_some() {
            return;
        }

        self.layout_pending = true;
        cx.defer_in(window, |panel, window, cx| {
            panel.layout_pending = false;
            panel.apply_layout(window, cx);
        });
    }

    /// 切到双栏：左栏固定看全部、不分组，右栏新建（默认看收藏）；切回单栏：释放右栏。
    fn apply_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let split = self.split(cx);
        if split == self.side.is_some() {
            return;
        }

        let mode = if split {
            BarMode::Categories
        } else {
            BarMode::Full
        };
        self.groups.update(cx, |bar, cx| bar.set_mode(mode, cx));
        if !split {
            if self.side_active {
                self.focus_list(window, cx);
            }
            if let Some(side) = self.side.take() {
                side.list.update(cx, |list, cx| list.dispose_preview(cx));
            }
            self.side_active = false;
            cx.notify();
            return;
        }

        self.update_filter(
            |filter| {
                filter.range = Range::All;
                filter.group_id = None;
            },
            cx,
        );
        self.side = Some(self.open_side(window, cx));
        cx.notify();
    }

    fn open_side(&mut self, window: &mut Window, cx: &mut Context<Self>) -> SidePane {
        let source = self.source.clone();
        let host = self.host.clone();
        let shown = self.list.read(cx).visible();
        let list = cx.new(|cx| {
            let mut list = ClipboardList::new(source, host, window, cx);
            list.set_side(cx);
            if shown {
                list.mark_shown(window, cx);
            }
            list
        });
        let bar = cx.new(|_| GroupBar::new(BarMode::Side, Vec::new()));
        let main = self.list.read(cx).filter();
        let filter = ListFilter {
            range: Range::Favorite,
            keyword: main.keyword.clone(),
            search: main.search.clone(),
            ..ListFilter::default()
        };
        let groups = self.group_list.clone();
        bar.update(cx, |bar, cx| {
            bar.set_groups(groups.clone(), cx);
            bar.set_filter(&filter, cx);
        });
        list.update(cx, |list, cx| {
            list.set_groups(groups, cx);
            list.set_filter(filter, cx);
            if let Some(events) = &self.core_events {
                list.follow_core_events(events, cx);
            }
        });
        let focus = list.read(cx).focus_handle(cx);

        SidePane {
            _subscriptions: vec![
                cx.subscribe_in(&list, window, Self::on_list_intent),
                cx.subscribe_in(&bar, window, Self::on_side_group_event),
                cx.on_focus_in(&focus, window, |panel, _, cx| {
                    panel.set_side_active(true, cx);
                }),
            ],
            list,
            bar,
        }
    }

    /// 改右栏的筛选条件：列表重载，页签跟着高亮。
    fn update_side_filter(&mut self, change: impl FnOnce(&mut ListFilter), cx: &mut Context<Self>) {
        let Some(side) = &self.side else {
            return;
        };

        let mut filter = side.list.read(cx).filter().clone();
        change(&mut filter);
        side.bar.update(cx, |bar, cx| bar.set_filter(&filter, cx));
        side.list.update(cx, |list, cx| list.set_filter(filter, cx));
    }

    /// 右栏的页签：`None` 是收藏，否则是这个自定义分组。
    fn select_side_tab(&mut self, group: Option<Arc<str>>, cx: &mut Context<Self>) {
        self.update_side_filter(
            |filter| {
                filter.range = if group.is_some() {
                    Range::All
                } else {
                    Range::Favorite
                };
                filter.group_id = group;
            },
            cx,
        );
    }

    fn side_tab(&self, cx: &App) -> Option<Arc<str>> {
        self.side
            .as_ref()
            .and_then(|side| side.list.read(cx).filter().group_id.clone())
    }

    /// 右栏页签按「收藏、各个可见的自定义分组」循环。
    fn cycle_side_tab(&mut self, reverse: bool, cx: &mut Context<Self>) {
        let Some(side) = &self.side else {
            return;
        };

        let mut tabs: Vec<Option<Arc<str>>> = vec![None];
        tabs.extend(side.bar.read(cx).visible_ids().into_iter().map(Some));
        let current = self.side_tab(cx);
        let index = tabs.iter().position(|tab| *tab == current).unwrap_or(0);
        let next = if reverse {
            (index + tabs.len() - 1) % tabs.len()
        } else {
            (index + 1) % tabs.len()
        };
        let tab = tabs.get(next).cloned().flatten();
        self.select_side_tab(tab, cx);
    }

    fn on_side_group_event(
        &mut self,
        _: &Entity<GroupBar>,
        event: &GroupBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            GroupBarEvent::SelectRange(_) => self.select_side_tab(None, cx),
            // 页签始终有一个选中：再点当前分组不取消。
            GroupBarEvent::ToggleGroup(id) => self.select_side_tab(Some(id.clone()), cx),
            GroupBarEvent::ToggleCategory(_) => {}
            GroupBarEvent::EditGroup(group) => self.edit_group(Some(group.clone()), window, cx),
            GroupBarEvent::HideGroup(group) => self.hide_group(group.clone(), window, cx),
            GroupBarEvent::DeleteGroup(group) => self.delete_group(group.clone(), window, cx),
            GroupBarEvent::NewGroup => self.edit_group(None, window, cx),
            GroupBarEvent::ManageGroups => self.manage_groups(window, cx),
        }
    }

    /// 焦点换到另一栏（双栏的 Mod+Q）。
    fn switch_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(side) = &self.side else {
            return;
        };

        let target = if self.side_active {
            self.list.read(cx).focus_handle(cx)
        } else {
            side.list.read(cx).focus_handle(cx)
        };
        window.focus(&target, cx);
        self.set_side_active(!self.side_active, cx);
    }

    // ------------------------------------------------------------ 分组

    /// 重读自定义分组；选中的分组已不存在时回到全部（1.x `ensureSelectedGroupStillExists`）。
    pub fn reload_groups(&mut self, cx: &mut Context<Self>) {
        let future = self.source.groups();

        cx.spawn(async move |panel, cx| {
            let result = future.await;
            panel
                .update(cx, |panel, cx| match result {
                    Ok(groups) => panel.apply_groups(groups, cx),
                    Err(err) => log::warn!("clipboard groups could not be loaded: {err:#}"),
                })
                .ok();
        })
        .detach();
    }

    fn apply_groups(&mut self, groups: Vec<Group>, cx: &mut Context<Self>) {
        let selected = self.list.read(cx).filter().group_id.clone();
        if let Some(selected) = selected
            && !groups.iter().any(|group| group.id == selected)
        {
            self.update_filter(|filter| filter.group_id = None, cx);
        }
        if let Some(tab) = self.side_tab(cx)
            && !groups.iter().any(|group| group.id == tab)
        {
            self.select_side_tab(None, cx);
        }

        self.groups_loaded = true;
        self.group_list = groups.clone();
        self.groups
            .update(cx, |bar, cx| bar.set_groups(groups.clone(), cx));
        if let Some(side) = &self.side {
            side.bar
                .update(cx, |bar, cx| bar.set_groups(groups.clone(), cx));
            side.list
                .update(cx, |list, cx| list.set_groups(groups.clone(), cx));
        }
        self.list.update(cx, |list, cx| list.set_groups(groups, cx));
    }

    /// 改筛选条件：列表重载，分组栏跟着高亮。分组栏上隐藏的按钮不会被选中（Mod+Q、打开时选中）。
    fn update_filter(&mut self, change: impl FnOnce(&mut ListFilter), cx: &mut Context<Self>) {
        let mut filter = self.list.read(cx).filter().clone();
        change(&mut filter);
        filter.keep_to_bar(self.groups.read(cx).entries());
        self.groups
            .update(cx, |bar, cx| bar.set_filter(&filter, cx));
        self.list.update(cx, |list, cx| list.set_filter(filter, cx));
    }

    fn toggle_group(&mut self, id: Arc<str>, cx: &mut Context<Self>) {
        self.update_filter(
            |filter| {
                filter.group_id = if filter.group_id.as_ref() == Some(&id) {
                    None
                } else {
                    Some(id)
                };
            },
            cx,
        );
    }

    /// ←/→：在分组栏显示的分类之间切换。
    fn cycle_category(&mut self, forward: bool, cx: &mut Context<Self>) {
        let categories = bar_categories(self.groups.read(cx).entries());
        let Some(kind) = self
            .list
            .read(cx)
            .filter()
            .adjacent_category(forward, &categories)
        else {
            return;
        };
        self.update_filter(|filter| filter.category = Some(kind), cx);
    }

    fn toggle_category(&mut self, kind: ItemKind, cx: &mut Context<Self>) {
        self.update_filter(
            |filter| {
                filter.category = if filter.category == Some(kind) {
                    None
                } else {
                    Some(kind)
                };
            },
            cx,
        );
    }

    fn on_group_event(
        &mut self,
        _: &Entity<GroupBar>,
        event: &GroupBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            GroupBarEvent::SelectRange(range) => {
                let range = *range;
                self.update_filter(|filter| filter.range = range, cx);
            }
            GroupBarEvent::ToggleCategory(kind) => self.toggle_category(*kind, cx),
            GroupBarEvent::ToggleGroup(id) => self.toggle_group(id.clone(), cx),
            GroupBarEvent::EditGroup(group) => self.edit_group(Some(group.clone()), window, cx),
            GroupBarEvent::HideGroup(group) => self.hide_group(group.clone(), window, cx),
            GroupBarEvent::DeleteGroup(group) => self.delete_group(group.clone(), window, cx),
            GroupBarEvent::NewGroup => self.edit_group(None, window, cx),
            GroupBarEvent::ManageGroups => self.manage_groups(window, cx),
        }
    }

    /// 新增（`None`）或编辑分组；保存后重读分组。
    pub(super) fn edit_group(
        &mut self,
        group: Option<Group>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity().downgrade();
        group_dialogs::edit_group(
            self.source.clone(),
            group,
            move |_, _, cx| {
                panel.update(cx, |panel, cx| panel.reload_groups(cx)).ok();
            },
            window,
            cx,
        );
    }

    /// 管理分组（1.x 在偏好设置里，偏好设置窗口在 U3）：任何写入之后重读分组。
    pub(super) fn manage_groups(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.entity().downgrade();
        group_dialogs::manage_groups(
            self.source.clone(),
            self.group_list.clone(),
            move |_, cx| {
                panel.update(cx, |panel, cx| panel.reload_groups(cx)).ok();
            },
            window,
            cx,
        );
    }

    /// 隐藏分组（1.x 右键菜单“隐藏分组”）：名称和图标不变，只改显隐。
    fn hide_group(&mut self, group: Group, window: &mut Window, cx: &mut Context<Self>) {
        let future = self.source.hide_group(group);

        cx.spawn_in(window, async move |panel, cx| {
            let result = future.await;
            panel
                .update_in(cx, |panel, window, cx| {
                    match result {
                        Ok(()) => toast::show(
                            Toast::success(t("commands:messages.clipboardGroupSaved")),
                            window,
                            cx,
                        ),
                        Err(err) => Self::toast_error(
                            "commands:labels.saveClipboardGroup",
                            &err,
                            window,
                            cx,
                        ),
                    }
                    panel.reload_groups(cx);
                })
                .ok();
        })
        .detach();
    }

    /// 删除分组：先确认，组内记录回到未分组；删的是选中的分组时回到全部。
    fn delete_group(&mut self, group: Group, window: &mut Window, cx: &mut Context<Self>) {
        let answer = confirm(
            ConfirmSpec::new(t("clipboard:groups.delete"))
                .content(t_args(
                    "clipboard:groups.deleteConfirmDescription",
                    &[("group", &group.name)],
                ))
                .ok_text(t("common:actions.delete"))
                .cancel_text(t("common:actions.cancel"))
                .danger(),
            window,
            cx,
        );
        let source = self.source.clone();

        cx.spawn_in(window, async move |panel, cx| {
            if !answer.await.unwrap_or(false) {
                return;
            }
            let result = source.delete_group(group.id.clone()).await;
            panel
                .update_in(cx, |panel, window, cx| {
                    match result {
                        Ok(()) => {
                            toast::show(
                                Toast::success(t("commands:messages.clipboardGroupDeleted")),
                                window,
                                cx,
                            );
                            if panel.list.read(cx).filter().group_id.as_ref() == Some(&group.id) {
                                panel.update_filter(|filter| filter.group_id = None, cx);
                            }
                            if panel.side_tab(cx).as_ref() == Some(&group.id) {
                                panel.select_side_tab(None, cx);
                            }
                        }
                        Err(err) => Self::toast_error(
                            "commands:labels.deleteClipboardGroup",
                            &err,
                            window,
                            cx,
                        ),
                    }
                    panel.reload_groups(cx);
                })
                .ok();
        })
        .detach();
    }

    fn toast_error(label: &str, err: &anyhow::Error, window: &mut Window, cx: &mut Context<Self>) {
        log::warn!("{label} failed: {err:#}");
        let message = t_args(
            "commands:error",
            &[("label", &t(label)), ("message", &err.to_string())],
        );
        toast::show(Toast::error(message), window, cx);
    }

    fn emit_intent(&mut self, intent: PanelIntent, cx: &mut Context<Self>) {
        log::info!("panel intent: {intent:?}");
        cx.emit(intent);
    }

    // ------------------------------------------------------------ 头部与编辑态

    fn on_header_event(
        &mut self,
        _: &Entity<Header>,
        event: &HeaderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            HeaderEvent::Keyword(keyword) => self.apply_keyword(keyword.clone(), cx),
            HeaderEvent::RequestEditing(trigger) => {
                editing::begin(EditTarget::Search, *trigger, cx);
            }
            HeaderEvent::TogglePin => self.toggle_pin(cx),
            HeaderEvent::Filters(search) => {
                let side_search = search.clone();
                let search = search.clone();
                self.update_filter(|filter| filter.search = search, cx);
                self.update_side_filter(|filter| filter.search = side_search, cx);
            }
        }
    }

    fn apply_keyword(&mut self, keyword: Arc<str>, cx: &mut Context<Self>) {
        let side_keyword = keyword.clone();
        self.update_filter(|filter| filter.keyword = keyword, cx);
        self.update_side_filter(|filter| filter.keyword = side_keyword, cx);
    }

    /// 粘贴类按键之前：搜索词还在防抖就当场应用，列表随即挂起按键，等新结果的第一页落地再执行。
    fn flush_keyword(&mut self, cx: &mut Context<Self>) {
        if let Some(keyword) = self
            .header
            .update(cx, |header, _| header.take_pending_keyword())
        {
            self.apply_keyword(keyword, cx);
        }
    }

    /// 重读筛选菜单里可选的来源应用。
    fn reload_filter_apps(&mut self, cx: &mut Context<Self>) {
        let future = self.source.history_apps();

        cx.spawn(async move |panel, cx| {
            let result = future.await;
            panel
                .update(cx, |panel, cx| match result {
                    Ok(apps) => panel
                        .header
                        .update(cx, |header, cx| header.set_apps(apps, cx)),
                    Err(err) => log::warn!("filter apps could not be loaded: {err:#}"),
                })
                .ok();
        })
        .detach();
    }

    fn focus_list(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.list.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }

    fn focus_dialog_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = editing::dialog_input(cx) {
            input.focus(window, cx);
        }
    }

    /// 固定 / 取消固定窗口（头部图钉、Mod+P）：点外部不隐藏，粘贴、复制后留着面板。
    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        let pinned = !pin::pinned(cx);
        log::info!("panel pinned: {pinned}");
        pin::set_pinned(pinned, cx);
        self.header
            .update(cx, |header, cx| header.set_pinned(pinned, cx));
    }

    /// 面板事件：显示时按设置重置筛选与搜索框，编辑态的进出交给对应的输入框。
    fn on_panel_event(&mut self, event: PanelEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PanelEvent::Shown => self.on_shown(window, cx),
            PanelEvent::Hidden => {
                self.header.update(cx, |header, cx| {
                    header.set_editing(false, cx);
                    header.release_icons(window, cx);
                });
                if self.list.read(cx).settings().clipboard.search.clear_on_hide {
                    self.clear_search(window, cx);
                }
                self.set_key_hints(false, cx);
                // 还开着的分组弹框、确认框当作取消（隐藏时编辑态已经结束，再显示时名称框打不了字）。
                for _ in 0..4 {
                    if !has_dialog(window, cx) {
                        break;
                    }
                    close_dialog(window, cx);
                }
            }
            PanelEvent::EditingStarted => match editing::target(cx) {
                Some(EditTarget::Search) => {
                    self.header.update(cx, |header, cx| {
                        header.set_editing(true, cx);
                        header.focus_input(window, cx);
                    });
                }
                Some(EditTarget::Dialog) => self.focus_dialog_input(window, cx),
                _ => {}
            },
            PanelEvent::EditingRefused => match editing::target(cx) {
                Some(EditTarget::Search) => {
                    editing::clear(cx);
                    self.focus_list(window, cx);
                }
                // 没拿到前台也聚焦，Esc 仍能关掉弹框。
                Some(EditTarget::Dialog) => self.focus_dialog_input(window, cx),
                _ => {}
            },
            PanelEvent::EditingEnded => {
                self.header
                    .update(cx, |header, cx| header.set_editing(false, cx));
                editing::clear(cx);
                // 左栏的列表退出编辑态时会收回焦点；编辑前焦点在右栏就还给右栏。
                if let Some(side) = self.side.as_ref().filter(|_| self.side_active) {
                    let focus = side.list.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }
            }
            PanelEvent::PopupDismissed => {}
        }
    }

    /// 显示时（1.x `handleWindowVisibility`）：清空搜索词，按“打开窗口时选中”的设置切换范围、
    /// 分类、分组；设置了默认聚焦搜索框时请求编辑态。
    fn on_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_layout(window, cx);
        self.reload_filter_apps(cx);
        let settings = self.list.read(cx).settings().clipboard.clone();
        let window_settings = settings.window;
        let clear = settings.search.clear_on_hide;
        let split = self.side.is_some();

        if clear {
            self.header
                .update(cx, |header, cx| header.clear(window, cx));
        }
        let startup_group = startup_group(
            &window_settings.select_group_on_open,
            &self.group_list,
            self.groups_loaded,
        );
        // 双栏时「打开时选中的分组」作用于右栏的页签，左栏始终是全部。
        if split {
            let group = startup_group.clone();
            self.update_side_filter(
                |filter| {
                    if clear {
                        filter.keyword = Arc::from("");
                        filter.search = SearchFilters::default();
                    }
                    if window_settings
                        .select_group_on_open
                        .starts_with(WINDOW_OPEN_GROUP_PREFIX)
                    {
                        filter.range = if group.is_some() {
                            Range::All
                        } else {
                            Range::Favorite
                        };
                        filter.group_id = group;
                    }
                },
                cx,
            );
        }
        self.update_filter(
            |filter| {
                if clear {
                    filter.keyword = Arc::from("");
                    filter.search = SearchFilters::default();
                }
                match window_settings.select_range_on_open {
                    WindowOpenRangeSelection::Preserve => {}
                    WindowOpenRangeSelection::All => filter.range = Range::All,
                    WindowOpenRangeSelection::Favorite => filter.range = Range::Favorite,
                }
                match window_settings.select_category_on_open {
                    WindowOpenCategorySelection::Preserve => {}
                    WindowOpenCategorySelection::All => filter.category = None,
                    WindowOpenCategorySelection::Text => filter.category = Some(ItemKind::Text),
                    WindowOpenCategorySelection::Image => filter.category = Some(ItemKind::Image),
                    WindowOpenCategorySelection::Files => filter.category = Some(ItemKind::Files),
                }
                let group = window_settings.select_group_on_open.as_str();
                if split {
                    filter.range = Range::All;
                    filter.group_id = None;
                } else if group == WINDOW_OPEN_SELECTION_ALL {
                    filter.group_id = None;
                } else if group.starts_with(WINDOW_OPEN_GROUP_PREFIX) {
                    filter.group_id = startup_group;
                }
            },
            cx,
        );

        if settings.search.default_focus {
            editing::begin(EditTarget::Search, EditTrigger::Keyboard, cx);
        }
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.header
            .update(cx, |header, cx| header.clear(window, cx));
        let clear = |filter: &mut ListFilter| {
            filter.keyword = Arc::from("");
            filter.search = SearchFilters::default();
        };
        self.update_filter(clear, cx);
        self.update_side_filter(clear, cx);
    }

    fn set_key_hints(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.key_hints == on {
            return;
        }

        self.key_hints = on;
        self.header
            .update(cx, |header, cx| header.set_hints(on, cx));
        self.groups.update(cx, |bar, cx| bar.set_hints(on, cx));
        if let Some(side) = &self.side {
            side.bar.update(cx, |bar, cx| bar.set_hints(on, cx));
        }
        self.current()
            .update(cx, |list, cx| list.set_key_hints(on, cx));
    }

    // ------------------------------------------------------------ 按键

    /// Esc 按预览、多选、分组、分类、窗口的顺序逐层退出（1.x `closeTopEscapeLayer`）；双栏时焦点在右栏
    /// 先回到左栏。
    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current();
        if current.update(cx, |list, cx| list.dismiss_reorder(cx)) {
            return;
        }
        if current.update(cx, |list, cx| list.close_preview(cx)) {
            return;
        }
        if current.read(cx).selecting() {
            current.update(cx, |list, cx| list.exit_selection(cx));
            return;
        }
        if self.side_active {
            self.switch_pane(window, cx);
            return;
        }

        let filter = self.list.read(cx).filter().clone();
        if filter.group_id.is_some() {
            self.update_filter(|filter| filter.group_id = None, cx);
            return;
        }
        if filter.category.is_some() {
            self.update_filter(|filter| filter.category = None, cx);
            return;
        }

        request_panel(cx, PanelCommand::Hide(Trigger::now(TriggerSource::Ui)));
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        if self.header.read(cx).editing() {
            self.header
                .update(cx, |header, cx| header.focus_input(window, cx));
            return;
        }

        editing::begin(EditTarget::Search, EditTrigger::Keyboard, cx);
    }

    /// 作用于当前项的快捷键；多选时不响应（1.x `handleSelectionKeyDown` 吞掉其余修饰键组合）。
    fn with_active(
        &mut self,
        cx: &mut Context<Self>,
        act: impl FnOnce(&mut ClipboardList, Arc<ListItem>, &mut Context<ClipboardList>),
    ) {
        self.current().update(cx, |list, cx| {
            if list.selecting() {
                return;
            }
            if let Some(item) = list.active_item() {
                act(list, item, cx);
            }
        });
    }
}

impl Render for ClipboardPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_layout(window, cx);
        let tokens = theme::semantic(cx);
        let surface = crate::platform::material::panel_surface(cx);

        let root = div()
            .id("clipboard-panel")
            .key_context(KEY_CONTEXT)
            .size_full()
            .flex()
            .flex_col()
            .bg(surface)
            .text_color(tokens.text.primary)
            .on_modifiers_changed(cx.listener(|panel, event: &ModifiersChangedEvent, _, cx| {
                panel.set_key_hints(event.modifiers.secondary(), cx);
            }))
            .on_action(cx.listener(unless_menu(|panel, _: &SelectPrevious, _, cx| {
                panel
                    .current()
                    .update(cx, |list, cx| list.select_previous(cx));
            })))
            .on_action(cx.listener(unless_menu(|panel, _: &SelectNext, _, cx| {
                panel.current().update(cx, |list, cx| list.select_next(cx));
            })))
            .on_action(cx.listener(unless_menu(|panel, _: &PasteSelected, _, cx| {
                panel.flush_keyword(cx);
                panel.current().update(cx, |list, cx| {
                    if list.preview_words(cx).is_some() {
                        // 预览里选了词：Enter 粘贴选中的词（1.x `enterPastePreviewWords`）。
                        list.use_preview_words(true, cx);
                    } else if !list.selecting() {
                        list.paste_active(false, cx);
                    } else if let Some(item) = list.active_item() {
                        // 多选时 Enter 勾选 / 取消当前项（1.x `handleSelectionKeyDown`）。
                        list.toggle_checked(&item, cx);
                    }
                });
            })))
            .on_action(
                cx.listener(unless_menu(|panel, _: &PasteSelectedPlain, _, cx| {
                    panel.flush_keyword(cx);
                    panel.current().update(cx, |list, cx| {
                        if !list.selecting() {
                            list.paste_active(true, cx);
                        }
                    });
                })),
            )
            .on_action(cx.listener(unless_menu(Self::dismiss)))
            .on_action(cx.listener(unless_menu(Self::focus_search)))
            .on_action(
                cx.listener(unless_menu(|panel, _: &ToggleRange, window, cx| {
                    // 双栏时收藏常驻右栏：Mod+Q 在两栏之间换焦点。
                    if panel.side.is_some() {
                        panel.switch_pane(window, cx);
                        return;
                    }
                    panel.update_filter(|filter| filter.range = filter.range.toggled(), cx);
                })),
            )
            .on_action(
                cx.listener(unless_menu(|panel, _: &PreviousCategory, _, cx| {
                    if panel.side_active {
                        panel.cycle_side_tab(true, cx);
                        return;
                    }
                    panel.cycle_category(false, cx);
                })),
            )
            .on_action(cx.listener(unless_menu(|panel, _: &NextCategory, _, cx| {
                if panel.side_active {
                    panel.cycle_side_tab(false, cx);
                    return;
                }
                panel.cycle_category(true, cx);
            })))
            .on_action(cx.listener(unless_menu(|panel, _: &NextGroup, _, cx| {
                panel.cycle_group(false, cx);
            })))
            .on_action(cx.listener(unless_menu(|panel, _: &PreviousGroup, _, cx| {
                panel.cycle_group(true, cx);
            })))
            .on_action(
                cx.listener(unless_menu(|panel, _: &CopySelected, window, cx| {
                    if panel.current().read(cx).preview_words(cx).is_some() {
                        panel
                            .current()
                            .update(cx, |list, cx| list.use_preview_words(false, cx));
                        return;
                    }
                    if panel.current().read(cx).selecting() {
                        panel
                            .current()
                            .update(cx, |list, cx| list.copy_checked(window, cx));
                        return;
                    }
                    panel.with_active(cx, |list, item, cx| {
                        list.copy(item.id.clone(), false, None, window, cx)
                    });
                })),
            )
            .on_action(
                cx.listener(unless_menu(|panel, _: &ToggleFavorite, window, cx| {
                    panel.with_active(cx, |list, item, cx| {
                        list.toggle_favorite(item.id.clone(), window, cx)
                    });
                })),
            )
            .on_action(
                cx.listener(unless_menu(|panel, _: &TogglePinned, window, cx| {
                    panel.with_active(cx, |list, item, cx| {
                        list.toggle_pinned(item.id.clone(), window, cx)
                    });
                })),
            )
            .on_action(cx.listener(unless_menu(|panel, _: &EditNote, window, cx| {
                panel.with_active(cx, |list, item, cx| {
                    list.edit_note(item.id.clone(), window, cx)
                });
            })))
            .on_action(
                cx.listener(unless_menu(|panel, _: &EditContent, window, cx| {
                    panel.with_active(cx, |list, item, cx| {
                        list.edit_content(item.id.clone(), window, cx)
                    });
                })),
            )
            .on_action(
                cx.listener(unless_menu(|panel, _: &OpenSelected, window, cx| {
                    panel.with_active(cx, |list, _, cx| list.open_active(window, cx));
                })),
            )
            .on_action(cx.listener(unless_menu(|panel, _: &SplitSelected, _, cx| {
                panel.with_active(cx, |list, item, cx| list.split(&item, cx));
            })))
            .on_action(
                cx.listener(unless_menu(|panel, _: &DeleteSelected, window, cx| {
                    panel.current().update(cx, |list, cx| {
                        if list.selecting() {
                            list.delete_checked(window, cx);
                        } else if let Some(item) = list.active_item() {
                            list.delete(item.id.clone(), window, cx);
                        }
                    });
                })),
            )
            .on_action(cx.listener(unless_menu(|panel, _: &SelectAll, window, cx| {
                panel.current().update(cx, |list, cx| {
                    if list.total() > 0 {
                        list.toggle_all(window, cx);
                    }
                });
            })))
            .on_action(
                cx.listener(unless_menu(|panel, action: &QuickPaste, _, cx| {
                    let key = action.key;
                    panel.flush_keyword(cx);
                    panel
                        .current()
                        .update(cx, |list, cx| list.quick_paste(key, cx));
                })),
            )
            .on_action(cx.listener(unless_menu(|panel, _: &PinWindow, _, cx| {
                panel.toggle_pin(cx)
            })))
            .on_action(
                cx.listener(unless_menu(|panel, _: &OpenPreferences, _, cx| {
                    panel.emit_intent(PanelIntent::OpenPreferences, cx);
                })),
            )
            .on_action(cx.listener(unless_menu(|panel, _: &ShowShortcuts, _, cx| {
                panel
                    .list
                    .update(cx, |_, cx| cx.emit(ListIntent::ShowShortcuts));
            })))
            .on_action(cx.listener(unless_menu(|panel, _: &NewGroup, window, cx| {
                panel.edit_group(None, window, cx);
            })))
            .child(self.header.clone());

        let column = |bar: Entity<GroupBar>, list: Entity<ClipboardList>| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(bar)
                .child(div().flex().flex_col().flex_1().min_h_0().child(list))
        };
        let left = column(self.groups.clone(), self.list.clone());
        let Some(side) = &self.side else {
            return root.child(left);
        };

        // 双栏：两列等宽，中间一条分隔线。
        root.child(
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(left)
                .child(div().flex_none().w(gpui::px(1.)).bg(tokens.border.divider))
                .child(column(side.bar.clone(), side.list.clone())),
        )
    }
}

impl ClipboardPanel {
    /// Tab / Shift+Tab：在可见的自定义分组间循环，落到当前分组上时取消（1.x `toggleCustomGroup`）。
    fn cycle_group(&mut self, reverse: bool, cx: &mut Context<Self>) {
        if self.side.is_some() {
            self.cycle_side_tab(reverse, cx);
            return;
        }
        let visible = self.groups.read(cx).visible_ids();
        let current = self.list.read(cx).filter().group_id.clone();
        if let Some(next) = adjacent_group(&visible, current.as_deref(), reverse) {
            self.toggle_group(next, cx);
        }
    }
}

/// 首次分组加载完成前沿用保存值，加载后只允许仍存在的分组。
fn startup_group(selection: &str, groups: &[Group], loaded: bool) -> Option<Arc<str>> {
    let id = selection.strip_prefix(WINDOW_OPEN_GROUP_PREFIX)?;
    (!loaded || groups.iter().any(|group| group.id.as_ref() == id)).then(|| Arc::from(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_group_drops_deleted_ids_after_loading() {
        let groups = vec![Group {
            id: Arc::from("kept"),
            name: Arc::from("Kept"),
            icon: Arc::from(""),
            is_hidden: false,
        }];
        assert_eq!(
            startup_group("group:kept", &groups, true).as_deref(),
            Some("kept")
        );
        assert_eq!(startup_group("group:deleted", &groups, true), None);
        assert_eq!(
            startup_group("group:kept", &[], false).as_deref(),
            Some("kept")
        );
        assert_eq!(startup_group("all", &groups, true), None);
        assert_eq!(startup_group("preserve", &groups, true), None);
    }
}
