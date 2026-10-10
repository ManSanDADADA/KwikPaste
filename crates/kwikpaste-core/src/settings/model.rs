//! 设置数据模型。
//!
//! 每个字段都 `#[serde(default)]`，缺字段时回落到 `Default`，这样新增字段不破坏旧配置文件。

use serde::{Deserialize, Serialize};

use crate::db::models::ClipboardItemSort;
use crate::db::overview::ContentCategory;

pub const WINDOW_OPEN_SELECTION_PRESERVE: &str = "preserve";
pub const WINDOW_OPEN_SELECTION_ALL: &str = "all";
pub const WINDOW_OPEN_GROUP_PREFIX: &str = "group:";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub general: General,
    pub appearance: Appearance,
    pub shortcuts: Shortcuts,
    pub clipboard: Clipboard,
    pub sync: SyncSettings,
    pub onboarding: Onboarding,
    pub update: Update,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct General {
    pub auto_start: bool,
    /// Windows: persist the user's intent to run KwikPaste with administrator privileges.
    pub run_as_admin: bool,
    /// macOS 菜单栏 / Windows 系统托盘图标。
    pub tray_icon: bool,
    /// Windows：左键单击托盘图标打开的窗口。macOS 单击托盘弹出菜单，不读这一项。
    pub tray_click: TrayClick,
    /// macOS Dock / Windows 任务栏图标。
    pub dock_icon: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            auto_start: false,
            run_as_admin: false,
            tray_icon: true,
            tray_click: TrayClick::Clipboard,
            dock_icon: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TrayClick {
    #[default]
    Clipboard,
    Preference,
}

/// 首次启动引导状态。业务数据仍由各自设置项持久化，本结构只记录引导进度。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Onboarding {
    pub completed: bool,
    pub last_step: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Appearance {
    pub theme: Theme,
    pub material: Material,
    pub language: Language,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: Theme::Auto,
            material: Material::Default,
            language: Language::ZhCN,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Material {
    #[default]
    Default,
    Mica,
    Acrylic,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Language {
    #[default]
    #[serde(rename = "zh-CN")]
    ZhCN,
    #[serde(rename = "en-US")]
    EnUS,
}

impl Language {
    /// 把系统 locale（如 `zh_CN.UTF-8` / `en-US` / `ja-JP`）映射到支持的语言；
    /// 任何 zh-* 都归到 zh-CN，其余一律 en-US。
    pub fn from_system_locale(tag: &str) -> Self {
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("zh") {
            Self::ZhCN
        } else {
            Self::EnUS
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Shortcuts {
    /// 全局：唤起剪贴板窗口。
    pub open_clipboard: String,
    /// 全局：打开偏好设置窗口。
    pub open_preference: String,
    /// 全局：把当前系统剪贴板去除格式后直接粘贴。空字符串表示禁用。
    pub paste_plain: String,
    /// 仅 Windows：用 Win+V 唤起剪贴板窗口，替代系统剪贴板历史面板。默认关闭。
    pub win_v: bool,
    /// 仅 Windows：单击这个鼠标按键打开或隐藏剪贴板窗口，按键原有的单击功能随之停用。默认关闭。
    pub mouse_trigger: MouseTrigger,
    /// 仅 Windows（macOS 待实现）：前台应用全屏时（如游戏），全局快捷键、鼠标按键唤起和 Win+V
    /// 都交还给它。默认开启。
    pub pause_in_fullscreen: bool,
    /// 前台是这些应用时同样交还。id 写法与 `clipboard.filters.excludedAppIds` 相同（Windows 为
    /// exe 路径，macOS 为 bundle id）。默认为空。
    pub pause_app_ids: Vec<String>,
    /// 全局：修饰键 + 数字直接粘贴历史记录，不唤起剪贴板窗口。默认关闭。
    pub quick_paste: QuickPaste,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            open_clipboard: "Alt+C".into(),
            open_preference: "Alt+X".into(),
            paste_plain: String::new(),
            win_v: false,
            mouse_trigger: MouseTrigger::Disabled,
            pause_in_fullscreen: true,
            pause_app_ids: Vec::new(),
            quick_paste: QuickPaste::default(),
        }
    }
}

/// 唤起剪贴板窗口的鼠标按键。侧键对应 Windows 的 XBUTTON1 / XBUTTON2，多数鼠标上是后退、前进。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MouseTrigger {
    #[default]
    Disabled,
    Middle,
    Back,
    Forward,
}

/// 全局快速粘贴：修饰键 + 1–9 粘贴第 1–9 条，修饰键 + 0 粘贴第 10 条。
/// 条目顺序与剪贴板窗口「全部」视图一致：置顶按手动置顶顺序在前，其余按 `content.sort`。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct QuickPaste {
    pub enabled: bool,
    pub modifiers: QuickPasteModifiers,
}

/// 快速粘贴可选的修饰键组合。`Control` 在 macOS 上是 ⌃，`Alt` 在 macOS 上是 ⌥。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum QuickPasteModifiers {
    #[default]
    ControlShift,
    ControlAlt,
    AltShift,
    Alt,
    Control,
}

impl QuickPasteModifiers {
    /// 全局快捷键里的修饰键前缀；顺序与前端快捷键录入器一致，便于按字面量比对冲突。
    pub fn accelerator(self) -> &'static str {
        match self {
            Self::ControlShift => "Control+Shift",
            Self::ControlAlt => "Control+Alt",
            Self::AltShift => "Alt+Shift",
            Self::Alt => "Alt",
            Self::Control => "Control",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Clipboard {
    pub capture: Capture,
    pub content: Content,
    pub display: Display,
    pub sensitive: Sensitive,
    pub history: History,
    pub search: Search,
    pub window: Window,
    pub preview: Preview,
    pub feedback: Feedback,
    pub filters: Filters,
}

/// 剪贴板内容类型采集开关。关闭后监听与手动读取都不入库对应类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Capture {
    pub text: bool,
    pub html: bool,
    pub rtf: bool,
    pub image: bool,
    pub files: bool,
    /// 文本最大收录大小，单位 MB。`0` = 不限制。
    pub max_text_mb: u32,
    /// 图片最大收录大小，单位 MB。`0` = 不限制。
    pub max_image_mb: u32,
    /// 剪贴板同时提供多种表示时的采集优先级。
    pub order: Vec<CaptureKind>,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            text: true,
            html: true,
            rtf: true,
            image: true,
            files: true,
            max_text_mb: 4,
            max_image_mb: 100,
            order: CaptureKind::default_order(),
        }
    }
}

impl Capture {
    /// 返回文本最大收录字节数；`None` 表示不限制。
    pub fn max_text_bytes(&self) -> Option<u64> {
        mb_to_bytes(self.max_text_mb)
    }

    /// 返回图片最大收录字节数；`None` 表示不限制。
    pub fn max_image_bytes(&self) -> Option<u64> {
        mb_to_bytes(self.max_image_mb)
    }

    /// 返回去重且补齐缺失项后的采集顺序，避免配置文件里手改出重复项后影响读取。
    pub fn ordered_kinds(&self) -> Vec<CaptureKind> {
        let mut order = Vec::new();
        for kind in self
            .order
            .iter()
            .copied()
            .chain(CaptureKind::default_order())
        {
            if !order.contains(&kind) {
                order.push(kind);
            }
        }

        order
    }

    /// 判断某个采集类型当前是否开启。
    pub fn is_enabled(&self, kind: CaptureKind) -> bool {
        match kind {
            CaptureKind::Text => self.text,
            CaptureKind::Html => self.html,
            CaptureKind::Rtf => self.rtf,
            CaptureKind::Image => self.image,
            CaptureKind::Files => self.files,
        }
    }
}

/// 把用户设置的 MB 值转换为字节阈值；`0` 表示不限。
fn mb_to_bytes(mb: u32) -> Option<u64> {
    if mb == 0 {
        return None;
    }

    Some(u64::from(mb) * 1024 * 1024)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CaptureKind {
    Files,
    Image,
    Html,
    Rtf,
    Text,
}

impl CaptureKind {
    /// 默认顺序保持历史硬编码语义：文件 > 图片 > HTML > RTF > 纯文本。
    pub fn default_order() -> Vec<Self> {
        vec![Self::Files, Self::Image, Self::Html, Self::Rtf, Self::Text]
    }
}

/// 隐私保护设置。命中规则的内容可分别控制是否收录、是否脱敏展示。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Sensitive {
    /// 命中高置信密钥 / Token 时是否保存到历史记录。
    pub collect_secrets: bool,
    /// 已保存的敏感内容是否在列表与预览中脱敏展示。
    pub redact_secrets: bool,
}

impl Default for Sensitive {
    fn default() -> Self {
        Self {
            collect_secrets: true,
            redact_secrets: true,
        }
    }
}

/// 应用过滤规则。
/// `excluded_app_ids` 命中复制来源时，对应剪贴板内容不入库。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Filters {
    pub excluded_app_ids: Vec<String>,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            excluded_app_ids: default_excluded_app_ids(),
        }
    }
}

fn default_excluded_app_ids() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        // 系统级密码 / 密钥工具：用户从这里复制的几乎都是敏感凭据，默认不入库。
        // - com.apple.keychainaccess：钥匙串访问
        // - com.apple.Passwords：macOS 15 起的「密码」App
        vec![
            "com.apple.keychainaccess".to_owned(),
            "com.apple.Passwords".to_owned(),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        // Windows 无系统内置的密码管理 App（凭据管理器是 Control Panel 子项，不会作为复制来源）。
        // 第三方密码管理器（1Password / Bitwarden / KeePass 等）因人而异，留给用户在 UI 勾选。
        Vec::new()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn macos_defaults_keep_sensitive_system_apps_excluded() {
        let ids = default_excluded_app_ids();

        assert!(ids.contains(&"com.apple.keychainaccess".to_owned()));
        assert!(ids.contains(&"com.apple.Passwords".to_owned()));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Content {
    /// 点击列表项时的自动粘贴行为。
    pub auto_paste: AutoPaste,
    /// 中键点击列表项时执行的动作。
    pub middle_click: MiddleClickAction,
    /// 复制（写回剪贴板）时去除格式。
    pub copy_plain: bool,
    /// 从历史复制后隐藏剪贴板窗口。
    pub copy_then_hide_window: bool,
    /// 粘贴时去除格式。
    pub paste_plain: bool,
    /// 粘贴文件记录时，默认写入路径文本而不是文件本身。
    pub paste_files_as_path: bool,
    /// 鼠标悬停时显示原始内容预览（HTML/RTF 渲染前的原文）。
    pub show_original_preview: bool,
    /// 删除普通条目前是否需要二次确认；收藏 / 置顶条目由各自确认开关单独控制。
    pub delete_confirm: bool,
    /// 是否允许删除收藏条目；关闭时收藏条目不显示删除入口。
    pub delete_favorite_items: bool,
    /// 删除收藏条目前是否需要二次确认。
    pub delete_favorite_confirm: bool,
    /// 是否允许删除置顶条目；关闭时置顶条目不显示删除入口。
    pub delete_pinned_items: bool,
    /// 删除置顶条目前是否需要二次确认。
    pub delete_pinned_confirm: bool,
    /// 开启后已收藏条目仅能在收藏分组删除，普通条目不受影响。
    pub delete_favorite_items_only_in_favorite_group: bool,
    pub auto_favorite: bool,
    /// 从历史中复制 / 粘贴时，是否刷新使用次数与 `updated_at`。
    pub update_on_reuse: bool,
    /// 历史列表默认排序，和 `ClipboardItemQuery.sort` 使用同一套契约字面量。
    pub sort: ClipboardItemSort,
    /// 列表项悬停操作按钮（仅保存已启用项，顺序按 `item_action_order` 过滤）。
    pub item_actions: Vec<ItemAction>,
    /// 列表项悬停操作按钮的完整排序，包含未启用项，供偏好弹框下次打开时恢复位置。
    pub item_action_order: Vec<ItemAction>,
}

impl Default for Content {
    fn default() -> Self {
        Self {
            auto_paste: AutoPaste::DoubleClickPaste,
            middle_click: MiddleClickAction::Disabled,
            copy_plain: false,
            copy_then_hide_window: false,
            paste_plain: false,
            paste_files_as_path: false,
            show_original_preview: true,
            delete_confirm: true,
            delete_favorite_items: false,
            delete_favorite_confirm: true,
            delete_pinned_items: false,
            delete_pinned_confirm: true,
            delete_favorite_items_only_in_favorite_group: true,
            auto_favorite: false,
            update_on_reuse: false,
            sort: ClipboardItemSort::UpdatedAt,
            item_actions: vec![
                ItemAction::Copy,
                ItemAction::SplitWords,
                ItemAction::Star,
                ItemAction::PinItem,
                ItemAction::Delete,
            ],
            item_action_order: vec![
                ItemAction::Paste,
                ItemAction::PastePlain,
                ItemAction::PastePath,
                ItemAction::Copy,
                ItemAction::CopyPlain,
                ItemAction::SplitWords,
                ItemAction::OpenLink,
                ItemAction::SendEmail,
                ItemAction::Reveal,
                ItemAction::Note,
                ItemAction::Star,
                ItemAction::PinItem,
                ItemAction::Delete,
            ],
        }
    }
}

/// 历史列表里不同内容类型的展示上限。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Display {
    /// 文本摘要最多显示行数。
    pub text_max_lines: u8,
    /// 图片缩略图显示高度，单位 px。
    pub image_max_height: u16,
    /// 文件列表最多返回并显示的条目数。
    pub file_max_count: u8,
    /// 文本记录下方列出识别到的编号、数字、链接等快捷信息，点击单独粘贴。
    /// 旧配置没有这个字段，按默认开启读取。
    pub quick_snippets: bool,
    /// 列表条目画成独立卡片，还是贴边排列、用分隔线隔开。
    pub list_style: ListStyle,
    /// 列表疏密。旧配置没有这个字段，存量用户升级后同样按「标准」读取，比旧版的「舒适」紧一档。
    pub density: ListDensity,
    /// 密度选「自定义」时使用的各项尺寸，其它密度下保留不动，切回自定义时原样恢复。
    pub custom_layout: CustomListLayout,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            text_max_lines: 3,
            image_max_height: 64,
            file_max_count: 3,
            quick_snippets: true,
            list_style: ListStyle::Card,
            density: ListDensity::Standard,
            custom_layout: CustomListLayout::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ListStyle {
    #[default]
    Card,
    Seamless,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ListDensity {
    /// 旧版的排布：间距 12px、头部行 24px。
    Comfortable,
    #[default]
    Standard,
    /// 来源图标并入正文，不再单独占一行。
    Compact,
    Custom,
}

/// 自定义密度的尺寸，单位 px。前端只提供固定档位，不在档位里的值按最接近的档位渲染。
/// 默认值与「标准」密度一致。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct CustomListLayout {
    /// 来源图标、类型和时间单独占一行；关闭时来源图标并入正文左侧，类型和时间不再显示。
    pub header_row: bool,
    /// 卡片之间的间距，无间风格下不生效。
    pub item_gap: u8,
    /// 条目的上下内边距。
    pub padding_y: u8,
}

impl Default for CustomListLayout {
    fn default() -> Self {
        Self {
            header_row: true,
            item_gap: 8,
            padding_y: 6,
        }
    }
}

impl Display {
    /// 返回主列表文件条目上限，并夹在 UI 支持的范围内控制 IPC 与 icon 抽取成本。
    pub fn file_entry_limit(self) -> usize {
        usize::from(self.file_max_count.clamp(1, 5))
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AutoPaste {
    /// 点击只选中，不自动执行动作。
    Disabled,
    SingleClickPaste,
    #[default]
    DoubleClickPaste,
    SingleClickCopy,
    DoubleClickCopy,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MiddleClickAction {
    /// 中键点击仅选中，不自动执行动作。
    #[default]
    Disabled,
    SingleClickPaste,
    SingleClickPastePlain,
    SingleClickCopy,
    SingleClickCopyPlain,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ItemAction {
    Paste,
    PastePlain,
    PastePath,
    Copy,
    CopyPlain,
    SplitWords,
    OpenLink,
    SendEmail,
    Reveal,
    Note,
    Star,
    PinItem,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Preview {
    pub hover_enabled: bool,
    pub hover_delay_ms: PreviewHoverDelayMs,
    pub space_enabled: bool,
    /// 新装默认选词；早于 v1.3.2 的设置文件没有这个字段，读取时按原文，保持升级前的样子。
    #[serde(default)]
    pub text_view: PreviewTextView,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            hover_enabled: true,
            hover_delay_ms: PreviewHoverDelayMs::Ms500,
            space_enabled: false,
            text_view: PreviewTextView::Words,
        }
    }
}

/// 文本预览的展示方式：原文，或拆成词块逐个点选。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PreviewTextView {
    #[default]
    Plain,
    Words,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PreviewHoverDelayMs {
    Ms300,
    #[default]
    Ms500,
    Ms1000,
}

pub const DEFAULT_STORAGE_LIMIT_MB: u32 = 1024;
pub const MIN_STORAGE_LIMIT_MB: u32 = 100;
/// 自定义清理规则条数上限，控制每轮清理 SQL 的规模。
pub const MAX_RETENTION_RULES: usize = 32;

/// 历史记录自动清理。收藏、置顶、放进自定义分组和写了备注的记录始终保留。
///
/// 已发布版本还有 `cleanupIntervalHours`（清理周期）：现在改为随设置变更、新记录与后台检查即时清理，
/// 旧文件里的这个字段读取时直接忽略。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct History {
    /// 默认保留时长：没有命中任何自定义规则的记录，超过这段时间没用过就清理。
    pub retention: Retention,
    /// 自定义规则，自上而下匹配，记录按第一条命中的规则清理。
    pub rules: Vec<RetentionRule>,
    /// 普通记录最多保留条数，超出后清理最久没用的。`0` = 不限。
    pub max_count: u32,
    /// 本地存储上限（MB），偏好页的存储占用以它为满格。
    pub storage_limit_mb: u32,
    /// 占用超过 `storage_limit_mb` 后的处理方式。
    pub storage_limit_action: StorageLimitAction,
}

impl Default for History {
    fn default() -> Self {
        Self {
            retention: Retention::default(),
            rules: Vec::new(),
            max_count: 0,
            storage_limit_mb: DEFAULT_STORAGE_LIMIT_MB,
            storage_limit_action: StorageLimitAction::Remind,
        }
    }
}

impl History {
    /// 存储上限字节数；手改配置写入过小的值时按下限计，避免自动清理把普通记录删光。
    pub fn storage_limit_bytes(&self) -> u64 {
        u64::from(self.storage_limit_mb.max(MIN_STORAGE_LIMIT_MB)) * 1024 * 1024
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StorageLimitAction {
    /// 只提醒，不删除任何数据。
    #[default]
    Remind,
    /// 从最久没用的普通记录开始自动清理，直到回到上限以内。
    Cleanup,
}

/// 一条自定义清理规则。所有条件同时满足才算命中；条件留空表示不限。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct RetentionRule {
    /// 前端生成的稳定 id，列表渲染和逐条统计按它对应。
    pub id: String,
    pub enabled: bool,
    /// 内容类别，空 = 全部。
    pub categories: Vec<ContentCategory>,
    /// 只命中大于这个大小（KB）的记录，`0` = 不限；文件记录没有大小，设了就不会命中。
    pub min_size_kb: u32,
    /// 来源应用 id，空 = 全部。
    pub source_app_ids: Vec<String>,
    /// 只命中识别为密钥 / Token 的敏感记录。
    pub sensitive_only: bool,
    /// 只命中采集后再没用过的记录。
    pub unused_only: bool,
    /// 命中后超过这段时间没用过就清理；`Forever` 表示命中的记录不按时间清理。
    pub keep: Retention,
}

impl Default for RetentionRule {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: true,
            categories: Vec::new(),
            min_size_kb: 0,
            source_app_ids: Vec::new(),
            sensitive_only: false,
            unused_only: false,
            keep: Retention::default(),
        }
    }
}

/// 保留时长。`unit = Forever` 或 `value = 0` 表示不按时间清理。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Retention {
    pub value: u32,
    pub unit: RetentionUnit,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            value: 0,
            unit: RetentionUnit::Forever,
        }
    }
}

/// 保留时长单位。`Minutes` 只用于自定义规则：默认保留时长仍只写已发布版本认识的单位。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RetentionUnit {
    Minutes,
    Hours,
    Days,
    Weeks,
    Months,
    #[default]
    Forever,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Search {
    /// 剪贴板窗口每次显示时自动聚焦搜索框。
    pub default_focus: bool,
    /// 剪贴板窗口隐藏时清空搜索关键词。
    pub clear_on_hide: bool,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            default_focus: false,
            clear_on_hide: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Window {
    pub position: WindowPosition,
    /// 打开剪贴板窗口时把历史列表回到顶部。
    pub scroll_to_top_on_open: bool,
    /// 打开剪贴板窗口时切换到指定范围；`Preserve` 表示保持上次状态。
    pub select_range_on_open: WindowOpenRangeSelection,
    /// 打开剪贴板窗口时切换到指定分类；`Preserve` 表示保持上次状态。
    pub select_category_on_open: WindowOpenCategorySelection,
    /// 打开剪贴板窗口时切换到指定自定义分组；可为 preserve / all / group:<id>。
    pub select_group_on_open: String,
    /// 隐藏窗口轻量化：剪贴板窗口隐藏后进入 dormant，非剪贴板窗口空闲后释放 WebView。
    pub lightweight_mode: bool,
    /// 非剪贴板窗口隐藏后释放 WebView 的空闲秒数。
    pub idle_destroy_seconds: u32,
}

/// 设置文件落盘时写入完整结构，已有安装会保留各自显式保存的打开选中项；这里的默认值只作用于全新安装与恢复默认。
impl Default for Window {
    fn default() -> Self {
        Self {
            position: WindowPosition::FollowCursor,
            scroll_to_top_on_open: true,
            select_range_on_open: WindowOpenRangeSelection::All,
            select_category_on_open: WindowOpenCategorySelection::All,
            select_group_on_open: WINDOW_OPEN_SELECTION_ALL.to_owned(),
            lightweight_mode: true,
            idle_destroy_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WindowOpenRangeSelection {
    Preserve,
    #[default]
    All,
    Favorite,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WindowOpenCategorySelection {
    Preserve,
    #[default]
    All,
    Text,
    Image,
    Files,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WindowPosition {
    Remember,
    #[default]
    FollowCursor,
    Center,
}

/// 多设备同步。设备身份与已配对设备不放这里：`settings.json` 会进备份包，导到别的电脑就成了冒充。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncSettings {
    pub lan: LanSync,
}

pub const LAN_SYNC_DEVICE_NAME_MAX_CHARS: usize = 40;
pub const LAN_SYNC_MAX_IMAGE_MB_MIN: u32 = 1;
pub const LAN_SYNC_MAX_IMAGE_MB_MAX: u32 = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct LanSync {
    pub enabled: bool,
    /// 在其他设备上显示的名称；留空时用系统的电脑名。
    pub device_name: String,
    /// 收到其他设备的复制后，同时写入本机系统剪贴板，可以直接粘贴。
    pub write_clipboard: bool,
    pub text: bool,
    pub image: bool,
    /// 单张图片超过这个大小就不发送也不接收。
    pub max_image_mb: u32,
}

impl Default for LanSync {
    fn default() -> Self {
        Self {
            enabled: false,
            device_name: String::new(),
            write_clipboard: true,
            text: true,
            image: true,
            max_image_mb: 20,
        }
    }
}

impl LanSync {
    /// 同步图片的大小上限。设置文件按宽松规则读入、不过校验，手改出来的值在这里收进允许范围，
    /// 免得接收缓冲跟着一个离谱的值涨。
    pub fn max_image_bytes(&self) -> u64 {
        let mb = self
            .max_image_mb
            .clamp(LAN_SYNC_MAX_IMAGE_MB_MIN, LAN_SYNC_MAX_IMAGE_MB_MAX);
        u64::from(mb) * 1024 * 1024
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Feedback {
    pub copy_sound: bool,
    /// 复制提示音的音量百分比；手改文件超出 100 的值在播放时夹取。
    pub copy_sound_volume: u8,
}

impl Default for Feedback {
    fn default() -> Self {
        Self {
            copy_sound: false,
            copy_sound_volume: 100,
        }
    }
}

/// 1.x 的渠道开关 `includeBeta` / `includeNightly` 在 2.x 删掉了（2.x 只有一个更新渠道）：读取时忽略，
/// 下次写盘不再写出。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Update {
    pub auto_check: bool,
    pub frequency: UpdateFrequency,
    pub last_checked_at: Option<String>,
    pub skipped_version: Option<String>,
}

/// 设置文件落盘时写入完整结构，已有安装会保留各自显式保存的值；这里的默认值只作用于全新安装。
impl Default for Update {
    fn default() -> Self {
        Self {
            auto_check: true,
            frequency: UpdateFrequency::default(),
            last_checked_at: None,
            skipped_version: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UpdateFrequency {
    #[default]
    Daily,
    Weekly,
    Monthly,
}
