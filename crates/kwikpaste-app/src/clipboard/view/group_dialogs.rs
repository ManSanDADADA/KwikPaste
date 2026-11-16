//! 分组弹框：新增 / 编辑分组（1.x `ClipboardGroupModal`）与管理分组（1.x 偏好设置里的
//! `ClipboardGroupManagerModal`）。
//!
//! - 新增 / 编辑：名称必填（去首尾空白、最多 32 个字），图标是 12 个预设之一或导入的 SVG；
//!   名称框要打字，弹框打开时请求编辑态（见 [`editing::begin_dialog_input`]）。
//! - 管理：拖动排序、勾选显示在分组栏上、每行的“…”里编辑和删除，页脚“新增”；新增、编辑、删除当场
//!   生效，排序和显隐点保存时一次写入（1.x 同）。1.x 的管理框在偏好设置窗口里，偏好设置窗口在 U3，
//!   这里先在主窗口里弹出同一个管理框。
//!
//! 1.x 的分组没有颜色设置，这里也没有。

use std::{collections::HashSet, rc::Rc, sync::Arc};

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, svg,
};
use kwikpaste_ui::{
    Button, ButtonSize, Checkbox, ConfirmSpec, DialogSpec, IconName, Input, KpStyled as _,
    MenuEntry, MenuItem, MenuTrigger, TextInput, TextInputEvent, confirm, form_dialog,
    group_icon_path,
    theme::{self, TextSize, radius, space},
    toast::{self, Toast},
};

use super::{editing, pin};
use crate::{
    clipboard::source::{ClipboardSource, Group, GroupInput},
    i18n::{t, t_args},
};

/// 1.x `DEFAULT_GROUP_ICON`。
const DEFAULT_ICON: &str = "i-lets-icons:folder";
/// 1.x `PRESET_GROUP_ICONS`，每行 6 个。
const PRESET_ICONS: [&str; 12] = [
    DEFAULT_ICON,
    "i-lets-icons:star",
    "i-lets-icons:book",
    "i-lets-icons:bookmark",
    "i-lets-icons:box",
    "i-lets-icons:database",
    "i-lets-icons:code",
    "i-lets-icons:link",
    "i-lets-icons:notebook",
    "i-lets-icons:calendar",
    "i-lets-icons:bell",
    "i-lets-icons:setting-line",
];
/// 管理框里有写入之后通知主窗口（重读分组）。
pub(super) type Changed = Rc<dyn Fn(&mut Window, &mut App)>;

/// 1.x 名称框 `maxLength={32}`。
const NAME_MAX_CHARS: usize = 32;

fn is_custom(icon: &str) -> bool {
    icon.trim_start().starts_with("<svg")
}

fn error_toast(label: &str, err: &anyhow::Error, window: &mut Window, cx: &mut App) {
    log::warn!("{label} failed: {err:#}");
    let message = t_args(
        "commands:error",
        &[("label", &t(label)), ("message", &err.to_string())],
    );
    toast::show(Toast::error(message), window, cx);
}

/// 表单项的标题（antd `Form.Item` 竖排的 label；必填项前面一个红色星号）。
fn field_label(label: SharedString, required: bool, cx: &App) -> AnyElement {
    let tokens = theme::semantic(cx);

    div()
        .flex()
        .items_center()
        .gap(space(1.))
        .kp_text(TextSize::Sm)
        .text_color(tokens.text.primary)
        .when(required, |row| {
            row.child(div().text_color(tokens.status.danger.solid).child("*"))
        })
        .child(label)
        .into_any_element()
}

// ---------------------------------------------------------------- 新增 / 编辑

/// 新增 / 编辑分组弹框的内容。
pub struct GroupEditor {
    name: TextInput,
    icon: Arc<str>,
    is_hidden: bool,
    /// 点了保存而名称为空：名称框下显示必填提示。
    missing_name: bool,
    source: Arc<dyn ClipboardSource>,
    _subscriptions: Vec<Subscription>,
}

impl GroupEditor {
    pub(super) fn new(
        group: Option<&Group>,
        source: Arc<dyn ClipboardSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = TextInput::new(t("clipboard:groups.namePlaceholder"), window, cx);
        if let Some(group) = group {
            name.set_value(group.name.to_string(), window, cx);
        }
        let changes = name.on_event_in(window, cx, |editor, event, window, cx| {
            if let TextInputEvent::Change(value) = event {
                editor.name_changed(&value, window, cx);
            }
        });

        Self {
            name,
            icon: group.map_or_else(|| Arc::from(DEFAULT_ICON), |group| group.icon.clone()),
            is_hidden: group.is_some_and(|group| group.is_hidden),
            missing_name: false,
            source,
            _subscriptions: vec![changes],
        }
    }

    pub fn input(&self) -> &TextInput {
        &self.name
    }

    /// 自测与截图用：当作用户在名称框里输入了 `value`。
    pub(super) fn type_name(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name.set_value(value.to_owned(), window, cx);
        self.name_changed(&SharedString::from(value.to_owned()), window, cx);
    }

    fn name_changed(&mut self, value: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if value.chars().count() > NAME_MAX_CHARS {
            let cut: String = value.chars().take(NAME_MAX_CHARS).collect();
            self.name.set_value(cut, window, cx);
        }
        if self.missing_name && !value.trim().is_empty() {
            self.missing_name = false;
            cx.notify();
        }
    }

    /// 点保存时校验（antd `required` + `whitespace`）：名称去首尾空白后为空时留在弹框里提示。
    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<GroupInput> {
        let name = self.name.value(cx).trim().to_owned();
        if name.is_empty() {
            self.missing_name = true;
            cx.notify();
            return None;
        }

        Some(GroupInput {
            name,
            icon: self.icon.to_string(),
            is_hidden: self.is_hidden,
        })
    }

    pub fn pick_icon(&mut self, icon: &str, cx: &mut Context<Self>) {
        self.icon = Arc::from(icon);
        cx.notify();
    }

    /// 导入 SVG（1.x `importSvg`）：系统文件对话框打开期间点它不会让面板隐藏。
    fn import_svg(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = pin::prompt_for_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        let source = self.source.clone();

        cx.spawn_in(window, async move |editor, cx| {
            let Some(path) = prompt.await.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            let result = source.import_group_svg(path).await;
            editor
                .update_in(cx, |editor, window, cx| match result {
                    Ok(svg) => editor.pick_icon(&svg, cx),
                    Err(err) => {
                        error_toast("commands:labels.importClipboardGroupSvg", &err, window, cx)
                    }
                })
                .ok();
        })
        .detach();
    }

    fn preset_button(&self, icon: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let selected = *self.icon == *icon;

        div()
            .id(SharedString::from(format!("group-icon-{icon}")))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(space((36.) / 4.))
            .rounded(radius::MD)
            .cursor_pointer()
            // 浅灰底的图标块（同分组栏的扁平按钮），选中的是实心主色。
            .map(|button| {
                if selected {
                    button.bg(tokens.accent.solid)
                } else {
                    button
                        .bg(tokens.fill.subtle)
                        .hover(|style| style.bg(tokens.fill.default))
                }
            })
            .child(
                svg()
                    .path(group_icon_path(icon))
                    .size(space((16.) / 4.))
                    .text_color(if selected {
                        tokens.text.on_accent
                    } else {
                        tokens.text.secondary
                    }),
            )
            .on_click(cx.listener(move |editor, _, _, cx| editor.pick_icon(icon, cx)))
            .into_any_element()
    }
}

impl Render for GroupEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let custom = is_custom(&self.icon);
        let rows: Vec<AnyElement> = PRESET_ICONS
            .chunks(6)
            .map(|row| {
                div()
                    .flex()
                    .gap(space((8.) / 4.))
                    .children(row.iter().map(|icon| self.preset_button(icon, cx)))
                    .into_any_element()
            })
            .collect();
        let custom_label = if custom {
            t("clipboard:groups.customIcon")
        } else {
            t("clipboard:groups.useCustomIcon")
        };

        div()
            .flex()
            .flex_col()
            .gap(space((24.) / 4.))
            .pb(space((12.) / 4.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space((8.) / 4.))
                    .child(field_label(t("clipboard:groups.name"), true, cx))
                    .child(Input::new(&self.name))
                    .when(self.missing_name, |field| {
                        field.child(
                            div()
                                .kp_text(TextSize::Sm)
                                .text_color(tokens.status.danger.solid)
                                .child(t("clipboard:groups.nameRequired")),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space((8.) / 4.))
                    .child(field_label(t("clipboard:groups.icon"), false, cx))
                    .child(div().flex().flex_col().gap(space((8.) / 4.)).children(rows))
                    .child(
                        div()
                            .flex()
                            .gap(space((8.) / 4.))
                            .mt(space(1.))
                            .child(
                                div()
                                    .id("group-icon-custom")
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .justify_center()
                                    .gap(space((8.) / 4.))
                                    .h(space((32.) / 4.))
                                    .px(space((15.) / 4.))
                                    .rounded(radius::MD)
                                    .border_1()
                                    .border_color(tokens.border.default)
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style
                                            .border_color(tokens.accent.hover)
                                            .text_color(tokens.accent.hover)
                                    })
                                    .kp_text(TextSize::Sm)
                                    .child(
                                        svg()
                                            .path(group_icon_path(&self.icon))
                                            .flex_none()
                                            .size(space((16.) / 4.))
                                            .text_color(tokens.text.secondary),
                                    )
                                    .child(div().truncate().child(custom_label))
                                    .on_click(cx.listener(|editor, _, window, cx| {
                                        editor.import_svg(window, cx);
                                    })),
                            )
                            .when(custom, |row| {
                                row.child(
                                    Button::icon(
                                        "group-icon-remove",
                                        IconName::Trash2,
                                        t("clipboard:groups.removeIcon"),
                                    )
                                    .kind(kwikpaste_ui::ButtonKind::Default)
                                    .on_click(cx.listener(
                                        |editor, _, _, cx| {
                                            editor.pick_icon(DEFAULT_ICON, cx);
                                        },
                                    )),
                                )
                            }),
                    ),
            )
    }
}

/// 打开新增（`group` 为 `None`）或编辑分组弹框。保存成功后提示“已保存分组”，并把建好 / 改好的分组
/// 交给 `on_saved`；失败时提示原因、弹框已关。
pub fn edit_group(
    source: Arc<dyn ClipboardSource>,
    group: Option<Group>,
    on_saved: impl FnOnce(Group, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Entity<GroupEditor> {
    let editor = cx.new(|cx| GroupEditor::new(group.as_ref(), source.clone(), window, cx));
    let title = if group.is_some() {
        t("clipboard:groups.edit")
    } else {
        t("clipboard:groups.add")
    };
    let check = editor.clone();
    let content = editor.clone();
    let answer = form_dialog(
        DialogSpec::new(title)
            .ok_text(t("common:actions.save"))
            .cancel_text(t("common:actions.cancel"))
            .validate(move |_, cx| check.update(cx, |editor, cx| editor.submit(cx)).is_some()),
        move |_, _| content.clone().into_any_element(),
        window,
        cx,
    );
    editing::begin_dialog_input(editor.read(cx).input().clone(), cx);

    let saved = editor.clone();
    window
        .spawn(cx, async move |cx| {
            let save = answer.await.unwrap_or(false);
            let input = cx
                .update(|_, cx| {
                    editing::end_dialog_input(cx);
                    save.then(|| saved.update(cx, |editor, cx| editor.submit(cx)))
                        .flatten()
                })
                .ok()
                .flatten();
            let Some(input) = input else {
                return;
            };

            let result = save_group(source.as_ref(), group.as_ref(), input).await;
            cx.update(|window, cx| match result {
                Ok(group) => {
                    toast::show(
                        Toast::success(t("commands:messages.clipboardGroupSaved")),
                        window,
                        cx,
                    );
                    on_saved(group, window, cx);
                }
                Err(err) => error_toast("commands:labels.saveClipboardGroup", &err, window, cx),
            })
            .ok();
        })
        .detach();

    editor
}

/// 写入新增（`group` 为 `None`）或编辑的分组，返回写入后的分组。
pub(super) async fn save_group(
    source: &dyn ClipboardSource,
    group: Option<&Group>,
    input: GroupInput,
) -> anyhow::Result<Group> {
    let Some(group) = group else {
        return source.create_group(input).await;
    };

    source.update_group(group.id.clone(), input.clone()).await?;
    Ok(Group {
        id: group.id.clone(),
        name: Arc::from(input.name.trim()),
        icon: Arc::from(input.icon.as_str()),
        is_hidden: input.is_hidden,
    })
}

/// 删除分组前确认（1.x `requestDeleteGroup`）；确认并删除成功后调用 `on_deleted`。
pub fn delete_group(
    source: Arc<dyn ClipboardSource>,
    group: Group,
    on_deleted: impl FnOnce(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
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

    window
        .spawn(cx, async move |cx| {
            if !answer.await.unwrap_or(false) {
                return;
            }
            let result = source.delete_group(group.id.clone()).await;
            cx.update(|window, cx| match result {
                Ok(()) => {
                    toast::show(
                        Toast::success(t("commands:messages.clipboardGroupDeleted")),
                        window,
                        cx,
                    );
                    on_deleted(window, cx);
                }
                Err(err) => error_toast("commands:labels.deleteClipboardGroup", &err, window, cx),
            })
            .ok();
        })
        .detach();
}

// ---------------------------------------------------------------- 管理

/// 拖动中的分组（拖动排序的载荷）。
#[derive(Clone)]
struct DraggedGroup {
    index: usize,
    group: Group,
}

/// 拖动时跟着指针的那一行。
struct DragPreview {
    group: Group,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);

        div()
            .flex()
            .items_center()
            .gap(space((8.) / 4.))
            .h(space((32.) / 4.))
            .px(space((12.) / 4.))
            .rounded(radius::MD)
            .border_1()
            .border_color(tokens.accent.solid)
            .bg(tokens.surface.raised)
            .shadow(tokens.shadow.overlay.to_vec())
            .kp_text(TextSize::Sm)
            .text_color(tokens.text.primary)
            .child(
                svg()
                    .path(group_icon_path(&self.group.icon))
                    .size(space((16.) / 4.))
                    .text_color(tokens.text.secondary),
            )
            .child(self.group.name.to_string())
    }
}

/// 管理分组弹框的内容：分组的顺序与勾选（显示在分组栏上）在点保存前只改这里。
pub struct GroupManager {
    groups: Vec<Group>,
    visible: HashSet<Arc<str>>,
    source: Arc<dyn ClipboardSource>,
    /// 新增、编辑、删除都当场写入，写入后通知主窗口重读分组。
    on_changed: Changed,
}

impl GroupManager {
    pub(super) fn new(
        groups: Vec<Group>,
        source: Arc<dyn ClipboardSource>,
        on_changed: Changed,
    ) -> Self {
        let visible = groups
            .iter()
            .filter(|group| !group.is_hidden)
            .map(|group| group.id.clone())
            .collect();

        Self {
            groups,
            visible,
            source,
            on_changed,
        }
    }

    /// 把第 `from` 个分组挪到第 `to` 个的位置（拖动排序）。
    pub fn move_group(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.groups.len() || to >= self.groups.len() {
            return;
        }
        let group = self.groups.remove(from);
        self.groups.insert(to, group);
        cx.notify();
    }

    pub fn set_visible(&mut self, id: Arc<str>, visible: bool, cx: &mut Context<Self>) {
        if visible {
            self.visible.insert(id);
        } else {
            self.visible.remove(&id);
        }
        cx.notify();
    }

    /// 新建的分组排到最后，没隐藏就勾上；编辑过的就地替换。
    fn saved(&mut self, group: Group, cx: &mut Context<Self>) {
        match self
            .groups
            .iter_mut()
            .find(|current| current.id == group.id)
        {
            Some(current) => *current = group,
            None => {
                if !group.is_hidden {
                    self.visible.insert(group.id.clone());
                }
                self.groups.push(group);
            }
        }
        cx.notify();
    }

    fn removed(&mut self, id: &Arc<str>, cx: &mut Context<Self>) {
        self.groups.retain(|group| group.id != *id);
        self.visible.remove(id);
        cx.notify();
    }

    fn open_editor(&self, group: Option<Group>, window: &mut Window, cx: &mut Context<Self>) {
        let manager = cx.entity().downgrade();
        let on_changed = self.on_changed.clone();
        edit_group(
            self.source.clone(),
            group,
            move |group, window, cx| {
                manager
                    .update(cx, |manager, cx| manager.saved(group, cx))
                    .ok();
                on_changed(window, cx);
            },
            window,
            cx,
        );
    }

    fn request_delete(&self, group: Group, window: &mut Window, cx: &mut Context<Self>) {
        let manager = cx.entity().downgrade();
        let on_changed = self.on_changed.clone();
        let id = group.id.clone();
        delete_group(
            self.source.clone(),
            group,
            move |window, cx| {
                manager
                    .update(cx, |manager, cx| manager.removed(&id, cx))
                    .ok();
                on_changed(window, cx);
            },
            window,
            cx,
        );
    }

    /// 点保存：一次写入顺序和显隐（1.x `updateClipboardGroupsLayout`）。
    pub fn layout(&self) -> (Vec<Arc<str>>, Vec<Arc<str>>) {
        let order: Vec<Arc<str>> = self.groups.iter().map(|group| group.id.clone()).collect();
        let visible = order
            .iter()
            .filter(|id| self.visible.contains(*id))
            .cloned()
            .collect();
        (order, visible)
    }

    fn row(&self, index: usize, group: &Group, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let id = group.id.clone();
        let visible = self.visible.contains(&group.id);
        let dragged = DraggedGroup {
            index,
            group: group.clone(),
        };
        let menu_group = group.clone();
        let entity = cx.entity().downgrade();
        let toggler = entity.clone();

        div()
            .id(SharedString::from(format!("manage-group-{}", group.id)))
            .flex()
            .items_center()
            .gap(space((8.) / 4.))
            .h(space((36.) / 4.))
            .pl(space(1.))
            .pr(space(1.))
            .rounded(radius::MD)
            // 扁平的行：悬停浅灰底，拖到上面时淡主色底（同列表的勾选色）。
            .hover(|style| style.bg(tokens.fill.faint))
            .on_drag(dragged, |dragged, _, _, cx| {
                cx.new(|_| DragPreview {
                    group: dragged.group.clone(),
                })
            })
            .drag_over::<DraggedGroup>(move |style, _, _, _| style.bg(tokens.accent.subtle))
            .on_drop(cx.listener(move |manager, dragged: &DraggedGroup, _, cx| {
                manager.move_group(dragged.index, index, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .cursor_grab()
                    .text_color(tokens.text.faint)
                    .child(kwikpaste_ui::Icon::new(IconName::GripVertical).size(space((14.) / 4.))),
            )
            .child(
                Checkbox::new(SharedString::from(format!(
                    "manage-group-visible-{}",
                    group.id
                )))
                .checked(visible)
                .accessibility_label(SharedString::from(group.name.to_string()))
                .on_change(move |checked, _, cx| {
                    toggler
                        .update(cx, |manager, cx| {
                            manager.set_visible(id.clone(), checked, cx)
                        })
                        .ok();
                }),
            )
            .child(
                svg()
                    .path(group_icon_path(&group.icon))
                    .flex_none()
                    .size(space((16.) / 4.))
                    .text_color(tokens.text.secondary),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .kp_text(TextSize::Sm)
                    .text_color(tokens.text.primary)
                    .child(group.name.to_string()),
            )
            .child(
                MenuTrigger::new(SharedString::from(format!(
                    "manage-group-more-{}",
                    group.id
                )))
                .flex_none()
                .child(
                    Button::icon(
                        SharedString::from(format!("manage-group-more-button-{}", group.id)),
                        IconName::Ellipsis,
                        t("clipboard:groups.manage"),
                    )
                    .size(ButtonSize::Small),
                )
                .dropdown(move |_, _| row_menu(&menu_group, entity.clone())),
            )
            .into_any_element()
    }
}

/// 每行“…”的菜单：编辑、删除（1.x `buildGroupTreeMenuItems`）。
fn row_menu(group: &Group, manager: gpui::WeakEntity<GroupManager>) -> Vec<MenuEntry> {
    let edit = group.clone();
    let delete = group.clone();
    let edit_manager = manager.clone();

    vec![
        MenuItem::new(t("clipboard:groups.edit"), move |window, cx| {
            edit_manager
                .update(cx, |manager, cx| {
                    manager.open_editor(Some(edit.clone()), window, cx)
                })
                .ok();
        })
        .icon(IconName::Pencil)
        .into(),
        MenuItem::new(t("clipboard:groups.delete"), move |window, cx| {
            manager
                .update(cx, |manager, cx| {
                    manager.request_delete(delete.clone(), window, cx)
                })
                .ok();
        })
        .icon(IconName::Trash2)
        .danger()
        .into(),
    ]
}

impl Render for GroupManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = self.groups.clone();

        div()
            .id("group-manager")
            .flex()
            .flex_col()
            .gap(space((2.) / 4.))
            .max_h(space((360.) / 4.))
            .overflow_y_scroll()
            .children(
                groups
                    .iter()
                    .enumerate()
                    .map(|(index, group)| self.row(index, group, cx)),
            )
    }
}

/// 打开管理分组弹框。`on_changed` 在分组有任何写入（新增、编辑、删除、保存布局）后调用。
pub fn manage_groups(
    source: Arc<dyn ClipboardSource>,
    groups: Vec<Group>,
    on_changed: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Entity<GroupManager> {
    let on_changed: Changed = Rc::new(on_changed);
    let manager = cx.new(|_| GroupManager::new(groups, source.clone(), on_changed.clone()));
    let content = manager.clone();
    let adder = manager.downgrade();
    let answer = form_dialog(
        DialogSpec::new(t("clipboard:groups.manage"))
            .ok_text(t("common:actions.save"))
            .cancel_text(t("common:actions.cancel"))
            .footer_extra(move |_, _| {
                let adder = adder.clone();
                Button::new("manage-group-add", t("common:actions.add"))
                    .on_click(move |_, window, cx| {
                        adder
                            .update(cx, |manager, cx| manager.open_editor(None, window, cx))
                            .ok();
                    })
                    .into_any_element()
            }),
        move |_, _| content.clone().into_any_element(),
        window,
        cx,
    );

    let saved = manager.clone();
    window
        .spawn(cx, async move |cx| {
            if !answer.await.unwrap_or(false) {
                return;
            }
            let Ok((order, visible)) = cx.update(|_, cx| saved.read(cx).layout()) else {
                return;
            };
            let result = source.update_groups_layout(order, visible).await;
            cx.update(|window, cx| match result {
                Ok(()) => {
                    toast::show(
                        Toast::success(t("commands:messages.clipboardGroupsLayoutSaved")),
                        window,
                        cx,
                    );
                    on_changed(window, cx);
                }
                Err(err) => error_toast(
                    "commands:labels.saveClipboardGroupsLayout",
                    &err,
                    window,
                    cx,
                ),
            })
            .ok();
        })
        .detach();

    manager
}
