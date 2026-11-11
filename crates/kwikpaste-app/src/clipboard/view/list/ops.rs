//! 列表上的记录操作：筛选、复制、收藏、置顶、备注、删除、打开、粘贴（1.x `List.tsx` 的各个 handler
//! 与 `commands/index.ts` 的提示）。
//!
//! core 的记录操作不发事件：成功后就地改列表（收藏视图里取消收藏、删除时移除，其余合并标记），
//! 置顶影响排序，改完重拉当前范围。成功、失败都用顶部提示告知（与 1.x 的 `getMessageApi()` 一致）。

use std::{path::Path, sync::Arc, time::Duration};

use gpui::{
    App, Context, Div, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, Task, Window, div,
};
use kwikpaste_core::{
    clipboard::{ClipboardFragment, SnippetKind},
    db::models::ClipboardItemQuery,
    settings::{AutoPaste, Settings},
};
use kwikpaste_ui::{
    ConfirmSpec, DialogSpec, TextArea, TextAreaInput, confirm, form_dialog,
    toast::{self, Toast},
};

use super::{ClipboardList, ListIntent, NoteEdit};
use crate::{
    clipboard::{
        model::{
            actions::{DeletePolicy, OpenTarget, QuickAction, is_available, is_copy, open_target},
            filter::ListFilter,
            freshness::Activation,
            item::{ItemKind, ListItem, SubKind},
            layout::LayoutSpec,
        },
        source::{Group, core_source::item_query},
        view::{
            card,
            editing::{self, EditTarget},
            editor::{self, EditItem},
            viewer::{self, ViewerImage},
        },
    },
    i18n::{t, t_args},
    platform::{self, EditTrigger, PanelCommand, Trigger, TriggerSource},
};

/// 弹框里的多行输入框。打开时没拿到编辑态（面板仍不可激活，没有光标）的话，点它再按鼠标触发请求一次。
fn editable_area(input: &TextAreaInput, target: EditTarget) -> Div {
    div()
        .capture_any_mouse_down(move |event: &MouseDownEvent, _, cx| {
            if event.button == MouseButton::Left {
                editing::begin(target, EditTrigger::Mouse, cx);
            }
        })
        .child(TextArea::new(input))
}

/// 1.x 备注框 `maxLength={256}`。
const NOTE_MAX_CHARS: usize = 256;
/// 复制按钮显示“已复制”的时长（1.x 1000 ms）。
const COPIED_FEEDBACK: Duration = Duration::from_secs(1);
/// 挂起的按键最多等这么久；第一页查询正常几十毫秒就落地。
const ACTIVATION_TIMEOUT: Duration = Duration::from_millis(1500);

/// 粘贴键被系统拦截时的提示停留时长：要留出点“重启快贴”的时间。
const PASTE_BLOCKED_TOAST: Duration = Duration::from_secs(10);

/// 失败提示：“{动作}失败：{原因}”（1.x `commands:error`）；粘贴键被系统拦截时说明原因并给出重启按钮。
pub(in crate::clipboard::view) fn error_toast(
    label: &str,
    err: &anyhow::Error,
    window: &mut Window,
    cx: &mut App,
) {
    log::warn!("{label} failed: {err:#}");
    if platform::paste::injection_blocked(err) {
        let toast = Toast::error(t("commands:messages.pasteBlocked"))
            .key("paste-blocked")
            .duration(PASTE_BLOCKED_TOAST)
            .action(t("commands:labels.restartApp"), |_, cx| {
                platform::restart(cx)
            });
        toast::show(toast, window, cx);
        return;
    }
    let message = t_args(
        "commands:error",
        &[("label", &t(label)), ("message", &err.to_string())],
    );
    toast::show(Toast::error(message), window, cx);
}

impl ClipboardList {
    // ------------------------------------------------------------ 筛选与设置

    pub fn filter(&self) -> &ListFilter {
        self.controller.filter()
    }

    /// 换筛选条件：清空、回到顶部、重拉第一页；勾选清空（多选状态保留，1.x 同）。
    pub fn set_filter(&mut self, filter: ListFilter, cx: &mut Context<Self>) {
        if *self.controller.filter() == filter {
            return;
        }

        self.close_preview(cx);
        self.controller.set_filter(filter);
        self.selection.reset();
        self.hovered = None;
        self.reload_from_scratch(cx);
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(super) fn hides_favorites(&self) -> bool {
        self.filter()
            .hides_favorites(self.settings.clipboard.content.hide_favorites_in_all)
    }

    /// core 的设置变了：排序变了整体重载；影响列表载荷的展示设置变了重拉当前范围（1.x 同）。
    pub fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let old = &self.settings.clipboard;
        let new = &settings.clipboard;
        let resort = old.content.sort != new.content.sort
            || old.content.hide_favorites_in_all != new.content.hide_favorites_in_all;
        let rules_changed = old.custom_rules != new.custom_rules;
        let refresh = old.display != new.display || old.sensitive != new.sensitive || rules_changed;
        let preview_changed = old.preview != new.preview || rules_changed;
        // 风格、密度、行数、图片高度变了：行高全变，像换排序一样整体重排。
        let layout = LayoutSpec::from_settings(&new.display);
        let relayout = layout != self.layout;
        self.delete_policy = DeletePolicy::from_settings(&settings.clipboard.content);
        self.settings = settings;
        self.layout = layout;
        if preview_changed {
            self.preview_settings_changed(cx);
        }

        if resort || relayout {
            self.reload_from_scratch(cx);
        } else if refresh && let Some(request) = self.model.reload_current_range() {
            self.fetch(request, cx);
        }
        cx.notify();
    }

    /// 自定义分组（空态文案取分组名）。
    pub fn set_groups(&mut self, groups: Vec<Group>, cx: &mut Context<Self>) {
        self.groups = groups;
        cx.notify();
    }

    /// 按住修饰键：显示数字角标，预览里的快捷信息链接可点。
    pub fn set_key_hints(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.key_hints != on {
            self.key_hints = on;
            self.set_preview_links(on, cx);
            cx.notify();
        }
    }

    pub(super) fn delete_policy(&self) -> DeletePolicy {
        self.delete_policy
    }

    pub(super) fn can_delete(&self, is_favorite: bool, is_pinned: bool) -> bool {
        self.delete_policy
            .can_delete(is_favorite, is_pinned, self.filter().favorites())
    }

    // ------------------------------------------------------------ 提示

    /// 失败提示：“{动作}失败：{原因}”（1.x `commands:error`）。
    pub(super) fn toast_error(
        label: &str,
        err: &anyhow::Error,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        error_toast(label, err, window, cx);
    }

    /// 等宿主动作做完；失败时在面板上提示（宿主动作可能在面板隐藏之后才失败，提示照样留在面板里）。
    pub(super) fn report_host_failure(
        &self,
        label: &'static str,
        task: Task<anyhow::Result<()>>,
        cx: &mut Context<Self>,
    ) {
        let window = self.window;
        cx.spawn(async move |_, cx| {
            if let Err(err) = task.await {
                log::warn!("{label} failed: {err:#}");
                window
                    .update(cx, |_, window, cx| error_toast(label, &err, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    pub(super) fn toast_success(key: &str, window: &mut Window, cx: &mut Context<Self>) {
        toast::show(Toast::success(t(key)), window, cx);
    }

    // ------------------------------------------------------------ 作用于当前项

    /// 当前项（Enter 与各快捷键作用的记录）。
    pub fn active_item(&self) -> Option<Arc<ListItem>> {
        if self.model.total() == 0 {
            return None;
        }

        self.controller.active_item(&self.model).cloned()
    }

    /// 多选快照：交给 core 按完整列表排序并读取原文，不能使用可能脱敏或未加载的卡片正文。
    pub(super) fn checked_selection(&self) -> Option<(Vec<String>, ClipboardItemQuery)> {
        (self.selection.count() >= 2).then(|| {
            (
                self.selection
                    .ids()
                    .iter()
                    .map(|id| id.to_string())
                    .collect(),
                item_query(&self.view_query()),
            )
        })
    }

    /// 粘贴记录或合并后的多选：交给宿主写回、让出前台、注入粘贴键，同时发 `ListIntent::Paste` 通知。
    pub fn paste_item(&mut self, id: Arc<str>, plain: bool, cx: &mut Context<Self>) {
        log::info!("paste requested for {id} (plain: {plain})");
        self.close_preview(cx);
        let task = self
            .host
            .paste(id.clone(), plain, self.checked_selection(), cx);
        self.report_host_failure("commands:labels.paste", task, cx);
        cx.emit(ListIntent::Paste { id, plain });
    }

    /// 右键菜单“粘贴为文件”：把图片原文件作为文件列表粘贴，其余同 [`Self::paste_item`]。
    pub fn paste_image_file(&mut self, id: Arc<str>, cx: &mut Context<Self>) {
        log::info!("image-file paste requested for {id}");
        self.close_preview(cx);
        let task = self.host.paste_image_as_file(id.clone(), cx);
        self.report_host_failure("commands:labels.paste", task, cx);
        cx.emit(ListIntent::Paste { id, plain: false });
    }

    /// Enter / Mod+Enter：粘贴当前项。数据还没追上 core 时先挂起，第一页落地后按那时的第一行执行。
    pub fn paste_active(&mut self, plain: bool, cx: &mut Context<Self>) {
        self.activate(Activation::Paste { plain }, cx);
    }

    /// Mod+数字：粘贴第 N 个可见项（包含置顶行）。多选时不响应（也不显示角标）；数据没追上时同 Enter 挂起。
    pub fn quick_paste(&mut self, key: char, cx: &mut Context<Self>) {
        if self.selection.active() {
            return;
        }

        self.activate(Activation::QuickPaste { key }, cx);
    }

    /// 执行或挂起一个作用于当前项的按键。挂起时确保有一个覆盖全部变化的第一页请求在路上；
    /// 万一一直等不到（查询失败也算等到），[`ACTIVATION_TIMEOUT`] 后按当时的数据执行。
    fn activate(&mut self, activation: Activation, cx: &mut Context<Self>) {
        if self.freshness.fresh() && self.model.loaded_initial() {
            self.run_activation(activation, cx);
            return;
        }

        log::debug!("{activation:?} waits for the first page to catch up");
        self.pending_activation = Some(activation);
        if !self.freshness.awaiting() {
            self.reload(cx);
        }
        self.activation_timeout = Some(cx.spawn(async move |list, cx| {
            cx.background_executor().timer(ACTIVATION_TIMEOUT).await;
            list.update(cx, |list, cx| {
                if let Some(activation) = list.pending_activation.take() {
                    log::warn!("{activation:?} ran on data that has not caught up");
                    list.run_activation(activation, cx);
                }
            })
            .ok();
        }));
    }

    /// 替换范围落地后执行挂起的按键；可见目标尚未加载时先补齐。
    pub(super) fn run_pending_activation(&mut self, cx: &mut Context<Self>) {
        if !self.freshness.fresh() {
            return;
        }
        if let Some(activation) = self.pending_activation {
            let index = match activation {
                Activation::Paste { .. } => self.controller.active_index(&self.model),
                Activation::QuickPaste { key } => {
                    let Some(index) = self.controller.hint_index(key) else {
                        return;
                    };
                    index
                }
            };
            if index < self.model.total() && self.model.get(index).is_none() {
                if let Some(request) = self.model.load_range(index..index + 1) {
                    self.fetch(request, cx);
                }
                return;
            }
            self.pending_activation = None;
            self.activation_timeout = None;
            self.run_activation(activation, cx);
        }
    }

    fn run_activation(&mut self, activation: Activation, cx: &mut Context<Self>) {
        match activation {
            Activation::Paste { plain } => {
                if let Some(item) = self.active_item() {
                    self.paste_item(item.id.clone(), plain, cx);
                }
            }
            Activation::QuickPaste { key } => {
                let Some(item) = self
                    .controller
                    .hint_index(key)
                    .and_then(|index| self.model.get(index))
                    .cloned()
                else {
                    return;
                };
                self.controller.select(&item.id);
                self.paste_item(item.id.clone(), false, cx);
            }
        }
    }

    /// 点卡片下方的快捷信息（1.x `pickSnippet`）：点击设置为复制时只复制这个片段，否则粘贴它。
    pub fn pick_snippet(
        &mut self,
        item: Arc<ListItem>,
        text: Arc<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let copy = matches!(
            self.settings.clipboard.content.auto_paste,
            AutoPaste::SingleClickCopy | AutoPaste::DoubleClickCopy
        );

        if copy {
            self.copy_snippet(&item.id, text, window, cx);
        } else {
            self.paste_snippet(&item.id, text, cx);
        }
    }

    /// 只复制卡片上的这个片段。
    pub fn copy_snippet(
        &mut self,
        id: &Arc<str>,
        text: Arc<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.controller.select(id);
        self.close_preview_of(id, cx);
        let fragment = ClipboardFragment::Snippet {
            text: text.to_string(),
        };
        let task = self.host.copy_fragment(id.clone(), fragment, cx);
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await;
            cx.update(|window, cx| match result {
                Ok(()) => toast::show(Toast::success(t("commands:messages.copied")), window, cx),
                Err(err) => error_toast("commands:labels.copy", &err, window, cx),
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 把卡片上的这个片段粘贴到目标应用。
    pub fn paste_snippet(&mut self, id: &Arc<str>, text: Arc<str>, cx: &mut Context<Self>) {
        self.controller.select(id);
        log::info!("snippet paste requested for {id}");
        self.close_preview(cx);
        let fragment = ClipboardFragment::Snippet {
            text: text.to_string(),
        };
        let task = self.host.paste_fragment(id.clone(), fragment, cx);
        self.report_host_failure("commands:labels.paste", task, cx);
        cx.emit(ListIntent::PasteSnippet {
            id: id.clone(),
            text,
        });
        cx.notify();
    }

    /// 用浏览器 / 邮件客户端打开快捷信息里的链接或邮箱（中键、修饰键 + 单击、右键菜单）；
    /// 别的类型不响应。
    pub fn open_snippet(
        &mut self,
        id: &Arc<str>,
        text: &str,
        kind: SnippetKind,
        cx: &mut Context<Self>,
    ) {
        let Some(url) = card::snippet_open_url(kind, text) else {
            return;
        };
        self.close_preview_of(id, cx);
        cx.open_url(&url);
    }

    /// 指针进出卡片上的一块快捷信息。离开时只清掉自己：换到相邻一块时进入可能先于离开到达。
    pub(in crate::clipboard::view) fn hover_snippet(
        &mut self,
        id: &Arc<str>,
        text: Arc<str>,
        kind: SnippetKind,
        hovered: bool,
    ) {
        if hovered {
            self.hovered_snippet = Some((id.clone(), text, kind));
        } else if self
            .hovered_snippet
            .as_ref()
            .is_some_and(|(current, current_text, _)| current == id && *current_text == text)
        {
            self.hovered_snippet = None;
        }
    }

    // ------------------------------------------------------------ 大图

    /// 在大图预览窗里看这张图：已加载的图片记录按列表顺序做快照，面板按点外部处理（固定时留着）。
    pub fn view_image(&mut self, id: Arc<str>, cx: &mut Context<Self>) {
        let images: Vec<ViewerImage> = self
            .model
            .cached_items()
            .into_iter()
            .filter(|item| item.kind == ItemKind::Image)
            .map(|item| ViewerImage {
                id: item.id.clone(),
                size: item.width.zip(item.height),
            })
            .collect();
        let Some(index) = images.iter().position(|image| image.id == id) else {
            return;
        };

        self.close_preview(cx);
        editing::request_panel(
            cx,
            PanelCommand::Hide(Trigger::now(TriggerSource::OutsideClick)),
        );
        if let Err(err) = viewer::open(self.source.clone(), self.host.clone(), images, index, cx) {
            log::error!("could not open the image viewer: {err:#}");
        }
    }

    // ------------------------------------------------------------ 复制

    /// 写回剪贴板（宿主按设置决定复制后是否隐藏面板）。`feedback` 是点的那个快捷动作按钮，
    /// 成功后显示 1 秒“已复制”。
    pub fn copy(
        &mut self,
        id: Arc<str>,
        plain: bool,
        feedback: Option<QuickAction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_preview_of(&id, cx);
        let task = self
            .host
            .copy(id.clone(), plain, self.checked_selection(), cx);

        cx.spawn_in(window, async move |list, cx| {
            let result = task.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(()) => {
                    Self::toast_success("commands:messages.copied", window, cx);
                    if let Some(action) = feedback {
                        list.show_copied(id, action, cx);
                    }
                }
                Err(err) => Self::toast_error("commands:labels.copy", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 复制图片里识别出的文字（右键菜单「复制图中文字」）。
    pub fn copy_image_text(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_preview_of(&id, cx);
        let task = self.host.copy_image_text(id, cx);

        cx.spawn_in(window, async move |list, cx| {
            let result = task.await;
            list.update_in(cx, |_, window, cx| match result {
                Ok(()) => Self::toast_success("commands:messages.copied", window, cx),
                Err(err) => Self::toast_error("commands:labels.copyImageText", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    fn show_copied(&mut self, id: Arc<str>, action: QuickAction, cx: &mut Context<Self>) {
        self.copied = Some((id, action));
        self.copied_reset = Some(cx.spawn(async move |list, cx| {
            cx.background_executor().timer(COPIED_FEEDBACK).await;
            list.update(cx, |list, cx| {
                list.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ------------------------------------------------------------ 收藏、置顶

    /// 翻转收藏：收藏视图里取消收藏、隐藏收藏的「全部」里收藏的记录立即移出，其余只更新标记
    /// （1.x `handleFavoriteToggled`）。
    pub fn toggle_favorite(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.model.find(&id).map(|item| item.is_favorite) else {
            return;
        };
        let future = self.source.toggle_favorite(id.clone());

        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(favorite) => {
                    let key = if favorite {
                        "commands:messages.favoriteAdded"
                    } else {
                        "commands:messages.favoriteRemoved"
                    };
                    Self::toast_success(key, window, cx);
                    if (list.filter().favorites() && !favorite)
                        || (list.hides_favorites() && favorite)
                    {
                        list.remove_item(&id, cx);
                    } else {
                        list.patch_item(&id, |item| item.is_favorite = favorite, cx);
                    }
                    cx.emit(ListIntent::Changed);
                }
                Err(err) => {
                    let label = if current {
                        "commands:labels.cancelFavorite"
                    } else {
                        "commands:labels.toggleFavorite"
                    };
                    Self::toast_error(label, &err, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 翻转置顶：置顶影响排序，成功后重拉当前范围，继续信任 core 的顺序（1.x `handleTogglePinned`）。
    pub fn toggle_pinned(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.model.find(&id).map(|item| item.is_pinned) else {
            return;
        };
        let future = self.source.toggle_pinned(id.clone());

        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(pinned) => {
                    let key = if pinned {
                        "commands:messages.itemPinned"
                    } else {
                        "commands:messages.itemUnpinned"
                    };
                    Self::toast_success(key, window, cx);
                    if let Some(request) = list.model.reload_current_range() {
                        list.fetch(request, cx);
                    }
                    cx.emit(ListIntent::Changed);
                }
                Err(err) => {
                    let label = if current {
                        "commands:labels.unpinItem"
                    } else {
                        "commands:labels.pinItem"
                    };
                    Self::toast_error(label, &err, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------ 备注

    /// 打开备注框（1.x `NoteModal`）：多行输入 3–6 行，保存时 core 去首尾空白、空串清空。
    /// 备注框要打字，先请求编辑态，`EditingStarted` 后聚焦输入框（见列表的面板事件处理）。
    pub fn edit_note(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.model.find(&id).cloned() else {
            return;
        };
        self.close_preview_of(&id, cx);
        if self.note.is_some() {
            return;
        }

        let input = TextAreaInput::new(t("clipboard:note.placeholder"), 3, 6, window, cx);
        input.set_value(
            item.note.as_deref().unwrap_or_default().to_owned(),
            window,
            cx,
        );
        let content = input.clone();
        let answer = form_dialog(
            DialogSpec::new(t("clipboard:note.title"))
                .ok_text(t("common:actions.save"))
                .cancel_text(t("common:actions.cancel")),
            move |_, _| editable_area(&content, EditTarget::Note).into_any_element(),
            window,
            cx,
        );
        self.note = Some(NoteEdit { id, input });
        editing::begin(EditTarget::Note, EditTrigger::Keyboard, cx);

        cx.spawn_in(window, async move |list, cx| {
            let save = answer.await.unwrap_or(false);
            list.update_in(cx, |list, window, cx| list.finish_note(save, window, cx))
                .ok();
        })
        .detach();
    }

    /// 备注框关了：退出编辑态、焦点回到列表；点了保存就写入。
    pub(in crate::clipboard::view) fn finish_note(
        &mut self,
        save: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(note) = self.note.take() else {
            return;
        };
        if editing::target(cx) == Some(EditTarget::Note) {
            editing::end(cx);
        }
        window.focus(&self.focus, cx);
        if !save {
            return;
        }

        let value: String = note.input.value(cx).chars().take(NOTE_MAX_CHARS).collect();
        let id = note.id;
        let future = self.source.update_note(id.clone(), Some(value));

        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(saved) => {
                    let key = if saved.auto_favorited {
                        "commands:messages.noteSavedAndFavorited"
                    } else {
                        "commands:messages.noteSaved"
                    };
                    Self::toast_success(key, window, cx);
                    if saved.auto_favorited && list.hides_favorites() {
                        list.remove_item(&id, cx);
                        cx.emit(ListIntent::Changed);
                        return;
                    }
                    list.patch_item(
                        &id,
                        |item| {
                            item.note = saved.note;
                            item.is_favorite |= saved.auto_favorited;
                        },
                        cx,
                    );
                    cx.emit(ListIntent::Changed);
                }
                Err(err) => Self::toast_error("commands:labels.saveNote", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 异步读取完整正文，在独立的编辑窗里打开；面板按点外部处理（固定时留着）。
    pub fn edit_content(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.model.find(&id).cloned() else {
            return;
        };
        if item.kind != ItemKind::Text {
            return;
        }
        self.close_preview_of(&id, cx);
        let rich = matches!(item.sub_kind, Some(SubKind::Html | SubKind::Rtf));
        let future = self.source.text_content(id.clone());
        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(original) => {
                    editing::request_panel(
                        cx,
                        PanelCommand::Hide(Trigger::now(TriggerSource::OutsideClick)),
                    );
                    let item = EditItem { id, original, rich };
                    let list_entity = cx.entity().downgrade();
                    if let Err(err) = editor::open(item, list.source.clone(), list_entity, cx) {
                        log::error!("could not open the content editor: {err:#}");
                    }
                }
                Err(err) => Self::toast_error("clipboard:menu.editContent", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 编辑窗保存成功：就地替换列表载荷。
    pub(in crate::clipboard::view) fn content_saved(
        &mut self,
        saved: ListItem,
        cx: &mut Context<Self>,
    ) {
        let id = saved.id.clone();
        self.patch_item(&id, |item| *item = saved, cx);
        cx.emit(ListIntent::Changed);
    }

    /// 恢复编辑前的原文，刷新方式同保存编辑。
    pub fn restore_original(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_preview_of(&id, cx);
        let future = self.source.restore_original_content(id.clone());
        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(restored) => {
                    list.patch_item(&id, |item| *item = restored, cx);
                    cx.emit(ListIntent::Changed);
                }
                Err(err) => Self::toast_error("clipboard:menu.restoreOriginal", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 自测用：备注框里的输入框。
    pub fn note_input(&self) -> Option<TextAreaInput> {
        self.note.as_ref().map(|note| note.input.clone())
    }

    // ------------------------------------------------------------ 删除

    /// 删除一条：受保护的不响应；按设置二次确认（1.x `deleteClipboardItem`）。
    pub fn delete(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.model.find(&id).cloned() else {
            return;
        };
        self.close_preview_of(&id, cx);
        if !self.can_delete(item.is_favorite, item.is_pinned) {
            return;
        }
        if !self
            .delete_policy()
            .needs_confirm(item.is_favorite, item.is_pinned)
        {
            self.delete_now(id, window, cx);
            return;
        }

        let answer = confirm(
            ConfirmSpec::new(t("commands:deleteConfirm.title"))
                .content(t("commands:deleteConfirm.content"))
                .ok_text(t("common:actions.delete"))
                .cancel_text(t("common:actions.cancel"))
                .danger(),
            window,
            cx,
        );
        cx.spawn_in(window, async move |list, cx| {
            if answer.await.unwrap_or(false) {
                list.update_in(cx, |list, window, cx| list.delete_now(id, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn delete_now(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        let future = self.source.delete(id.clone());

        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(()) => {
                    Self::toast_success("commands:messages.deleted", window, cx);
                    list.remove_item(&id, cx);
                    cx.emit(ListIntent::Changed);
                }
                Err(err) => Self::toast_error("commands:labels.delete", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    // ------------------------------------------------------------ 打开、拆词

    /// 打开链接、发邮件或在文件管理器中定位（1.x `openClipboardItemLink` / `revealClipboardItem`）。
    pub fn open(
        &mut self,
        id: Arc<str>,
        target: OpenTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_preview_of(&id, cx);
        let future = self.source.open_target(id, target);

        cx.spawn_in(window, async move |_, cx| {
            let result = future.await;
            cx.update(|window, cx| match result {
                Ok(Some(destination)) => match target {
                    OpenTarget::Link | OpenTarget::Email => cx.open_url(&destination),
                    OpenTarget::Reveal => cx.reveal_path(Path::new(&destination)),
                },
                Ok(None) => {}
                Err(err) => {
                    let label = match target {
                        OpenTarget::Reveal => "commands:labels.reveal",
                        OpenTarget::Link | OpenTarget::Email => "commands:labels.openLink",
                    };
                    log::warn!("{label} failed: {err:#}");
                    let message = t_args(
                        "commands:error",
                        &[("label", &t(label)), ("message", &err.to_string())],
                    );
                    toast::show(Toast::error(message), window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 拆词：core 没给出拆词动作（非文本、脱敏展示的敏感内容）时不响应。发意图，由面板交给
    /// [`Self::split_words`] 打开（或退出）选词预览。
    pub fn split(&mut self, item: &ListItem, cx: &mut Context<Self>) {
        if !is_available(QuickAction::SplitWords, item) {
            return;
        }

        self.controller.select(&item.id);
        log::info!("split words requested for {}", item.id);
        cx.emit(ListIntent::SplitWords {
            id: item.id.clone(),
        });
    }

    // ------------------------------------------------------------ 快捷动作

    /// 卡片上的悬停快捷动作（1.x `handleCardQuickAction`）。
    pub(super) fn run_quick_action(
        &mut self,
        item: Arc<ListItem>,
        action: QuickAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if is_copy(action)
            && self
                .copied
                .as_ref()
                .is_some_and(|(id, copied)| *id == item.id && *copied == action)
        {
            return;
        }

        self.controller.select(&item.id);
        let id = item.id.clone();
        match action {
            QuickAction::Paste => self.paste_item(id, false, cx),
            QuickAction::PastePlain | QuickAction::PastePath => self.paste_item(id, true, cx),
            QuickAction::Copy => self.copy(id, false, Some(action), window, cx),
            QuickAction::CopyPlain => self.copy(id, true, Some(action), window, cx),
            QuickAction::SplitWords => self.split(&item, cx),
            QuickAction::OpenLink => self.open(id, OpenTarget::Link, window, cx),
            QuickAction::SendEmail => self.open(id, OpenTarget::Email, window, cx),
            QuickAction::Reveal => self.open(id, OpenTarget::Reveal, window, cx),
            QuickAction::Note => self.edit_note(id, window, cx),
            QuickAction::Star => self.toggle_favorite(id, window, cx),
            QuickAction::PinItem => self.toggle_pinned(id, window, cx),
            QuickAction::Delete => self.delete(id, window, cx),
        }
        cx.notify();
    }

    /// 卡片按住拖过系统阈值：把这条记录拖到别的应用（1.x `startDragClipboardItem`）；多选时按住的
    /// 是已勾选的卡片就整组一起拖出。拖出期间面板不激活；失败（文件已不存在等）时提示原因。
    pub(super) fn drag_out(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("drag-out requested for {id}");
        self.close_preview(cx);
        let selection = self
            .selection
            .is_checked(&id)
            .then(|| self.checked_selection())
            .flatten();
        let task = self.host.drag_out(id.clone(), selection, window, cx);
        self.report_host_failure("commands:labels.drag", task, cx);
        cx.emit(ListIntent::DragOut { id });
    }

    /// 按住修饰键点了链接、邮箱卡片的正文（1.x `openItemLink`）：关掉预览，打开链接或写邮件。
    pub fn open_link(&mut self, item: &ListItem, window: &mut Window, cx: &mut Context<Self>) {
        self.close_preview(cx);
        let target = if item.link_sub_kind() == Some(SubKind::Email) {
            OpenTarget::Email
        } else {
            OpenTarget::Link
        };
        self.open(item.id.clone(), target, window, cx);
    }

    /// Mod+O：按 core 声明的动作打开当前项（1.x `getOpenClipboardAction`）。
    pub fn open_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.active_item() else {
            return;
        };
        if let Some(target) = open_target(&item) {
            self.open(item.id.clone(), target, window, cx);
        }
    }
}
