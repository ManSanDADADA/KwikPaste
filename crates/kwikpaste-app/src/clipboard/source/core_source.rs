//! 接 core 的适配器：`Core::list_items` 的 `ClipboardItemView` 转成列表的 [`ListItem`]，筛选条件转成
//! `ClipboardItemQuery`，记录与分组操作直接调 core 的 CS2 接口。
//!
//! core 的公开 async 方法自己跳到 core runtime 上执行，这里的 future 可以直接在 GPUI 的执行器里 await。

use std::{path::PathBuf, sync::Arc};

use futures::{FutureExt as _, future::BoxFuture};
use kwikpaste_core::{
    AppEnv, AppInfo, Core, CoreEvent, CoreOptions, CorePaths, CoreRuntime,
    clipboard::{ClipboardPayload, ImagePayload, MemoryClipboard, TextPayload},
    db::models::{
        ClipboardGroup, ClipboardItemQuery, ClipboardKind, ClipboardSubKind,
        Platform as CorePlatform,
    },
    ops::{ClipboardGroupInput, ClipboardGroupLayoutInput},
    presenter::{ClipboardAction, ClipboardItemView, FileEntry, FilesPreviewKind},
    settings::Settings,
};

use super::{
    ClipboardSource, Group, GroupInput, ImageSave, ListQuery, NoteSaved, Preview, PreviewTextView,
    synthetic::{self, AssetSet, GenerateOptions},
};
use crate::clipboard::model::{
    actions::OpenTarget,
    filter::ListFilter,
    item::{
        FileRow, FilesPreview, ItemAction, ItemKind, ItemRef, ListItem, Platform, SubKind,
        TextSnippet,
    },
    layout::ImageBox,
    list_model::Page,
};

/// 以 core 为数据源。
pub struct CoreSource {
    core: Core,
    /// 自测时由这里持有 core 的 runtime；正式入口由宿主持有，这里为 `None`。
    _runtime: Option<Arc<CoreRuntime>>,
}

impl CoreSource {
    pub fn new(core: Core) -> Self {
        Self {
            core,
            _runtime: None,
        }
    }

    /// 连同 runtime 一起持有：数据源活多久，core 的线程就活多久。
    pub fn with_runtime(core: Core, runtime: Arc<CoreRuntime>) -> Self {
        Self {
            _runtime: Some(runtime),
            ..Self::new(core)
        }
    }
}

/// 列表的筛选条件转成 core 的查询（1.x `itemQuery`：`favorite`、`groupId`、`keyword`、`kind`、`sort`）。
pub fn item_query(query: &ListQuery) -> ClipboardItemQuery {
    let ListFilter {
        range: _,
        category,
        group_id,
        keyword,
    } = &query.filter;

    ClipboardItemQuery {
        kind: category.map(|kind| match kind {
            ItemKind::Text => ClipboardKind::Text,
            ItemKind::Image => ClipboardKind::Image,
            ItemKind::Files => ClipboardKind::Files,
        }),
        group_id: group_id.as_deref().map(str::to_owned),
        favorite: query.filter.favorites().then_some(true),
        group: Some(match category {
            Some(ItemKind::Text) => kwikpaste_core::db::models::ClipboardGroupFilter::Text,
            Some(ItemKind::Image) => kwikpaste_core::db::models::ClipboardGroupFilter::Image,
            Some(ItemKind::Files) => kwikpaste_core::db::models::ClipboardGroupFilter::Files,
            None => kwikpaste_core::db::models::ClipboardGroupFilter::All,
        }),
        keyword: (!keyword.is_empty()).then(|| keyword.to_string()),
        sort: query.sort,
        limit: i64::try_from(query.limit).unwrap_or(i64::MAX),
        offset: i64::try_from(query.offset).unwrap_or(i64::MAX),
        ..ClipboardItemQuery::default()
    }
}

fn group_input(input: GroupInput) -> ClipboardGroupInput {
    ClipboardGroupInput {
        name: input.name,
        icon: input.icon,
        is_hidden: input.is_hidden,
    }
}

fn group(group: ClipboardGroup) -> Group {
    Group {
        id: shared(group.id),
        name: shared(group.name),
        icon: shared(group.icon),
        is_hidden: group.is_hidden,
    }
}

impl ClipboardSource for CoreSource {
    fn list(&self, query: ListQuery) -> BoxFuture<'static, anyhow::Result<Page>> {
        let core = self.core.clone();

        async move {
            let page = core.list_items(item_query(&query)).await?;

            Ok(Page {
                items: page
                    .list
                    .into_iter()
                    .map(|view| Arc::new(ListItem::from(view)))
                    .collect(),
                total: usize::try_from(page.total).unwrap_or_default(),
            })
        }
        .boxed()
    }

    fn thumbnail(&self, file_name: Arc<str>) -> BoxFuture<'static, anyhow::Result<PathBuf>> {
        let core = self.core.clone();

        async move { Ok(core.ensure_thumbnail(&file_name).await?) }.boxed()
    }

    fn settings(&self) -> Settings {
        self.core.settings()
    }

    fn groups(&self) -> BoxFuture<'static, anyhow::Result<Vec<Group>>> {
        let core = self.core.clone();

        async move { Ok(core.list_groups().await?.into_iter().map(group).collect()) }.boxed()
    }

    fn item_refs(&self, query: ListQuery) -> BoxFuture<'static, anyhow::Result<Vec<ItemRef>>> {
        let core = self.core.clone();

        async move {
            let refs = core.list_item_refs(item_query(&query)).await?;
            Ok(refs
                .into_iter()
                .map(|item| ItemRef {
                    id: shared(item.id),
                    is_favorite: item.is_favorite,
                    is_pinned: item.is_pinned,
                })
                .collect())
        }
        .boxed()
    }

    fn copy(&self, id: Arc<str>, plain: bool) -> BoxFuture<'static, anyhow::Result<bool>> {
        let core = self.core.clone();

        async move { Ok(core.copy_item(&id, plain).await?.hide_window) }.boxed()
    }

    fn copy_image_text(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<bool>> {
        let core = self.core.clone();
        async move { Ok(core.copy_image_text(&id).await?.hide_window) }.boxed()
    }

    fn toggle_favorite(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<bool>> {
        let core = self.core.clone();

        async move { Ok(core.toggle_favorite(&id).await?) }.boxed()
    }

    fn toggle_pinned(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<bool>> {
        let core = self.core.clone();

        async move { Ok(core.toggle_pinned(&id).await?) }.boxed()
    }

    fn reorder(
        &self,
        section: kwikpaste_core::ops::ReorderSection,
        id: Arc<str>,
        anchor: kwikpaste_core::ops::ReorderAnchor,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let core = self.core.clone();

        async move { Ok(core.reorder_item(section, &id, anchor).await?) }.boxed()
    }

    fn update_note(
        &self,
        id: Arc<str>,
        note: Option<String>,
    ) -> BoxFuture<'static, anyhow::Result<NoteSaved>> {
        let core = self.core.clone();

        async move {
            let result = core.update_note(&id, note).await?;
            Ok(NoteSaved {
                note: result.note.map(shared),
                auto_favorited: result.auto_favorited,
            })
        }
        .boxed()
    }

    fn text_content(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<String>> {
        let core = self.core.clone();
        async move {
            let item = core
                .find_item(&id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Text record no longer exists"))?;
            anyhow::ensure!(
                item.kind == ClipboardKind::Text,
                "Only text records can be edited"
            );
            Ok(
                if matches!(
                    item.sub_kind,
                    Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf)
                ) {
                    item.search_text.unwrap_or(item.content)
                } else {
                    item.content
                },
            )
        }
        .boxed()
    }

    fn update_text_content(
        &self,
        id: Arc<str>,
        content: String,
    ) -> BoxFuture<'static, anyhow::Result<ListItem>> {
        let core = self.core.clone();
        async move {
            core.update_text_content(&id, content).await?;
            let view = core
                .list_item(&id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Text record no longer exists"))?;
            Ok(ListItem::from(view))
        }
        .boxed()
    }

    fn delete(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<()>> {
        let core = self.core.clone();

        async move { Ok(core.delete_item(&id).await?) }.boxed()
    }

    fn delete_many(&self, ids: Vec<Arc<str>>) -> BoxFuture<'static, anyhow::Result<u64>> {
        let core = self.core.clone();
        let ids = ids.iter().map(|id| id.to_string()).collect();

        async move { Ok(core.delete_items(ids).await?) }.boxed()
    }

    fn open_target(
        &self,
        id: Arc<str>,
        target: OpenTarget,
    ) -> BoxFuture<'static, anyhow::Result<Option<String>>> {
        let core = self.core.clone();

        async move {
            Ok(match target {
                OpenTarget::Link => core.link_target(&id, false).await?,
                OpenTarget::Email => core.link_target(&id, true).await?,
                OpenTarget::Reveal => core.reveal_target(&id).await?,
            })
        }
        .boxed()
    }

    fn set_item_group(
        &self,
        id: Arc<str>,
        group_id: Option<Arc<str>>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let core = self.core.clone();

        async move {
            match group_id {
                Some(group_id) => core.set_item_group(&id, &group_id).await?,
                None => core.clear_item_group(&id).await?,
            }
            Ok(())
        }
        .boxed()
    }

    fn image_save(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<ImageSave>> {
        let core = self.core.clone();

        async move {
            let save = core.prepare_image_save(&id).await?;
            Ok(ImageSave {
                source: save.source,
                file_name: save.default_file_name,
            })
        }
        .boxed()
    }

    fn hide_group(&self, group: Group) -> BoxFuture<'static, anyhow::Result<()>> {
        let core = self.core.clone();

        async move {
            let input = ClipboardGroupInput {
                name: group.name.to_string(),
                icon: group.icon.to_string(),
                is_hidden: true,
            };
            Ok(core.update_group(&group.id, input).await?)
        }
        .boxed()
    }

    fn delete_group(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<()>> {
        let core = self.core.clone();

        async move { Ok(core.delete_group(&id).await?) }.boxed()
    }

    fn create_group(&self, input: GroupInput) -> BoxFuture<'static, anyhow::Result<Group>> {
        let core = self.core.clone();

        async move { Ok(group(core.create_group(group_input(input)).await?)) }.boxed()
    }

    fn update_group(
        &self,
        id: Arc<str>,
        input: GroupInput,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let core = self.core.clone();

        async move { Ok(core.update_group(&id, group_input(input)).await?) }.boxed()
    }

    fn update_groups_layout(
        &self,
        order: Vec<Arc<str>>,
        visible: Vec<Arc<str>>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let core = self.core.clone();
        let input = ClipboardGroupLayoutInput {
            order: order.iter().map(|id| id.to_string()).collect(),
            visible_ids: visible.iter().map(|id| id.to_string()).collect(),
        };

        async move { Ok(core.update_groups_layout(input).await?) }.boxed()
    }

    fn import_group_svg(&self, path: PathBuf) -> BoxFuture<'static, anyhow::Result<String>> {
        let core = self.core.clone();

        async move { Ok(core.import_group_svg(&path)?) }.boxed()
    }

    fn preview(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<Option<Preview>>> {
        let core = self.core.clone();

        async move {
            let (payload, metrics) =
                futures::try_join!(core.preview_payload(&id), core.preview_metrics(&id))?;
            Ok(payload
                .zip(metrics)
                .map(|(payload, metrics)| Preview { payload, metrics }))
        }
        .boxed()
    }

    fn image_text_preview(
        &self,
        id: Arc<str>,
    ) -> BoxFuture<'static, anyhow::Result<Option<Preview>>> {
        let core = self.core.clone();

        async move {
            Ok(core
                .image_text_preview(&id)
                .await?
                .map(|(payload, metrics)| Preview { payload, metrics }))
        }
        .boxed()
    }

    fn set_preview_text_view(
        &self,
        view: PreviewTextView,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let core = self.core.clone();
        let patch = serde_json::json!({ "clipboard": { "preview": { "textView": view } } });

        async move {
            core.update_settings(patch).await?;
            Ok(())
        }
        .boxed()
    }
}

/// 自测：在临时目录里启动一个独立的 core（开发环境、内存剪贴板、不启动监听），灌入合成记录。
///
/// 只碰 `<临时目录>/kwikpaste-selftest-core-<pid>`，不读本机任何 1.x 或开发版数据，不碰系统剪贴板。
pub fn start_selftest_core(assets: &AssetSet) -> anyhow::Result<CoreSource> {
    let root = std::env::temp_dir().join(format!("kwikpaste-selftest-core-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let local = root.join("local");
    let paths = CorePaths::new(AppEnv::Dev, local.clone(), local.join("logs"), None);
    let runtime = Arc::new(CoreRuntime::new()?);
    let info = AppInfo {
        name: kwikpaste_core::APP_NAME,
        identifier: SELFTEST_IDENTIFIER,
        version: semver::Version::parse(env!("CARGO_PKG_VERSION"))?,
        env: AppEnv::Dev,
    };
    let sink = |event: CoreEvent| log::debug!("selftest core event: {event:?}");

    let core = futures::executor::block_on(Core::start(
        info,
        paths,
        CoreOptions::default(),
        Arc::new(sink),
        runtime.handle(),
    ))?;
    core.set_clipboard_provider(Arc::new(MemoryClipboard::default()));
    futures::executor::block_on(seed(&core, assets))?;
    log::info!("selftest core started in {}", root.display());

    Ok(CoreSource::with_runtime(core, runtime))
}

/// 自测 core 的 identifier（只写进它自己临时目录里的 storage.json）。
const SELFTEST_IDENTIFIER: &str = "com.fastthree.kwikpaste.native-dev.selftest";

/// 按 core 的采集流程（`build_item` → `store_item`）存几十条合成记录：文本、各子类型、图片、文件。
async fn seed(core: &Core, assets: &AssetSet) -> anyhow::Result<()> {
    let mut payloads = Vec::new();
    for index in 0..24usize {
        let item = synthetic::item(
            assets,
            index,
            GenerateOptions {
                pinned: 0,
                missing_thumbnails: false,
                rows: 24,
            },
        );
        let payload = match item.kind {
            ItemKind::Text => item.summary.as_ref().map(|summary| {
                ClipboardPayload::Text(TextPayload {
                    text: summary.to_string(),
                    html: None,
                    rtf: None,
                })
            }),
            ItemKind::Image => match item.image_thumbnail_path.as_deref() {
                Some(path) => {
                    let bytes = std::fs::read(path)?;
                    let (width, height) = image::image_dimensions(path)?;
                    Some(ClipboardPayload::Image(ImagePayload {
                        bytes,
                        width,
                        height,
                    }))
                }
                None => None,
            },
            ItemKind::Files => Some(ClipboardPayload::Files(
                assets
                    .app_icons
                    .iter()
                    .take(1 + index % 3)
                    .map(|path| path.to_string())
                    .collect(),
            )),
        };
        payloads.extend(payload);
    }

    // 最旧的先存，列表按更新时间倒序，最后存的在最上面。
    for payload in payloads.into_iter().rev() {
        if let Some(item) = core.build_item(&payload)? {
            core.store_item(item, None).await?;
        }
    }

    Ok(())
}

fn shared(text: String) -> Arc<str> {
    Arc::from(text)
}

fn dimension(value: Option<i64>) -> Option<u32> {
    value.and_then(|value| u32::try_from(value).ok())
}

/// core 的记录类型转成列表的类型。
pub fn item_kind(kind: ClipboardKind) -> ItemKind {
    match kind {
        ClipboardKind::Text => ItemKind::Text,
        ClipboardKind::Image => ItemKind::Image,
        ClipboardKind::Files => ItemKind::Files,
    }
}

impl From<ClipboardItemView> for ListItem {
    fn from(view: ClipboardItemView) -> Self {
        let item = view.item;

        Self {
            id: shared(item.id),
            kind: item_kind(item.kind),
            sub_kind: item.sub_kind.map(|sub_kind| match sub_kind {
                ClipboardSubKind::Rtf => SubKind::Rtf,
                ClipboardSubKind::Html => SubKind::Html,
                ClipboardSubKind::Url => SubKind::Url,
                ClipboardSubKind::Email => SubKind::Email,
                ClipboardSubKind::Color => SubKind::Color,
                ClipboardSubKind::Path => SubKind::Path,
            }),
            group_id: item.group_id.map(shared),
            content: shared(item.content),
            summary: item.summary.map(shared),
            width: dimension(item.width),
            height: dimension(item.height),
            is_favorite: item.is_favorite,
            is_pinned: item.is_pinned,
            is_sensitive: item.is_sensitive,
            platform: match item.platform {
                CorePlatform::Macos => Platform::Macos,
                CorePlatform::Windows => Platform::Windows,
            },
            note: item.note.map(shared),
            created_at: item.created_at,
            source_app_id: item.source_app_id.map(shared),
            source_app_name: item.source_app_name.map(shared),
            source_app_icon_path: view.source_app_icon_path.map(shared),
            origin_device_id: item.origin_device_id.map(shared),
            origin_device_name: view.origin_device_name.map(shared),
            image_thumbnail_path: view.image_thumbnail_path.map(shared),
            file_entries: view
                .file_entries
                .map(|entries| entries.into_iter().map(FileRow::from).collect()),
            files_preview_kind: view.files_preview_kind.map(|kind| match kind {
                FilesPreviewKind::ImagePreview => FilesPreview::ImagePreview,
                FilesPreviewKind::List => FilesPreview::List,
            }),
            available_actions: view
                .available_actions
                .into_iter()
                .map(ItemAction::from)
                .collect(),
            color_preview: view.color_preview.map(shared),
            quick_snippets: view.quick_snippets.into_iter().map(shared).collect(),
            image_display: view.image_display_size.map(|size| ImageBox {
                width: size.width as f32,
                height: size.height as f32,
            }),
            has_image_text: view.has_image_text,
            image_text_snippet: view.image_text_snippet.map(|snippet| TextSnippet {
                text: shared(snippet.text),
                matched: snippet.matched,
            }),
        }
    }
}

impl From<FileEntry> for FileRow {
    fn from(entry: FileEntry) -> Self {
        Self {
            path: shared(entry.path),
            name: shared(entry.name),
            is_dir: entry.is_dir,
            is_image: entry.is_image,
            exists: entry.exists,
            icon_path: entry.icon_path.map(shared),
            width: None,
            height: None,
        }
    }
}

impl From<ClipboardAction> for ItemAction {
    fn from(action: ClipboardAction) -> Self {
        match action {
            ClipboardAction::Paste => Self::Paste,
            ClipboardAction::PasteAsPlainText => Self::PasteAsPlainText,
            ClipboardAction::PasteAsPath => Self::PasteAsPath,
            ClipboardAction::Copy => Self::Copy,
            ClipboardAction::SaveImage => Self::SaveImage,
            ClipboardAction::CopyImageText => Self::CopyImageText,
            ClipboardAction::SplitWords => Self::SplitWords,
            ClipboardAction::OpenLink => Self::OpenLink,
            ClipboardAction::SendEmail => Self::SendEmail,
            ClipboardAction::RevealInFinder => Self::RevealInFinder,
            ClipboardAction::RevealInExplorer => Self::RevealInExplorer,
            ClipboardAction::ToggleFavorite => Self::ToggleFavorite,
            ClipboardAction::TogglePinned => Self::TogglePinned,
            ClipboardAction::EditNote => Self::EditNote,
            ClipboardAction::Select => Self::Select,
            ClipboardAction::Delete => Self::Delete,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::clipboard::model::item::FilesPreview;

    /// 真 core（临时目录、内存剪贴板）存入合成记录，经适配器读回列表。
    #[test]
    fn lists_seeded_records_through_the_real_core() {
        let dir = std::env::temp_dir().join(format!("kp-core-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let png = |name: &str, width: u32, height: u32| -> Arc<str> {
            let path = dir.join(name);
            image::RgbaImage::from_pixel(width, height, image::Rgba([40, 90, 200, 255]))
                .save(&path)
                .expect("write png");
            Arc::from(path.to_string_lossy().as_ref())
        };
        let images = (0..2)
            .map(|index| synthetic::SyntheticImage {
                file_name: Arc::from(format!("seed-{index}.png")),
                width: 120,
                height: 60,
                thumbnail: png(&format!("seed-{index}.png"), 120, 60),
            })
            .collect();
        let assets = AssetSet {
            root: PathBuf::from(&dir),
            images,
            originals: Vec::new(),
            app_icons: vec![png("icon-a.png", 16, 16), png("icon-b.png", 16, 16)],
            file_icons: Vec::new(),
        };

        let source = start_selftest_core(&assets).expect("core starts");
        let page = futures::executor::block_on(source.list(ListQuery {
            offset: 0,
            limit: 100,
            filter: ListFilter::default(),
            sort: Default::default(),
        }))
        .expect("lists");

        assert_eq!(page.total, page.items.len());
        assert!(page.items.iter().any(|item| item.kind == ItemKind::Text));
        let image = page
            .items
            .iter()
            .find(|item| item.kind == ItemKind::Image)
            .expect("an image record");
        assert!(image.image_display.is_some(), "core sends the display size");
        assert!(
            page.items
                .iter()
                .any(|item| { item.kind == ItemKind::Files && item.files_preview_kind.is_some() })
        );
        assert!(
            page.items
                .iter()
                .all(|item| !item.available_actions.is_empty())
        );
        // 单个存在的 PNG 文件按图片预览显示。
        assert!(
            page.items
                .iter()
                .any(|item| { item.files_preview_kind == Some(FilesPreview::ImagePreview) })
        );

        let thumbnail = futures::executor::block_on(source.thumbnail(image.content.clone()))
            .expect("thumbnail generated");
        assert!(thumbnail.exists());

        // CS2 的操作经适配器生效，筛选条件映射到 core 的查询。
        let text = page
            .items
            .iter()
            .find(|item| item.kind == ItemKind::Text)
            .expect("a text record")
            .id
            .clone();
        let favorite = futures::executor::block_on(source.toggle_favorite(text.clone()))
            .expect("toggles favorite");
        assert!(favorite);
        let favorites = futures::executor::block_on(source.list(ListQuery {
            offset: 0,
            limit: 100,
            filter: ListFilter {
                range: crate::clipboard::model::filter::Range::Favorite,
                ..ListFilter::default()
            },
            sort: Default::default(),
        }))
        .expect("lists favorites");
        assert_eq!(favorites.total, 1);
        assert_eq!(
            favorites.items.first().map(|item| item.id.clone()),
            Some(text.clone())
        );

        let saved =
            futures::executor::block_on(source.update_note(text.clone(), Some("  备注 ".into())))
                .expect("saves the note");
        assert_eq!(saved.note.as_deref(), Some("备注"));

        let images = futures::executor::block_on(source.item_refs(ListQuery {
            offset: 0,
            limit: 0,
            filter: ListFilter {
                category: Some(ItemKind::Image),
                ..ListFilter::default()
            },
            sort: Default::default(),
        }))
        .expect("lists refs");
        assert!(!images.is_empty());
        assert!(images.len() < page.total);

        futures::executor::block_on(source.delete(text.clone())).expect("deletes");
        let after = futures::executor::block_on(source.list(ListQuery {
            offset: 0,
            limit: 100,
            filter: ListFilter::default(),
            sort: Default::default(),
        }))
        .expect("lists again");
        assert_eq!(after.total, page.total - 1);
        assert!(
            futures::executor::block_on(source.groups())
                .expect("groups")
                .is_empty()
        );

        drop(source);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("kwikpaste-selftest-core-{}", std::process::id())),
        );
    }
}
