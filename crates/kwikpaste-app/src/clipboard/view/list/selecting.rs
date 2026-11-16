//! 多选（与 1.3.8 一致）：点击勾选、Shift 连选、Mod+A 全选 / 取消全选、批量删除；另有合并粘贴、
//! 合并复制和整组拖出（见 `ops.rs` 的 `checked_selection`）。不在多选时按住 Ctrl/Cmd 或 Shift
//! 点卡片直接进入多选（资源管理器的习惯）。
//!
//! 收藏、置顶记录也能勾选；批量删除时跳过受删除保护的，一条都删不了时提示可以去设置里放开
//! （同一条提示只留一份）。连选两端都已加载时就地取中间的行，否则向数据源要当前视图的完整顺序
//! 再截取；全选也是。请求在路上时换了视图（换代）就作废。

use std::sync::Arc;

use gpui::{Context, Window};
use kwikpaste_ui::{
    ConfirmSpec, confirm,
    toast::{self, Toast},
};

use super::ClipboardList;
use crate::{
    clipboard::{
        model::{
            item::{ItemRef, ListItem},
            selection::{loaded_range, range_in},
        },
        source::ListQuery,
    },
    i18n::{t, t_count},
};

/// 受保护提示的 key：连续碰到时原地刷新，不叠出多条（1.x `PROTECTED_HINT_KEY`）。
const PROTECTED_HINT_KEY: &str = "clipboard-selection-protected";

impl ClipboardList {
    pub fn selecting(&self) -> bool {
        self.selection.active()
    }

    pub fn checked_count(&self) -> usize {
        self.selection.count()
    }

    pub fn enter_selection(&mut self, cx: &mut Context<Self>) {
        self.selection.enter();
        cx.notify();
    }

    pub fn exit_selection(&mut self, cx: &mut Context<Self>) {
        if self.selection.active() {
            self.selection.exit();
            cx.notify();
        }
    }

    fn show_protected_hint(window: &mut Window, cx: &mut Context<Self>) {
        toast::show(
            Toast::info(t("clipboard:selection.protected")).key(PROTECTED_HINT_KEY),
            window,
            cx,
        );
    }

    fn deletable(&self) -> Vec<Arc<str>> {
        self.selection
            .refs()
            .filter(|item| self.can_delete(item.is_favorite, item.is_pinned))
            .map(|item| item.id.clone())
            .collect()
    }

    /// 当前视图的查询（全选、跨页连选向数据源要完整顺序时用）。
    pub(super) fn view_query(&self) -> ListQuery {
        ListQuery {
            offset: 0,
            limit: 0,
            filter: self.filter().clone(),
            sort: self.settings.clipboard.content.sort,
            hide_favorites: self.hides_favorites(),
        }
    }

    /// 不在多选时按住修饰键点卡片：直接进入多选，不触发拖出、排序或单击动作。Ctrl/Cmd 只勾被点的
    /// 那条：当前项跟着指针走，点下去时就是它，更早点过的那条已不再高亮，顺带勾上会把看不见的一条
    /// 混进合并粘贴。Shift 从最近一次点击或键盘选中的那条连选到被点的那条，没有就只勾被点的。
    /// 指针移进卡片时排上的悬停预览一并取消：多选时不预览。
    pub(super) fn begin_selection(
        &mut self,
        item: Arc<ListItem>,
        range: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_preview(cx);
        let anchor = range.then(|| self.controller.picked().cloned()).flatten();
        self.selection.begin(anchor);
        self.controller.select(&item.id);
        if range {
            self.check_range(item, window, cx);
        } else {
            self.toggle_checked(&item, cx);
        }
    }

    /// 切换一条的勾选，并把它记为连选起点。
    pub fn toggle_checked(&mut self, item: &ListItem, cx: &mut Context<Self>) {
        self.selection.toggle(ItemRef::of(item));
        cx.notify();
    }

    /// Shift 点击：勾上起点到目标之间（含两端）的记录，起点不变。
    pub fn check_range(
        &mut self,
        target: Arc<ListItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(anchor) = self
            .selection
            .anchor()
            .filter(|anchor| **anchor != target.id)
            .cloned()
        else {
            self.toggle_checked(&target, cx);
            return;
        };

        if let Some(range) = loaded_range(&self.model, &anchor, &target.id) {
            self.selection.add(range);
            cx.notify();
            return;
        }

        let token = self.selection.token();
        let future = self.source.item_refs(self.view_query());
        self.selection.set_busy(true);
        cx.notify();
        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| {
                list.selection.set_busy(false);
                cx.notify();
                if token != list.selection.token() {
                    return;
                }
                match result {
                    Ok(refs) => match range_in(&refs, &anchor, &target.id) {
                        Some(range) => list.selection.add(range),
                        None => list.toggle_checked(&target, cx),
                    },
                    Err(err) => {
                        Self::toast_error("commands:labels.selectClipboardItems", &err, window, cx)
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Mod+A / 全选按钮：全选当前视图的记录，已全选时全部取消；还没进入多选时顺带进入。
    pub fn toggle_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selection.enter();
        if self.selection.all_checked() {
            self.selection.reset();
            cx.notify();
            return;
        }

        let token = self.selection.token();
        let future = self.source.item_refs(self.view_query());
        self.selection.set_busy(true);
        cx.notify();
        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| {
                list.selection.set_busy(false);
                cx.notify();
                if token != list.selection.token() {
                    return;
                }
                match result {
                    Ok(refs) if !refs.is_empty() => list.selection.check_all(refs),
                    Ok(_) => {}
                    Err(err) => {
                        Self::toast_error("commands:labels.selectClipboardItems", &err, window, cx)
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// 合并粘贴已勾选的记录（文本合成一段，图片 / 文件合成一份文件列表）；只勾了一条就粘贴那一条。
    pub fn paste_checked(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selection.ids().into_iter().next() {
            self.paste_item(id, false, cx);
        }
    }

    /// 多选时的 Mod+C：合并复制已勾选的记录，规则同 [`Self::paste_checked`]。
    pub fn copy_checked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selection.ids().into_iter().next() {
            self.copy(id, false, None, window, cx);
        }
    }

    /// 删除已勾选的记录（跳过受删除保护的）：总是二次确认；成功后退出多选，按新数据刷新列表；
    /// 取消或失败时保留勾选。
    pub fn delete_checked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.count() == 0 || self.selection.busy() {
            return;
        }
        let ids = self.deletable();
        if ids.is_empty() {
            Self::show_protected_hint(window, cx);
            return;
        }

        let count = i64::try_from(ids.len()).unwrap_or(i64::MAX);
        let answer = confirm(
            ConfirmSpec::new(t("commands:batchDeleteConfirm.title"))
                .content(t_count("commands:batchDeleteConfirm.content", count, &[]))
                .ok_text(t("common:actions.delete"))
                .cancel_text(t("common:actions.cancel"))
                .danger(),
            window,
            cx,
        );
        cx.spawn_in(window, async move |list, cx| {
            if !answer.await.unwrap_or(false) {
                return;
            }
            list.update_in(cx, |list, window, cx| {
                list.delete_checked_now(ids, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn delete_checked_now(
        &mut self,
        ids: Vec<Arc<str>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let future = self.source.delete_many(
            ids,
            self.can_delete(true, false),
            self.can_delete(false, true),
        );
        self.selection.set_busy(true);
        cx.notify();

        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| {
                list.selection.set_busy(false);
                match result {
                    Ok(removed) => {
                        let removed = i64::try_from(removed).unwrap_or(i64::MAX);
                        toast::show(
                            Toast::success(t_count("commands:messages.itemsDeleted", removed, &[])),
                            window,
                            cx,
                        );
                        list.selection.exit();
                        list.controller.clear_selection();
                        list.refresh_after_removal(cx);
                        cx.emit(super::ListIntent::Changed);
                    }
                    Err(err) => Self::toast_error("commands:labels.delete", &err, window, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 批量删除后刷新：先用第一页替换整份缓存，第一页落地后再补视图范围（1.x `refreshAfterRemoval`）。
    fn refresh_after_removal(&mut self, cx: &mut Context<Self>) {
        let request = self.model.refresh_after_removal();
        self.refetch_view_after_first_page = true;
        self.fetch(request, cx);
    }
}
