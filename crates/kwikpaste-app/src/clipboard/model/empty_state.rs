//! 空列表的提示文案（1.x `getEmptyDescription`）：按搜索词、范围、分类、自定义分组组合出 16 种；
//! 用了搜索框的筛选时只有一种。

use super::filter::{Category, ListFilter};

/// 空态文案的 key 与要插进去的参数。分类名、分组名由视图翻译后填入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmptyText {
    pub key: &'static str,
    /// `{{keyword}}`：搜索时才有。
    pub searching: bool,
    /// `{{category}}`：选了分类时才有。
    pub category: Option<Category>,
    /// `{{group}}`：选了自定义分组时才有。
    pub group: bool,
    /// 用了搜索框的筛选：只提示没有符合筛选的记录。
    pub filtered: bool,
}

/// 当前筛选条件下列表为空时的提示。
pub fn empty_text(filter: &ListFilter) -> EmptyText {
    let searching = filter.searching();
    let favorites = filter.favorites();
    let group = filter.group_id.is_some();
    let category = filter.category;
    if !filter.search.is_empty() {
        return EmptyText {
            key: "clipboard:empty.filtered",
            searching: false,
            category: None,
            group: false,
            filtered: true,
        };
    }

    let key = match (searching, group, favorites, category.is_some()) {
        (true, true, true, true) => "clipboard:empty.searchGroupFavoriteCategory",
        (true, true, true, false) => "clipboard:empty.searchGroupFavorites",
        (true, true, false, true) => "clipboard:empty.searchGroupCategory",
        (true, true, false, false) => "clipboard:empty.searchGroup",
        (true, false, true, true) => "clipboard:empty.searchFavoriteCategory",
        (true, false, true, false) => "clipboard:empty.searchFavorites",
        (true, false, false, true) => "clipboard:empty.searchCategory",
        (true, false, false, false) => "clipboard:empty.searchHistory",
        (false, true, true, true) => "clipboard:empty.groupFavoriteCategory",
        (false, true, true, false) => "clipboard:empty.groupFavorites",
        (false, true, false, true) => "clipboard:empty.groupCategory",
        (false, true, false, false) => "clipboard:empty.group",
        (false, false, true, true) => "clipboard:empty.favoriteCategory",
        (false, false, true, false) => "clipboard:empty.favorites",
        (false, false, false, true) => "clipboard:empty.category",
        (false, false, false, false) => "clipboard:empty.history",
    };

    EmptyText {
        key,
        searching,
        category,
        group,
        filtered: false,
    }
}

/// 分类名在空态文案里的写法（中文“文本”，英文小写 “text”）。
pub fn category_key(kind: Category) -> &'static str {
    match kind {
        Category::Text => "clipboard:empty.categories.text",
        Category::Image => "clipboard:empty.categories.image",
        Category::Files => "clipboard:empty.categories.files",
        Category::Link => "clipboard:empty.categories.link",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::clipboard::model::filter::Range;

    /// 16 种组合各对应一条不同的文案，参数与组合一致。
    #[test]
    fn every_combination_has_its_own_text() {
        let mut keys = HashSet::new();
        for searching in [false, true] {
            for group in [false, true] {
                for range in [Range::All, Range::Favorite] {
                    for category in [None, Some(Category::Image)] {
                        let filter = ListFilter {
                            range,
                            category,
                            group_id: group.then(|| "g".into()),
                            keyword: if searching { "k".into() } else { "".into() },
                            ..ListFilter::default()
                        };
                        let text = empty_text(&filter);

                        assert_eq!(text.searching, searching);
                        assert_eq!(text.group, group);
                        assert_eq!(text.category, category);
                        assert_eq!(text.key.contains("search"), searching, "{}", text.key);
                        assert_eq!(text.key.contains("roup"), group, "{}", text.key);
                        assert_eq!(
                            text.key.contains("avorite"),
                            range == Range::Favorite,
                            "{}",
                            text.key
                        );
                        assert_eq!(
                            text.key.contains("ategory"),
                            category.is_some(),
                            "{}",
                            text.key
                        );
                        keys.insert(text.key);
                    }
                }
            }
        }

        assert_eq!(keys.len(), 16);
    }

    #[test]
    fn plain_history() {
        assert_eq!(
            empty_text(&ListFilter::default()).key,
            "clipboard:empty.history"
        );
    }
}
