//! 列表的筛选条件，移植自 1.x `clipboardViewState`（范围、分类、自定义分组、搜索词）
//! 与 `Group.tsx` 的切换规则。

use std::sync::Arc;

use chrono::{DateTime, Days, Local, NaiveTime, Utc};
use kwikpaste_core::settings::GroupBarEntry;

use super::item::{ItemKind, ListItem, SubKind};

/// 范围：全部或收藏，始终有一个选中（1.x `ClipboardRange`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    #[default]
    All,
    Favorite,
}

impl Range {
    /// Mod+Q：在全部与收藏之间切换。
    pub fn toggled(self) -> Self {
        match self {
            Self::All => Self::Favorite,
            Self::Favorite => Self::All,
        }
    }
}

/// 分组栏按钮对应的分类；收藏不是分类。
pub fn entry_category(entry: GroupBarEntry) -> Option<ItemKind> {
    match entry {
        GroupBarEntry::Favorite => None,
        GroupBarEntry::Text => Some(ItemKind::Text),
        GroupBarEntry::Image => Some(ItemKind::Image),
        GroupBarEntry::Files => Some(ItemKind::Files),
    }
}

/// 分组栏上显示的分类，按设置的顺序。
pub fn bar_categories(entries: &[GroupBarEntry]) -> Vec<ItemKind> {
    entries.iter().copied().filter_map(entry_category).collect()
}

/// 搜索框筛选里的时间范围：从本地某天零点起创建的记录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Since {
    Today,
    Week,
    Month,
}

impl Since {
    pub const ALL: [Self; 3] = [Self::Today, Self::Week, Self::Month];

    /// 起点：今天、近 7 天、近 30 天都含今天。
    pub fn start(self, now: DateTime<Local>) -> DateTime<Utc> {
        let days_back = match self {
            Self::Today => 0,
            Self::Week => 6,
            Self::Month => 29,
        };
        let midnight = now
            .date_naive()
            .checked_sub_days(Days::new(days_back))
            .map(|day| day.and_time(NaiveTime::MIN))
            .and_then(|start| start.and_local_timezone(Local).earliest());

        midnight.unwrap_or(now).with_timezone(&Utc)
    }
}

/// 搜索框筛选里的类型：文本的子类型，富文本包括 RTF 和 HTML。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKind {
    Link,
    Email,
    Color,
    Path,
    Rich,
}

impl TextKind {
    pub const ALL: [Self; 5] = [Self::Link, Self::Email, Self::Color, Self::Path, Self::Rich];

    pub fn sub_kinds(self) -> &'static [SubKind] {
        match self {
            Self::Link => &[SubKind::Url],
            Self::Email => &[SubKind::Email],
            Self::Color => &[SubKind::Color],
            Self::Path => &[SubKind::Path],
            Self::Rich => &[SubKind::Rtf, SubKind::Html],
        }
    }
}

/// 历史记录里出现过的来源应用（core `HistoryApp`）：同一应用的各安装版本合成一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceApp {
    pub ids: Arc<[Arc<str>]>,
    pub name: Arc<str>,
    pub icon_path: Option<Arc<str>>,
}

/// 搜索框的筛选条件：同一类里多选取“或”，不同类之间、与分组栏和搜索词之间取“且”。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub since: Option<Since>,
    pub apps: Vec<SourceApp>,
    pub kinds: Vec<TextKind>,
}

impl SearchFilters {
    pub fn is_empty(&self) -> bool {
        self.since.is_none() && self.apps.is_empty() && self.kinds.is_empty()
    }

    /// 选中或取消一个应用（按 id 集合比较）。
    pub fn toggle_app(&mut self, app: SourceApp) {
        if let Some(index) = self.apps.iter().position(|it| it.ids == app.ids) {
            self.apps.remove(index);
        } else {
            self.apps.push(app);
        }
    }

    pub fn toggle_kind(&mut self, kind: TextKind) {
        if let Some(index) = self.kinds.iter().position(|it| *it == kind) {
            self.kinds.remove(index);
        } else {
            self.kinds.push(kind);
        }
    }

    fn matches(&self, item: &ListItem, now: DateTime<Local>) -> bool {
        if self
            .since
            .is_some_and(|since| item.created_at < since.start(now))
        {
            return false;
        }
        if !self.apps.is_empty()
            && !item.source_app_id.as_ref().is_some_and(|id| {
                self.apps
                    .iter()
                    .any(|app| app.ids.iter().any(|it| it == id))
            })
        {
            return false;
        }

        self.kinds.is_empty()
            || item.sub_kind.is_some_and(|sub| {
                self.kinds
                    .iter()
                    .any(|kind| kind.sub_kinds().contains(&sub))
            })
    }
}

/// 当前视图的筛选条件。改动任何一项列表都整体重载。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListFilter {
    pub range: Range,
    /// 分类；再次点击当前分类时取消。
    pub category: Option<ItemKind>,
    /// 自定义分组；再次点击当前分组时取消。
    pub group_id: Option<Arc<str>>,
    /// 搜索词（已去掉首尾空白）。
    pub keyword: Arc<str>,
    /// 搜索框的时间、来源应用、类型筛选。
    pub search: SearchFilters,
}

impl ListFilter {
    pub fn favorites(&self) -> bool {
        self.range == Range::Favorite
    }

    pub fn searching(&self) -> bool {
        !self.keyword.is_empty()
    }

    /// 偏好「全部视图隐藏收藏」只作用于「全部」本身：搜索和自定义分组照常包含收藏。
    pub fn hides_favorites(&self, setting: bool) -> bool {
        setting && !self.favorites() && self.group_id.is_none() && !self.searching()
    }

    /// 一条新记录可能出现在当前视图里吗（1.x `shouldRefreshCurrentGroup`）。`group_id` 是它自动归入的分组。
    /// 与 1.x 一样不看搜索词：搜索时有新记录也回到顶部刷新。
    pub fn may_include(&self, kind: Option<ItemKind>, group_id: Option<&str>) -> bool {
        if self.favorites()
            || self
                .group_id
                .as_deref()
                .is_some_and(|group| group_id != Some(group))
        {
            return false;
        }

        self.may_include_existing(kind)
    }

    /// 已有记录变化可能影响当前分类；收藏和分组成员资格由重查确认。
    pub fn may_include_existing(&self, kind: Option<ItemKind>) -> bool {
        match (self.category, kind) {
            (None, _) => true,
            (Some(_), None) => false,
            // 单个图片文件的记录属于「图片」分组，这里只知道类型，宁可多刷新一次。
            (Some(ItemKind::Image), Some(ItemKind::Files)) => true,
            (Some(category), Some(kind)) => category == kind,
        }
    }

    /// 夹具数据源的过滤：与 core 的查询语义一致（分类、收藏、分组、搜索框筛选、搜索词不区分大小写
    /// 地包含在摘要、备注或文件名里）。core 的搜索走 FTS，这里只求自测可用。
    pub fn matches(&self, item: &ListItem) -> bool {
        if self.category.is_some_and(|category| category != item.kind) {
            return false;
        }
        if !self.search.matches(item, Local::now()) {
            return false;
        }
        if self.favorites() && !item.is_favorite {
            return false;
        }
        if let Some(group) = &self.group_id
            && item.group_id.as_deref() != Some(&**group)
        {
            return false;
        }
        if !self.searching() {
            return true;
        }

        let keyword = self.keyword.to_lowercase();
        let contains = |text: &str| text.to_lowercase().contains(&keyword);
        item.summary.as_deref().is_some_and(contains)
            || item.note.as_deref().is_some_and(contains)
            || item.file_rows().iter().any(|row| contains(&row.name))
    }

    /// ←/→：在分组栏显示的分类里循环；还没选分类时从方向对应的一端进入（1.x
    /// `selectAdjacentCategory`）。分类都隐藏了时没有可选的。
    pub fn adjacent_category(&self, forward: bool, categories: &[ItemKind]) -> Option<ItemKind> {
        let count = categories.len();
        if count == 0 {
            return None;
        }

        let next = match self
            .category
            .and_then(|category| categories.iter().position(|kind| *kind == category))
        {
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
            None if forward => 0,
            None => count - 1,
        };

        categories.get(next).copied()
    }

    /// 分组栏上隐藏的按钮不能停在选中状态：分类回到不限，收藏回到全部。
    pub fn keep_to_bar(&mut self, entries: &[GroupBarEntry]) {
        if self.category.is_some_and(|kind| {
            !entries
                .iter()
                .any(|entry| entry_category(*entry) == Some(kind))
        }) {
            self.category = None;
        }
        if self.favorites() && !entries.contains(&GroupBarEntry::Favorite) {
            self.range = Range::All;
        }
    }
}

/// Tab / Shift+Tab：在可见的自定义分组之间循环；没有选中分组时正向取第一个、反向取最后一个
/// （1.x `selectAdjacentCustomGroup`）。
pub fn adjacent_group(
    groups: &[Arc<str>],
    current: Option<&str>,
    reverse: bool,
) -> Option<Arc<str>> {
    let count = groups.len();
    if count == 0 {
        return None;
    }

    let current = current.and_then(|id| groups.iter().position(|group| &**group == id));
    let next = match (current, reverse) {
        (None, false) => 0,
        (None, true) => count - 1,
        (Some(index), false) => (index + 1) % count,
        (Some(index), true) => (index + count - 1) % count,
    };

    groups.get(next).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_cycle_from_the_matching_end() {
        let all = bar_categories(&GroupBarEntry::ALL);
        let none = ListFilter::default();
        assert_eq!(none.adjacent_category(true, &all), Some(ItemKind::Text));
        assert_eq!(none.adjacent_category(false, &all), Some(ItemKind::Files));

        let image = ListFilter {
            category: Some(ItemKind::Image),
            ..ListFilter::default()
        };
        assert_eq!(image.adjacent_category(true, &all), Some(ItemKind::Files));
        assert_eq!(image.adjacent_category(false, &all), Some(ItemKind::Text));

        let files = ListFilter {
            category: Some(ItemKind::Files),
            ..ListFilter::default()
        };
        assert_eq!(
            files.adjacent_category(true, &all),
            Some(ItemKind::Text),
            "wraps"
        );

        let reordered = bar_categories(&[GroupBarEntry::Files, GroupBarEntry::Text]);
        assert_eq!(
            files.adjacent_category(true, &reordered),
            Some(ItemKind::Text)
        );
        assert_eq!(none.adjacent_category(true, &[]), None);
    }

    #[test]
    fn hidden_bar_entries_are_not_left_selected() {
        let mut filter = ListFilter {
            range: Range::Favorite,
            category: Some(ItemKind::Image),
            ..ListFilter::default()
        };
        filter.keep_to_bar(&GroupBarEntry::ALL);
        assert_eq!(
            (filter.range, filter.category),
            (Range::Favorite, Some(ItemKind::Image))
        );

        filter.keep_to_bar(&[GroupBarEntry::Text]);
        assert_eq!((filter.range, filter.category), (Range::All, None));
    }

    #[test]
    fn custom_groups_cycle_in_both_directions() {
        let groups: Vec<Arc<str>> = vec!["a".into(), "b".into(), "c".into()];

        assert_eq!(adjacent_group(&groups, None, false).as_deref(), Some("a"));
        assert_eq!(adjacent_group(&groups, None, true).as_deref(), Some("c"));
        assert_eq!(
            adjacent_group(&groups, Some("c"), false).as_deref(),
            Some("a")
        );
        assert_eq!(
            adjacent_group(&groups, Some("a"), true).as_deref(),
            Some("c")
        );
        assert_eq!(
            adjacent_group(&groups, Some("hidden"), false).as_deref(),
            Some("a"),
            "a group that is no longer visible counts as none"
        );
        assert_eq!(adjacent_group(&[], None, false), None);
    }

    #[test]
    fn new_records_refresh_only_views_that_may_show_them() {
        let all = ListFilter::default();
        assert!(all.may_include(Some(ItemKind::Image), None));
        assert!(all.may_include(Some(ItemKind::Text), Some("g")));

        let text = ListFilter {
            category: Some(ItemKind::Text),
            ..ListFilter::default()
        };
        assert!(text.may_include(Some(ItemKind::Text), None));
        assert!(!text.may_include(Some(ItemKind::Files), None));
        assert!(!text.may_include(None, None));

        let image = ListFilter {
            category: Some(ItemKind::Image),
            ..ListFilter::default()
        };
        assert!(
            image.may_include(Some(ItemKind::Files), None),
            "image files"
        );
        assert!(!image.may_include(Some(ItemKind::Text), None));

        let favorites = ListFilter {
            range: Range::Favorite,
            ..ListFilter::default()
        };
        assert!(!favorites.may_include(Some(ItemKind::Text), None));

        let group = ListFilter {
            group_id: Some("g".into()),
            ..ListFilter::default()
        };
        assert!(!group.may_include(Some(ItemKind::Text), None));
        assert!(!group.may_include(Some(ItemKind::Text), Some("other")));
        assert!(
            group.may_include(Some(ItemKind::Text), Some("g")),
            "auto-grouped"
        );

        let searching = ListFilter {
            keyword: "abc".into(),
            ..ListFilter::default()
        };
        assert!(searching.may_include(Some(ItemKind::Text), None));
    }

    #[test]
    fn since_starts_at_local_midnight_including_today() {
        let now = Local::now();
        let today = now.date_naive();

        let start = |since: Since| Since::start(since, now).with_timezone(&Local);
        assert_eq!(start(Since::Today).date_naive(), today);
        assert_eq!(start(Since::Today).time(), NaiveTime::MIN);
        assert_eq!((today - start(Since::Week).date_naive()).num_days(), 6);
        assert_eq!((today - start(Since::Month).date_naive()).num_days(), 29);
    }

    #[test]
    fn hiding_favorites_covers_only_the_plain_all_view() {
        let all = ListFilter::default();
        assert!(all.hides_favorites(true));
        assert!(!all.hides_favorites(false));

        let text = ListFilter {
            category: Some(ItemKind::Text),
            ..ListFilter::default()
        };
        assert!(text.hides_favorites(true), "categories inside All");

        for filter in [
            ListFilter {
                range: Range::Favorite,
                ..ListFilter::default()
            },
            ListFilter {
                group_id: Some("g".into()),
                ..ListFilter::default()
            },
            ListFilter {
                keyword: "abc".into(),
                ..ListFilter::default()
            },
        ] {
            assert!(!filter.hides_favorites(true), "{filter:?}");
        }
    }

    #[test]
    fn range_toggles() {
        assert_eq!(Range::All.toggled(), Range::Favorite);
        assert_eq!(Range::Favorite.toggled(), Range::All);
    }
}
