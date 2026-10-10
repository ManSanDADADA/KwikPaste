use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ClipboardKind {
    Text,
    Image,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ClipboardSubKind {
    Rtf,
    Html,
    Url,
    Email,
    Color,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Windows,
}

/// 一条剪贴板记录的数据库行（含列表查询 JOIN 出的来源应用名与图标文件名）。
/// 列表卡片要的附加字段在展示层的 `ClipboardItemView` 里。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardItem {
    pub id: String,
    pub kind: ClipboardKind,
    pub sub_kind: Option<ClipboardSubKind>,
    pub group_id: Option<String>,
    /// 复制时的来源应用 id（macOS = bundle id，Windows = 可执行文件绝对路径）；
    /// 监听 / 命令链路若未能取到前台应用则为 `None`。引用 `clipboard_apps(id)`，
    /// 删除应用记录时置 NULL，不会级联删条目。
    pub source_app_id: Option<String>,
    pub content: String,
    /// 去重指纹：`blake3(kind:content)`，由 `db::items::content_hash` 计算并在入库前比对。
    pub content_hash: String,
    pub search_text: Option<String>,
    /// 列表渲染用的纯文本摘要（最多 512 字符）。HTML/RTF 也只存纯文本截断
    /// （来源是 OS 同时提供的纯文本，不解析富文本）；Image/Files 为 `None`。
    /// 完整内容仍在 `content`，预览/写回时再读。
    pub summary: Option<String>,
    /// Files 类型专用：紧凑格式记录每个路径的类型，如 "d,f,f" 表示 [dir, file, file]。
    /// d=directory, f=file。用于删除文件后仍能准确显示 icon。
    pub file_types: Option<String>,
    pub size: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub use_count: i64,
    pub is_favorite: bool,
    pub is_pinned: bool,
    /// 命中敏感内容规则且被收录的条目；展示是否脱敏由当前设置决定。
    pub is_sensitive: bool,
    pub platform: Platform,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 局域网同步收到的记录所来自的设备 id；本机采集的记录为 `None`。
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_device_id: Option<String>,

    /// 来源应用名称。仅 list 查询通过 LEFT JOIN `clipboard_apps` 填充；
    /// 单条 `SELECT_ITEM` 路径与 `INSERT` 不读不写，`#[sqlx(default)]` 保证缺列时为 `None`。
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_app_name: Option<String>,
    /// 来源应用图标文件名（`<hash>.png`）。同 [`source_app_name`] 由 list 查询补齐，
    /// 展示层据此解析为绝对路径（`ClipboardItemView::source_app_icon_path`）。
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_app_icon_file: Option<String>,
}

/// 列表多选用的轻量记录：只带 id 与决定能否删除的收藏 / 置顶标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardItemRef {
    pub id: String,
    pub is_favorite: bool,
    pub is_pinned: bool,
}

/// 剪贴板来源应用（macOS bundle id / Windows exe 路径作主键），
/// 名称与图标按 id 去重共享，单条剪贴板记录通过 `source_app_id` 引用。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardApp {
    pub id: String,
    pub name: String,
    /// `app-icons/<hash>.png` 形式的文件名（无分片目录前缀）；无图标则 `None`。
    pub icon_file: Option<String>,
    pub platform: Platform,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardGroup {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub is_hidden: bool,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ClipboardItemSort {
    #[serde(rename = "createdAtDesc")]
    CreatedAt,
    #[default]
    #[serde(rename = "updatedAtDesc")]
    UpdatedAt,
    #[serde(rename = "useCountDesc")]
    UseCount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClipboardItemQuery {
    /// 宿主调用 Core 查询时由设置快照覆盖，不接受外部 JSON 控制。
    #[serde(skip)]
    pub ocr_enabled: bool,
    pub kind: Option<ClipboardKind>,
    pub group_id: Option<String>,
    pub favorite: Option<bool>,
    pub pinned: Option<bool>,
    /// 列表顶部 Tab 过滤（前端只需传这一个；Rust 侧翻译成 kind / favorite）。
    /// 显式设置时覆盖 `kind`，`favorite` 仍与之叠加；为 None 时走显式 `kind`（保留给单测）。
    pub group: Option<ClipboardGroupFilter>,
    pub keyword: Option<String>,
    pub sort: ClipboardItemSort,
    pub limit: i64,
    pub offset: i64,
}

/// 列表顶部分组 Tab：UI 概念，与 `ClipboardGroup`（用户自建分组）不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipboardGroupFilter {
    All,
    Text,
    Image,
    Files,
    Favorite,
}

impl Default for ClipboardItemQuery {
    fn default() -> Self {
        Self {
            ocr_enabled: false,
            kind: None,
            group_id: None,
            favorite: None,
            pinned: None,
            group: None,
            keyword: None,
            sort: ClipboardItemSort::UpdatedAt,
            limit: 20,
            offset: 0,
        }
    }
}
