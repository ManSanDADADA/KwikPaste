//! 夹具数据源：内存里的一份有序列表和几个分组，按 core 的分页与筛选语义切页，记录操作就地改这份数据。
//!
//! 数据来自 JSON（`ClipboardItemPage` 形状，即 1.4.0 / core `list_items` 的输出）或合成生成器。
//! 可选的延迟模拟 core 查询耗时：在独立线程上睡眠后返回，不阻塞 UI。排序只有夹具本身的顺序
//! （相当于 core 的默认排序），查询里的排序字段不起作用。

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context as _;
use futures::{FutureExt as _, future::BoxFuture};
use serde::Deserialize;

use kwikpaste_core::{
    ops::{ReorderAnchor, ReorderSection},
    settings::Settings,
};

use super::{
    ClipboardSource, Group, GroupInput, ImageSave, ListQuery, NoteSaved, Preview,
    PreviewContentMetrics, PreviewPayload, PreviewTextView, synthetic::AssetSet,
};
use crate::clipboard::model::{
    actions::OpenTarget,
    filter::ListFilter,
    item::{ItemRef, ListItem},
    list_model::Page,
};

/// JSON 夹具里资源路径的占位前缀，加载时换成合成资源目录。
pub const ASSETS_PLACEHOLDER: &str = "{assets}";

#[derive(Deserialize)]
struct PageJson {
    list: Vec<ListItem>,
}

/// 夹具的“数据库”：一份按列表顺序排好的记录（置顶在前）和自定义分组。
#[derive(Debug, Default)]
pub struct FixtureStore {
    items: Vec<Arc<ListItem>>,
    groups: Vec<Group>,
}

impl FixtureStore {
    pub fn new(items: Vec<Arc<ListItem>>) -> Self {
        Self {
            items,
            groups: Vec::new(),
        }
    }

    /// 加上自定义分组；`members` 是要放进各分组的记录下标（按夹具顺序）。
    pub fn with_groups(mut self, groups: Vec<(Group, Vec<usize>)>) -> Self {
        for (group, members) in groups {
            for index in members {
                if let Some(item) = self.items.get_mut(index) {
                    Arc::make_mut(item).group_id = Some(group.id.clone());
                }
            }
            self.groups.push(group);
        }
        self
    }

    /// 解析 `ClipboardItemPage` 形状的 JSON；字符串里的 `{assets}` 换成 `assets_dir`。
    pub fn from_page_json(json: &str, assets_dir: &Path) -> anyhow::Result<Self> {
        let dir = assets_dir.to_string_lossy().replace('\\', "/");
        let json = json.replace(ASSETS_PLACEHOLDER, &dir);
        let page: PageJson = serde_json::from_str(&json).context("fixture is not a list page")?;

        Ok(Self::new(page.list.into_iter().map(Arc::new).collect()))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn page(&self, query: &ListQuery) -> Page {
        // 不筛选时直接切片：跑分的 1 万行每页都走这里。
        if query.filter == ListFilter::default() {
            return Page {
                items: self
                    .items
                    .iter()
                    .skip(query.offset)
                    .take(query.limit)
                    .cloned()
                    .collect(),
                total: self.items.len(),
            };
        }

        let matching: Vec<&Arc<ListItem>> = self
            .items
            .iter()
            .filter(|item| query.filter.matches(item))
            .collect();
        Page {
            total: matching.len(),
            items: matching
                .into_iter()
                .skip(query.offset)
                .take(query.limit)
                .cloned()
                .collect(),
        }
    }

    pub fn refs(&self, filter: &ListFilter) -> Vec<ItemRef> {
        self.items
            .iter()
            .filter(|item| filter.matches(item))
            .map(|item| ItemRef::of(item))
            .collect()
    }

    pub fn groups(&self) -> Vec<Group> {
        self.groups.clone()
    }

    #[cfg(test)]
    pub fn get(&self, index: usize) -> Option<&Arc<ListItem>> {
        self.items.get(index)
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|item| &*item.id == id)
    }

    /// 新记录进库：排在置顶行之后的第一位（core 按 `is_pinned DESC, updated_at DESC` 排序）。
    pub fn insert_newest(&mut self, item: ListItem) {
        let at = self.items.iter().take_while(|item| item.is_pinned).count();
        self.items.insert(at, Arc::new(item));
    }

    pub fn remove(&mut self, id: &str) -> Option<Arc<ListItem>> {
        let index = self.index_of(id)?;
        Some(self.items.remove(index))
    }

    /// 切换置顶：置顶的移到列表首行，取消置顶的放回置顶行之后。
    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> bool {
        let Some(item) = self.remove(id) else {
            return false;
        };
        let mut item = (*item).clone();
        item.is_pinned = pinned;
        let at = if pinned {
            0
        } else {
            self.items.iter().take_while(|item| item.is_pinned).count()
        };
        self.items.insert(at, Arc::new(item));
        true
    }

    pub fn reorder(&mut self, section: ReorderSection, id: &str, anchor: ReorderAnchor) -> bool {
        let (anchor, after) = match anchor {
            ReorderAnchor::Before(anchor) => (anchor, false),
            ReorderAnchor::After(anchor) => (anchor, true),
        };
        if id == anchor {
            return true;
        }
        let matches = |item: &ListItem| match section {
            ReorderSection::Pinned => item.is_pinned,
            ReorderSection::Favorite => item.is_favorite && !item.is_pinned,
        };
        let Some(source_ix) = self
            .items
            .iter()
            .position(|item| &*item.id == id && matches(item))
        else {
            return false;
        };
        let Some(anchor_ix) = self
            .items
            .iter()
            .position(|item| *item.id == anchor && matches(item))
        else {
            return false;
        };
        let item = self.items.remove(source_ix);
        let anchor_ix = self
            .items
            .iter()
            .position(|candidate| *candidate.id == anchor)
            .unwrap_or(anchor_ix);
        let insert_ix = if after { anchor_ix + 1 } else { anchor_ix };
        self.items.insert(insert_ix.min(self.items.len()), item);
        true
    }

    pub fn patch(&mut self, id: &str, patch: impl FnOnce(&mut ListItem)) -> bool {
        let Some(item) = self.items.iter_mut().find(|item| &*item.id == id) else {
            return false;
        };
        patch(Arc::make_mut(item));
        true
    }

    fn find(&self, id: &str) -> Option<&Arc<ListItem>> {
        self.items.iter().find(|item| &*item.id == id)
    }

    fn hide_group(&mut self, id: &str) -> bool {
        let Some(group) = self.groups.iter_mut().find(|group| &*group.id == id) else {
            return false;
        };
        group.is_hidden = true;
        true
    }

    /// 按 core 的规则归一化名称（去首尾空白、非空、不超过 32 个字）。
    fn group_name(name: &str) -> anyhow::Result<Arc<str>> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("分组名称不能为空");
        }
        if name.chars().count() > 32 {
            anyhow::bail!("分组名称不能超过 32 个字符");
        }
        Ok(Arc::from(name))
    }

    fn create_group(&mut self, input: GroupInput) -> anyhow::Result<Group> {
        let group = Group {
            id: Arc::from(format!("fixture-group-{}", self.groups.len() + 1)),
            name: Self::group_name(&input.name)?,
            icon: Arc::from(input.icon),
            is_hidden: input.is_hidden,
        };
        self.groups.push(group.clone());
        Ok(group)
    }

    fn update_group(&mut self, id: &str, input: GroupInput) -> anyhow::Result<()> {
        let name = Self::group_name(&input.name)?;
        let group = self
            .groups
            .iter_mut()
            .find(|group| &*group.id == id)
            .ok_or_else(|| anyhow::anyhow!("group not found: {id}"))?;
        group.name = name;
        group.icon = Arc::from(input.icon);
        group.is_hidden = input.is_hidden;
        Ok(())
    }

    /// 按 `order` 重排（不在里面的排到最后），`visible` 之外的都隐藏。
    fn update_layout(&mut self, order: &[Arc<str>], visible: &[Arc<str>]) {
        let rank = |group: &Group| {
            order
                .iter()
                .position(|id| *id == group.id)
                .unwrap_or(usize::MAX)
        };
        self.groups.sort_by_key(rank);
        for group in &mut self.groups {
            group.is_hidden = !visible.contains(&group.id);
        }
    }

    fn delete_group(&mut self, id: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|group| &*group.id != id);
        for item in &mut self.items {
            if item.group_id.as_deref() == Some(id) {
                Arc::make_mut(item).group_id = None;
            }
        }
        self.groups.len() != before
    }
}

/// 预览数据：按 core `preview_payload` / `preview_metrics` 的规则从夹具记录算出（文本取摘要，图片的
/// “原图”用缩略图，文件没有大小）。
fn fixture_preview(item: &ListItem, thumbnails: &Path, text_view: PreviewTextView) -> Preview {
    use kwikpaste_core::{
        clipboard::{split_words, word_spans},
        db::models::{ClipboardKind, ClipboardSubKind},
        presenter::{ClipboardPreviewFileEntry, PreviewWordChip},
    };

    use crate::clipboard::model::item::{ItemKind, SubKind};

    let text = (item.kind == ItemKind::Text)
        .then(|| item.summary.as_deref().unwrap_or_default().to_owned());
    let (words, words_truncated) = match &text {
        Some(text) if !item.is_sensitive => word_spans(text),
        _ => (Vec::new(), false),
    };
    let files: Vec<ClipboardPreviewFileEntry> = item
        .file_rows()
        .iter()
        .map(|row| ClipboardPreviewFileEntry {
            path: row.path.to_string(),
            name: row.name.to_string(),
            is_dir: row.is_dir,
            is_image: row.is_image,
            exists: row.exists,
            size: None,
            icon_path: row.icon_path.as_deref().map(str::to_owned),
        })
        .collect();
    let metrics = match item.kind {
        ItemKind::Image => PreviewContentMetrics::Image {
            width: item.width.map(f64::from),
            height: item.height.map(f64::from),
        },
        ItemKind::Files => PreviewContentMetrics::Files {
            shown: u32::try_from(files.len()).unwrap_or(u32::MAX),
            total: u32::try_from(files.len()).unwrap_or(u32::MAX),
        },
        ItemKind::Text => {
            let text = text.as_deref().unwrap_or_default();
            if text_view == PreviewTextView::Words && !words.is_empty() {
                PreviewContentMetrics::Words {
                    chips: split_words(text)
                        .tokens
                        .iter()
                        .map(|token| PreviewWordChip::new(&token.text, token.line_break))
                        .collect(),
                }
            } else {
                // 实际行数由预览按面板宽度折行后算，这里只按换行估。
                PreviewContentMetrics::Text {
                    rows: u32::try_from(text.split('\n').count()).unwrap_or(u32::MAX),
                }
            }
        }
    };
    let image_path = (item.kind == ItemKind::Image)
        .then(|| thumbnails.join(&*item.content))
        .filter(|path| path.exists());

    Preview {
        payload: PreviewPayload {
            id: item.id.to_string(),
            kind: match item.kind {
                ItemKind::Text => ClipboardKind::Text,
                ItemKind::Image => ClipboardKind::Image,
                ItemKind::Files => ClipboardKind::Files,
            },
            sub_kind: item.sub_kind.map(|sub_kind| match sub_kind {
                SubKind::Rtf => ClipboardSubKind::Rtf,
                SubKind::Html => ClipboardSubKind::Html,
                SubKind::Url => ClipboardSubKind::Url,
                SubKind::Email => ClipboardSubKind::Email,
                SubKind::Color => ClipboardSubKind::Color,
                SubKind::Path => ClipboardSubKind::Path,
            }),
            updated_at: item.created_at,
            size: text
                .as_deref()
                .map(|text| i64::try_from(text.encode_utf16().count()).unwrap_or(i64::MAX)),
            text,
            image_exists: image_path.is_some(),
            image_path: image_path.map(|path| path.to_string_lossy().into_owned()),
            image_width: item.width.map(i64::from),
            image_height: item.height.map(i64::from),
            is_sensitive: item.is_sensitive,
            total_files: files.len(),
            files,
            words,
            words_truncated,
        },
        metrics,
    }
}

/// 以 [`FixtureStore`] 为后端的数据源。
pub struct FixtureSource {
    store: Arc<Mutex<FixtureStore>>,
    thumbnails: PathBuf,
    latency: Duration,
    /// 预览的文本方式（夹具没有设置文件，改了就记在这里）。
    text_view: Arc<Mutex<PreviewTextView>>,
}

impl FixtureSource {
    pub fn new(store: FixtureStore, assets: &AssetSet) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            thumbnails: assets.thumbnails_dir(),
            latency: Duration::ZERO,
            text_view: Arc::new(Mutex::new(Settings::default().clipboard.preview.text_view)),
        }
    }

    /// 每次查询前等待 `latency`，模拟 core 查库与加工的耗时。
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// 后端数据的句柄，自测和跑分用它模拟新记录、删除、置顶等变化。
    pub fn store(&self) -> Arc<Mutex<FixtureStore>> {
        self.store.clone()
    }

    /// 锁住数据做一件事（在 [`Self::respond`] 的工作线程上调用）。
    fn with_store<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut FixtureStore) -> anyhow::Result<T> + Send + 'static,
    ) -> BoxFuture<'static, anyhow::Result<T>> {
        let store = self.store.clone();

        self.respond(move || {
            let mut store = store
                .lock()
                .map_err(|_| anyhow::anyhow!("fixture store is poisoned"))?;
            work(&mut store)
        })
    }

    /// 在后台线程上等 `latency` 再算结果；没有延迟时就地算好。
    fn respond<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> BoxFuture<'static, anyhow::Result<T>> {
        if self.latency.is_zero() {
            return futures::future::ready(work()).boxed();
        }

        let latency = self.latency;
        let (sender, receiver) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            std::thread::sleep(latency);
            let _ = sender.send(work());
        });

        async move { receiver.await.context("fixture worker stopped")? }.boxed()
    }
}

/// 夹具里找不到记录时的错误（与 core 的“记录不存在”相当）。
fn missing(id: &str) -> anyhow::Error {
    anyhow::anyhow!("clipboard item not found: {id}")
}

impl ClipboardSource for FixtureSource {
    fn list(&self, query: ListQuery) -> BoxFuture<'static, anyhow::Result<Page>> {
        self.with_store(move |store| Ok(store.page(&query)))
    }

    /// 合成缩略图都已在资源目录里；文件名以 `missing-` 开头的模拟生成失败。
    fn thumbnail(&self, file_name: Arc<str>) -> BoxFuture<'static, anyhow::Result<PathBuf>> {
        let path = self.thumbnails.join(&*file_name);

        self.respond(move || {
            if file_name.starts_with("missing-") || !path.exists() {
                anyhow::bail!("no thumbnail for {file_name}");
            }
            Ok(path)
        })
    }

    fn settings(&self) -> Settings {
        let mut settings = Settings::default();
        if let Ok(view) = self.text_view.lock() {
            settings.clipboard.preview.text_view = *view;
        }
        // 截图验收换列表排布：`KP_LIST_STYLE`（card / seamless）、`KP_LIST_DENSITY`
        // （comfortable / standard / compact）。夹具只在自测里用；值不认识时保持默认。
        let display = &mut settings.clipboard.display;
        if let Some(style) = std::env::var("KP_LIST_STYLE")
            .ok()
            .and_then(|value| serde_json::from_value(serde_json::Value::String(value)).ok())
        {
            display.list_style = style;
        }
        if let Some(density) = std::env::var("KP_LIST_DENSITY")
            .ok()
            .and_then(|value| serde_json::from_value(serde_json::Value::String(value)).ok())
        {
            display.density = density;
        }
        settings
    }

    fn groups(&self) -> BoxFuture<'static, anyhow::Result<Vec<Group>>> {
        self.with_store(|store| Ok(store.groups()))
    }

    fn item_refs(&self, query: ListQuery) -> BoxFuture<'static, anyhow::Result<Vec<ItemRef>>> {
        self.with_store(move |store| Ok(store.refs(&query.filter)))
    }

    /// 夹具不碰系统剪贴板：只确认记录存在，按默认设置不隐藏窗口。
    fn copy(&self, id: Arc<str>, _plain: bool) -> BoxFuture<'static, anyhow::Result<bool>> {
        self.with_store(move |store| {
            store.find(&id).ok_or_else(|| missing(&id))?;
            Ok(false)
        })
    }

    fn toggle_favorite(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<bool>> {
        self.with_store(move |store| {
            let Some(original_ix) = store.index_of(&id) else {
                return Err(missing(&id));
            };
            let Some(item) = store.remove(&id) else {
                return Err(missing(&id));
            };
            let mut item = (*item).clone();
            item.is_favorite = !item.is_favorite;
            let favorite = item.is_favorite;
            if favorite && !item.is_pinned {
                let at = store
                    .items
                    .iter()
                    .position(|candidate| candidate.is_favorite && !candidate.is_pinned)
                    .unwrap_or_else(|| {
                        store
                            .items
                            .iter()
                            .take_while(|candidate| candidate.is_pinned)
                            .count()
                    });
                store.items.insert(at, Arc::new(item));
            } else {
                store
                    .items
                    .insert(original_ix.min(store.items.len()), Arc::new(item));
            }
            Ok(favorite)
        })
    }

    fn toggle_pinned(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<bool>> {
        self.with_store(move |store| {
            let pinned = !store.find(&id).ok_or_else(|| missing(&id))?.is_pinned;
            store.set_pinned(&id, pinned);
            Ok(pinned)
        })
    }

    fn reorder(
        &self,
        section: ReorderSection,
        id: Arc<str>,
        anchor: ReorderAnchor,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.with_store(move |store| {
            if store.reorder(section, &id, anchor) {
                Ok(())
            } else {
                Err(anyhow::anyhow!("记录不在可排序分区中"))
            }
        })
    }

    fn update_note(
        &self,
        id: Arc<str>,
        note: Option<String>,
    ) -> BoxFuture<'static, anyhow::Result<NoteSaved>> {
        self.with_store(move |store| {
            let note: Option<Arc<str>> = note
                .as_deref()
                .map(str::trim)
                .filter(|note| !note.is_empty())
                .map(Arc::from);
            let saved = note.clone();
            if !store.patch(&id, |item| item.note = saved) {
                return Err(missing(&id));
            }
            Ok(NoteSaved {
                note,
                auto_favorited: false,
            })
        })
    }

    fn text_content(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<String>> {
        self.with_store(move |store| {
            let item = store.find(&id).ok_or_else(|| missing(&id))?;
            Ok(item.summary.as_deref().unwrap_or(&item.content).to_owned())
        })
    }

    fn update_text_content(
        &self,
        _id: Arc<str>,
        _content: String,
    ) -> BoxFuture<'static, anyhow::Result<ListItem>> {
        Box::pin(async { anyhow::bail!("Content editing requires the core data source") })
    }

    fn delete(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<()>> {
        self.with_store(move |store| {
            store.remove(&id).ok_or_else(|| missing(&id))?;
            Ok(())
        })
    }

    fn delete_many(&self, ids: Vec<Arc<str>>) -> BoxFuture<'static, anyhow::Result<u64>> {
        self.with_store(move |store| {
            let removed = ids.iter().filter(|id| store.remove(id).is_some()).count();
            Ok(removed as u64)
        })
    }

    /// 链接取摘要（夹具的文本类记录摘要就是全文），文件取第一个路径。
    fn open_target(
        &self,
        id: Arc<str>,
        target: OpenTarget,
    ) -> BoxFuture<'static, anyhow::Result<Option<String>>> {
        self.with_store(move |store| {
            let item = store.find(&id).ok_or_else(|| missing(&id))?;
            Ok(match target {
                OpenTarget::Link => item.summary.as_deref().map(str::to_owned),
                OpenTarget::Email => item.summary.as_deref().map(|mail| format!("mailto:{mail}")),
                OpenTarget::Reveal => item.file_rows().first().map(|row| row.path.to_string()),
            })
        })
    }

    fn set_item_group(
        &self,
        id: Arc<str>,
        group_id: Option<Arc<str>>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.with_store(move |store| {
            if let Some(group_id) = &group_id
                && !store.groups.iter().any(|group| group.id == *group_id)
            {
                anyhow::bail!("group not found: {group_id}");
            }
            if !store.patch(&id, |item| item.group_id = group_id) {
                return Err(missing(&id));
            }
            Ok(())
        })
    }

    /// 夹具的图片只有缩略图，没有可另存的原图。
    fn image_save(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<ImageSave>> {
        self.respond(move || anyhow::bail!("fixture image {id} has no original to save"))
    }

    fn hide_group(&self, group: Group) -> BoxFuture<'static, anyhow::Result<()>> {
        self.with_store(move |store| {
            if !store.hide_group(&group.id) {
                anyhow::bail!("group not found: {}", group.id);
            }
            Ok(())
        })
    }

    fn delete_group(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<()>> {
        self.with_store(move |store| {
            if !store.delete_group(&id) {
                anyhow::bail!("group not found: {id}");
            }
            Ok(())
        })
    }

    fn create_group(&self, input: GroupInput) -> BoxFuture<'static, anyhow::Result<Group>> {
        self.with_store(move |store| store.create_group(input))
    }

    fn update_group(
        &self,
        id: Arc<str>,
        input: GroupInput,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.with_store(move |store| store.update_group(&id, input))
    }

    fn update_groups_layout(
        &self,
        order: Vec<Arc<str>>,
        visible: Vec<Arc<str>>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.with_store(move |store| {
            store.update_layout(&order, &visible);
            Ok(())
        })
    }

    fn preview(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<Option<Preview>>> {
        let thumbnails = self.thumbnails.clone();
        let text_view = self.text_view.lock().map(|view| *view).unwrap_or_default();

        self.with_store(move |store| {
            Ok(store
                .find(&id)
                .map(|item| fixture_preview(item, &thumbnails, text_view)))
        })
    }

    fn set_preview_text_view(
        &self,
        view: PreviewTextView,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let text_view = self.text_view.clone();

        self.respond(move || {
            *text_view
                .lock()
                .map_err(|_| anyhow::anyhow!("fixture settings are poisoned"))? = view;
            Ok(())
        })
    }

    /// 与 core 相同的基本校验：`.svg` 扩展名、`<svg` 开头。
    fn import_group_svg(&self, path: PathBuf) -> BoxFuture<'static, anyhow::Result<String>> {
        self.respond(move || {
            let is_svg = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"));
            if !is_svg {
                anyhow::bail!("请选择 SVG 文件");
            }
            let markup = std::fs::read_to_string(&path)?;
            if !markup.trim_start().starts_with("<svg") {
                anyhow::bail!("请选择有效的 SVG 图标");
            }
            Ok(markup)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../../../fixtures/list-sample.json");

    #[test]
    fn the_sample_fixture_parses_and_resolves_assets() {
        let store = FixtureStore::from_page_json(SAMPLE, Path::new("C:\\tmp\\fixtures"))
            .expect("sample fixture parses");
        assert!(store.len() >= 12);

        let page = store.page(&ListQuery {
            offset: 0,
            limit: 100,
            filter: ListFilter::default(),
            sort: Default::default(),
        });
        assert_eq!(page.total, store.len());
        let icon = page
            .items
            .iter()
            .find_map(|item| item.source_app_icon_path.clone())
            .expect("some item has an app icon");
        assert!(icon.starts_with("C:/tmp/fixtures/"), "{icon}");
        assert!(
            !page
                .items
                .iter()
                .any(|item| item.id.contains(ASSETS_PLACEHOLDER))
        );
    }

    /// 夹具的每一条都能按 core 的数据库行反序列化：必填字段齐全、类型一致。
    #[test]
    fn the_sample_fixture_has_the_core_row_shape() {
        let value: serde_json::Value = serde_json::from_str(SAMPLE).expect("json");
        let list = value
            .get("list")
            .and_then(serde_json::Value::as_array)
            .expect("list array");
        for entry in list {
            let row: Result<kwikpaste_core::db::models::ClipboardItem, _> =
                serde_json::from_value(entry.clone());
            assert!(row.is_ok(), "{entry}: {row:?}");
        }
    }

    /// core 展示层的 golden 输出（1.4.0 的列表 JSON）原样能读成 [`ListItem`]。
    #[test]
    fn core_golden_list_payloads_parse() {
        for json in [
            include_str!("../../../../kwikpaste-core/tests/fixtures/presenter/list-default.json"),
            include_str!(
                "../../../../kwikpaste-core/tests/fixtures/presenter/list-unredacted.json"
            ),
        ] {
            let store =
                FixtureStore::from_page_json(json, Path::new("/tmp")).expect("golden parses");
            assert!(store.len() > 10);
        }
    }

    #[test]
    fn store_mutations_keep_pinned_rows_first() {
        let store = FixtureStore::from_page_json(SAMPLE, Path::new("/tmp")).expect("parses");
        let mut store = store;
        let pinned = store.items.iter().take_while(|item| item.is_pinned).count();
        let first_regular = store.get(pinned).map(|item| item.id.clone()).expect("rows");

        assert!(store.set_pinned(&first_regular, true));
        assert_eq!(
            store.get(0).map(|item| item.id.clone()),
            Some(first_regular.clone())
        );
        assert!(store.get(0).is_some_and(|item| item.is_pinned));

        assert!(store.set_pinned(&first_regular, false));
        assert_eq!(
            store.get(pinned).map(|item| item.id.clone()),
            Some(first_regular.clone())
        );

        let removed = store.remove(&first_regular);
        assert!(removed.is_some());
        assert_eq!(store.index_of(&first_regular), None);
    }

    #[test]
    fn latency_runs_off_the_calling_thread() {
        let store = FixtureStore::from_page_json(SAMPLE, Path::new("/tmp")).expect("parses");
        let assets = AssetSet {
            root: PathBuf::from("/tmp"),
            images: Vec::new(),
            originals: Vec::new(),
            app_icons: Vec::new(),
            file_icons: Vec::new(),
        };
        let source = FixtureSource::new(store, &assets).with_latency(Duration::from_millis(5));

        let page = futures::executor::block_on(source.list(ListQuery {
            offset: 2,
            limit: 3,
            filter: ListFilter::default(),
            sort: Default::default(),
        }))
        .expect("page");
        assert_eq!(page.items.len(), 3);

        let missing = futures::executor::block_on(source.thumbnail(Arc::from("missing-1.png")));
        assert!(missing.is_err());
    }

    #[test]
    fn filters_and_mutations_follow_core_semantics() {
        use crate::clipboard::model::{filter::Range, item::ItemKind};

        let store = FixtureStore::from_page_json(SAMPLE, Path::new("/tmp"))
            .expect("parses")
            .with_groups(vec![(
                Group {
                    id: "g1".into(),
                    name: "工作".into(),
                    icon: "i-lets-icons:book".into(),
                    is_hidden: false,
                },
                vec![0, 2],
            )]);
        let total = store.len();
        let assets = AssetSet {
            root: PathBuf::from("/tmp"),
            images: Vec::new(),
            originals: Vec::new(),
            app_icons: Vec::new(),
            file_icons: Vec::new(),
        };
        let source = FixtureSource::new(store, &assets);
        let list = |filter: ListFilter| {
            futures::executor::block_on(source.list(ListQuery {
                offset: 0,
                limit: 100,
                filter,
                sort: Default::default(),
            }))
            .expect("page")
        };

        let grouped = list(ListFilter {
            group_id: Some("g1".into()),
            ..ListFilter::default()
        });
        assert_eq!(grouped.total, 2);

        let images = list(ListFilter {
            category: Some(ItemKind::Image),
            ..ListFilter::default()
        });
        assert!(images.items.iter().all(|item| item.kind == ItemKind::Image));
        assert!(images.total > 0 && images.total < total);

        let first = list(ListFilter::default())
            .items
            .first()
            .map(|item| item.id.clone())
            .expect("a record");
        let before = list(ListFilter {
            range: Range::Favorite,
            ..ListFilter::default()
        })
        .total;
        let favorite =
            futures::executor::block_on(source.toggle_favorite(first.clone())).expect("toggles");
        let after = list(ListFilter {
            range: Range::Favorite,
            ..ListFilter::default()
        })
        .total;
        assert_eq!(after, if favorite { before + 1 } else { before - 1 });

        let removed =
            futures::executor::block_on(source.delete_many(vec![first.clone(), "nope".into()]))
                .expect("deletes");
        assert_eq!(removed, 1);
        assert_eq!(list(ListFilter::default()).total, total - 1);

        futures::executor::block_on(source.delete_group("g1".into())).expect("deletes the group");
        assert_eq!(
            list(ListFilter {
                group_id: Some("g1".into()),
                ..ListFilter::default()
            })
            .total,
            0
        );
    }
}
