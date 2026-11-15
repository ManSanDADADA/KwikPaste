//! 头部下方的分组栏（1.x `Group.tsx`）：范围（全部 / 收藏）、分类（文本 / 图片 / 文件 / 链接）和自定义分组。
//!
//! - 「全部」之后的收藏和分类按偏好 `clipboard.window.groupBar` 显示和排序，相邻的范围与分类之间加分隔线；
//! - 范围始终有一个选中；分类、自定义分组再点一次取消；
//! - 隐藏的分组不出现在栏上；放不下的自定义分组收进“更多”菜单；
//! - 自定义分组右键可以编辑、隐藏、删除；新增、编辑、管理分组的弹框由主窗口打开（`group_dialogs`）；
//! - 双栏布局拆成两条：左栏只有分类，右栏是「收藏」加自定义分组的页签（始终有一个选中）。

use std::sync::Arc;

use gpui::{
    AnyElement, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use kwikpaste_core::settings::GroupBarEntry;
use kwikpaste_ui::{
    Glyph, IconButtonLook as Look, IconName, MenuEntry, MenuItem, MenuTrigger, context_menu,
    group_icon_path, icon_button, separator, theme::space,
};

use crate::{
    clipboard::{
        model::filter::{Category, ListFilter, Range, bar_categories, entry_category},
        source::Group,
    },
    i18n::t,
    platform::window_drag::WindowDragArea as _,
};

/// 1.x 分组按钮的尺寸与间距（设计 px），用来算一行放得下几个自定义分组。
const BUTTON: f32 = 24.;
const GAP: f32 = 4.;
/// 分组栏左内边距。
const PADDING: f32 = 12.;
/// 分隔线连左右外边距的宽度。
const SEPARATOR: f32 = 1. + 2. * 4.;
/// 末尾留给“更多”或“新增”按钮的位置。
const ACTION_SLOT: f32 = GAP + BUTTON;

/// 分组栏放哪些按钮。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarMode {
    /// 单栏：范围、分类、自定义分组。
    Full,
    /// 双栏的左栏：只有分类。
    Categories,
    /// 双栏的右栏：收藏和自定义分组。
    Side,
}

/// 分组栏发给主窗口的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupBarEvent {
    SelectRange(Range),
    ToggleCategory(Category),
    ToggleGroup(Arc<str>),
    EditGroup(Group),
    HideGroup(Group),
    DeleteGroup(Group),
    NewGroup,
    ManageGroups,
}

pub struct GroupBar {
    mode: BarMode,
    entries: Vec<GroupBarEntry>,
    groups: Vec<Group>,
    range: Range,
    category: Option<Category>,
    group_id: Option<Arc<str>>,
    hints: bool,
}

impl EventEmitter<GroupBarEvent> for GroupBar {}

/// 第一个自定义分组的起点（设计 px）：左内边距，加上它前面的按钮和分隔线，各带一个间距。
fn custom_start(buttons: usize, separators: usize) -> f32 {
    PADDING + buttons as f32 * (BUTTON + GAP) + separators as f32 * (SEPARATOR + GAP)
}

/// 一行能放下几个自定义分组（宽度为设计 px，`start` 是第一个自定义分组的起点）。
fn capacity_from(width: f32, start: f32) -> usize {
    let available = (width - start - ACTION_SLOT).max(0.);
    ((available + GAP) / (BUTTON + GAP)).floor().max(0.) as usize
}

/// 内置按钮的图标和文案，偏好设置里的分组栏管理也用。
pub(crate) fn entry_glyph(entry: GroupBarEntry) -> (IconName, SharedString) {
    let (icon, key) = match entry {
        GroupBarEntry::Favorite => (IconName::GroupFavorite, "favorite"),
        GroupBarEntry::Text => (IconName::GroupText, "text"),
        GroupBarEntry::Image => (IconName::GroupImage, "image"),
        GroupBarEntry::Files => (IconName::GroupFiles, "files"),
        GroupBarEntry::Link => (IconName::Link, "link"),
    };
    (icon, t(&format!("clipboard:groups.{key}")))
}

impl GroupBar {
    /// `entries` 是偏好里显示的内置按钮；右栏页签不用它。
    pub fn new(mode: BarMode, entries: Vec<GroupBarEntry>) -> Self {
        Self {
            mode,
            entries,
            groups: Vec::new(),
            range: Range::All,
            category: None,
            group_id: None,
            hints: false,
        }
    }

    pub fn entries(&self) -> &[GroupBarEntry] {
        &self.entries
    }

    pub fn set_entries(&mut self, entries: Vec<GroupBarEntry>, cx: &mut Context<Self>) {
        self.entries = entries;
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: BarMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            cx.notify();
        }
    }

    pub fn set_groups(&mut self, groups: Vec<Group>, cx: &mut Context<Self>) {
        self.groups = groups;
        cx.notify();
    }

    /// 分组栏上可见的自定义分组的 id（Tab 循环用）。
    pub fn visible_ids(&self) -> Vec<Arc<str>> {
        self.groups
            .iter()
            .filter(|group| !group.is_hidden)
            .map(|group| group.id.clone())
            .collect()
    }

    pub fn set_filter(&mut self, filter: &ListFilter, cx: &mut Context<Self>) {
        self.range = filter.range;
        self.category = filter.category;
        self.group_id = filter.group_id.clone();
        cx.notify();
    }

    pub fn set_hints(&mut self, hints: bool, cx: &mut Context<Self>) {
        if self.hints != hints {
            self.hints = hints;
            cx.notify();
        }
    }

    fn range_button(&self, range: Range, cx: &mut Context<Self>) -> AnyElement {
        let (id, (icon, label)) = match range {
            Range::All => (
                "group-range-all",
                (IconName::GroupAll, t("clipboard:groups.all")),
            ),
            Range::Favorite => ("group-range-favorite", entry_glyph(GroupBarEntry::Favorite)),
        };
        let (selected, hint) = if self.mode == BarMode::Side {
            (self.group_id.is_none(), None)
        } else {
            // Mod+Q 的角标标在按下去会切到的那个按钮上。
            let hint = self.hints && self.range.toggled() == range;
            (self.range == range, hint.then(|| SharedString::from("Q")))
        };

        icon_button(cx, id, Glyph::Icon(icon), label, hint, look(selected))
            .role(Role::Tab)
            .aria_selected(selected)
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(GroupBarEvent::SelectRange(range))))
            .into_any_element()
    }

    fn category_button(&self, kind: Category, cx: &mut Context<Self>) -> AnyElement {
        let (id, entry) = match kind {
            Category::Text => ("group-text", GroupBarEntry::Text),
            Category::Image => ("group-image", GroupBarEntry::Image),
            Category::Files => ("group-files", GroupBarEntry::Files),
            Category::Link => ("group-link", GroupBarEntry::Link),
        };
        let (icon, label) = entry_glyph(entry);

        icon_button(
            cx,
            id,
            Glyph::Icon(icon),
            label,
            None,
            look(self.category == Some(kind)),
        )
        .on_click(cx.listener(move |_, _, _, cx| cx.emit(GroupBarEvent::ToggleCategory(kind))))
        .role(Role::Tab)
        .aria_selected(self.category == Some(kind))
        .into_any_element()
    }

    fn custom_button(&self, group: &Group, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.group_id.as_deref() == Some(&*group.id);
        let id = group.id.clone();
        let entity = cx.entity().downgrade();
        let menu_group = group.clone();

        let button = icon_button(
            cx,
            SharedString::from(format!("group-custom-{}", group.id)),
            Glyph::Path(group_icon_path(&group.icon)),
            SharedString::from(group.name.to_string()),
            None,
            look(selected),
        )
        .role(Role::Tab)
        .aria_selected(selected)
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(GroupBarEvent::ToggleGroup(id.clone()));
        }));

        context_menu(button, move |_, _| group_menu(&menu_group, entity.clone()))
    }

    fn more_button(&self, overflow: Vec<Group>, cx: &mut Context<Self>) -> AnyElement {
        let selected = overflow
            .iter()
            .any(|group| self.group_id.as_deref() == Some(&*group.id));
        let current = self.group_id.clone();
        let entity = cx.entity().downgrade();
        let hint = self.hints.then(|| SharedString::from("N"));

        MenuTrigger::new("group-more")
            .child(icon_button(
                cx,
                "group-more-button",
                Glyph::Icon(IconName::Ellipsis),
                t("clipboard:groups.more"),
                hint,
                look(selected),
            ))
            .dropdown(move |_, _| {
                let mut entries: Vec<MenuEntry> = overflow
                    .iter()
                    .map(|group| {
                        let id = group.id.clone();
                        let entity = entity.clone();
                        MenuItem::new(SharedString::from(group.name.to_string()), move |_, cx| {
                            entity
                                .update(cx, |_, cx| {
                                    cx.emit(GroupBarEvent::ToggleGroup(id.clone()));
                                })
                                .ok();
                        })
                        .icon_path(group_icon_path(&group.icon))
                        .checked(current.as_deref() == Some(&*group.id))
                        .into()
                    })
                    .collect();
                entries.push(MenuEntry::Separator);
                entries.extend(create_entries(entity.clone()));
                entries
            })
    }

    fn create_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let hint = self.hints.then(|| SharedString::from("N"));

        let button = icon_button(
            cx,
            "group-create",
            Glyph::Icon(IconName::Plus),
            t("clipboard:groups.add"),
            hint,
            Look::Text,
        )
        .on_click(cx.listener(|_, _, _, cx| cx.emit(GroupBarEvent::NewGroup)));

        context_menu(button, move |_, _| {
            let entity = entity.clone();
            vec![
                MenuItem::new(t("clipboard:groups.manage"), move |_, cx| {
                    entity
                        .update(cx, |_, cx| cx.emit(GroupBarEvent::ManageGroups))
                        .ok();
                })
                .icon(IconName::Settings2)
                .into(),
            ]
        })
    }
}

fn look(selected: bool) -> Look {
    if selected { Look::Chip } else { Look::Text }
}

/// 自定义分组的右键菜单：编辑、隐藏、删除（1.x `buildGroupActionMenuItems`）。
fn group_menu(group: &Group, entity: gpui::WeakEntity<GroupBar>) -> Vec<MenuEntry> {
    let edit = group.clone();
    let hide = group.clone();
    let delete = group.clone();
    let edit_entity = entity.clone();
    let hide_entity = entity.clone();

    vec![
        MenuItem::new(t("clipboard:groups.edit"), move |_, cx| {
            edit_entity
                .update(cx, |_, cx| cx.emit(GroupBarEvent::EditGroup(edit.clone())))
                .ok();
        })
        .icon(IconName::Pencil)
        .into(),
        MenuItem::new(t("clipboard:groups.hide"), move |_, cx| {
            hide_entity
                .update(cx, |_, cx| cx.emit(GroupBarEvent::HideGroup(hide.clone())))
                .ok();
        })
        .icon(IconName::EyeOff)
        .into(),
        MenuEntry::Separator,
        MenuItem::new(t("clipboard:groups.delete"), move |_, cx| {
            entity
                .update(cx, |_, cx| {
                    cx.emit(GroupBarEvent::DeleteGroup(delete.clone()))
                })
                .ok();
        })
        .icon(IconName::Trash2)
        .danger()
        .into(),
    ]
}

fn create_entries(entity: gpui::WeakEntity<GroupBar>) -> Vec<MenuEntry> {
    let manage = entity.clone();

    vec![
        MenuItem::new(t("clipboard:groups.add"), move |_, cx| {
            entity
                .update(cx, |_, cx| cx.emit(GroupBarEvent::NewGroup))
                .ok();
        })
        .icon(IconName::Plus)
        .into(),
        MenuItem::new(t("clipboard:groups.manage"), move |_, cx| {
            manage
                .update(cx, |_, cx| cx.emit(GroupBarEvent::ManageGroups))
                .ok();
        })
        .icon(IconName::Settings2)
        .into(),
    ]
}

impl Render for GroupBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut width = window.viewport_size().width.as_f32() / window.rem_size().as_f32() * 16.;
        if self.mode != BarMode::Full {
            // 双栏两列等宽，每条分组栏占一半。
            width /= 2.;
        }
        let bar = div()
            .id(match self.mode {
                BarMode::Side => "clipboard-side-group-bar",
                _ => "clipboard-group-bar",
            })
            .role(Role::TabList)
            .aria_label(t("clipboard:accessibility.groupBar"))
            .flex()
            .flex_none()
            .w_full()
            .items_center()
            .gap(space((GAP) / 4.))
            .overflow_hidden()
            .px(space(PADDING / 4.))
            .pb(space((8.) / 4.));
        let drag_area = div()
            .flex_1()
            .min_w_0()
            .h(space((32.) / 4.))
            .window_drag_area();
        if self.mode == BarMode::Categories {
            let categories: Vec<AnyElement> = bar_categories(&self.entries)
                .into_iter()
                .map(|kind| self.category_button(kind, cx))
                .collect();
            return bar.children(categories).child(drag_area);
        }

        let side = self.mode == BarMode::Side;
        let mut leading =
            vec![self.range_button(if side { Range::Favorite } else { Range::All }, cx)];
        let mut separators = 0;
        if !side {
            let mut previous_range = true;
            for entry in &self.entries {
                let category = entry_category(*entry);
                if category.is_some() == previous_range {
                    leading.push(separator(cx).into_any_element());
                    separators += 1;
                }
                previous_range = category.is_none();
                leading.push(match category {
                    Some(kind) => self.category_button(kind, cx),
                    None => self.range_button(Range::Favorite, cx),
                });
            }
        }
        let start = custom_start(leading.len() - separators, separators + 1);

        let visible: Vec<Group> = self
            .groups
            .iter()
            .filter(|group| !group.is_hidden)
            .cloned()
            .collect();
        let inline = visible.len().min(capacity_from(width, start));
        let overflow: Vec<Group> = visible.iter().skip(inline).cloned().collect();

        let customs: Vec<AnyElement> = visible
            .iter()
            .take(inline)
            .map(|group| self.custom_button(group, cx))
            .collect();
        let action = if overflow.is_empty() {
            self.create_button(cx)
        } else {
            self.more_button(overflow, cx)
        };

        bar.children(leading)
            .child(separator(cx))
            .when(!customs.is_empty(), |bar| bar.children(customs))
            .child(action)
            .child(drag_area)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 360 px 宽的面板放 5 个自定义分组（1.x 同宽时的实测）；内置按钮都隐藏后放 10 个。
    #[test]
    fn the_default_panel_fits_five_custom_groups() {
        // 全部、收藏 | 文本、图片、文件 |
        let default = custom_start(5, 2);
        assert_eq!(capacity_from(360., default), 5);
        assert_eq!(capacity_from(200., default), 0);
        assert_eq!(capacity_from(500., default), 10);

        // 只剩「全部」，与右栏页签（收藏 |）的起点相同。
        assert_eq!(capacity_from(360., custom_start(1, 1)), 10);
    }
}
