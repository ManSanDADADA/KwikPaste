//! 列表控制器：当前项、键盘移动、数字提示和刷新策略，移植自 1.x `List.tsx` 的逻辑部分。
//!
//! - 当前项（“选中”）只有一个：没有显式选中时，第一个可见的项就是当前项；
//!   指针移进卡片就把它设为当前项，键盘和指针共用一个选中。
//! - ↑/↓ 夹在首尾、不循环；目标行没加载时先发请求，这一次移动作废（与 1.x 相同）。
//! - 剪贴板有新内容时：面板隐藏时立即重拉第一页（再显示总会回到顶部，显示时数据已经是新的）；
//!   显示中不在顶部只记挂起，回到顶部的那一帧再刷新（附录 D L3）。已有记录被复用时例外：
//!   就地重拉当前范围，让它马上离开原位、排到顶部，视口锚点由视图保持。

use std::sync::Arc;

use super::{
    filter::ListFilter,
    item::{ItemKind, ListItem},
    list_model::ListModel,
};

/// 方向键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
}

/// 一次方向键的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavOutcome {
    /// 当前项移到了模型下标 `index`，视图把它平滑滚进视口。
    Moved { index: usize },
    /// 目标行还没加载：已登记为可见范围，等数据到了再按。
    NeedsLoad { index: usize },
    /// 列表为空。
    Empty,
}

/// core 发来的列表变化（`CoreEvent` 中与列表有关的几种）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListUpdate {
    /// 新记录入库或已有记录被重新使用（`deduplicated`）。`group_id` 是新记录自动归入的分组。
    Upserted {
        id: Arc<str>,
        kind: ItemKind,
        deduplicated: bool,
        group_id: Option<Arc<str>>,
    },
    /// 清理删掉了 `removed` 条。
    Cleaned { removed: u64 },
    /// 历史数据整体换了一份：导入备份、切换存储位置（core `ClipboardReloaded`，1.x `imported`）。
    Reloaded,
    /// 图片文字识别有了进展或开关变了（core `OcrChanged`）：刷新搜索结果和图片能力。
    ImageTextChanged,
}

/// 收到列表变化后要做的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    /// 与当前视图无关。
    Ignore,
    /// 已在顶部：立即重拉第一页。
    ReloadNow {
        /// 同时清掉当前选中（清理、导入时）。
        reset_selection: bool,
    },
    /// 不在顶部或面板隐藏：记成挂起，回到顶部再刷新。
    Defer { reset_selection: bool },
    /// 显示中不在顶部时复用了已有记录或识别有了进展：就地重拉当前范围，视口锚点由视图保持。
    RefreshInPlace,
}

/// 数字提示 1–9、0（Mod+数字粘贴第 N 个可见项）。
pub const HINT_KEYS: [char; 10] = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'];

#[derive(Debug, Default)]
pub struct ListController {
    selected: Option<Arc<str>>,
    /// 第一个可见的项的模型下标（包含置顶行）。
    first_visible: usize,
    pending_reload: bool,
    filter: ListFilter,
}

impl ListController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn filter(&self) -> &ListFilter {
        &self.filter
    }

    pub fn set_first_visible(&mut self, index: usize) {
        self.first_visible = index;
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<&Arc<str>> {
        self.selected.as_ref()
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// 显式选中一条（点击卡片、快捷动作、右键菜单作用到它时）。
    pub fn select(&mut self, id: &Arc<str>) {
        self.selected = Some(id.clone());
    }

    /// 指针移进卡片：它成为当前项（1.x 的 hover 与键盘共用选中）。返回选中是否变了。
    pub fn hover(&mut self, id: &Arc<str>) -> bool {
        if self.selected.as_ref() == Some(id) {
            return false;
        }

        self.selected = Some(id.clone());
        true
    }

    /// 第 `index` 行、id 为 `id` 的卡片是不是当前项。
    pub fn is_active(&self, index: usize, id: &str) -> bool {
        match &self.selected {
            Some(selected) => &**selected == id,
            None => index == self.first_visible,
        }
    }

    /// 当前项的模型下标：显式选中且仍在缓存里时取它，否则取第一个可见的项。
    pub fn active_index(&self, model: &ListModel) -> usize {
        self.selected
            .as_deref()
            .and_then(|id| model.index_of(id))
            .unwrap_or(self.first_visible)
    }

    /// Enter 作用的记录：显式选中项，否则第一个可见项；未加载时不回退到屏外记录。
    pub fn active_item<'a>(&self, model: &'a ListModel) -> Option<&'a Arc<ListItem>> {
        if let Some(item) = self.selected.as_deref().and_then(|id| model.find(id)) {
            return Some(item);
        }

        model.get(self.first_visible)
    }

    /// 处理 ↑/↓（1.x `getNextKeyboardIndex` + `getNextKeyboardTarget`）。
    pub fn navigate(
        &mut self,
        nav: Nav,
        model: &mut ListModel,
    ) -> (NavOutcome, Option<super::list_model::FetchRequest>) {
        let total = model.total();
        if total == 0 {
            return (NavOutcome::Empty, None);
        }

        let current = self.active_index(model);
        let next = match nav {
            Nav::Up => current.saturating_sub(1),
            Nav::Down => (current + 1).min(total - 1),
        };

        match model.get(next) {
            Some(item) => {
                self.selected = Some(item.id.clone());
                (NavOutcome::Moved { index: next }, None)
            }
            None => {
                let request = model.load_range(next..next + 1);
                (NavOutcome::NeedsLoad { index: next }, request)
            }
        }
    }

    /// 删除记录后的选中（1.x `getSelectedIdAfterDelete`）：删的是当前项时选下一项，到底时选上一项；
    /// 删的不是当前项时保持原样。在模型删除之前调用。
    pub fn select_after_delete(&mut self, model: &ListModel, deleted_id: &str) {
        let Some(deleted) = model.index_of(deleted_id) else {
            return;
        };
        let active = self
            .selected
            .clone()
            .or_else(|| model.get(self.first_visible).map(|item| item.id.clone()))
            .or_else(|| model.get(0).map(|item| item.id.clone()));
        if active.as_deref() != Some(deleted_id) {
            return;
        }

        self.selected = model
            .get(deleted + 1)
            .or_else(|| {
                deleted
                    .checked_sub(1)
                    .and_then(|previous| model.get(previous))
            })
            .map(|item| item.id.clone());
    }

    /// 第 `index` 行的数字提示：只标在前 10 个可见项上，多选时不显示。
    pub fn hint_key(&self, index: usize, selecting: bool) -> Option<char> {
        if selecting {
            return None;
        }

        let relative = index.checked_sub(self.first_visible)?;
        HINT_KEYS.get(relative).copied()
    }

    /// Mod+数字：数字键对应的模型下标（`1` 是第一个可见项，`0` 是第十个）。
    pub fn hint_index(&self, key: char) -> Option<usize> {
        HINT_KEYS
            .iter()
            .position(|hint| *hint == key)
            .map(|slot| self.first_visible + slot)
    }

    /// 列表变化的处理决定（1.x `handleClipboardUpdated` + `requestReloadAtTop`）。
    pub fn on_update(&mut self, update: &ListUpdate, visible: bool, at_top: bool) -> UpdateAction {
        let reset_selection = match update {
            ListUpdate::Cleaned { .. } | ListUpdate::Reloaded => true,
            ListUpdate::Upserted {
                kind,
                deduplicated,
                group_id,
                ..
            } => {
                let relevant = if *deduplicated {
                    self.filter.may_include_existing(Some(*kind))
                } else {
                    self.filter.may_include(Some(*kind), group_id.as_deref())
                };
                if !relevant {
                    return UpdateAction::Ignore;
                }
                false
            }
            ListUpdate::ImageTextChanged => {
                if !self.filter.may_include_existing(Some(ItemKind::Image)) {
                    return UpdateAction::Ignore;
                }
                false
            }
        };
        if reset_selection {
            self.selected = None;
        }
        // 隐藏时立即重拉：不打断任何人的浏览位置，显示时也不用再等一次查询。
        if !visible {
            self.pending_reload = false;
            return UpdateAction::ReloadNow { reset_selection };
        }
        if !at_top
            && matches!(
                update,
                ListUpdate::Upserted {
                    deduplicated: true,
                    ..
                } | ListUpdate::ImageTextChanged
            )
        {
            return UpdateAction::RefreshInPlace;
        }

        self.request_reload_at_top(at_top, reset_selection)
    }

    /// 是否有挂起的刷新。
    #[cfg(test)]
    pub fn has_pending_reload(&self) -> bool {
        self.pending_reload
    }

    /// 每帧调用：到顶且有挂起的刷新时消费它（1.x `consumeDeferredReloadAtTop`，附录 D L3）。
    pub fn take_reload_at_top(&mut self, at_top: bool) -> bool {
        if !self.pending_reload || !at_top {
            return false;
        }

        self.pending_reload = false;
        true
    }

    /// 面板显示时（`scrollToTopOnOpen` 默认开启）：清掉选中，回到顶部后由下一帧消费挂起的刷新。
    pub fn on_shown(&mut self) {
        self.selected = None;
    }

    /// 过滤条件变化：清掉选中和挂起的刷新（视图会整体重载）。
    pub fn set_filter(&mut self, filter: ListFilter) {
        self.filter = filter;
        self.selected = None;
        self.pending_reload = false;
    }

    fn request_reload_at_top(&mut self, at_top: bool, reset_selection: bool) -> UpdateAction {
        if !at_top {
            self.pending_reload = true;
            return UpdateAction::Defer { reset_selection };
        }

        self.pending_reload = false;
        UpdateAction::ReloadNow { reset_selection }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::super::{
        item::Platform,
        list_model::{FetchRequest, Page},
    };
    use super::*;

    fn item(id: &str, pinned: bool) -> Arc<ListItem> {
        Arc::new(ListItem {
            id: id.into(),
            kind: ItemKind::Text,
            sub_kind: None,
            group_id: None,
            content: "".into(),
            summary: None,
            width: None,
            height: None,
            is_favorite: false,
            is_pinned: pinned,
            is_sensitive: false,
            platform: Platform::Windows,
            note: None,
            created_at: Utc.timestamp_opt(0, 0).single().unwrap_or_default(),
            source_app_id: None,
            source_app_name: None,
            source_app_icon_path: None,
            origin_device_id: None,
            origin_device_name: None,
            image_thumbnail_path: None,
            file_entries: None,
            files_preview_kind: None,
            available_actions: Vec::new(),
            color_preview: None,
            quick_snippets: Vec::new(),
            image_display: None,
            has_image_text: false,
            image_text_snippet: None,
        })
    }

    /// 加载了前 `loaded` 行（共 `total` 行，前 `pinned` 行置顶）的模型。
    fn model(total: usize, loaded: usize, pinned: usize) -> ListModel {
        let mut model = ListModel::new();
        let request = model.reset_and_reload();
        let page = Page {
            items: (0..loaded.min(30))
                .map(|index| item(&format!("r{index}"), index < pinned))
                .collect(),
            total,
        };
        model.apply(&request, page);
        model
    }

    #[test]
    fn default_active_item_is_the_first_visible_row_including_pins() {
        let model = model(100, 30, 2);
        let mut controller = ListController::new();
        assert!(controller.is_active(0, "r0"));
        assert_eq!(
            controller.active_item(&model).map(|item| &*item.id),
            Some("r0")
        );
        controller.set_first_visible(1);
        assert!(controller.is_active(1, "r1"));
        assert_eq!(controller.active_index(&model), 1);
        controller.set_first_visible(5);
        assert!(!controller.is_active(0, "r0"));
        assert_eq!(
            controller.active_item(&model).map(|item| &*item.id),
            Some("r5")
        );
    }

    #[test]
    fn activation_after_current_range_refresh_uses_visible_rows() {
        let mut model = model(500, 30, 0);
        let mut controller = ListController::new();
        controller.set_first_visible(200);
        assert!(controller.active_item(&model).is_none());
        model.load_range(200..210);
        let request = model.reload_current_range().expect("current range");
        let mut freshness = super::super::freshness::Freshness::default();
        freshness.changed();
        freshness.first_page_sent(request.token);
        assert!(freshness.awaiting());
        let rows = request
            .range
            .clone()
            .map(|i| item(&format!("r{i}"), false))
            .collect();
        assert!(
            model
                .apply(
                    &request,
                    Page {
                        items: rows,
                        total: 500
                    }
                )
                .is_some()
        );
        freshness.first_page_done(request.token);
        assert!(freshness.fresh());
        controller.hover(&"r203".into());
        assert_eq!(
            controller.active_item(&model).map(|item| &*item.id),
            Some("r203")
        );
        assert_eq!(
            controller
                .hint_index('1')
                .and_then(|i| model.get(i))
                .map(|item| &*item.id),
            Some("r200")
        );
    }

    #[test]
    fn hover_selects() {
        let model = model(100, 30, 0);
        let mut controller = ListController::new();
        let id: Arc<str> = "r7".into();

        assert!(controller.hover(&id));
        assert!(
            !controller.hover(&id),
            "hovering the same card again is a no-op"
        );
        assert!(controller.is_active(7, "r7"));
        assert!(!controller.is_active(0, "r0"));
        assert_eq!(controller.active_index(&model), 7);
    }

    #[test]
    fn arrows_move_and_clamp() {
        let mut model = model(30, 30, 0);
        let mut controller = ListController::new();
        controller.set_first_visible(0);

        assert_eq!(
            controller.navigate(Nav::Up, &mut model).0,
            NavOutcome::Moved { index: 0 }
        );
        assert_eq!(
            controller.navigate(Nav::Down, &mut model).0,
            NavOutcome::Moved { index: 1 }
        );
        for _ in 0..40 {
            controller.navigate(Nav::Down, &mut model);
        }
        assert_eq!(controller.selected().map(|id| &**id), Some("r29"));
        assert_eq!(
            controller.navigate(Nav::Down, &mut model).0,
            NavOutcome::Moved { index: 29 }
        );
    }

    #[test]
    fn arrow_into_an_unloaded_row_requests_it_and_does_not_move() {
        let mut model = model(100, 30, 0);
        let mut controller = ListController::new();
        let last: Arc<str> = "r29".into();
        controller.hover(&last);

        let (outcome, request) = controller.navigate(Nav::Down, &mut model);
        assert_eq!(outcome, NavOutcome::NeedsLoad { index: 30 });
        assert_eq!(
            request.map(|request: FetchRequest| request.range),
            Some(0..90),
            "30 ± preload, aligned"
        );
        assert_eq!(controller.selected().map(|id| &**id), Some("r29"));
    }

    #[test]
    fn arrows_cross_the_pinned_boundary_and_reach_the_top() {
        let mut model = model(50, 30, 2);
        let mut controller = ListController::new();
        controller.set_first_visible(2);

        assert_eq!(
            controller.navigate(Nav::Up, &mut model).0,
            NavOutcome::Moved { index: 1 }
        );
        assert_eq!(
            controller.navigate(Nav::Up, &mut model).0,
            NavOutcome::Moved { index: 0 }
        );
        assert_eq!(
            controller.navigate(Nav::Down, &mut model).0,
            NavOutcome::Moved { index: 1 }
        );
        assert_eq!(
            controller.navigate(Nav::Down, &mut model).0,
            NavOutcome::Moved { index: 2 }
        );
    }

    #[test]
    fn delete_selects_next_then_previous() {
        let model = model(30, 30, 0);
        let mut controller = ListController::new();
        controller.hover(&"r5".into());
        controller.select_after_delete(&model, "r5");
        assert_eq!(controller.selected().map(|id| &**id), Some("r6"));

        controller.hover(&"r29".into());
        controller.select_after_delete(&model, "r29");
        assert_eq!(controller.selected().map(|id| &**id), Some("r28"));

        // 删的不是当前项：保持。
        controller.hover(&"r3".into());
        controller.select_after_delete(&model, "r10");
        assert_eq!(controller.selected().map(|id| &**id), Some("r3"));
    }

    #[test]
    fn number_hints_cover_ten_visible_rows_including_pins() {
        let mut controller = ListController::new();
        assert_eq!(controller.hint_key(0, false), Some('1'));
        assert_eq!(controller.hint_key(1, false), Some('2'));
        assert_eq!(controller.hint_index('1'), Some(0));
        controller.set_first_visible(5);

        assert_eq!(controller.hint_key(5, false), Some('1'));
        assert_eq!(controller.hint_key(13, false), Some('9'));
        assert_eq!(controller.hint_key(14, false), Some('0'));
        assert_eq!(controller.hint_key(15, false), None);
        assert_eq!(controller.hint_key(4, false), None);
        assert_eq!(controller.hint_key(6, true), None, "hidden while selecting");
    }

    #[test]
    fn number_keys_map_to_visible_rows() {
        let mut controller = ListController::new();
        controller.set_first_visible(7);

        assert_eq!(controller.hint_index('1'), Some(7));
        assert_eq!(controller.hint_index('9'), Some(15));
        assert_eq!(controller.hint_index('0'), Some(16));
        assert_eq!(controller.hint_index('x'), None);
    }

    #[test]
    fn updates_defer_until_the_top() {
        let mut controller = ListController::new();
        let upsert = ListUpdate::Upserted {
            id: "r1".into(),
            kind: ItemKind::Text,
            deduplicated: false,
            group_id: None,
        };

        assert_eq!(
            controller.on_update(&upsert, true, true),
            UpdateAction::ReloadNow {
                reset_selection: false
            }
        );
        assert!(!controller.has_pending_reload());

        assert_eq!(
            controller.on_update(&upsert, true, false),
            UpdateAction::Defer {
                reset_selection: false
            }
        );
        assert!(!controller.take_reload_at_top(false));
        assert!(controller.take_reload_at_top(true));
        assert!(!controller.take_reload_at_top(true), "consumed once");
    }

    #[test]
    fn reuse_refreshes_in_place_below_the_top() {
        let mut controller = ListController::new();
        let reuse = ListUpdate::Upserted {
            id: "r1".into(),
            kind: ItemKind::Text,
            deduplicated: true,
            group_id: None,
        };

        assert_eq!(
            controller.on_update(&reuse, true, false),
            UpdateAction::RefreshInPlace
        );
        assert!(!controller.has_pending_reload());
        assert_eq!(
            controller.on_update(&reuse, true, true),
            UpdateAction::ReloadNow {
                reset_selection: false
            }
        );
    }

    #[test]
    fn hidden_panel_reloads_right_away() {
        let mut controller = ListController::new();
        controller.hover(&"r1".into());
        let action = controller.on_update(
            &ListUpdate::Upserted {
                id: "r1".into(),
                kind: ItemKind::Text,
                deduplicated: false,
                group_id: None,
            },
            false,
            false,
        );

        assert_eq!(
            action,
            UpdateAction::ReloadNow {
                reset_selection: false
            },
            "not at the top does not matter while hidden"
        );
        assert!(!controller.has_pending_reload());

        let cleaned = controller.on_update(&ListUpdate::Cleaned { removed: 3 }, false, true);
        assert_eq!(
            cleaned,
            UpdateAction::ReloadNow {
                reset_selection: true
            }
        );
        assert!(controller.selected().is_none());
    }

    #[test]
    fn cleanup_and_import_reset_the_selection() {
        let mut controller = ListController::new();
        controller.hover(&"r1".into());

        assert_eq!(
            controller.on_update(&ListUpdate::Cleaned { removed: 3 }, true, false),
            UpdateAction::Defer {
                reset_selection: true
            }
        );
        assert!(controller.selected().is_none());

        controller.hover(&"r2".into());
        assert_eq!(
            controller.on_update(&ListUpdate::Reloaded, true, true),
            UpdateAction::ReloadNow {
                reset_selection: true
            }
        );
        assert!(controller.selected().is_none());
    }

    /// 识别进展刷新图片能力和搜索结果，而且不清掉当前选中。
    #[test]
    fn image_text_progress_refreshes_image_capabilities_without_a_search() {
        let mut controller = ListController::new();
        controller.hover(&"r1".into());
        assert_eq!(
            controller.on_update(&ListUpdate::ImageTextChanged, true, true),
            UpdateAction::ReloadNow {
                reset_selection: false
            }
        );

        for filter in [
            ListFilter {
                range: super::super::filter::Range::Favorite,
                ..ListFilter::default()
            },
            ListFilter {
                group_id: Some("group".into()),
                ..ListFilter::default()
            },
        ] {
            controller.set_filter(filter);
            assert_eq!(
                controller.on_update(&ListUpdate::ImageTextChanged, true, false),
                UpdateAction::RefreshInPlace
            );
        }

        controller.set_filter(ListFilter {
            keyword: "发票".into(),
            ..ListFilter::default()
        });
        controller.hover(&"r1".into());
        assert_eq!(
            controller.on_update(&ListUpdate::ImageTextChanged, true, true),
            UpdateAction::ReloadNow {
                reset_selection: false
            }
        );
        assert_eq!(controller.selected().map(|id| &**id), Some("r1"));

        controller.set_filter(ListFilter {
            keyword: "发票".into(),
            category: Some(ItemKind::Text),
            ..ListFilter::default()
        });
        assert_eq!(
            controller.on_update(&ListUpdate::ImageTextChanged, true, true),
            UpdateAction::Ignore
        );
    }

    #[test]
    fn group_reuse_refreshes_order_but_insertion_does_not() {
        let mut controller = ListController::new();
        controller.set_filter(ListFilter {
            group_id: Some("group".into()),
            ..ListFilter::default()
        });
        assert_eq!(
            controller.on_update(
                &ListUpdate::Upserted {
                    id: "r1".into(),
                    kind: ItemKind::Text,
                    deduplicated: false,
                    group_id: None,
                },
                true,
                false
            ),
            UpdateAction::Ignore
        );
        assert_eq!(
            controller.on_update(
                &ListUpdate::Upserted {
                    id: "r1".into(),
                    kind: ItemKind::Text,
                    deduplicated: true,
                    group_id: None,
                },
                true,
                false
            ),
            UpdateAction::RefreshInPlace
        );
    }

    #[test]
    fn irrelevant_updates_are_ignored() {
        let mut controller = ListController::new();
        controller.set_filter(ListFilter {
            range: crate::clipboard::model::filter::Range::Favorite,
            ..ListFilter::default()
        });

        let action = controller.on_update(
            &ListUpdate::Upserted {
                id: "r1".into(),
                kind: ItemKind::Text,
                deduplicated: false,
                group_id: None,
            },
            true,
            true,
        );
        assert_eq!(action, UpdateAction::Ignore);
        assert!(!controller.has_pending_reload());
    }

    #[test]
    fn showing_clears_the_selection() {
        let mut controller = ListController::new();
        controller.hover(&"r4".into());
        controller.on_shown();

        assert!(controller.selected().is_none());
    }
}
