//! 列表的数据接口（适配层）。视图只认 [`ClipboardSource`]；数据来自哪里由这里的实现决定：
//!
//! - [`core_source::CoreSource`]：真正的 core（`Core::list_items`、CS2 的记录与分组操作），把展示层的
//!   `ClipboardItemView` 转成列表的 `ListItem`；
//! - [`FixtureSource`]：合成夹具（JSON 或生成器），不碰任何真实数据库，供自测和跑分使用。
//!
//! core 的记录操作不发事件（附录 D §3.3）：调用方按返回值就地改列表。分组的增删改由 core 发
//! `GroupsUpdated`，夹具没有事件，分组栏在操作成功后自己重读。

pub mod core_source;
mod fixture;
pub mod synthetic;

use std::{path::PathBuf, sync::Arc};

use futures::future::BoxFuture;
use kwikpaste_core::{
    db::models::ClipboardItemSort,
    ops::{ReorderAnchor, ReorderSection},
    settings::Settings,
};

pub use self::core_source::start_selftest_core;
pub use self::fixture::{FixtureSource, FixtureStore};
use super::model::{actions::OpenTarget, filter::ListFilter, item::ItemRef, list_model::Page};

/// 一次列表查询：分页加上当前视图的筛选条件和排序（core `ClipboardItemQuery`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListQuery {
    pub offset: usize,
    pub limit: usize,
    pub filter: ListFilter,
    pub sort: ClipboardItemSort,
}

/// 一个自定义分组（core `ClipboardGroup`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: Arc<str>,
    pub name: Arc<str>,
    /// 预设图标的类名（`i-lets-icons:folder`）或自定义 SVG 源码。
    pub icon: Arc<str>,
    pub is_hidden: bool,
}

pub use kwikpaste_core::presenter::{
    ClipboardPreviewPayload as PreviewPayload, PreviewContentMetrics,
};
pub use kwikpaste_core::settings::PreviewTextView;

/// 预览窗的数据（core `preview_payload`）与定尺寸用的内容度量（core `preview_metrics`）。
#[derive(Clone, Debug)]
pub struct Preview {
    pub payload: PreviewPayload,
    pub metrics: PreviewContentMetrics,
}

/// 新建或编辑分组的输入（core `ClipboardGroupInput`）：`icon` 是预设图标的类名或 SVG 源码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInput {
    pub name: String,
    pub icon: String,
    pub is_hidden: bool,
}

/// 图片另存的来源（core `ImageSave`）：原图路径和默认文件名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSave {
    pub source: PathBuf,
    pub file_name: String,
}

/// 备注保存的结果（core `UpdateNoteResult`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteSaved {
    /// 归一化后的备注：去掉首尾空白，空串为 `None`。
    pub note: Option<Arc<str>>,
    /// 设置了“备注自动收藏”，这条顺带收藏了。
    pub auto_favorited: bool,
}

/// 列表的数据来源。实现必须能在任意线程上被调用，返回的 future 在 UI 线程上 await。
pub trait ClipboardSource: Send + Sync + 'static {
    /// 取一页列表载荷（core 已经裁剪、脱敏，并算好文件条目、可用动作等）。
    fn list(&self, query: ListQuery) -> BoxFuture<'static, anyhow::Result<Page>>;

    /// 确保图片记录的缩略图存在并返回路径（1.x `get_clipboard_image_path(fileName, thumbnail: true)`）。
    /// 列表载荷只带已生成的缩略图，没有时卡片先画骨架再调这里。
    fn thumbnail(&self, file_name: Arc<str>) -> BoxFuture<'static, anyhow::Result<PathBuf>>;

    /// 当前设置（快捷动作、删除保护、点击行为、搜索框和打开窗口时的偏好）。之后的变化走 core 事件。
    fn settings(&self) -> Settings;

    /// 全部自定义分组，按排序。
    fn groups(&self) -> BoxFuture<'static, anyhow::Result<Vec<Group>>>;

    /// 当前视图全部记录的 id 与保护标记（多选全选、跨页连选）。
    fn item_refs(&self, query: ListQuery) -> BoxFuture<'static, anyhow::Result<Vec<ItemRef>>>;

    /// 写回剪贴板（不粘贴）。返回设置是否要求随后隐藏窗口。
    fn copy(&self, id: Arc<str>, plain: bool) -> BoxFuture<'static, anyhow::Result<bool>>;

    /// 把图片识别出的文字写回剪贴板；夹具没有识别文字，真实数据源转给 core。
    fn copy_image_text(&self, _id: Arc<str>) -> BoxFuture<'static, anyhow::Result<bool>> {
        Box::pin(async { anyhow::bail!("image text is unavailable for this source") })
    }

    /// 翻转收藏，返回新状态。
    fn toggle_favorite(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<bool>>;

    /// 翻转置顶，返回新状态。
    fn toggle_pinned(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<bool>>;

    /// 在置顶或收藏分区内按锚点重排。
    fn reorder(
        &self,
        section: ReorderSection,
        id: Arc<str>,
        anchor: ReorderAnchor,
    ) -> BoxFuture<'static, anyhow::Result<()>>;

    fn update_note(
        &self,
        id: Arc<str>,
        note: Option<String>,
    ) -> BoxFuture<'static, anyhow::Result<NoteSaved>>;

    /// 读取完整文本表示供内容编辑；富文本使用采集时的纯文本表示。
    fn text_content(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<String>>;

    /// 保存内容并返回重新计算后的列表载荷。
    fn update_text_content(
        &self,
        id: Arc<str>,
        content: String,
    ) -> BoxFuture<'static, anyhow::Result<super::model::item::ListItem>>;

    fn delete(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<()>>;

    /// 批量删除，返回实际删除条数。
    fn delete_many(&self, ids: Vec<Arc<str>>) -> BoxFuture<'static, anyhow::Result<u64>>;

    /// “打开”的目标：链接、`mailto:` 地址或要在文件管理器里定位的路径；内容为空时为 `None`。
    fn open_target(
        &self,
        id: Arc<str>,
        target: OpenTarget,
    ) -> BoxFuture<'static, anyhow::Result<Option<String>>>;

    /// 把记录移到自定义分组；`None` 移出分组。
    fn set_item_group(
        &self,
        id: Arc<str>,
        group_id: Option<Arc<str>>,
    ) -> BoxFuture<'static, anyhow::Result<()>>;

    /// 图片另存：校验是图片且原图还在，给出原图路径与默认文件名。
    fn image_save(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<ImageSave>>;

    /// 在分组栏隐藏一个分组（名称、图标不变）。
    fn hide_group(&self, group: Group) -> BoxFuture<'static, anyhow::Result<()>>;

    /// 删除分组，组内记录回到未分组。
    fn delete_group(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<()>>;

    /// 新建分组（排在最后），返回建好的分组。名称去首尾空白，空名称、超过 32 个字报错。
    fn create_group(&self, input: GroupInput) -> BoxFuture<'static, anyhow::Result<Group>>;

    /// 改分组的名称、图标和显隐。
    fn update_group(
        &self,
        id: Arc<str>,
        input: GroupInput,
    ) -> BoxFuture<'static, anyhow::Result<()>>;

    /// 一次保存全部分组的顺序和显隐（`visible` 是显示在分组栏上的分组）。
    fn update_groups_layout(
        &self,
        order: Vec<Arc<str>>,
        visible: Vec<Arc<str>>,
    ) -> BoxFuture<'static, anyhow::Result<()>>;

    /// 读取用户选的 SVG 文件作为分组图标（扩展名、大小、内容经过校验），返回 SVG 源码。
    fn import_group_svg(&self, path: PathBuf) -> BoxFuture<'static, anyhow::Result<String>>;

    /// 预览窗的数据与内容度量；记录已经不在时为 `None`。
    fn preview(&self, id: Arc<str>) -> BoxFuture<'static, anyhow::Result<Option<Preview>>>;

    /// 图片识别出的文字按文本记录的样子给预览窗（纯文本 / 选词视图）；没有识别文字时为 `None`。
    /// 夹具没有识别文字。
    fn image_text_preview(
        &self,
        _id: Arc<str>,
    ) -> BoxFuture<'static, anyhow::Result<Option<Preview>>> {
        Box::pin(async { Ok(None) })
    }

    /// 改预览文本的展示方式（设置 `clipboard.preview.textView`）。
    fn set_preview_text_view(
        &self,
        view: PreviewTextView,
    ) -> BoxFuture<'static, anyhow::Result<()>>;
}
