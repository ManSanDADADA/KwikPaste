//! 卡片右键菜单（1.x `menu/clipboard_item.rs` 与 Windows 上的自绘菜单窗）：画在面板窗口里，不抢前台。
//!
//! 整个列表只挂一个右键菜单：按下右键时指针下的卡片就是悬停中的那张，菜单在下一帧按它构建；
//! 指针下没有卡片（空白处、页脚）或在多选时为空，不弹出。动作、顺序、快捷键提示与 1.x 相同，
//! 处理与 1.x `handleMenuActionRef` 相同；“移动到分组”的子菜单勾着当前分组，再点一次移出分组。

use std::sync::Arc;

use gpui::{Context, Window};
use kwikpaste_ui::{
    MenuEntry, MenuItem, Submenu,
    toast::{self, Toast},
};

use super::{ClipboardList, ops::error_toast};
use crate::{
    clipboard::{
        model::{
            actions::OpenTarget,
            item::ListItem,
            menu::{MenuAction, menu_groups},
            shortcut,
        },
        view::pin,
    },
    i18n::t,
};

impl ClipboardList {
    /// 悬停中的卡片的右键菜单；没有卡片或在多选时为空。
    pub(in crate::clipboard::view) fn context_menu_entries(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<MenuEntry> {
        if self.selection.active() {
            return Vec::new();
        }
        let Some(item) = self
            .hovered
            .as_ref()
            .and_then(|id| self.model.find(id))
            .cloned()
        else {
            return Vec::new();
        };

        let can_delete = self.can_delete(item.is_favorite, item.is_pinned);
        let groups: Vec<_> = self
            .groups
            .iter()
            .filter(|group| !group.is_hidden)
            .cloned()
            .collect();
        let layout = menu_groups(&item, can_delete, !groups.is_empty());
        let mut entries = Vec::new();

        for (index, actions) in layout.into_iter().enumerate() {
            if index > 0 {
                entries.push(MenuEntry::Separator);
            }
            for action in actions {
                let label = t(action.label_key(&item));
                if action == MenuAction::MoveToGroup {
                    let options = groups
                        .iter()
                        .map(|group| {
                            let current = item.group_id.as_ref() == Some(&group.id);
                            let target = (!current).then(|| group.id.clone());
                            let entity = cx.entity().downgrade();
                            let moved = item.clone();
                            MenuItem::new(group.name.to_string(), move |window, cx| {
                                entity
                                    .update(cx, |list, cx| {
                                        list.move_to_group(
                                            moved.clone(),
                                            target.clone(),
                                            window,
                                            cx,
                                        );
                                    })
                                    .ok();
                            })
                            .checked(current)
                            .into()
                        })
                        .collect();
                    entries.push(Submenu::new(label, options).into());
                    continue;
                }

                let entity = cx.entity().downgrade();
                let target = item.clone();
                let mut entry = MenuItem::new(label, move |window, cx| {
                    entity
                        .update(cx, |list, cx| {
                            list.run_menu_action(target.clone(), action, window, cx);
                        })
                        .ok();
                });
                if let Some(accelerator) = action.accelerator() {
                    entry = entry.shortcut(shortcut::display(accelerator));
                }
                if action == MenuAction::Delete {
                    entry = entry.danger();
                }
                entries.push(entry.into());
            }
        }

        entries
    }

    /// 执行右键菜单里的动作（1.x `handleMenuActionRef`）。
    pub fn run_menu_action(
        &mut self,
        item: Arc<ListItem>,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::info!("menu action {action:?} on {}", item.id);
        self.controller.select(&item.id);
        let id = item.id.clone();

        match action {
            MenuAction::Paste => self.paste_item(id, false, cx),
            MenuAction::PasteAsPlainText | MenuAction::PasteAsPath => self.paste_item(id, true, cx),
            MenuAction::Copy => self.copy(id, false, None, window, cx),
            MenuAction::CopyImageText => self.copy_image_text(id, window, cx),
            MenuAction::SaveImage => {
                self.close_preview_of(&id, cx);
                self.save_image(id, window, cx);
            }
            MenuAction::SplitWords => self.split(&item, cx),
            MenuAction::OpenLink => self.open(id, OpenTarget::Link, window, cx),
            MenuAction::SendEmail => self.open(id, OpenTarget::Email, window, cx),
            MenuAction::RevealInFinder | MenuAction::RevealInExplorer => {
                self.open(id, OpenTarget::Reveal, window, cx)
            }
            MenuAction::ToggleFavorite => self.toggle_favorite(id, window, cx),
            MenuAction::TogglePinned => self.toggle_pinned(id, window, cx),
            MenuAction::EditNote => self.edit_note(id, window, cx),
            MenuAction::EditContent => self.edit_content(id, window, cx),
            MenuAction::Select => {
                // 进入多选并勾上这一条（受删除保护的勾不上，1.x 同）。
                self.selection.enter();
                if self.can_delete(item.is_favorite, item.is_pinned)
                    && !self.selection.is_checked(&id)
                {
                    self.toggle_checked(&item, window, cx);
                }
            }
            MenuAction::Delete => self.delete(id, window, cx),
            MenuAction::MoveToGroup => {}
        }
        cx.notify();
    }

    /// 移到分组或移出分组（`None`）。在别的分组视图里移走的记录立即移出列表，其余只更新分组
    /// （1.x `handleMoveToGroup`）。
    pub fn move_to_group(
        &mut self,
        item: Arc<ListItem>,
        group_id: Option<Arc<str>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.controller.select(&item.id);
        self.close_preview_of(&item.id, cx);
        let future = self
            .source
            .set_item_group(item.id.clone(), group_id.clone());
        let id = item.id.clone();

        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(()) => {
                    let key = if group_id.is_some() {
                        "commands:messages.itemMovedToGroup"
                    } else {
                        "commands:messages.itemRemovedFromGroup"
                    };
                    Self::toast_success(key, window, cx);
                    let viewing = list.filter().group_id.clone();
                    if viewing.is_some() && viewing != group_id {
                        list.remove_item(&id, cx);
                    } else {
                        list.patch_item(&id, |item| item.group_id = group_id, cx);
                    }
                }
                Err(err) => Self::toast_error("commands:labels.moveToGroup", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 图片另存为（1.x `save_clipboard_image_to_file`）：默认放在下载文件夹，文件名是复制时间。
    /// 系统对话框打开期间点它不会让面板隐藏（见 [`pin`]）。
    fn save_image(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        let future = self.source.image_save(id);

        cx.spawn_in(window, async move |_, cx| {
            let save = match future.await {
                Ok(save) => save,
                Err(err) => {
                    cx.update(|window, cx| {
                        error_toast("commands:labels.saveImage", &err, window, cx)
                    })
                    .ok();
                    return;
                }
            };
            let directory = dirs::download_dir().unwrap_or_default();
            let Ok(prompt) =
                cx.update(|_, cx| pin::prompt_for_new_path(&directory, &save.file_name, cx))
            else {
                return;
            };
            let Some(target) = prompt.await else {
                return;
            };
            let source = save.source;
            let copied = cx
                .background_executor()
                .spawn(async move { std::fs::copy(source, target) })
                .await;
            cx.update(|window, cx| match copied {
                Ok(_) => toast::show(
                    Toast::success(t("commands:messages.imageSaved")),
                    window,
                    cx,
                ),
                Err(err) => error_toast(
                    "commands:labels.saveImage",
                    &anyhow::Error::from(err),
                    window,
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }
}
