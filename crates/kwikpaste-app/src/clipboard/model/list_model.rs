//! 列表的稀疏分页缓存，移植自 1.x `src/hooks/useClipboardItems.ts`。
//!
//! 数据源（core）仍是排序、搜索、载荷裁剪的唯一真相；这里只按可见范围缓存少量已加载行：
//! 请求按 30 行对齐、两侧各预取 30 行，超过 180 行时围绕视图中心保留 ±90 行。
//! 每次重载递增请求令牌，过期响应直接丢弃。
//!
//! 下标一律是「模型下标」：包含开头的置顶行，与虚拟列表行号一致。
//! 所有操作都只改数据，不碰 GPUI；视图拿返回值去同步 `ListState`。

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
};

use super::item::ListItem;

/// 请求对齐的页大小。
pub const PAGE_SIZE: usize = 30;
/// 可见范围两侧各预取的行数。
pub const PRELOAD_ROWS: usize = 30;
/// 缓存行数上限。
pub const CACHE_MAX_ROWS: usize = 180;
/// 裁剪时围绕视图中心保留的半径。
pub const CACHE_KEEP_RADIUS: usize = 90;

/// 一次分页请求。`range` 是模型下标的半开区间。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchRequest {
    pub range: Range<usize>,
    pub token: u64,
    /// 结果整体替换缓存（重载第一页），而不是合并进去。
    pub replace: bool,
}

/// 数据源返回的一页。
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub items: Vec<Arc<ListItem>>,
    pub total: usize,
}

/// 一页数据落进缓存后的变化，视图据此同步 `ListState`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// 写入了新数据的模型下标范围（已截到新总数以内）。
    pub range: Range<usize>,
    pub replaced: bool,
    pub total_before: usize,
    pub total_after: usize,
}

/// 删除一行后的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub index: usize,
    /// 删除后强制重拉视图范围，补上后面挪上来的行。
    pub refetch: Option<FetchRequest>,
}

#[derive(Debug, Default)]
pub struct ListModel {
    items: HashMap<usize, Arc<ListItem>>,
    total: usize,
    token: u64,
    loading: Vec<Range<usize>>,
    loaded_initial: bool,
    /// 视图当前可见的模型下标范围（半开）。
    view: Range<usize>,
}

impl ListModel {
    pub fn new() -> Self {
        Self {
            view: 0..PAGE_SIZE,
            ..Self::default()
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    /// 是否已经收到过第一页（之前显示加载态，之后才可能显示空态）。
    pub fn loaded_initial(&self) -> bool {
        self.loaded_initial
    }

    #[allow(
        dead_code,
        reason = "1.x 的同名逻辑，加载态与预览接进来之前只有单测在用"
    )]
    pub fn is_loading(&self) -> bool {
        !self.loading.is_empty()
    }

    pub fn cached_rows(&self) -> usize {
        self.items.len()
    }

    #[allow(
        dead_code,
        reason = "1.x 的同名逻辑，加载态与预览接进来之前只有单测在用"
    )]
    pub fn view(&self) -> Range<usize> {
        self.view.clone()
    }

    pub fn get(&self, index: usize) -> Option<&Arc<ListItem>> {
        self.items.get(&index)
    }

    /// 已缓存的行（总数以内），按模型下标排序。
    pub fn cached_items(&self) -> Vec<&Arc<ListItem>> {
        let mut rows: Vec<_> = self
            .items
            .iter()
            .filter(|(index, _)| **index < self.total)
            .collect();
        rows.sort_unstable_by_key(|(index, _)| **index);
        rows.into_iter().map(|(_, item)| item).collect()
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.items
            .iter()
            .find_map(|(index, item)| (&*item.id == id).then_some(*index))
    }

    pub fn find(&self, id: &str) -> Option<&Arc<ListItem>> {
        self.items.values().find(|item| &*item.id == id)
    }

    /// 记下可见范围（模型下标，半开），缺数据时返回要发的请求（1.x `loadRange`）。
    pub fn load_range(&mut self, visible: Range<usize>) -> Option<FetchRequest> {
        let start = visible.start.min(visible.end);
        let end = visible.end.max(visible.start).max(start + 1);
        self.view = start..end;

        self.fetch(
            start as i64 - PRELOAD_ROWS as i64,
            (end - 1) as i64 + PRELOAD_ROWS as i64,
            false,
            false,
        )
    }

    /// 有新内容时重拉第一页；已经显示过数据时保留旧行，不闪加载态（1.x `reload`）。
    pub fn reload(&mut self) -> FetchRequest {
        self.bump_token();
        if !self.loaded_initial {
            self.items.clear();
            self.total = 0;
        }
        self.view = 0..PAGE_SIZE;

        self.first_page()
    }

    /// 查询条件变了：清空，显示加载态，拉第一页（1.x `resetAndReload`）。
    pub fn reset_and_reload(&mut self) -> FetchRequest {
        self.bump_token();
        self.items.clear();
        self.total = 0;
        self.loaded_initial = false;
        self.view = 0..PAGE_SIZE;

        self.first_page()
    }

    /// 重拉视图 ± 预取范围，响应到达前保留旧行，随后整体替换过时的排序缓存。
    pub fn reload_current_range(&mut self) -> Option<FetchRequest> {
        self.bump_token();
        let view = self.view.clone();

        self.fetch(
            view.start as i64 - PRELOAD_ROWS as i64,
            (view.end - 1) as i64 + PRELOAD_ROWS as i64,
            true,
            true,
        )
    }

    /// 批量删除后刷新的第一步：用第一页替换整份缓存（1.x `refreshAfterRemoval`）。
    /// 第一页落地后再调用 [`Self::refetch_view`] 补视图范围。
    pub fn refresh_after_removal(&mut self) -> FetchRequest {
        self.bump_token();

        self.first_page()
    }

    /// 强制重拉视图 ± 预取范围，不清缓存。
    pub fn refetch_view(&mut self) -> Option<FetchRequest> {
        let view = self.view.clone();

        self.fetch(
            view.start as i64 - PRELOAD_ROWS as i64,
            (view.end - 1) as i64 + PRELOAD_ROWS as i64,
            true,
            false,
        )
    }

    /// 把一页数据写进缓存；令牌过期时返回 `None`。
    pub fn apply(&mut self, request: &FetchRequest, page: Page) -> Option<Applied> {
        if request.token != self.token {
            return None;
        }
        self.finish(request);

        let total_before = self.total;
        let total_after = page.total;
        if request.replace {
            self.items.clear();
        }

        drop_stale_duplicates(&mut self.items, &request.range, &page.items);
        let mut written_end = request.range.start;
        for (offset, item) in page.items.into_iter().enumerate() {
            let index = request.range.start + offset;
            if index < total_after {
                self.items.insert(index, item);
                written_end = index + 1;
            }
        }
        // 总数变小时，超出新总数的旧行一并丢掉。
        self.items.retain(|index, _| *index < total_after);

        self.total = total_after;
        self.loaded_initial = true;
        self.trim();

        Some(Applied {
            range: request.range.start..written_end.max(request.range.start),
            replaced: request.replace,
            total_before,
            total_after,
        })
    }

    /// 请求失败。令牌仍有效且从没加载成功过时当作空列表，免得一直停在加载态；返回是否改了状态。
    pub fn fail(&mut self, request: &FetchRequest) -> bool {
        if request.token != self.token {
            return false;
        }
        self.finish(request);
        if self.loaded_initial {
            return false;
        }

        self.items.clear();
        self.total = 0;
        self.loaded_initial = true;
        true
    }

    /// 删掉一行并把后面的下标前移（单条删除、在收藏里取消收藏、移出当前分组，1.x `removeItemById`）。
    pub fn remove_by_id(&mut self, id: &str) -> Option<Removed> {
        let removed = self.index_of(id)?;
        self.bump_token();

        let old = std::mem::take(&mut self.items);
        for (index, item) in old {
            if &*item.id == id {
                continue;
            }
            let index = if index > removed { index - 1 } else { index };
            self.items.insert(index, item);
        }
        self.total = self.total.saturating_sub(1);
        self.trim();

        let refetch = self.refetch_view();

        Some(Removed {
            index: removed,
            refetch,
        })
    }

    /// 在本地合并一条记录的改动（收藏、置顶标记、便签、分组，1.x `patchItemById`）。返回它的下标。
    pub fn patch_by_id(&mut self, id: &str, patch: impl FnOnce(&mut ListItem)) -> Option<usize> {
        let index = self.index_of(id)?;
        let item = self.items.get_mut(&index)?;
        patch(Arc::make_mut(item));

        Some(index)
    }

    /// 在已缓存行中乐观地移动一个置顶 / 收藏条目；空洞的分页位置保持不变。
    pub fn reorder_local(
        &mut self,
        id: &str,
        anchor: &str,
        favorite_section: bool,
        after: bool,
    ) -> bool {
        if id == anchor {
            return false;
        }
        let section = |item: &ListItem| {
            if favorite_section {
                item.is_favorite && !item.is_pinned
            } else {
                item.is_pinned
            }
        };
        let mut keys: Vec<usize> = self
            .items
            .iter()
            .filter_map(|(index, item)| section(item).then_some(*index))
            .collect();
        keys.sort_unstable();
        let Some(source_key) = keys
            .iter()
            .copied()
            .find(|index| self.items.get(index).is_some_and(|item| &*item.id == id))
        else {
            return false;
        };
        let Some(anchor_key) = keys.iter().copied().find(|index| {
            self.items
                .get(index)
                .is_some_and(|item| &*item.id == anchor)
        }) else {
            return false;
        };
        let Some(source_pos) = keys.iter().position(|key| *key == source_key) else {
            return false;
        };
        let Some(anchor_pos) = keys.iter().position(|key| *key == anchor_key) else {
            return false;
        };
        if (!after && source_pos + 1 == anchor_pos) || (after && anchor_pos + 1 == source_pos) {
            return false;
        }
        let source = self.items.remove(&source_key);
        let mut remaining: Vec<Arc<ListItem>> = keys
            .iter()
            .filter(|key| **key != source_key)
            .filter_map(|key| self.items.get(key).cloned())
            .collect();
        let anchor_pos = remaining
            .iter()
            .position(|item| &*item.id == anchor)
            .unwrap_or_else(|| {
                keys.iter()
                    .position(|key| *key == anchor_key)
                    .unwrap_or_default()
                    .min(remaining.len())
            });
        let insert_pos = (anchor_pos + usize::from(after)).min(remaining.len());
        if let Some(source) = source {
            remaining.insert(insert_pos, source);
        }
        for (key, item) in keys.into_iter().zip(remaining) {
            self.items.insert(key, item);
        }
        true
    }

    /// 面板隐藏时只留第一页：再显示时会回到顶部，其余行按需重拉（附录 B §5.3 第 8 条“隐藏即释放”）。
    pub fn release_rows(&mut self) {
        self.items.retain(|index, _| *index < PAGE_SIZE);
    }

    fn first_page(&mut self) -> FetchRequest {
        let range = 0..PAGE_SIZE.min(self.total.max(PAGE_SIZE));
        self.loading = vec![range.clone()];

        FetchRequest {
            range,
            token: self.token,
            replace: true,
        }
    }

    fn bump_token(&mut self) {
        self.token += 1;
        self.loading.clear();
    }

    fn finish(&mut self, request: &FetchRequest) {
        if let Some(position) = self
            .loading
            .iter()
            .position(|range| *range == request.range)
        {
            self.loading.remove(position);
        }
    }

    /// 1.x `fetchRange` 的判定部分：对齐、跳过已加载或正在加载的范围，登记新请求。
    fn fetch(
        &mut self,
        raw_start: i64,
        raw_end: i64,
        force: bool,
        replace: bool,
    ) -> Option<FetchRequest> {
        let range = normalize_fetch_range(raw_start, raw_end, self.total)?;
        if !force {
            if range.clone().all(|index| self.items.contains_key(&index)) {
                return None;
            }
            if self
                .loading
                .iter()
                .any(|loading| loading.start <= range.start && loading.end >= range.end)
            {
                return None;
            }
        }

        self.loading.push(range.clone());
        Some(FetchRequest {
            range,
            token: self.token,
            replace,
        })
    }

    /// 超过上限时围绕视图中心保留 ±90 行，置顶行和普通行采用相同的淘汰策略。
    fn trim(&mut self) {
        if self.items.len() <= CACHE_MAX_ROWS {
            return;
        }

        let last = self.view.end.saturating_sub(1).max(self.view.start);
        let center = (self.view.start + last) / 2;
        let keep_start = center.saturating_sub(CACHE_KEEP_RADIUS);
        let keep_end = (center + CACHE_KEEP_RADIUS).min(self.total.saturating_sub(1));
        self.items
            .retain(|index, _| (keep_start..=keep_end).contains(index));
    }
}

/// 把请求范围对齐到页边界并夹到总数以内；`raw_end < 0` 时不请求（1.x `normalizeFetchRange`）。
/// 返回半开区间。总数未知（0）时按请求的结束位置估计。
fn normalize_fetch_range(raw_start: i64, raw_end: i64, total: usize) -> Option<Range<usize>> {
    if raw_end < 0 {
        return None;
    }

    let total = total as i64;
    let page = PAGE_SIZE as i64;
    let max_known = if total > 0 { total - 1 } else { raw_end.max(0) };
    let clamped_start = raw_start.min(max_known).max(0);
    let clamped_end = raw_end.min(max_known).max(clamped_start);
    let start = clamped_start / page * page;
    let mut end = (clamped_end + 1 + page - 1) / page * page - 1;
    if total > 0 {
        end = end.min(total - 1);
    }

    Some(start as usize..(end + 1) as usize)
}

/// 两次请求之间后端顺序可能变了：新页里出现的 id 若还留在缓存的别处，以新页为准删掉旧位置
/// （1.x `dropStaleDuplicates`）。
fn drop_stale_duplicates(
    items: &mut HashMap<usize, Arc<ListItem>>,
    range: &Range<usize>,
    page: &[Arc<ListItem>],
) {
    let ids: HashSet<&str> = page.iter().map(|item| &*item.id).collect();

    items.retain(|index, item| range.contains(index) || !ids.contains(&*item.id));
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::super::item::{ItemKind, Platform};
    use super::*;

    fn item(id: &str, pinned: bool) -> Arc<ListItem> {
        Arc::new(ListItem {
            id: id.into(),
            kind: ItemKind::Text,
            sub_kind: None,
            group_id: None,
            content: "".into(),
            summary: Some(id.into()),
            width: None,
            height: None,
            is_favorite: false,
            is_pinned: pinned,
            is_sensitive: false,
            platform: Platform::Windows,
            note: None,
            created_at: Utc.timestamp_opt(0, 0).single().unwrap_or_default(),
            source_app_id: None,
            source_app_name: None,
            source_app_icon_path: None,
            origin_device_id: None,
            origin_device_name: None,
            image_thumbnail_path: None,
            file_entries: None,
            files_preview_kind: None,
            available_actions: Vec::new(),
            color_preview: None,
            quick_snippets: Vec::new(),
            image_display: None,
            has_image_text: false,
            image_text_snippet: None,
        })
    }

    /// 一个按下标命名的后端：`ids[i]` 是第 i 行。
    fn page(ids: &[String], pinned: usize, range: &Range<usize>) -> Page {
        Page {
            items: ids
                .iter()
                .enumerate()
                .skip(range.start)
                .take(range.len())
                .map(|(index, id)| item(id, index < pinned))
                .collect(),
            total: ids.len(),
        }
    }

    fn backend(total: usize) -> Vec<String> {
        (0..total).map(|index| format!("r{index}")).collect()
    }

    fn load(
        model: &mut ListModel,
        ids: &[String],
        pinned: usize,
        request: &FetchRequest,
    ) -> Applied {
        model
            .apply(request, page(ids, pinned, &request.range))
            .expect("current token")
    }

    #[test]
    fn ranges_align_to_pages() {
        assert_eq!(normalize_fetch_range(-30, 29, 0), Some(0..30));
        assert_eq!(normalize_fetch_range(35, 70, 1000), Some(30..90));
        assert_eq!(normalize_fetch_range(95, 140, 100), Some(90..100));
        assert_eq!(normalize_fetch_range(5, -1, 100), None);
        // 总数未知时按结束位置估计，结束位置向上对齐到页尾。
        assert_eq!(normalize_fetch_range(0, 40, 0), Some(0..60));
    }

    #[test]
    fn first_load_and_preload_both_sides() {
        let ids = backend(1000);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        assert_eq!(first.range, 0..30);
        assert!(first.replace);

        let applied = load(&mut model, &ids, 0, &first);
        assert_eq!(applied.total_after, 1000);
        assert!(model.loaded_initial());
        assert_eq!(model.cached_rows(), 30);

        // 看 100..110：预取到 70..140，对齐成 60..150。
        let request = model.load_range(100..110).expect("needs rows");
        assert_eq!(request.range, 60..150);
        assert!(!request.replace);
        // 同一范围正在加载时不重复请求。
        assert_eq!(model.load_range(100..110), None);

        load(&mut model, &ids, 0, &request);
        assert_eq!(model.load_range(100..110), None, "already cached");
        assert_eq!(model.get(105).map(|item| &*item.id), Some("r105"));
    }

    #[test]
    fn stale_tokens_are_ignored() {
        let ids = backend(100);
        let mut model = ListModel::new();
        let old = model.reset_and_reload();
        let new = model.reload();

        assert!(model.apply(&old, page(&ids, 0, &old.range)).is_none());
        assert!(!model.loaded_initial());
        assert!(model.apply(&new, page(&ids, 0, &new.range)).is_some());
        assert!(!model.fail(&old));
    }

    #[test]
    fn trimming_evicts_offscreen_pins_like_regular_rows() {
        let ids = backend(2000);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 3, &first);

        for start in (0..600).step_by(30) {
            if let Some(request) = model.load_range(start..start + 10) {
                load(&mut model, &ids, 3, &request);
            }
        }
        if let Some(request) = model.load_range(1000..1010) {
            load(&mut model, &ids, 3, &request);
        }

        assert!(model.cached_rows() <= CACHE_MAX_ROWS);
        // 视图中心 1004，只保留 914..=1094（实际加载了 960..1050），置顶行也会淘汰。
        assert!(model.get(0).is_none() && model.get(2).is_none());
        assert!(model.get(3).is_none());
        assert!(model.get(960).is_some() && model.get(1049).is_some());
        assert!(model.get(500).is_none());
        assert!(model.get(913).is_none());
        let request = model.load_range(0..10).expect("reload evicted pins");
        load(&mut model, &ids, 3, &request);
        assert!(model.get(0).is_some_and(|item| item.is_pinned));
    }

    #[test]
    fn stale_duplicates_drop_old_positions() {
        let mut ids = backend(100);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);
        let second = model.load_range(30..40).expect("needs rows");
        load(&mut model, &ids, 0, &second);

        // r45 被重新使用、挪到第 0 行；再拉 30..90 时它不在里面，但 0..30 的旧缓存里没有它——
        // 反过来：新拉第一页时 r45 出现在第 0 行，缓存里第 45 行的旧 r45 要删掉。
        let moved = ids.remove(45);
        ids.insert(0, moved);
        let reload = model.reload();
        let applied = model.apply(&reload, page(&ids, 0, &reload.range));
        assert!(applied.is_some());
        assert_eq!(model.get(0).map(|item| &*item.id), Some("r45"));
        assert!(model.index_of("r45") == Some(0));
    }

    #[test]
    fn merge_drops_moved_ids_outside_the_page() {
        let mut ids = backend(1000);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);

        // r5 移到第 200 行；拉 150..240 这一页时，缓存里第 5 行的旧 r5 被删掉，空位之后按需重拉。
        let moved = ids.remove(5);
        ids.insert(200, moved);
        let request = model.load_range(200..205).expect("needs rows");
        assert_eq!(request.range, 150..240);
        load(&mut model, &ids, 0, &request);

        assert_eq!(model.index_of("r5"), Some(200));
        assert!(model.get(5).is_none());
        assert_eq!(model.get(4).map(|item| &*item.id), Some("r4"));
    }

    #[test]
    fn reload_keeps_rows_but_reset_clears_them() {
        let ids = backend(100);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);

        let reload = model.reload();
        assert_eq!(
            model.cached_rows(),
            30,
            "old rows stay visible during a reload"
        );
        assert_eq!(reload.range, 0..30);

        model.reset_and_reload();
        assert_eq!(model.cached_rows(), 0);
        assert!(!model.loaded_initial());
    }

    #[test]
    fn reload_current_range_keeps_rows_until_the_response() {
        let ids = backend(1000);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);
        let request = model.load_range(200..210).expect("needs rows");
        load(&mut model, &ids, 0, &request);

        let cached = model.cached_rows();
        let refetch = model.reload_current_range().expect("view range");
        assert_eq!(model.cached_rows(), cached);
        assert_eq!(refetch.range, 150..240);
        assert!(refetch.replace);
        load(&mut model, &ids, 0, &refetch);
        assert_eq!(model.get(200).map(|item| &*item.id), Some("r200"));
        assert!(model.get(0).is_none());
    }

    #[test]
    fn refresh_after_removal_replaces_then_refetches() {
        let ids = backend(1000);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 2, &first);
        let request = model.load_range(300..305).expect("needs rows");
        load(&mut model, &ids, 2, &request);

        let after: Vec<String> = ids.iter().skip(10).cloned().collect();
        let page0 = model.refresh_after_removal();
        let applied = load(&mut model, &after, 2, &page0);
        assert!(applied.replaced);
        assert_eq!(applied.total_after, 990);
        assert_eq!(model.cached_rows(), 30);

        let view = model.refetch_view().expect("forced");
        assert_eq!(view.range, 270..360);
        load(&mut model, &after, 2, &view);
        assert_eq!(model.get(300).map(|item| &*item.id), Some("r310"));
    }

    #[test]
    fn remove_by_id_shifts_rows_and_refetches() {
        let ids = backend(100);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);
        model.load_range(0..10);

        let removed = model.remove_by_id("r3").expect("cached");
        assert_eq!(removed.index, 3);
        assert_eq!(model.total(), 99);
        assert_eq!(model.get(3).map(|item| &*item.id), Some("r4"));
        assert_eq!(model.get(28).map(|item| &*item.id), Some("r29"));
        assert!(model.get(29).is_none());
        assert_eq!(removed.refetch.map(|request| request.range), Some(0..60));
        assert!(model.remove_by_id("missing").is_none());
    }

    #[test]
    fn removal_rejects_responses_from_before_the_delete() {
        let mut ids = backend(100);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);
        let old = model.load_range(20..40).expect("prefetch");
        let stale_page = page(&ids, 0, &old.range);
        let removed = model.remove_by_id("r3").expect("cached");
        ids.remove(3);
        let refetch = removed.refetch.expect("replacement range");
        load(&mut model, &ids, 0, &refetch);

        assert!(model.apply(&old, stale_page).is_none());
        assert!(model.find("r3").is_none());
        assert_eq!(model.total(), 99);
        assert!(!model.is_loading());
    }

    #[test]
    fn patch_by_id_merges_locally() {
        let ids = backend(40);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);

        let index = model.patch_by_id("r7", |item| {
            item.is_favorite = true;
            item.note = Some("备注".into());
        });
        assert_eq!(index, Some(7));
        let item = model.get(7).expect("cached");
        assert!(item.is_favorite);
        assert_eq!(item.note.as_deref(), Some("备注"));
    }

    #[test]
    fn reorder_local_keeps_all_rows_and_moves_only_the_requested_section() {
        let ids = backend(8);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 2, &first);
        for id in ["r2", "r3", "r4"] {
            model.patch_by_id(id, |item| item.is_favorite = true);
        }

        assert!(!model.reorder_local("r0", "r2", false, true));
        assert!(!model.reorder_local("r2", "r0", true, true));
        assert!(model.reorder_local("r0", "r1", false, true));
        assert_eq!(model.get(0).map(|item| &*item.id), Some("r1"));
        assert_eq!(model.get(1).map(|item| &*item.id), Some("r0"));
        assert_eq!(model.get(2).map(|item| &*item.id), Some("r2"));
        assert_eq!(model.get(7).map(|item| &*item.id), Some("r7"));

        assert!(model.reorder_local("r2", "r4", true, false));
        assert_eq!(model.get(2).map(|item| &*item.id), Some("r3"));
        assert_eq!(model.get(3).map(|item| &*item.id), Some("r2"));
        assert_eq!(model.get(4).map(|item| &*item.id), Some("r4"));
        assert_eq!(model.get(5).map(|item| &*item.id), Some("r5"));
    }

    #[test]
    fn initial_failure_becomes_an_empty_list() {
        let mut model = ListModel::new();
        let first = model.reset_and_reload();

        assert!(model.fail(&first));
        assert!(model.loaded_initial());
        assert_eq!(model.total(), 0);
        assert!(!model.is_loading());
    }

    #[test]
    fn shrinking_total_drops_rows_past_the_end() {
        let ids = backend(100);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);
        let request = model.load_range(40..50).expect("needs rows");
        load(&mut model, &ids, 0, &request);

        let fewer: Vec<String> = ids.iter().take(35).cloned().collect();
        let request = model.refetch_view().expect("forced");
        let applied = load(&mut model, &fewer, 0, &request);
        assert_eq!(applied.total_after, 35);
        assert!(model.get(40).is_none());
        assert_eq!(model.total(), 35);
    }

    #[test]
    fn removing_a_pinned_row_shifts_the_same_model_indices() {
        let ids = backend(50);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 4, &first);

        let removed = model.remove_by_id("r1").expect("pinned row");
        assert_eq!(removed.index, 1);
        assert_eq!(model.total(), 49);
        assert_eq!(model.get(1).map(|item| &*item.id), Some("r2"));
        assert_eq!(model.get(3).map(|item| &*item.id), Some("r4"));
    }

    #[test]
    fn release_rows_keeps_the_first_page() {
        let ids = backend(500);
        let mut model = ListModel::new();
        let first = model.reset_and_reload();
        load(&mut model, &ids, 0, &first);
        let request = model.load_range(200..210).expect("needs rows");
        load(&mut model, &ids, 0, &request);

        model.release_rows();
        assert_eq!(model.cached_rows(), 30);
        assert_eq!(model.total(), 500);
    }
}
