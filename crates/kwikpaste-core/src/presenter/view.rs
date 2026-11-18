//! 列表卡片的视图模型。
//!
//! [`ClipboardItemView`] 由数据库行 [`ClipboardItem`] 加上展示层算出的字段组成；序列化结果与
//! 1.4.0 发给前端的 `ClipboardItem` JSON 一致（行字段在前、附加字段在后，键名与取舍规则相同）。

use serde::{Deserialize, Serialize};

use super::ImageDisplaySize;
use crate::clipboard::QuickSnippet;
use crate::db::models::ClipboardItem;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardItemView {
    #[serde(skip)]
    pub has_image_text: bool,
    #[serde(skip)]
    pub image_text_matched: bool,
    /// 靠识别文字命中搜索时，命中处附近的一小段文字，卡片在缩略图下展示。
    #[serde(skip)]
    pub image_text_snippet: Option<crate::ocr::TextSnippet>,
    /// 译文开头（最多 512 个字符，与摘要的上限一致）；没有可显示的译文时为 `None`。
    #[serde(skip)]
    pub translation: Option<String>,
    /// 正在翻译：排队中或请求中。
    #[serde(skip)]
    pub translating: bool,
    /// 数据库行。列表查询里文本记录的 `content` / `search_text` 已置空，卡片用 `summary` 渲染；
    /// 敏感内容按设置脱敏后 `summary` 是遮罩过的。
    #[serde(flatten)]
    pub item: ClipboardItem,
    /// 来源应用图标的磁盘绝对路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app_icon_path: Option<String>,
    /// 来源设备名称：局域网同步收到的记录显示「来自 xxx」。同步接入前恒为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_device_name: Option<String>,
    /// 图片记录**已生成**的缩略图绝对路径；还没生成时为 `None`，界面先画同尺寸占位再请求生成。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_thumbnail_path: Option<String>,
    /// 文件记录按设置截断后的文件条目。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_entries: Option<Vec<FileEntry>>,
    /// 文件卡片的渲染模式：单文件且为存在的图片时直接预览图片。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_preview_kind: Option<FilesPreviewKind>,
    /// 右键菜单可用的动作，按建议展示顺序排列。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available_actions: Vec<ClipboardAction>,
    /// 颜色记录的可信 CSS 颜色串，可直接用作色块背景。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_preview: Option<String>,
    /// 显示时间：今天 `HH:mm`、今年内 `MM-DD HH:mm`、跨年 `YYYY-MM-DD HH:mm`。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub display_created_at: String,
    /// 卡片下方可单独粘贴的快捷信息（编号、数字、链接等）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quick_snippets: Vec<QuickSnippet>,
    /// 图片卡片的显示尺寸，缩略图到达前后布局一致。1.x 由前端计算，不在 JSON 里。
    #[serde(skip)]
    pub image_display_size: Option<ImageDisplaySize>,
}

impl ClipboardItemView {
    /// 只带数据库行、附加字段全空的视图，展示层随后逐项补齐。
    pub(crate) fn bare(item: ClipboardItem) -> Self {
        Self {
            item,
            source_app_icon_path: None,
            origin_device_name: None,
            image_thumbnail_path: None,
            file_entries: None,
            files_preview_kind: None,
            available_actions: Vec::new(),
            color_preview: None,
            display_created_at: String::new(),
            quick_snippets: Vec::new(),
            image_display_size: None,
            has_image_text: false,
            image_text_matched: false,
            image_text_snippet: None,
            translation: None,
            translating: false,
        }
    }
}

/// 列表查询的一页结果：项 + 当前过滤下的总数 + 是否还有下一页（`offset + list.len() < total`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardItemPage {
    pub list: Vec<ClipboardItemView>,
    pub total: i64,
    pub has_more: bool,
}

/// 右键菜单可执行的动作种类。顺序约定见 `presenter::list::compute_available_actions`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipboardAction {
    /// 普通粘贴（恒在）。
    Paste,
    /// 文本条目额外提供「粘贴为纯文本」。
    PasteAsPlainText,
    /// 文件条目额外提供「粘贴为路径」（写回纯文本路径列表）。
    PasteAsPath,
    /// 复制回剪贴板（恒在）。
    Copy,
    /// 将图片条目另存到本地文件（`kind = image`）。
    SaveImage,
    /// 复制图片已识别的非空文字（OCR 启用时）。
    CopyImageText,
    /// 打开拆词面板，按词挑选后粘贴或复制（`kind = text`）。
    SplitWords,
    /// 翻译这条文本（翻译扩展启用、记录可翻译且还没有译文时）。
    Translate,
    /// 复制译文（有译文时）。
    CopyTranslation,
    /// 粘贴译文（有译文时）。
    PasteTranslation,
    /// 在浏览器打开链接（`sub_kind = url`）。
    OpenLink,
    /// 调起邮件客户端（`sub_kind = email`）。
    SendEmail,
    /// 在 Finder 中显示（macOS，`sub_kind = path` 或 `kind = files`）。
    RevealInFinder,
    /// 在资源管理器中显示（Windows，`sub_kind = path` 或 `kind = files`）。
    RevealInExplorer,
    /// 切换收藏（恒在；界面按 `is_favorite` 切「收藏 / 取消收藏」文案）。
    ToggleFavorite,
    /// 切换置顶（恒在；界面按 `is_pinned` 切「置顶 / 取消置顶」文案）。
    TogglePinned,
    /// 编辑备注（恒在）。
    EditNote,
    /// 恢复文本条目首次编辑前的原文（有快照时）。
    RestoreOriginal,
    /// 进入列表多选并选中该条（恒在）。
    Select,
    /// 删除条目（恒在）。
    Delete,
}

/// 文件记录里的单条文件/目录元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub is_image: bool,
    /// 组装时实时检测：路径是否仍存在于磁盘，界面据此对失效条目划删除线并回退预览。
    pub exists: bool,
    /// 文件修改时间（Unix 毫秒）；无法读取时为空，供原文件缩略图识别内容变化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_path: Option<String>,
}

/// 文件卡片的渲染模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilesPreviewKind {
    /// 单文件且为图片，直接渲染图片预览。
    ImagePreview,
    /// 多文件或非图片，渲染图标 + 文件名列表。
    List,
}
