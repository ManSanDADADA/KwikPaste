//! 多选（1.3.8 起的 `clipboardSelection` 与 `List.tsx` 的勾选逻辑）：点击勾选、Shift 连选、
//! Mod+A 全选 / 取消全选、批量删除、合并粘贴与拖出。
//!
//! 每次清空都换代（`token`）：还在路上的全选、连选结果回来时代号不对就作废。受删除保护的记录
//! 也能勾选（合并粘贴、拖出要用），批量删除时由调用方按 [`super::actions::DeletePolicy`] 跳过。

use std::{collections::HashMap, sync::Arc};

use super::{item::ItemRef, list_model::ListModel};

#[derive(Debug, Default)]
pub struct Selection {
    active: bool,
    checked: HashMap<Arc<str>, ItemRef>,
    /// Shift 连选的起点：最近一次单独勾选或取消的记录。
    anchor: Option<Arc<str>>,
    /// 当前视图的记录已全部勾上：全选按钮切成取消全选。
    all_checked: bool,
    token: u64,
    /// 正在向数据源要全选 / 连选范围，或正在删除。
    busy: bool,
}

impl Selection {
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn enter(&mut self) {
        self.active = true;
    }

    /// 不在多选时按住修饰键点卡片：进入多选，以 `anchor` 为连选起点（还没勾任何一条）。
    pub fn begin(&mut self, anchor: Option<Arc<str>>) {
        self.active = true;
        self.anchor = anchor;
    }

    /// 退出多选，同时清空勾选。
    pub fn exit(&mut self) {
        self.active = false;
        self.reset();
    }

    /// 清空勾选并换代（换了视图、清理、导入之后）。多选状态本身保留。
    pub fn reset(&mut self) {
        self.token += 1;
        self.checked.clear();
        self.anchor = None;
        self.all_checked = false;
    }

    pub fn token(&self) -> u64 {
        self.token
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    pub fn all_checked(&self) -> bool {
        self.all_checked
    }

    pub fn count(&self) -> usize {
        self.checked.len()
    }

    pub fn is_checked(&self, id: &str) -> bool {
        self.checked.contains_key(id)
    }

    pub fn ids(&self) -> Vec<Arc<str>> {
        self.checked.keys().cloned().collect()
    }

    pub fn refs(&self) -> impl Iterator<Item = &ItemRef> {
        self.checked.values()
    }

    pub fn anchor(&self) -> Option<&Arc<str>> {
        self.anchor.as_ref()
    }

    /// 切换一条的勾选，并把它记为连选起点。
    pub fn toggle(&mut self, item: ItemRef) {
        self.anchor = Some(item.id.clone());
        self.all_checked = false;
        if self.checked.remove(&item.id).is_none() {
            self.checked.insert(item.id.clone(), item);
        }
    }

    /// Shift 连选：勾上一段（起点不变）。
    pub fn add(&mut self, items: impl IntoIterator<Item = ItemRef>) {
        self.all_checked = false;
        self.checked
            .extend(items.into_iter().map(|item| (item.id.clone(), item)));
    }

    /// 全选：换成这一批，起点清空。
    pub fn check_all(&mut self, items: impl IntoIterator<Item = ItemRef>) {
        self.anchor = None;
        self.checked = items
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect();
        self.all_checked = true;
    }

    /// 某条记录不在视图里了（删除、移出），从勾选里拿掉。
    pub fn forget(&mut self, id: &str) {
        self.checked.remove(id);
        if self.anchor.as_deref() == Some(id) {
            self.anchor = None;
        }
    }
}

/// 两条记录之间（含两端）的已加载条目；有一端或中间某行没加载时返回 `None`（1.x `getLoadedRangeRefs`）。
pub fn loaded_range(model: &ListModel, from: &str, to: &str) -> Option<Vec<ItemRef>> {
    let from = model.index_of(from)?;
    let to = model.index_of(to)?;

    (from.min(to)..=from.max(to))
        .map(|index| model.get(index).map(|item| ItemRef::of(item)))
        .collect()
}

/// 从当前视图的完整顺序里截出两条记录之间（含两端）的部分（1.x `fetchRangeRefs`）。
pub fn range_in(refs: &[ItemRef], from: &str, to: &str) -> Option<Vec<ItemRef>> {
    let from = refs.iter().position(|item| &*item.id == from)?;
    let to = refs.iter().position(|item| &*item.id == to)?;

    refs.get(from.min(to)..=from.max(to))
        .map(<[ItemRef]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(ids: &[&str]) -> Vec<ItemRef> {
        ids.iter()
            .map(|id| ItemRef {
                id: (*id).into(),
                is_favorite: false,
                is_pinned: false,
            })
            .collect()
    }

    #[test]
    fn toggling_sets_the_anchor_and_clears_select_all() {
        let mut selection = Selection::default();
        selection.enter();
        selection.check_all(refs(&["a", "b"]));
        assert!(selection.all_checked());

        selection.toggle(refs(&["a"]).remove(0));
        assert!(!selection.all_checked());
        assert!(!selection.is_checked("a"));
        assert_eq!(selection.anchor().map(|id| &**id), Some("a"));

        selection.toggle(refs(&["a"]).remove(0));
        assert!(selection.is_checked("a"));
        assert_eq!(selection.count(), 2);
    }

    #[test]
    fn reset_bumps_the_token_and_exit_leaves() {
        let mut selection = Selection::default();
        selection.enter();
        selection.toggle(refs(&["a"]).remove(0));
        let token = selection.token();

        selection.reset();
        assert_ne!(selection.token(), token, "late answers are dropped");
        assert_eq!(selection.count(), 0);
        assert!(selection.active(), "reset keeps the mode");

        selection.toggle(refs(&["b"]).remove(0));
        selection.exit();
        assert!(!selection.active());
        assert_eq!(selection.count(), 0);
    }

    #[test]
    fn ranges_are_inclusive_in_either_direction() {
        let all = refs(&["a", "b", "c", "d"]);
        let ids = |range: Option<Vec<ItemRef>>| -> Vec<String> {
            range
                .unwrap_or_default()
                .into_iter()
                .map(|item| item.id.to_string())
                .collect()
        };

        assert_eq!(ids(range_in(&all, "b", "d")), ["b", "c", "d"]);
        assert_eq!(ids(range_in(&all, "d", "b")), ["b", "c", "d"]);
        assert_eq!(ids(range_in(&all, "c", "c")), ["c"]);
        assert!(range_in(&all, "a", "gone").is_none());
    }
}
