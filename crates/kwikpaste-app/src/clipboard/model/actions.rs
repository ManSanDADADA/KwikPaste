//! 卡片悬停快捷动作与删除保护，移植自 1.x `constants/itemActions.ts` 与 `List.tsx` 的 `canDeleteItem`。
//!
//! 设置里启用的快捷动作（`clipboard.content.itemActions`，顺序即显示顺序）再按记录的
//! `availableActions`（core 算好的右键菜单动作）过滤：core 没给的动作在这条记录上没有意义。

use kwikpaste_core::settings::Content;
pub use kwikpaste_core::settings::ItemAction as QuickAction;

use super::item::{ItemAction, ItemKind, ListItem};

/// 这个快捷动作在这条记录上可用吗（1.x `isItemActionAvailable`）。
pub fn is_available(action: QuickAction, item: &ListItem) -> bool {
    let has = |wanted: ItemAction| item.available_actions.contains(&wanted);

    match action {
        QuickAction::CopyPlain => item.kind != ItemKind::Image && has(ItemAction::Copy),
        QuickAction::OpenLink => has(ItemAction::OpenLink),
        QuickAction::Paste => has(ItemAction::Paste),
        QuickAction::PastePath => has(ItemAction::PasteAsPath),
        QuickAction::PastePlain => has(ItemAction::PasteAsPlainText),
        QuickAction::Reveal => has(ItemAction::RevealInFinder) || has(ItemAction::RevealInExplorer),
        QuickAction::SendEmail => has(ItemAction::SendEmail),
        QuickAction::SplitWords => has(ItemAction::SplitWords),
        QuickAction::Copy => has(ItemAction::Copy),
        QuickAction::Delete => has(ItemAction::Delete),
        QuickAction::Note => has(ItemAction::EditNote),
        QuickAction::PinItem => has(ItemAction::TogglePinned),
        QuickAction::Star => has(ItemAction::ToggleFavorite),
    }
}

/// 卡片上实际显示的快捷动作：设置的顺序，去掉不可用的。受删除保护的记录仍留着删除那一格
/// （由视图置灰）：整排右对齐，少一个按钮会让其余按钮换位置，习惯点右上角的人会点错。
pub fn visible_actions(configured: &[QuickAction], item: &ListItem) -> Vec<QuickAction> {
    configured
        .iter()
        .copied()
        .filter(|action| is_available(*action, item))
        .collect()
}

/// 写回剪贴板的动作：点完显示 1 秒“已复制”。
pub fn is_copy(action: QuickAction) -> bool {
    matches!(action, QuickAction::Copy | QuickAction::CopyPlain)
}

/// “打开”的去处：链接、邮件、在文件管理器中显示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    Link,
    Email,
    Reveal,
}

/// Mod+O 作用的“打开”（1.x `getOpenClipboardAction`：按 core 声明的动作顺序取第一个）。
pub fn open_target(item: &ListItem) -> Option<OpenTarget> {
    item.available_actions
        .iter()
        .find_map(|action| match action {
            ItemAction::OpenLink => Some(OpenTarget::Link),
            ItemAction::SendEmail => Some(OpenTarget::Email),
            ItemAction::RevealInFinder | ItemAction::RevealInExplorer => Some(OpenTarget::Reveal),
            _ => None,
        })
}

/// 删除保护（1.x `canDeleteItem` 与 `deleteClipboardItem` 的确认开关）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeletePolicy {
    pub favorite_items: bool,
    pub favorite_confirm: bool,
    pub pinned_items: bool,
    pub pinned_confirm: bool,
    pub confirm: bool,
    pub favorites_only_in_favorite_group: bool,
}

impl DeletePolicy {
    pub fn from_settings(content: &Content) -> Self {
        Self {
            favorite_items: content.delete_favorite_items,
            favorite_confirm: content.delete_favorite_confirm,
            pinned_items: content.delete_pinned_items,
            pinned_confirm: content.delete_pinned_confirm,
            confirm: content.delete_confirm,
            favorites_only_in_favorite_group: content.delete_favorite_items_only_in_favorite_group,
        }
    }

    /// 能删吗：置顶、收藏各受自己的开关约束；收藏还可以限定只在收藏视图里删。
    pub fn can_delete(&self, is_favorite: bool, is_pinned: bool, in_favorites: bool) -> bool {
        if is_pinned && !self.pinned_items {
            return false;
        }
        if !is_favorite {
            return true;
        }
        if !self.favorite_items {
            return false;
        }

        !self.favorites_only_in_favorite_group || in_favorites
    }

    /// 删除单条前要不要二次确认。
    pub fn needs_confirm(&self, is_favorite: bool, is_pinned: bool) -> bool {
        (is_favorite && self.favorite_confirm)
            || (is_pinned && self.pinned_confirm)
            || (!is_favorite && !is_pinned && self.confirm)
    }
}

impl Default for DeletePolicy {
    fn default() -> Self {
        Self::from_settings(&Content::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: ItemKind, actions: &[ItemAction]) -> ListItem {
        let mut item: ListItem = serde_json::from_str(
            r#"{"id":"x","kind":"text","isFavorite":false,"isPinned":false,"isSensitive":false,
                "platform":"windows","createdAt":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("parses");
        item.kind = kind;
        item.available_actions = actions.to_vec();
        item
    }

    #[test]
    fn actions_follow_the_core_menu_and_the_settings_order() {
        let text = item(
            ItemKind::Text,
            &[
                ItemAction::Copy,
                ItemAction::SplitWords,
                ItemAction::ToggleFavorite,
                ItemAction::TogglePinned,
                ItemAction::Delete,
            ],
        );
        let defaults = Content::default().item_actions;

        assert_eq!(
            visible_actions(&defaults, &text),
            vec![
                QuickAction::Copy,
                QuickAction::SplitWords,
                QuickAction::Star,
                QuickAction::PinItem,
                QuickAction::Delete
            ]
        );
        let image = item(ItemKind::Image, &[ItemAction::Copy]);
        assert!(!is_available(QuickAction::CopyPlain, &image));
        assert!(is_available(QuickAction::Copy, &image));
        assert!(!is_available(QuickAction::SplitWords, &image));
    }

    #[test]
    fn open_takes_the_first_open_like_action() {
        let link = item(ItemKind::Text, &[ItemAction::Copy, ItemAction::OpenLink]);
        assert_eq!(open_target(&link), Some(OpenTarget::Link));

        let file = item(ItemKind::Files, &[ItemAction::RevealInExplorer]);
        assert_eq!(open_target(&file), Some(OpenTarget::Reveal));

        assert_eq!(open_target(&item(ItemKind::Text, &[])), None);
    }

    #[test]
    fn delete_protection_matches_1x_defaults() {
        let policy = DeletePolicy::default();

        assert!(policy.can_delete(false, false, false));
        assert!(
            !policy.can_delete(true, false, true),
            "favorites are protected"
        );
        assert!(
            !policy.can_delete(false, true, false),
            "pinned are protected"
        );
        assert!(policy.needs_confirm(false, false));

        let open = DeletePolicy {
            favorite_items: true,
            pinned_items: true,
            ..policy
        };
        assert!(
            !open.can_delete(true, false, false),
            "only in the favorites view"
        );
        assert!(open.can_delete(true, false, true));
        assert!(open.can_delete(false, true, false));

        let anywhere = DeletePolicy {
            favorites_only_in_favorite_group: false,
            ..open
        };
        assert!(anywhere.can_delete(true, false, false));

        let quiet = DeletePolicy {
            confirm: false,
            ..open
        };
        assert!(!quiet.needs_confirm(false, false));
        assert!(
            quiet.needs_confirm(true, false),
            "favorites keep their own switch"
        );
    }
}
