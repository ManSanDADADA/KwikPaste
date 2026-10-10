//! 列表上的记录操作：筛选、复制、收藏、置顶、备注、删除、打开、粘贴（1.x `List.tsx` 的各个 handler
//! 与 `commands/index.ts` 的提示）。
//!
//! core 的记录操作不发事件：成功后就地改列表（收藏视图里取消收藏、删除时移除，其余合并标记），
//! 置顶影响排序，改完重拉当前范围。成功、失败都用顶部提示告知（与 1.x 的 `getMessageApi()` 一致）。

use std::{path::Path, sync::Arc, time::Duration};

use gpui::prelude::FluentBuilder as _;
use gpui::{App, Context, IntoElement as _, ParentElement as _, Styled as _, Task, Window, div};
use kwikpaste_core::{
    clipboard::ClipboardFragment,
    settings::{AutoPaste, Settings},
};
use kwikpaste_ui::{
    ConfirmSpec, DialogSpec, TextArea, TextAreaInput, confirm, form_dialog, theme,
    toast::{self, Toast},
};

use super::{ClipboardList, ContentEdit, ListIntent, NoteEdit};
use crate::{
    clipboard::{
        model::{
            actions::{DeletePolicy, OpenTarget, QuickAction, is_available, is_copy, open_target},
            filter::ListFilter,
            freshness::Activation,
            item::{ListItem, SubKind},
            layout::LayoutSpec,
        },
        source::Group,
        view::editing::{self, EditTarget},
    },
    i18n::{t, t_args},
    platform::EditTrigger,
};

/// 1.x 备注框 `maxLength={256}`。
const NOTE_MAX_CHARS: usize = 256;
/// 复制按钮显示“已复制”的时长（1.x 1000 ms）。
const COPIED_FEEDBACK: Duration = Duration::from_secs(1);
/// 挂起的按键最多等这么久；第一页查询正常几十毫秒就落地。
const ACTIVATION_TIMEOUT: Duration = Duration::from_millis(1500);

/// 失败提示：“{动作}失败：{原因}”（1.x `commands:error`）。
pub(super) fn error_toast(label: &str, err: &anyhow::Error, window: &mut Window, cx: &mut App) {
    log::warn!("{label} failed: {err:#}");
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

    /// core 的设置变了：排序变了整体重载；影响列表载荷的展示设置变了重拉当前范围（1.x 同）。
    pub fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let old = &self.settings.clipboard;
        let new = &settings.clipboard;
        let resort = old.content.sort != new.content.sort;
        let refresh = old.display != new.display || old.sensitive != new.sensitive;
        let preview_changed = old.preview != new.preview;
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

    /// 按住修饰键：显示数字角标。
    pub fn set_key_hints(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.key_hints != on {
            self.key_hints = on;
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

    /// 粘贴一条记录：交给宿主（写回剪贴板、让出前台、注入粘贴键），同时发 `ListIntent::Paste` 通知。
    pub fn paste_item(&mut self, id: Arc<str>, plain: bool, cx: &mut Context<Self>) {
        log::info!("paste requested for {id} (plain: {plain})");
        self.close_preview(cx);
        let task = self.host.paste(id.clone(), plain, cx);
        self.report_host_failure("commands:labels.paste", task, cx);
        cx.emit(ListIntent::Paste { id, plain });
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

    /// 数据追上之后执行挂起的按键（第一页落地时调用）。
    pub(super) fn run_pending_activation(&mut self, cx: &mut Context<Self>) {
        if !self.freshness.fresh() {
            return;
        }
        if let Some(activation) = self.pending_activation.take() {
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
        self.controller.select(&item.id);
        let fragment = ClipboardFragment::Snippet {
            text: text.to_string(),
        };
        let copy = matches!(
            self.settings.clipboard.content.auto_paste,
            AutoPaste::SingleClickCopy | AutoPaste::DoubleClickCopy
        );

        if copy {
            self.close_preview_of(&item.id, cx);
            let task = self.host.copy_fragment(item.id.clone(), fragment, cx);
            cx.spawn_in(window, async move |_, cx| {
                let result = task.await;
                cx.update(|window, cx| match result {
                    Ok(()) => {
                        toast::show(Toast::success(t("commands:messages.copied")), window, cx)
                    }
                    Err(err) => error_toast("commands:labels.copy", &err, window, cx),
                })
                .ok();
            })
            .detach();
        } else {
            log::info!("snippet paste requested for {}", item.id);
            self.close_preview(cx);
            let task = self.host.paste_fragment(item.id.clone(), fragment, cx);
            self.report_host_failure("commands:labels.paste", task, cx);
            cx.emit(ListIntent::PasteSnippet {
                id: item.id.clone(),
                text,
            });
        }
        cx.notify();
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
        let task = self.host.copy(id.clone(), plain, cx);

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

    /// 翻转收藏：收藏视图里取消收藏的记录立即移出，其余只更新标记（1.x `handleFavoriteToggled`）。
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
                    if list.filter().favorites() && !favorite {
                        list.remove_item(&id, cx);
                    } else {
                        list.patch_item(&id, |item| item.is_favorite = favorite, cx);
                    }
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
        if self.note.is_some() || self.content_edit.is_some() || self.content_loading.is_some() {
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
            move |_, _| TextArea::new(&content).into_any_element(),
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
                    list.patch_item(
                        &id,
                        |item| {
                            item.note = saved.note;
                            item.is_favorite |= saved.auto_favorited;
                        },
                        cx,
                    );
                }
                Err(err) => Self::toast_error("commands:labels.saveNote", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 异步读取完整正文，再按备注框的前台编辑流程打开内容编辑器。
    pub fn edit_content(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.model.find(&id).cloned() else {
            return;
        };
        if item.kind != crate::clipboard::model::item::ItemKind::Text
            || self.note.is_some()
            || self.content_edit.is_some()
            || self.content_loading.is_some()
        {
            return;
        }
        self.close_preview_of(&id, cx);
        let future = self.source.text_content(id.clone());
        self.content_loading = Some(cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| {
                list.content_loading = None;
                match result {
                    Ok(original) => {
                        let input = TextAreaInput::new("", 6, 16, window, cx);
                        input.set_value(original.clone(), window, cx);
                        let content = input.clone();
                        let rich = matches!(item.sub_kind, Some(SubKind::Html | SubKind::Rtf));
                        let answer = form_dialog(
                            DialogSpec::new(t("clipboard:menu.editContent"))
                                .ok_text(t("common:actions.save"))
                                .cancel_text(t("common:actions.cancel")),
                            move |_, cx| {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(TextArea::new(&content))
                                    .when(rich, |el| {
                                        el.child(
                                            div()
                                                .text_sm()
                                                .text_color(theme::semantic(cx).text.secondary)
                                                .child(t("clipboard:content.richWarning")),
                                        )
                                    })
                                    .into_any_element()
                            },
                            window,
                            cx,
                        );
                        list.content_edit = Some(ContentEdit {
                            id,
                            input,
                            original,
                        });
                        editing::begin(EditTarget::Content, EditTrigger::Keyboard, cx);
                        cx.spawn_in(window, async move |list, cx| {
                            let save = answer.await.unwrap_or(false);
                            list.update_in(cx, |list, window, cx| {
                                list.finish_content(save, window, cx)
                            })
                            .ok();
                        })
                        .detach();
                    }
                    Err(err) => Self::toast_error("clipboard:menu.editContent", &err, window, cx),
                }
            })
            .ok();
        }));
    }

    /// 内容框关闭后退出编辑态；空白报错，原样保存，成功后就地替换列表载荷。
    pub(in crate::clipboard::view) fn finish_content(
        &mut self,
        save: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.content_edit.take() else {
            return;
        };
        if editing::target(cx) == Some(EditTarget::Content) {
            editing::end(cx);
        }
        window.focus(&self.focus, cx);
        if !save {
            return;
        }
        let value = edit.input.value(cx).to_string();
        if value.trim().is_empty() {
            Self::toast_error(
                "clipboard:menu.editContent",
                &anyhow::anyhow!(t("clipboard:content.empty")),
                window,
                cx,
            );
            return;
        }
        if value == edit.original {
            return;
        }
        let id = edit.id;
        let future = self.source.update_text_content(id.clone(), value);
        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| match result {
                Ok(saved) => {
                    list.patch_item(&id, |item| *item = saved, cx);
                    Self::toast_success("commands:messages.noteSaved", window, cx);
                }
                Err(err) => Self::toast_error("clipboard:menu.editContent", &err, window, cx),
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

    /// 拆词：core 没给出拆词动作（非文本、脱敏展示的敏感内容）时不响应。拆词面板还没做，
    /// 这里先发意图。
    pub fn split(&mut self, item: &ListItem, cx: &mut Context<Self>) {
        if !is_available(QuickAction::SplitWords, item) {
            return;
        }

        self.controller.select(&item.id);
        self.close_preview(cx);
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

    /// 卡片按住拖过系统阈值：把这条记录拖到别的应用（1.x `startDragClipboardItem`）。拖出期间面板
    /// 不激活；失败（文件已不存在等）时提示原因。
    pub(super) fn drag_out(&mut self, id: Arc<str>, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("drag-out requested for {id}");
        self.close_preview(cx);
        let task = self.host.drag_out(id.clone(), window, cx);
        self.report_host_failure("commands:labels.drag", task, cx);
        cx.emit(ListIntent::DragOut { id });
    }

    /// 按住修饰键点了链接、邮箱卡片的正文（1.x `openItemLink`）：关掉预览，打开链接或写邮件。
    pub fn open_link(&mut self, item: &ListItem, window: &mut Window, cx: &mut Context<Self>) {
        self.close_preview(cx);
        let target = if item.sub_kind == Some(SubKind::Email) {
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
