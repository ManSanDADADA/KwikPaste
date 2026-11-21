//! 卡片右键菜单的条目，移植自 1.x `menu/clipboard_item.rs`：动作按四组排列、组间分隔，
//! 只列出 core 给这条记录算好的动作（`availableActions`）；受删除保护时去掉删除，有未隐藏的
//! 自定义分组时才有“移动到分组”。切换类动作的文案按当前状态翻转。

use super::item::{ItemAction, ListItem};

/// 右键菜单里的一个动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Paste,
    PasteImageAsFile,
    PasteAsPlainText,
    PasteAsPath,
    Copy,
    ViewImage,
    PinToScreen,
    SaveImage,
    CopyImageText,
    SplitWords,
    Translate,
    CopyTranslation,
    PasteTranslation,
    OpenLink,
    SendEmail,
    RevealInFinder,
    RevealInExplorer,
    ToggleFavorite,
    TogglePinned,
    MoveToGroup,
    EditNote,
    EditContent,
    RestoreOriginal,
    Select,
    Delete,
}

/// 视觉分组：组内顺序与组顺序即菜单顺序（1.x `ACTION_GROUPS`）。
const GROUPS: [&[MenuAction]; 4] = [
    &[
        MenuAction::Paste,
        MenuAction::PasteImageAsFile,
        MenuAction::PasteAsPlainText,
        MenuAction::PasteAsPath,
        MenuAction::Copy,
        MenuAction::ViewImage,
        MenuAction::PinToScreen,
        MenuAction::SaveImage,
        MenuAction::CopyImageText,
        MenuAction::SplitWords,
        MenuAction::Translate,
        MenuAction::CopyTranslation,
        MenuAction::PasteTranslation,
    ],
    &[
        MenuAction::OpenLink,
        MenuAction::SendEmail,
        MenuAction::RevealInFinder,
        MenuAction::RevealInExplorer,
    ],
    &[
        MenuAction::ToggleFavorite,
        MenuAction::TogglePinned,
        MenuAction::MoveToGroup,
        MenuAction::EditNote,
        MenuAction::EditContent,
        MenuAction::RestoreOriginal,
    ],
    &[MenuAction::Select, MenuAction::Delete],
];

impl MenuAction {
    /// core 声明的动作；“移动到分组”不由 core 声明。
    fn item_action(self) -> Option<ItemAction> {
        Some(match self {
            Self::Paste => ItemAction::Paste,
            Self::PasteAsPlainText => ItemAction::PasteAsPlainText,
            Self::PasteAsPath => ItemAction::PasteAsPath,
            Self::Copy => ItemAction::Copy,
            Self::SaveImage => ItemAction::SaveImage,
            Self::CopyImageText => ItemAction::CopyImageText,
            Self::SplitWords => ItemAction::SplitWords,
            Self::Translate => ItemAction::Translate,
            Self::CopyTranslation => ItemAction::CopyTranslation,
            Self::PasteTranslation => ItemAction::PasteTranslation,
            Self::OpenLink => ItemAction::OpenLink,
            Self::SendEmail => ItemAction::SendEmail,
            Self::RevealInFinder => ItemAction::RevealInFinder,
            Self::RevealInExplorer => ItemAction::RevealInExplorer,
            Self::ToggleFavorite => ItemAction::ToggleFavorite,
            Self::TogglePinned => ItemAction::TogglePinned,
            Self::EditNote => ItemAction::EditNote,
            Self::RestoreOriginal => ItemAction::RestoreOriginal,
            Self::Select => ItemAction::Select,
            Self::Delete => ItemAction::Delete,
            Self::PasteImageAsFile
            | Self::MoveToGroup
            | Self::EditContent
            | Self::ViewImage
            | Self::PinToScreen => {
                return None;
            }
        })
    }

    /// 右键菜单读取有效的面板主键位；解绑后不再显示提示。
    pub fn accelerator(self, shortcuts: &kwikpaste_core::settings::Shortcuts) -> Option<String> {
        use kwikpaste_core::settings::{PanelAction, effective_panel_keys};
        let action = match self {
            Self::Paste => PanelAction::PasteSelected,
            Self::PasteAsPlainText | Self::PasteAsPath => PanelAction::PasteSelectedPlain,
            Self::Copy => PanelAction::CopySelected,
            Self::SplitWords => PanelAction::SplitSelected,
            Self::Translate => PanelAction::TranslateSelected,
            Self::OpenLink | Self::SendEmail | Self::RevealInFinder | Self::RevealInExplorer => {
                PanelAction::OpenSelected
            }
            Self::ToggleFavorite => PanelAction::FavoriteSelected,
            Self::TogglePinned => PanelAction::PinSelected,
            Self::EditNote => PanelAction::NoteSelected,
            Self::EditContent => PanelAction::EditContent,
            Self::ViewImage => PanelAction::ViewImage,
            Self::Delete => PanelAction::DeleteSelected,
            Self::PasteImageAsFile
            | Self::PinToScreen
            | Self::SaveImage
            | Self::CopyImageText
            | Self::CopyTranslation
            | Self::PasteTranslation
            | Self::MoveToGroup
            | Self::RestoreOriginal
            | Self::Select => {
                return None;
            }
        };
        effective_panel_keys(shortcuts, action).into_iter().next()
    }

    /// 文案的 i18n key；收藏、置顶、备注按记录当前状态翻转。
    pub fn label_key(self, item: &ListItem) -> &'static str {
        match self {
            Self::Paste => "clipboard:menu.paste",
            Self::PasteImageAsFile => "clipboard:menu.pasteAsFile",
            Self::PasteAsPlainText => "clipboard:menu.pasteAsPlainText",
            Self::PasteAsPath => "clipboard:menu.pasteAsPath",
            Self::Copy => "clipboard:menu.copy",
            Self::ViewImage => "clipboard:menu.viewImage",
            Self::PinToScreen => "clipboard:menu.pinToScreen",
            Self::SaveImage => "clipboard:menu.saveImage",
            Self::CopyImageText => "clipboard:menu.copyImageText",
            Self::SplitWords => "clipboard:menu.splitWords",
            Self::Translate => "clipboard:menu.translate",
            Self::CopyTranslation => "clipboard:menu.copyTranslation",
            Self::PasteTranslation => "clipboard:menu.pasteTranslation",
            Self::OpenLink => "clipboard:menu.openLink",
            Self::SendEmail => "clipboard:menu.sendEmail",
            Self::RevealInFinder => "clipboard:menu.revealInFinder",
            Self::RevealInExplorer => "clipboard:menu.revealInExplorer",
            Self::ToggleFavorite if item.is_favorite => "clipboard:menu.unfavorite",
            Self::ToggleFavorite => "clipboard:menu.favorite",
            Self::TogglePinned if item.is_pinned => "clipboard:menu.unpinItem",
            Self::TogglePinned => "clipboard:menu.pinItem",
            Self::MoveToGroup => "clipboard:menu.moveToGroup",
            Self::EditNote if item.note.is_some() => "clipboard:menu.editNote",
            Self::EditNote => "clipboard:menu.addNote",
            Self::EditContent => "clipboard:menu.editContent",
            Self::RestoreOriginal => "clipboard:menu.restoreOriginal",
            Self::Select => "clipboard:menu.select",
            Self::Delete => "clipboard:menu.delete",
        }
    }
}

/// 这条记录的右键菜单，按组给出（空组不出现）。没有任何动作时为空，这时不弹菜单（1.x 同）。
pub fn menu_groups(item: &ListItem, can_delete: bool, has_groups: bool) -> Vec<Vec<MenuAction>> {
    let available = |action: MenuAction| match action {
        MenuAction::EditContent => item.kind == super::item::ItemKind::Text,
        MenuAction::PasteImageAsFile => {
            item.kind == super::item::ItemKind::Image
                && item.available_actions.contains(&ItemAction::Paste)
        }
        // 原图还在才能另存，也才能看大图、贴到屏幕。
        MenuAction::ViewImage | MenuAction::PinToScreen => {
            item.kind == super::item::ItemKind::Image
                && item.available_actions.contains(&ItemAction::SaveImage)
        }
        _ => match action.item_action() {
            Some(wanted) => item.available_actions.contains(&wanted),
            None => has_groups,
        },
    };
    let any_core_action = GROUPS.iter().flat_map(|group| group.iter()).any(|action| {
        action.item_action().is_some()
            && (can_delete || *action != MenuAction::Delete)
            && available(*action)
    });
    if !any_core_action {
        return Vec::new();
    }

    GROUPS
        .iter()
        .map(|group| {
            group
                .iter()
                .copied()
                .filter(|action| available(*action))
                .collect::<Vec<_>>()
        })
        .filter(|group| !group.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_actions(actions: &[ItemAction]) -> ListItem {
        let mut item: ListItem = serde_json::from_str(
            r#"{"id":"x","kind":"text","isFavorite":false,"isPinned":false,"isSensitive":false,
                "platform":"windows","createdAt":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("parses");
        item.available_actions = actions.to_vec();
        item
    }

    #[test]
    fn actions_follow_the_core_list_in_1x_order() {
        let item = with_actions(&[
            ItemAction::Delete,
            ItemAction::Copy,
            ItemAction::Paste,
            ItemAction::ToggleFavorite,
            ItemAction::OpenLink,
            ItemAction::Select,
        ]);

        assert_eq!(
            menu_groups(&item, true, false),
            vec![
                vec![MenuAction::Paste, MenuAction::Copy],
                vec![MenuAction::OpenLink],
                vec![MenuAction::ToggleFavorite, MenuAction::EditContent],
                vec![MenuAction::Select, MenuAction::Delete],
            ]
        );
    }

    #[test]
    fn protected_records_keep_delete_and_groups_add_move() {
        let item = with_actions(&[ItemAction::Paste, ItemAction::Delete, ItemAction::EditNote]);

        assert_eq!(
            menu_groups(&item, false, true),
            vec![
                vec![MenuAction::Paste],
                vec![
                    MenuAction::MoveToGroup,
                    MenuAction::EditNote,
                    MenuAction::EditContent
                ],
                vec![MenuAction::Delete],
            ]
        );
    }

    #[test]
    fn content_editor_is_text_only_and_has_edit_shortcut() {
        let mut item = with_actions(&[ItemAction::EditNote]);
        assert_eq!(
            menu_groups(&item, true, false),
            vec![vec![MenuAction::EditNote, MenuAction::EditContent]]
        );
        assert_eq!(
            MenuAction::EditContent
                .accelerator(&kwikpaste_core::settings::Shortcuts::default())
                .as_deref(),
            Some("secondary-e")
        );
        for kind in [
            super::super::item::ItemKind::Image,
            super::super::item::ItemKind::Files,
        ] {
            item.kind = kind;
            assert_eq!(
                menu_groups(&item, true, false),
                vec![vec![MenuAction::EditNote]]
            );
        }
    }

    #[test]
    fn images_can_be_pasted_as_files() {
        let mut item = with_actions(&[ItemAction::Paste, ItemAction::Copy]);
        item.kind = super::super::item::ItemKind::Image;
        assert_eq!(
            menu_groups(&item, true, false),
            vec![vec![
                MenuAction::Paste,
                MenuAction::PasteImageAsFile,
                MenuAction::Copy
            ]]
        );
    }

    #[test]
    fn images_with_an_original_can_be_viewed_and_pinned_to_the_screen() {
        let mut item = with_actions(&[ItemAction::Copy, ItemAction::SaveImage]);
        item.kind = super::super::item::ItemKind::Image;
        assert_eq!(
            menu_groups(&item, true, false),
            vec![vec![
                MenuAction::Copy,
                MenuAction::ViewImage,
                MenuAction::PinToScreen,
                MenuAction::SaveImage
            ]]
        );
    }

    #[test]
    fn translation_actions_close_the_first_group() {
        let item = with_actions(&[
            ItemAction::Paste,
            ItemAction::Copy,
            ItemAction::SplitWords,
            ItemAction::Translate,
        ]);
        assert_eq!(
            menu_groups(&item, true, false)[0],
            vec![
                MenuAction::Paste,
                MenuAction::Copy,
                MenuAction::SplitWords,
                MenuAction::Translate
            ]
        );
        let item = with_actions(&[
            ItemAction::Paste,
            ItemAction::SplitWords,
            ItemAction::CopyTranslation,
            ItemAction::PasteTranslation,
        ]);
        assert_eq!(
            menu_groups(&item, true, false)[0],
            vec![
                MenuAction::Paste,
                MenuAction::SplitWords,
                MenuAction::CopyTranslation,
                MenuAction::PasteTranslation
            ]
        );
        assert_eq!(
            MenuAction::Translate
                .accelerator(&kwikpaste_core::settings::Shortcuts::default())
                .as_deref(),
            Some("secondary-l")
        );
    }

    #[test]
    fn nothing_to_do_means_no_menu() {
        let item = with_actions(&[ItemAction::Delete]);
        assert!(menu_groups(&item, false, true).is_empty());
    }

    #[test]
    fn toggles_flip_their_labels() {
        let mut item = with_actions(&[]);
        assert_eq!(
            MenuAction::ToggleFavorite.label_key(&item),
            "clipboard:menu.favorite"
        );
        assert_eq!(
            MenuAction::EditNote.label_key(&item),
            "clipboard:menu.addNote"
        );

        item.is_favorite = true;
        item.is_pinned = true;
        item.note = Some("n".into());
        assert_eq!(
            MenuAction::ToggleFavorite.label_key(&item),
            "clipboard:menu.unfavorite"
        );
        assert_eq!(
            MenuAction::TogglePinned.label_key(&item),
            "clipboard:menu.unpinItem"
        );
        assert_eq!(
            MenuAction::EditNote.label_key(&item),
            "clipboard:menu.editNote"
        );
    }
}
