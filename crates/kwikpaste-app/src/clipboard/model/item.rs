//! 列表条目的视图模型。
//!
//! 字段名和 JSON 形状与 core 的 `ClipboardItem`（S1 之后的 `ItemView`）一致：serde camelCase，
//! 列表载荷里文本类的 `content` 已置空、敏感内容已脱敏、文件条目和可用动作已由 core 算好。
//! 这里只收列表要用的字段，其余（`contentHash`、`searchText`、`useCount` 等）反序列化时忽略。
//! 接 core 时把 `ItemView` 转成 [`ListItem`]（见 [`super::source`]），视图层不用改。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use kwikpaste_core::clipboard::QuickSnippet;
use serde::Deserialize;

use super::layout::ImageBox;

/// 记录的主类型（core `ClipboardKind`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Text,
    Image,
    Files,
}

/// 文本记录的子类型（core `ClipboardSubKind`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubKind {
    Rtf,
    Html,
    Url,
    Email,
    Color,
    Path,
}

/// 记录产生的平台（core `Platform`）；局域网同步的记录用它选设备图标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Windows,
}

/// 文件卡片的渲染方式（core `FilesPreviewKind`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilesPreview {
    /// 单个仍存在的图片文件：按图片卡片显示。
    ImagePreview,
    /// 图标加文件名列表。
    List,
}

/// 列表和右键菜单可用的动作（core `ClipboardAction`）。core 新增的动作在旧版 UI 里落到 `Unknown`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemAction {
    Paste,
    PasteAsPlainText,
    PasteAsPath,
    Copy,
    SaveImage,
    CopyImageText,
    SplitWords,
    Translate,
    CopyTranslation,
    PasteTranslation,
    OpenLink,
    SendEmail,
    RevealInFinder,
    RevealInExplorer,
    ToggleFavorite,
    TogglePinned,
    EditNote,
    RestoreOriginal,
    Select,
    Delete,
    #[serde(other)]
    Unknown,
}

/// 文件卡片里的一行（core `FileEntry`）。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRow {
    pub path: Arc<str>,
    pub name: Arc<str>,
    #[serde(default)]
    pub is_dir: bool,
    #[serde(default)]
    pub is_image: bool,
    /// 路径是否仍在磁盘上；不在时文件名划删除线。
    #[serde(default = "default_true")]
    pub exists: bool,
    /// 文件修改时间（Unix 毫秒）；未知时不参与原文件缩略图的版本区分。
    #[serde(default)]
    pub modified: Option<i64>,
    /// 文件类型图标（PNG 绝对路径）。
    #[serde(default)]
    pub icon_path: Option<Arc<str>>,
    /// 单图预览时图片的像素尺寸。今天的 `FileEntry` 没有这两个字段，需要 core S1 补上（见报告）；
    /// 缺省时按方形预测占位尺寸。
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

/// 列表里的一条记录。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItem {
    pub id: Arc<str>,
    pub kind: ItemKind,
    #[serde(default)]
    pub sub_kind: Option<SubKind>,
    #[serde(default)]
    pub group_id: Option<Arc<str>>,
    /// 图片记录是原图文件名（取缩略图时用）；文本类在列表载荷里是空串。
    #[serde(default)]
    pub content: Arc<str>,
    /// 列表摘要（core 已截断、已脱敏）；图片和文件为空。
    #[serde(default)]
    pub summary: Option<Arc<str>>,
    /// 图片原始像素尺寸，用来预测缩略图的显示尺寸（附录 D L6）。
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    pub is_favorite: bool,
    pub is_pinned: bool,
    pub is_sensitive: bool,
    pub platform: Platform,
    #[serde(default)]
    pub note: Option<Arc<str>>,
    /// UTC 创建时间；时间标签由 UI 按本地时区格式化（附录 D §8 第 4 条）。
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub source_app_id: Option<Arc<str>>,
    #[serde(default)]
    pub source_app_name: Option<Arc<str>>,
    /// 来源应用图标（PNG 绝对路径）。
    #[serde(default)]
    pub source_app_icon_path: Option<Arc<str>>,
    #[serde(default)]
    pub origin_device_id: Option<Arc<str>>,
    #[serde(default)]
    pub origin_device_name: Option<Arc<str>>,
    /// 已存在的缩略图绝对路径；还没生成时为空，由 UI 向数据源要。
    #[serde(default)]
    pub image_thumbnail_path: Option<Arc<str>>,
    #[serde(default)]
    pub file_entries: Option<Vec<FileRow>>,
    #[serde(default)]
    pub files_preview_kind: Option<FilesPreview>,
    #[serde(default)]
    pub available_actions: Vec<ItemAction>,
    /// core 校验过的 CSS 颜色串（颜色子类型才有；脱敏时为空）。
    #[serde(default)]
    pub color_preview: Option<Arc<str>>,
    #[serde(default)]
    pub quick_snippets: Vec<QuickSnippet>,
    /// core 展示层算好的图片显示尺寸（`ClipboardItemView::image_display_size`，不在 JSON 里）。
    /// 夹具没有它，视图按同一公式自己预测（[`super::layout::predict_image_box`]）。
    #[serde(skip)]
    pub image_display: Option<ImageBox>,
    /// 图片识别出了文字（core `ClipboardItemView::has_image_text`）：预览窗给出「图片 / 文字」切换。
    #[serde(skip)]
    pub has_image_text: bool,
    /// 搜索靠图片识别文字命中时的片段（core `ClipboardItemView::image_text_snippet`）。
    #[serde(skip)]
    pub image_text_snippet: Option<TextSnippet>,
    /// 译文开头（core `ClipboardItemView::translation`，最多 512 个字符）：卡片在原文下面显示。
    #[serde(skip)]
    pub translation: Option<Arc<str>>,
    /// 正在翻译（core `ClipboardItemView::translating`）。
    #[serde(skip)]
    pub translating: bool,
}

/// 识别文字里命中关键词的一小段，`matched` 是关键词在 `text` 里的字节范围（可能为空）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSnippet {
    pub text: Arc<str>,
    pub matched: std::ops::Range<usize>,
}

fn default_true() -> bool {
    true
}

impl ListItem {
    /// 链接交互用的子类型：富文本仅采用 core 从完整纯文本摘要判定的可用动作。
    pub fn link_sub_kind(&self) -> Option<SubKind> {
        match self.sub_kind {
            Some(SubKind::Html | SubKind::Rtf) => {
                if self.available_actions.contains(&ItemAction::OpenLink) {
                    Some(SubKind::Url)
                } else if self.available_actions.contains(&ItemAction::SendEmail) {
                    Some(SubKind::Email)
                } else {
                    None
                }
            }
            Some(SubKind::Url | SubKind::Email) => self.sub_kind,
            _ => None,
        }
    }

    /// 头部类型标签用的类型：有子类型时取子类型（1.x `types.{subKind ?? kind}`）。
    pub fn type_key(&self) -> TypeKey {
        match (self.link_sub_kind().or(self.sub_kind), self.kind) {
            (Some(SubKind::Rtf), _) => TypeKey::Rtf,
            (Some(SubKind::Html), _) => TypeKey::Html,
            (Some(SubKind::Url), _) => TypeKey::Url,
            (Some(SubKind::Email), _) => TypeKey::Email,
            (Some(SubKind::Color), _) => TypeKey::Color,
            (Some(SubKind::Path), _) => TypeKey::Path,
            (None, ItemKind::Text) => TypeKey::Text,
            (None, ItemKind::Image) => TypeKey::Image,
            (None, ItemKind::Files) => TypeKey::Files,
        }
    }

    /// 是否显示敏感标记：只对文本类记录（1.x `isSensitive && kind === "text"`）。
    pub fn shows_sensitive_mark(&self) -> bool {
        self.is_sensitive && self.kind == ItemKind::Text
    }

    pub fn file_rows(&self) -> &[FileRow] {
        self.file_entries.as_deref().unwrap_or_default()
    }
}

/// 一条记录的 id 与删除保护要看的标记（core `ClipboardItemRef`），多选全选、连选用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemRef {
    pub id: Arc<str>,
    pub is_favorite: bool,
    pub is_pinned: bool,
}

impl ItemRef {
    pub fn of(item: &ListItem) -> Self {
        Self {
            id: item.id.clone(),
            is_favorite: item.is_favorite,
            is_pinned: item.is_pinned,
        }
    }
}

/// 卡片头部的类型标签，对应 1.x `clipboard:types.*`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeKey {
    Text,
    Html,
    Rtf,
    Url,
    Email,
    Color,
    Path,
    Image,
    Files,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_the_core_list_payload_shape() {
        let json = r#"{
            "id": "a1", "kind": "text", "subKind": "url", "content": "", "contentHash": "h",
            "summary": "https://example.com", "useCount": 3, "isFavorite": false, "isPinned": true,
            "isSensitive": false, "platform": "windows", "createdAt": "2026-10-02T01:02:03.456Z",
            "updatedAt": "2026-10-02T01:02:03.456Z", "availableActions": ["paste", "openLink", "futureThing"],
            "quickSnippets": [{"text": "example.com", "kind": "link"}], "displayCreatedAt": "09:02"
        }"#;
        let item: ListItem = serde_json::from_str(json).expect("parses");

        assert_eq!(&*item.id, "a1");
        assert_eq!(item.type_key(), TypeKey::Url);
        assert!(item.is_pinned);
        assert_eq!(
            item.available_actions,
            vec![ItemAction::Paste, ItemAction::OpenLink, ItemAction::Unknown]
        );
        assert_eq!(item.summary.as_deref(), Some("https://example.com"));
        assert_eq!(item.quick_snippets[0].text, "example.com");
        assert_eq!(
            item.quick_snippets[0].kind,
            kwikpaste_core::clipboard::SnippetKind::Link
        );
        assert!(item.file_rows().is_empty());
    }

    #[test]
    fn sensitive_mark_is_text_only() {
        let base: ListItem = serde_json::from_str(
            r#"{"id":"x","kind":"image","isFavorite":false,"isPinned":false,"isSensitive":true,
                "platform":"macos","createdAt":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("parses");
        assert!(!base.shows_sensitive_mark());

        let text = ListItem {
            kind: ItemKind::Text,
            ..base
        };
        assert!(text.shows_sensitive_mark());
    }

    #[test]
    fn rich_text_link_labels_preserve_the_stored_sub_kind() {
        let mut item: ListItem = serde_json::from_str(
            r#"{"id":"x","kind":"text","subKind":"html","isFavorite":false,"isPinned":false,
                "isSensitive":false,"platform":"windows","createdAt":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("parses");
        for (sub_kind, original_label) in
            [(SubKind::Html, TypeKey::Html), (SubKind::Rtf, TypeKey::Rtf)]
        {
            item.sub_kind = Some(sub_kind);
            for (action, link_kind, label) in [
                (ItemAction::OpenLink, SubKind::Url, TypeKey::Url),
                (ItemAction::SendEmail, SubKind::Email, TypeKey::Email),
            ] {
                item.available_actions = vec![action];
                assert_eq!(item.type_key(), label);
                assert_eq!(item.link_sub_kind(), Some(link_kind));
                assert_eq!(item.sub_kind, Some(sub_kind));
            }
            item.available_actions.clear();
            assert_eq!(item.type_key(), original_label);
            assert_eq!(item.link_sub_kind(), None);
        }
    }
}
