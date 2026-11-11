//! 偏好窗的结构：分类（侧栏）→ 分组 → 设置项，照 1.x `src/pages/Preference/config/preferenceSchema.ts`。
//!
//! 设置项 id 是 i18n key（`preferences:schema.settings.<id>.*`）与搜索跳转的目标，挪动分类时保持不变；
//! `path` 是 `settings.json` 的点分 camelCase 路径，读值和写补丁都按它走。

use std::sync::LazyLock;

use super::icons::PrefIcon;
use kwikpaste_core::settings::{ListDensity, ListStyle, Settings};

/// 侧栏分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabId {
    General,
    Shortcuts,
    Appearance,
    Window,
    Paste,
    Capture,
    Sync,
    Extensions,
    Data,
    About,
}

impl TabId {
    pub fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Shortcuts => "shortcuts",
            Self::Appearance => "appearance",
            Self::Capture => "capture",
            Self::Window => "window",
            Self::Paste => "paste",
            Self::Sync => "sync",
            Self::Extensions => "extensions",
            Self::Data => "data",
            Self::About => "about",
        }
    }
}

/// 侧栏里分隔线隔开的三组。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    App,
    Clipboard,
    Data,
}

pub struct Tab {
    pub id: TabId,
    pub group: Group,
    pub icon: PrefIcon,
    pub sections: Vec<Section>,
}

pub struct Section {
    pub id: &'static str,
    pub settings: Vec<Setting>,
}

/// 带示意图的磁贴：选项本身有视觉含义的设置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TilesKind {
    Theme,
    Material,
    ListStyle,
    PanelLayout,
    WindowPosition,
}

impl TilesKind {
    pub fn values(self) -> &'static [&'static str] {
        match self {
            Self::Theme => &["auto", "light", "dark"],
            Self::Material => &["default", "mica", "acrylic"],
            Self::ListStyle => LIST_STYLES,
            Self::PanelLayout => PANEL_LAYOUTS,
            Self::WindowPosition => WINDOW_POSITIONS,
        }
    }
}

/// 权限行的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionKind {
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    Accessibility,
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    FullDiskAccess,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    RunAsAdministrator,
}

/// 下拉或分段的选项：值的文案取 `schema.settings.<id>.options.<value>`；快捷键选项直接显示按键。
#[derive(Clone, Copy, Debug)]
pub enum Options {
    Values(&'static [&'static str]),
    /// 数字取值（列表间距、内边距），文案同样按值取。
    Numbers(&'static [u64]),
    /// `(值, 快捷键字面量)`：快速粘贴的修饰键。
    Shortcuts(&'static [(&'static str, &'static str)]),
}

/// 设置项右侧的控件。
#[derive(Clone, Copy, Debug)]
pub enum Control {
    Switch,
    /// 选项多或文案长：下拉。
    Select(Options),
    /// 两到六个短选项：分段控件，全部选项一眼可见。
    Segmented(Options),
    Number {
        min: u64,
        max: u64,
        suffix: Option<&'static str>,
    },
    Slider {
        min: u8,
        max: u8,
    },
    Tiles(TilesKind),
    /// 要记录的内容类型：一组复选框，提交时展开成 `capture.<kind>` 五个开关。
    CaptureKinds,
    /// 采集优先级：可拖动排序的列表（1.x `sortableTree`）。
    CaptureOrder,
    /// 卡片上显示哪些快捷按钮及顺序（1.x `sortableCheckboxTree`）。
    ActionVisibility,
    /// 分组栏上显示哪些内置按钮及顺序，与 `ActionVisibility` 共用排序弹框。
    GroupBarEntries,
    /// 忽略的应用：一行加「管理」对话框。
    AppExclusion,
    /// 打开时选中的分组。
    GroupSelect,
    /// 按钮。
    Action {
        danger: bool,
    },
    Permission(PermissionKind),
    /// 保留时长：数值加单位。
    Retention,
    /// 按类型、来源、大小的清理规则。
    RetentionRules,
    /// 用户文本匹配规则列表。
    CustomRules,
    /// 按来源应用自动归入分组的规则列表。
    AutoGroupRules,
    /// 清理状态与「立即清理」。
    CleanupStatus,
    /// 数据概览。
    StorageOverview,
    /// 局域网同步的设备、配对与连接控制面板。
    LanSync,
    /// 官方扩展的安装、更新与启用控制面板。
    Extensions,
    /// 图片文字识别的进度、结果和系统能力（开关下面的一行，按状态换标题和按钮）。
    ImageTextStatus,
    ShortcutRecorder,
    /// 一个全局功能的全部触发方式：行内列出，点进二级页增删。
    ShortcutEntry,
    /// 剪贴板窗口内的按键表，点按键重新录制。
    PanelShortcuts,
}

impl Control {
    /// 采集类型、采集顺序、清理规则和磁贴需要整行宽度，换到标题下方单独一行。
    pub fn full_width(self) -> bool {
        matches!(
            self,
            Self::Tiles(_)
                | Self::CaptureKinds
                | Self::CaptureOrder
                | Self::RetentionRules
                | Self::CustomRules
                | Self::AutoGroupRules
                | Self::LanSync
                | Self::Extensions
                | Self::PanelShortcuts
        )
    }
}

pub struct Setting {
    pub id: &'static str,
    /// 仅在偏好页的更多选项中显示；首次引导忽略此标记。
    pub advanced: bool,
    pub keywords: &'static [&'static str],
    pub control: Control,
    /// `settings.json` 里的点分路径；按钮、概览这类没有设置值的行为 `None`。
    pub path: Option<&'static str>,
    /// 子项：父开关关闭时整行收起。
    pub parent: Option<&'static str>,
    pub disabled_when: Option<fn(&Settings) -> bool>,
}

impl Setting {
    const fn new(id: &'static str, control: Control) -> Self {
        Self {
            id,
            advanced: false,
            keywords: &[],
            control,
            path: None,
            parent: None,
            disabled_when: None,
        }
    }

    const fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }

    /// 已有自定义清理规则始终显示，避免隐藏正在生效的规则。
    pub fn is_advanced(&self, settings: &Settings) -> bool {
        self.advanced && (self.id != "history.rules" || settings.clipboard.history.rules.is_empty())
    }

    const fn path(mut self, path: &'static str) -> Self {
        self.path = Some(path);
        self
    }

    const fn keywords(mut self, keywords: &'static [&'static str]) -> Self {
        self.keywords = keywords;
        self
    }

    const fn child_of(
        mut self,
        parent: &'static str,
        disabled_when: fn(&Settings) -> bool,
    ) -> Self {
        self.parent = Some(parent);
        self.disabled_when = Some(disabled_when);
        self
    }

    /// 只是不可用、不收起（不是父开关的子项）。
    const fn disabled_when(mut self, disabled_when: fn(&Settings) -> bool) -> Self {
        self.disabled_when = Some(disabled_when);
        self
    }

    pub fn is_disabled(&self, settings: &Settings) -> bool {
        self.disabled_when
            .is_some_and(|disabled| disabled(settings))
    }

    /// 父开关关闭时子项整行收起，不显示也不参与搜索高亮。
    pub fn is_collapsed(&self, settings: &Settings) -> bool {
        self.parent.is_some() && self.is_disabled(settings)
    }
}

const CLICK_ACTIONS: &[&str] = &[
    "disabled",
    "singleClickPaste",
    "doubleClickPaste",
    "singleClickCopy",
    "doubleClickCopy",
];
const COPY_SOUND_TONES: &[&str] = &["classic", "chirp", "bubble", "chime", "tick"];
const MIDDLE_CLICK_ACTIONS: &[&str] = &[
    "disabled",
    "singleClickPaste",
    "singleClickPastePlain",
    "singleClickCopy",
    "singleClickCopyPlain",
];
#[cfg(target_os = "windows")]
const TRAY_CLICK: &[&str] = &["clipboard", "preference"];
#[cfg(target_os = "windows")]
const MOUSE_TRIGGERS: &[&str] = &["disabled", "middle", "back", "forward"];
const SORTS: &[&str] = &["createdAtDesc", "updatedAtDesc", "useCountDesc"];
const STORAGE_LIMIT_ACTIONS: &[&str] = &["remind", "cleanup"];
const LANGUAGES: &[&str] = &["zh-CN", "en-US"];
const LIST_STYLES: &[&str] = &["card", "seamless"];
const PANEL_LAYOUTS: &[&str] = &["single", "split"];
const LIST_DENSITIES: &[&str] = &["comfortable", "standard", "compact", "custom"];
/// 自定义密度可选的条目间距与上下内边距（px），与列表的映射一一对应。
const LIST_ITEM_GAPS: &[u64] = &[0, 2, 4, 6, 8, 12];
const LIST_PADDINGS_Y: &[u64] = &[2, 4, 6, 8];
const WINDOW_POSITIONS: &[&str] = &["followCursor", "center", "remember"];
const OPEN_RANGES: &[&str] = &["preserve", "all", "favorite"];
const OPEN_CATEGORIES: &[&str] = &["preserve", "all", "text", "image", "files"];
const HOVER_DELAYS: &[&str] = &["ms300", "ms500", "ms1000"];
const TEXT_VIEWS: &[&str] = &["plain", "words"];
/// 预览窗与卡片之间可选的距离（px）。
const PREVIEW_GAPS: &[u64] = &[0, 8, 16, 28, 40, 56];
#[cfg(target_os = "windows")]
const DOUBLE_TAP_KEYS: &[&str] = &["disabled", "control", "shift", "alt"];
#[cfg(target_os = "macos")]
const DOUBLE_TAP_KEYS: &[&str] = &["disabled", "control", "shift", "alt", "command"];
const KEY_SIDES: &[&str] = &["any", "left", "right"];
const DOUBLE_TAP_KEYWORDS: &[&str] = &[
    "double tap",
    "double press",
    "双击",
    "ctrl",
    "shift",
    "alt",
    "option",
    "command",
    "modifier",
];
/// 快速粘贴的修饰键：显示顺序与 1.x 相同，快捷键字面量与 `QuickPasteModifiers::accelerator` 一致。
pub const QUICK_PASTE_MODIFIERS: &[(&str, &str)] = &[
    ("controlShift", "Control+Shift"),
    ("controlAlt", "Control+Alt"),
    ("altShift", "Alt+Shift"),
    ("alt", "Alt"),
    ("control", "Control"),
];
/// 与 Rust `MIN_STORAGE_LIMIT_MB` 一致；上限 1 TB。
pub const STORAGE_LIMIT_MIN_MB: u64 = 100;
pub const STORAGE_LIMIT_MAX_MB: u64 = 1024 * 1024;

fn not_custom_density(settings: &Settings) -> bool {
    settings.clipboard.display.density != ListDensity::Custom
}

/// 全部分类。平台条件与 1.x 相同；`portable` 时不显示以管理员身份运行（计划任务记着 exe 路径）。
/// 结构只随 `portable` 变化，偏好窗每帧都要遍历，所以各建一次后共用。
pub fn tabs(portable: bool) -> &'static [Tab] {
    static INSTALLED: LazyLock<Vec<Tab>> = LazyLock::new(|| build_tabs(false));
    static PORTABLE: LazyLock<Vec<Tab>> = LazyLock::new(|| build_tabs(true));
    if portable { &PORTABLE } else { &INSTALLED }
}

fn build_tabs(portable: bool) -> Vec<Tab> {
    vec![
        Tab {
            id: TabId::General,
            group: Group::App,
            icon: PrefIcon::Settings,
            sections: general_sections(portable),
        },
        Tab {
            id: TabId::Shortcuts,
            group: Group::App,
            icon: PrefIcon::Keyboard,
            sections: shortcut_sections(),
        },
        Tab {
            id: TabId::Appearance,
            group: Group::App,
            icon: PrefIcon::Palette,
            sections: appearance_sections(),
        },
        Tab {
            id: TabId::Window,
            group: Group::Clipboard,
            icon: PrefIcon::PanelTop,
            sections: window_sections(),
        },
        Tab {
            id: TabId::Paste,
            group: Group::Clipboard,
            icon: PrefIcon::ClipboardPaste,
            sections: paste_sections(),
        },
        Tab {
            id: TabId::Capture,
            group: Group::Clipboard,
            icon: PrefIcon::ClipboardPlus,
            sections: capture_sections(),
        },
        Tab {
            id: TabId::Sync,
            group: Group::Data,
            icon: PrefIcon::FolderSync,
            sections: vec![Section {
                id: "lanSync",
                settings: vec![
                    Setting::new("sync.lan.devices", Control::LanSync)
                        .keywords(&["sync", "lan", "network", "pair", "device", "ipv4", "ipv6"]),
                ],
            }],
        },
        Tab {
            id: TabId::Extensions,
            group: Group::Data,
            icon: PrefIcon::Puzzle,
            sections: extensions_sections(),
        },
        Tab {
            id: TabId::Data,
            group: Group::Data,
            icon: PrefIcon::Database,
            sections: data_sections(),
        },
        Tab {
            id: TabId::About,
            group: Group::Data,
            icon: PrefIcon::Info,
            sections: about_sections(),
        },
    ]
}

fn extensions_sections() -> Vec<Section> {
    vec![
        Section {
            id: "extensions",
            settings: vec![
                Setting::new("extensions.manager", Control::Extensions).keywords(&[
                    "扩展",
                    "插件",
                    "extension",
                    "plugin",
                    "OCR",
                    "文字识别",
                ]),
            ],
        },
        Section {
            id: "imageText",
            settings: vec![
                Setting::new("ocr.status", Control::ImageTextStatus).keywords(&[
                    "ocr",
                    "image",
                    "text",
                    "recognize",
                    "progress",
                    "language",
                    "文字识别",
                ]),
            ],
        },
    ]
}

fn general_sections(portable: bool) -> Vec<Section> {
    let mut sections = vec![Section {
        id: "startup",
        settings: vec![
            Setting::new("control.autoStart", Control::Switch)
                .path("general.autoStart")
                .keywords(&["startup", "login", "autostart"]),
            Setting::new("control.trayIcon", Control::Switch)
                .path("general.trayIcon")
                .keywords(&["tray", "menu bar", "system"]),
            // macOS 单击菜单栏图标弹出菜单，没有可选的单击行为。
            #[cfg(target_os = "windows")]
            Setting::new(
                "control.trayClick",
                Control::Segmented(Options::Values(TRAY_CLICK)),
            )
            .path("general.trayClick")
            .keywords(&["tray", "click", "preferences", "clipboard"])
            .child_of("control.trayIcon", |settings| !settings.general.tray_icon),
            #[cfg(target_os = "macos")]
            Setting::new("control.dockIcon", Control::Switch)
                .path("general.dockIcon")
                .keywords(&["dock", "taskbar", "icon"]),
            Setting::new(
                "appearance.language",
                Control::Select(Options::Values(LANGUAGES)),
            )
            .path("appearance.language")
            .keywords(&["language", "locale", "english"]),
        ],
    }];
    let mut permissions = vec![
        #[cfg(target_os = "macos")]
        Setting::new(
            "permissions.accessibility",
            Control::Permission(PermissionKind::Accessibility),
        )
        .keywords(&["accessibility", "permission", "paste", "macos"]),
        #[cfg(target_os = "macos")]
        Setting::new(
            "permissions.fullDiskAccess",
            Control::Permission(PermissionKind::FullDiskAccess),
        )
        .keywords(&["full disk", "permission", "privacy", "macos"]),
    ];
    if cfg!(target_os = "windows") && !portable {
        permissions.push(
            Setting::new(
                "permissions.runAsAdministrator",
                Control::Permission(PermissionKind::RunAsAdministrator),
            )
            .keywords(&["administrator", "admin", "permission", "windows", "uac"])
            .advanced(),
        );
    }
    if !permissions.is_empty() {
        sections.push(Section {
            id: "permissions",
            settings: permissions,
        });
    }
    sections.push(Section {
        id: "performance",
        settings: vec![
            Setting::new("window.lightweightMode", Control::Switch)
                .path("clipboard.window.lightweightMode")
                .keywords(&["system", "performance", "memory", "idle"])
                .advanced(),
            Setting::new(
                "window.idleDestroySeconds",
                Control::Number {
                    min: 5,
                    max: 86_400,
                    suffix: Some("seconds"),
                },
            )
            .path("clipboard.window.idleDestroySeconds")
            .keywords(&["system", "idle", "destroy", "seconds"])
            .child_of("window.lightweightMode", |settings| {
                !settings.clipboard.window.lightweight_mode
            })
            .advanced(),
        ],
    });
    sections
}

const PAUSE_KEYWORDS: &[&str] = &[
    "游戏",
    "全屏",
    "game",
    "fullscreen",
    "full screen",
    "exclude",
    "pause",
];

/// 快捷键页：每个全局功能一行（点进去管理触发方式）、停用规则、剪贴板窗口内的按键。
/// 双击按键、鼠标按键、Win+V 的设置都在「打开剪贴板窗口」的二级页里，关键词挂在这一行上。
fn shortcut_sections() -> Vec<Section> {
    vec![
        Section {
            id: "globalShortcuts",
            settings: vec![
                Setting::new("shortcuts.openClipboard", Control::ShortcutEntry)
                    .path("shortcuts.openClipboard")
                    .keywords(&[
                        "shortcut",
                        "hotkey",
                        "open",
                        "double tap",
                        "双击",
                        "mouse",
                        "middle click",
                        "side button",
                        "鼠标",
                        "win",
                        "winv",
                        "clipboard history",
                    ]),
                Setting::new("shortcuts.pastePlain", Control::ShortcutEntry)
                    .path("shortcuts.pastePlain")
                    .keywords(&["shortcut", "hotkey", "plain", "text", "paste", "纯文本"]),
                Setting::new("shortcuts.quickPaste", Control::ShortcutEntry)
                    .path("shortcuts.quickPaste")
                    .keywords(&["quick paste", "number", "digit", "paste", "modifier"]),
                Setting::new("shortcuts.openPreference", Control::ShortcutEntry)
                    .path("shortcuts.openPreference")
                    .keywords(&["shortcut", "hotkey", "preference"]),
            ],
        },
        Section {
            id: "shortcutPause",
            settings: vec![
                // TODO: macOS 还没有全屏判断（见 `kwikpaste_os::mac::trigger_pause`），先只在 Windows 显示。
                #[cfg(target_os = "windows")]
                Setting::new("shortcuts.pauseInFullscreen", Control::Switch)
                    .path("shortcuts.pauseInFullscreen")
                    .keywords(PAUSE_KEYWORDS),
                Setting::new("shortcuts.pauseApps", Control::AppExclusion)
                    .path("shortcuts.pauseAppIds")
                    .keywords(PAUSE_KEYWORDS),
            ],
        },
        Section {
            id: "panelShortcuts",
            settings: vec![
                Setting::new("shortcuts.panel", Control::PanelShortcuts).keywords(&[
                    "shortcut", "hotkey", "key", "window", "favorite", "pin", "copy", "delete",
                    "收藏", "置顶", "复制", "删除", "按键",
                ]),
            ],
        },
    ]
}

/// 首次引导使用完整列表，不按 advanced 标记折叠。
pub(super) fn shortcut_settings() -> Vec<Setting> {
    vec![
        Setting::new("shortcuts.openClipboard", Control::ShortcutRecorder)
            .path("shortcuts.openClipboard")
            .keywords(&["shortcut", "hotkey", "open"]),
        Setting::new("shortcuts.pastePlain", Control::ShortcutRecorder)
            .path("shortcuts.pastePlain")
            .keywords(&[
                "shortcut",
                "hotkey",
                "plain",
                "text",
                "paste",
                "format",
                "纯文本",
            ]),
        #[cfg(target_os = "windows")]
        Setting::new("shortcuts.winV", Control::Switch)
            .path("shortcuts.winV")
            .keywords(&["win", "winv", "windows", "clipboard history", "super"]),
        Setting::new(
            "shortcuts.doubleTap",
            Control::Select(Options::Values(DOUBLE_TAP_KEYS)),
        )
        .path("shortcuts.doubleTap.key")
        .keywords(DOUBLE_TAP_KEYWORDS),
        Setting::new(
            "shortcuts.doubleTapSide",
            Control::Segmented(Options::Values(KEY_SIDES)),
        )
        .path("shortcuts.doubleTap.side")
        .keywords(DOUBLE_TAP_KEYWORDS)
        .child_of("shortcuts.doubleTap", |settings| {
            settings.shortcuts.double_tap.key == kwikpaste_core::settings::DoubleTapKey::Disabled
        }),
        Setting::new("shortcuts.quickPaste", Control::Switch)
            .path("shortcuts.quickPaste.enabled")
            .keywords(&["quick paste", "number", "digit", "paste", "hotkey"]),
        Setting::new(
            "shortcuts.quickPasteModifiers",
            Control::Select(Options::Shortcuts(QUICK_PASTE_MODIFIERS)),
        )
        .path("shortcuts.quickPaste.modifiers")
        .keywords(&["quick paste", "modifier", "ctrl", "alt", "shift"])
        .child_of("shortcuts.quickPaste", |settings| {
            !settings.shortcuts.quick_paste.enabled
        }),
        Setting::new("shortcuts.openPreference", Control::ShortcutRecorder)
            .path("shortcuts.openPreference")
            .keywords(&["shortcut", "hotkey", "preference"])
            .advanced(),
        #[cfg(target_os = "windows")]
        Setting::new(
            "shortcuts.mouseTrigger",
            Control::Select(Options::Values(MOUSE_TRIGGERS)),
        )
        .path("shortcuts.mouseTrigger")
        .keywords(&[
            "mouse",
            "middle click",
            "middle button",
            "side button",
            "back",
            "forward",
            "xbutton",
            "wheel",
        ])
        .advanced(),
        // TODO: macOS 还没有全屏判断（见 `kwikpaste_os::mac::trigger_pause`），先只在 Windows 显示。
        #[cfg(target_os = "windows")]
        Setting::new("shortcuts.pauseInFullscreen", Control::Switch)
            .path("shortcuts.pauseInFullscreen")
            .keywords(PAUSE_KEYWORDS)
            .advanced(),
        Setting::new("shortcuts.pauseApps", Control::AppExclusion)
            .path("shortcuts.pauseAppIds")
            .keywords(PAUSE_KEYWORDS)
            .advanced(),
    ]
}

fn appearance_sections() -> Vec<Section> {
    vec![
        Section {
            id: "appearance",
            settings: vec![
                Setting::new("appearance.theme", Control::Tiles(TilesKind::Theme))
                    .path("appearance.theme")
                    .keywords(&["theme", "dark", "light"]),
                Setting::new("appearance.material", Control::Tiles(TilesKind::Material))
                    .path("appearance.material")
                    .keywords(&["material", "mica", "frosted", "blur", "transparency"]),
                Setting::new("appearance.listStyle", Control::Tiles(TilesKind::ListStyle))
                    .path("clipboard.display.listStyle")
                    .keywords(&["list", "card", "seamless", "divider", "style"]),
                Setting::new(
                    "appearance.listDensity",
                    Control::Segmented(Options::Values(LIST_DENSITIES)),
                )
                .path("clipboard.display.density")
                .keywords(&["density", "compact", "spacing", "height", "custom"]),
                Setting::new("appearance.headerRow", Control::Switch)
                    .path("clipboard.display.customLayout.headerRow")
                    .keywords(&["density", "header", "time", "type", "icon"])
                    .child_of("appearance.listDensity", not_custom_density),
                // 无间风格条目贴边排列，间距不生效，一并收起。
                Setting::new(
                    "appearance.itemGap",
                    Control::Segmented(Options::Numbers(LIST_ITEM_GAPS)),
                )
                .path("clipboard.display.customLayout.itemGap")
                .keywords(&["density", "gap", "spacing", "margin"])
                .child_of("appearance.listDensity", |settings| {
                    not_custom_density(settings)
                        || settings.clipboard.display.list_style == ListStyle::Seamless
                }),
                Setting::new(
                    "appearance.itemPadding",
                    Control::Segmented(Options::Numbers(LIST_PADDINGS_Y)),
                )
                .path("clipboard.display.customLayout.paddingY")
                .keywords(&["density", "padding", "height"])
                .child_of("appearance.listDensity", not_custom_density),
                Setting::new("appearance.groupBar", Control::GroupBarEntries)
                    .path("clipboard.window.groupBar")
                    .keywords(&[
                        "group",
                        "bar",
                        "favorite",
                        "category",
                        "text",
                        "image",
                        "files",
                        "order",
                        "分组栏",
                        "快捷入口",
                    ]),
            ],
        },
        Section {
            id: "cards",
            settings: vec![
                Setting::new("actions.visible", Control::ActionVisibility)
                    .path("clipboard.content.itemActions")
                    .keywords(&["action", "hover", "buttons"])
                    .advanced(),
                Setting::new("paste.quickSnippets", Control::Switch)
                    .path("clipboard.display.quickSnippets")
                    .keywords(&["quick", "snippet", "extract", "number", "code"])
                    .advanced(),
                Setting::new("paste.quickSnippetColors", Control::Switch)
                    .path("clipboard.display.quickSnippetColors")
                    .keywords(&["quick", "snippet", "color", "highlight"])
                    .advanced()
                    .child_of("paste.quickSnippets", |settings| {
                        !settings.clipboard.display.quick_snippets
                    }),
                Setting::new(
                    "appearance.textMaxLines",
                    Control::Number {
                        min: 1,
                        max: 5,
                        suffix: Some("lines"),
                    },
                )
                .path("clipboard.display.textMaxLines")
                .keywords(&["density", "text", "line", "compact"])
                .advanced(),
                Setting::new(
                    "appearance.imageMaxHeight",
                    Control::Number {
                        min: 20,
                        max: 100,
                        suffix: Some("px"),
                    },
                )
                .path("clipboard.display.imageMaxHeight")
                .keywords(&["density", "image", "height", "thumbnail"])
                .advanced(),
                Setting::new(
                    "appearance.fileMaxCount",
                    Control::Number {
                        min: 1,
                        max: 5,
                        suffix: Some("files"),
                    },
                )
                .path("clipboard.display.fileMaxCount")
                .keywords(&["density", "file", "count", "array"])
                .advanced(),
            ],
        },
    ]
}

fn window_sections() -> Vec<Section> {
    vec![
        Section {
            id: "window",
            settings: vec![
                Setting::new(
                    "appearance.panelLayout",
                    Control::Tiles(TilesKind::PanelLayout),
                )
                .path("clipboard.display.layout")
                .keywords(&["layout", "split", "column", "favorite", "group", "sidebar"]),
                Setting::new("window.position", Control::Tiles(TilesKind::WindowPosition))
                    .path("clipboard.window.position")
                    .keywords(&["window", "position", "cursor"]),
                Setting::new("search.defaultFocus", Control::Switch)
                    .path("clipboard.search.defaultFocus")
                    .keywords(&["search", "focus", "open"]),
                Setting::new("window.scrollToTopOnOpen", Control::Switch)
                    .path("clipboard.window.scrollToTopOnOpen")
                    .keywords(&["window", "scroll", "top", "open"])
                    .advanced(),
                Setting::new(
                    "window.selectRangeOnOpen",
                    Control::Segmented(Options::Values(OPEN_RANGES)),
                )
                .path("clipboard.window.selectRangeOnOpen")
                .keywords(&["window", "range", "all", "favorite", "open"])
                .advanced(),
                Setting::new(
                    "window.selectCategoryOnOpen",
                    Control::Select(Options::Values(OPEN_CATEGORIES)),
                )
                .path("clipboard.window.selectCategoryOnOpen")
                .keywords(&["window", "category", "kind", "all", "open"])
                .advanced(),
                Setting::new("window.selectGroupOnOpen", Control::GroupSelect)
                    .path("clipboard.window.selectGroupOnOpen")
                    .keywords(&["window", "group", "folder", "all", "open"])
                    .advanced(),
                Setting::new("search.clearOnHide", Control::Switch)
                    .path("clipboard.search.clearOnHide")
                    .keywords(&["search", "clear", "hide"])
                    .advanced(),
            ],
        },
        Section {
            id: "sort",
            settings: vec![
                Setting::new("search.sort", Control::Segmented(Options::Values(SORTS)))
                    .path("clipboard.content.sort")
                    .keywords(&["sort", "frequency", "usage", "created", "updated"]),
                Setting::new("copy.updateOnReuse", Control::Switch)
                    .path("clipboard.content.updateOnReuse")
                    .keywords(&["copy", "paste", "reuse", "sort", "frequency"])
                    .advanced(),
            ],
        },
        Section {
            id: "preview",
            settings: vec![
                Setting::new("preview.hover", Control::Switch)
                    .path("clipboard.preview.hoverEnabled")
                    .keywords(&["preview", "hover"]),
                Setting::new(
                    "preview.delay",
                    Control::Segmented(Options::Values(HOVER_DELAYS)),
                )
                .path("clipboard.preview.hoverDelayMs")
                .keywords(&["preview", "delay", "hover"])
                .child_of("preview.hover", |settings| {
                    !settings.clipboard.preview.hover_enabled
                }),
                Setting::new("preview.space", Control::Switch)
                    .path("clipboard.preview.spaceEnabled")
                    .keywords(&["space", "preview", "keyboard"]),
                Setting::new(
                    "preview.textView",
                    Control::Segmented(Options::Values(TEXT_VIEWS)),
                )
                .path("clipboard.preview.textView")
                .keywords(&["preview", "text", "words", "split"])
                .advanced(),
                Setting::new(
                    "preview.gap",
                    Control::Segmented(Options::Numbers(PREVIEW_GAPS)),
                )
                .path("clipboard.preview.gap")
                .keywords(&["preview", "gap", "spacing", "distance", "margin"])
                .advanced(),
                Setting::new("appearance.showOriginalPreview", Control::Switch)
                    .path("clipboard.content.showOriginalPreview")
                    .keywords(&["note", "hover", "original", "preview"])
                    .advanced(),
            ],
        },
        Section {
            id: "organizing",
            settings: vec![
                Setting::new("organizing.customGroups", Control::Action { danger: false })
                    .keywords(&["group", "folder", "organize"]),
                Setting::new("organizing.hideFavoritesInAll", Control::Switch)
                    .path("clipboard.content.hideFavoritesInAll")
                    .keywords(&["favorite", "hide", "all", "收藏", "隐藏"]),
                Setting::new("organizing.autoFavorite", Control::Switch)
                    .path("clipboard.content.autoFavorite")
                    .keywords(&["note", "favorite", "auto"])
                    .advanced(),
            ],
        },
        Section {
            id: "deleteProtection",
            settings: vec![
                Setting::new("actions.deleteConfirm", Control::Switch)
                    .path("clipboard.content.deleteConfirm")
                    .keywords(&["delete", "confirm"]),
                Setting::new("actions.deleteFavoriteItems", Control::Switch)
                    .path("clipboard.content.deleteFavoriteItems")
                    .keywords(&["delete", "favorite", "allow"])
                    .advanced(),
                Setting::new("actions.deleteFavoriteConfirm", Control::Switch)
                    .path("clipboard.content.deleteFavoriteConfirm")
                    .keywords(&["delete", "favorite", "confirm"])
                    .child_of("actions.deleteFavoriteItems", |settings| {
                        !settings.clipboard.content.delete_favorite_items
                    })
                    .advanced(),
                Setting::new(
                    "actions.deleteFavoriteItemsOnlyInFavoriteGroup",
                    Control::Switch,
                )
                .path("clipboard.content.deleteFavoriteItemsOnlyInFavoriteGroup")
                .keywords(&["delete", "favorite", "group"])
                .child_of("actions.deleteFavoriteItems", |settings| {
                    !settings.clipboard.content.delete_favorite_items
                })
                .advanced(),
                Setting::new("actions.deletePinnedItems", Control::Switch)
                    .path("clipboard.content.deletePinnedItems")
                    .keywords(&["delete", "pinned", "pin", "allow"])
                    .advanced(),
                Setting::new("actions.deletePinnedConfirm", Control::Switch)
                    .path("clipboard.content.deletePinnedConfirm")
                    .keywords(&["delete", "pinned", "pin", "confirm"])
                    .child_of("actions.deletePinnedItems", |settings| {
                        !settings.clipboard.content.delete_pinned_items
                    })
                    .advanced(),
            ],
        },
    ]
}

fn paste_sections() -> Vec<Section> {
    vec![
        Section {
            id: "click",
            settings: vec![
                Setting::new(
                    "paste.autoPaste",
                    Control::Select(Options::Values(CLICK_ACTIONS)),
                )
                .path("clipboard.content.autoPaste")
                .keywords(&["paste", "click", "auto"]),
                Setting::new(
                    "paste.middleClick",
                    Control::Select(Options::Values(MIDDLE_CLICK_ACTIONS)),
                )
                .path("clipboard.content.middleClick")
                .keywords(&["paste", "middle click", "mouse"]),
                Setting::new("copy.hideWindow", Control::Switch)
                    .path("clipboard.content.copyThenHideWindow")
                    .keywords(&["copy", "hide", "window"]),
            ],
        },
        Section {
            id: "format",
            settings: vec![
                Setting::new("paste.plainDefault", Control::Switch)
                    .path("clipboard.content.pastePlain")
                    .keywords(&["plain", "paste", "format"]),
                Setting::new("paste.fileMode", Control::Switch)
                    .path("clipboard.content.pasteFilesAsPath")
                    .keywords(&["file", "path", "paste"])
                    .advanced(),
                Setting::new("copy.plainDefault", Control::Switch)
                    .path("clipboard.content.copyPlain")
                    .keywords(&["copy", "plain", "format"])
                    .advanced(),
            ],
        },
        Section {
            id: "sound",
            settings: vec![
                Setting::new("copy.sound", Control::Switch)
                    .path("clipboard.feedback.copySound")
                    .keywords(&["sound", "feedback", "copy", "音效", "提示音", "声音"])
                    .advanced(),
                Setting::new(
                    "copy.sound.tone",
                    Control::Segmented(Options::Values(COPY_SOUND_TONES)),
                )
                .path("clipboard.feedback.copySoundTone")
                .keywords(&["音色", "音效", "提示音", "试听", "tone", "sound", "preview"])
                .child_of("copy.sound", |settings| {
                    !settings.clipboard.feedback.copy_sound
                })
                .advanced(),
                Setting::new("copy.sound.volume", Control::Slider { min: 0, max: 100 })
                    .path("clipboard.feedback.copySoundVolume")
                    .keywords(&["音量", "音效", "试听", "volume", "sound", "preview"])
                    .child_of("copy.sound", |settings| {
                        !settings.clipboard.feedback.copy_sound
                    })
                    .advanced(),
            ],
        },
    ]
}

fn capture_sections() -> Vec<Section> {
    vec![
        Section {
            id: "capture",
            settings: vec![
                Setting::new("capture.kinds", Control::CaptureKinds)
                    .path("clipboard.capture")
                    .keywords(&[
                        "text",
                        "plain",
                        "html",
                        "rtf",
                        "rich text",
                        "image",
                        "picture",
                        "file",
                        "folder",
                        "record",
                    ]),
                Setting::new("capture.order", Control::CaptureOrder)
                    .path("clipboard.capture.order")
                    .keywords(&["priority", "order", "format", "rich text"])
                    .advanced(),
                Setting::new(
                    "capture.maxTextMb",
                    Control::Number {
                        min: 0,
                        max: u64::from(u32::MAX),
                        suffix: Some("mb"),
                    },
                )
                .path("clipboard.capture.maxTextMb")
                .keywords(&["text", "size", "limit", "mb"])
                .advanced(),
                Setting::new(
                    "capture.maxImageMb",
                    Control::Number {
                        min: 0,
                        max: u64::from(u32::MAX),
                        suffix: Some("mb"),
                    },
                )
                .path("clipboard.capture.maxImageMb")
                .keywords(&["image", "picture", "size", "limit", "mb"])
                .advanced(),
            ],
        },
        Section {
            id: "sensitive",
            settings: vec![
                Setting::new("sensitive.collectSecrets", Control::Switch)
                    .path("clipboard.sensitive.collectSecrets")
                    .keywords(&["token", "key", "secret", "code"]),
                Setting::new("sensitive.redactSecrets", Control::Switch)
                    .path("clipboard.sensitive.redactSecrets")
                    .keywords(&["token", "key", "secret", "redact", "mask"]),
                Setting::new("source.excludedApps", Control::AppExclusion)
                    .path("clipboard.filters.excludedAppIds")
                    .keywords(&["exclude", "ignore", "app", "source"]),
            ],
        },
        Section {
            id: "autoGroup",
            settings: vec![
                Setting::new("autoGroupRules", Control::AutoGroupRules)
                    .path("clipboard.autoGroupRules")
                    .keywords(&["group", "app", "source", "code", "editor", "snippet"]),
            ],
        },
        Section {
            id: "customRules",
            settings: vec![
                Setting::new("customRules", Control::CustomRules)
                    .path(kwikpaste_core::settings::paths::CUSTOM_RULES)
                    .keywords(&["regex", "pattern", "snippet", "sensitive", "ignore"]),
            ],
        },
        Section {
            id: "cleanup",
            settings: vec![
                Setting::new("history.retention", Control::Retention)
                    .path("clipboard.history.retention")
                    .keywords(&["retention", "cleanup", "history", "expire"]),
                Setting::new("history.cleanupStatus", Control::CleanupStatus).keywords(&[
                    "cleanup",
                    "status",
                    "protected",
                    "now",
                ]),
                Setting::new("history.rules", Control::RetentionRules)
                    .path("clipboard.history.rules")
                    .keywords(&[
                        "retention",
                        "rules",
                        "cleanup",
                        "expire",
                        "image",
                        "text",
                        "size",
                        "sensitive",
                    ])
                    .advanced(),
                Setting::new(
                    "history.maxCount",
                    Control::Number {
                        min: 0,
                        max: u64::from(u32::MAX),
                        suffix: Some("items"),
                    },
                )
                .path("clipboard.history.maxCount")
                .keywords(&["max", "count", "limit"])
                .advanced(),
                Setting::new(
                    "localData.storageLimit",
                    Control::Number {
                        min: STORAGE_LIMIT_MIN_MB,
                        max: STORAGE_LIMIT_MAX_MB,
                        suffix: Some("mb"),
                    },
                )
                .path("clipboard.history.storageLimitMb")
                .keywords(&["storage", "limit", "size", "quota", "disk"])
                .advanced(),
                Setting::new(
                    "localData.storageLimitAction",
                    Control::Segmented(Options::Values(STORAGE_LIMIT_ACTIONS)),
                )
                .path("clipboard.history.storageLimitAction")
                .keywords(&["storage", "limit", "cleanup", "remind"])
                .advanced(),
            ],
        },
    ]
}

fn data_sections() -> Vec<Section> {
    vec![
        Section {
            id: "overview",
            settings: vec![
                Setting::new("overview.dashboard", Control::StorageOverview).keywords(&[
                    "overview",
                    "statistics",
                    "storage",
                    "usage",
                    "count",
                    "category",
                    "trend",
                    "source",
                ]),
            ],
        },
        Section {
            id: "backup",
            settings: vec![
                Setting::new("backup.export", Control::Action { danger: false }).keywords(&[
                    "export",
                    "backup",
                    "history",
                    "excel",
                    "xlsx",
                    "markdown",
                    "groups",
                    "favorites",
                ]),
                Setting::new("backup.importHistory", Control::Action { danger: false })
                    .keywords(&["import", "backup", "history"]),
            ],
        },
        Section {
            id: "localData",
            settings: vec![
                Setting::new("localData.dataDirectory", Control::Action { danger: false })
                    .keywords(&["database", "sqlite", "local", "cache", "image", "icon"]),
                Setting::new("localData.cleanCache", Control::Action { danger: false })
                    .keywords(&["cache", "clean", "storage"]),
                Setting::new("localData.clearHistory", Control::Action { danger: true })
                    .keywords(&["clear", "history", "records", "delete"]),
            ],
        },
    ]
}

fn about_sections() -> Vec<Section> {
    vec![
        Section {
            id: "about",
            settings: vec![
                Setting::new("about.website", Control::Action { danger: false })
                    .keywords(&["website", "homepage", "download"]),
                Setting::new("about.github", Control::Action { danger: false }).keywords(&[
                    "github",
                    "source",
                    "repository",
                    "open source",
                ]),
                Setting::new(
                    "control.reopenOnboarding",
                    Control::Action { danger: false },
                )
                .keywords(&["onboarding", "guide", "welcome", "help"]),
            ],
        },
        Section {
            id: "updates",
            settings: vec![
                Setting::new("about.checkUpdates", Control::Action { danger: false })
                    .keywords(&["update", "version"]),
            ],
        },
        Section {
            id: "diagnostics",
            settings: vec![
                Setting::new(
                    "diagnostics.reportProblem",
                    Control::Action { danger: false },
                )
                .keywords(&[
                    "feedback", "bug", "report", "problem", "issue", "log", "反馈", "日志",
                ])
                .advanced(),
                Setting::new("localData.logDirectory", Control::Action { danger: false })
                    .keywords(&["log", "diagnostic"])
                    .advanced(),
                Setting::new(
                    "diagnostics.resetPreferences",
                    Control::Action { danger: true },
                )
                .keywords(&["reset", "preferences"])
                .advanced(),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::Value;

    use super::*;

    fn pointer(path: &str) -> String {
        format!("/{}", path.replace('.', "/"))
    }

    #[test]
    fn double_tap_rows_are_primary_and_side_collapses_only_when_disabled() {
        use kwikpaste_core::settings::DoubleTapKey;

        let rows = shortcut_settings();
        let key = rows
            .iter()
            .find(|row| row.id == "shortcuts.doubleTap")
            .unwrap();
        let side = rows
            .iter()
            .find(|row| row.id == "shortcuts.doubleTapSide")
            .unwrap();
        assert!(!key.advanced && !side.advanced);
        assert_eq!(key.path, Some("shortcuts.doubleTap.key"));
        assert_eq!(side.path, Some("shortcuts.doubleTap.side"));
        assert_eq!(side.parent, Some(key.id));
        let mut settings = Settings::default();
        assert!(side.is_collapsed(&settings));
        settings.shortcuts.double_tap.key = DoubleTapKey::Control;
        assert!(!side.is_collapsed(&settings));
        #[cfg(target_os = "windows")]
        assert!(!DOUBLE_TAP_KEYS.contains(&"command"));
        #[cfg(target_os = "macos")]
        assert!(DOUBLE_TAP_KEYS.contains(&"command"));
        for keyword in DOUBLE_TAP_KEYWORDS {
            assert!(key.keywords.contains(keyword));
        }
    }

    #[test]
    fn sidebar_order_and_moved_settings_keep_their_new_home() {
        let tabs = tabs(false);
        assert_eq!(
            tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            [
                TabId::General,
                TabId::Shortcuts,
                TabId::Appearance,
                TabId::Window,
                TabId::Paste,
                TabId::Capture,
                TabId::Sync,
                TabId::Extensions,
                TabId::Data,
                TabId::About,
            ]
        );
        assert!(tabs[..3].iter().all(|tab| tab.group == Group::App));
        assert!(tabs[3..6].iter().all(|tab| tab.group == Group::Clipboard));
        assert!(tabs[6..].iter().all(|tab| tab.group == Group::Data));
        let find = |id| {
            tabs.iter()
                .find_map(|tab| {
                    tab.sections.iter().find_map(|section| {
                        section
                            .settings
                            .iter()
                            .find(|setting| setting.id == id)
                            .map(|setting| (tab.id, section.id, setting.advanced))
                    })
                })
                .unwrap()
        };
        assert_eq!(
            find("appearance.language"),
            (TabId::General, "startup", false)
        );
        assert_eq!(find("actions.visible"), (TabId::Appearance, "cards", true));
        assert_eq!(
            find("appearance.panelLayout"),
            (TabId::Window, "window", false)
        );
        assert_eq!(
            find("localData.storageLimit"),
            (TabId::Capture, "cleanup", true)
        );
        assert_eq!(
            find("control.reopenOnboarding"),
            (TabId::About, "about", false)
        );
        assert_eq!(
            find("diagnostics.reportProblem"),
            (TabId::About, "diagnostics", true)
        );
        assert_eq!(tabs[8].sections[0].settings[0].id, "overview.dashboard");
        // Onboarding consumes this entire list, including advanced shortcuts.
        assert!(
            shortcut_settings()
                .iter()
                .any(|setting| setting.id == "shortcuts.openPreference" && setting.advanced)
        );
    }

    #[test]
    fn custom_cleanup_rules_stay_visible_even_when_disabled() {
        let sections = capture_sections();
        let rule = sections
            .iter()
            .flat_map(|section| &section.settings)
            .find(|setting| setting.id == "history.rules")
            .unwrap();
        let mut settings = Settings::default();
        assert!(rule.is_advanced(&settings));
        settings
            .clipboard
            .history
            .rules
            .push(kwikpaste_core::settings::RetentionRule {
                enabled: false,
                ..Default::default()
            });
        assert!(!rule.is_advanced(&settings));
        settings.clipboard.history.rules.clear();
        assert!(rule.is_advanced(&settings));
    }

    #[test]
    fn ids_are_unique_and_paths_name_real_settings() {
        let defaults = serde_json::to_value(Settings::default()).unwrap_or(Value::Null);
        let mut ids = HashSet::new();
        for tab in tabs(false) {
            for section in &tab.sections {
                for setting in &section.settings {
                    assert!(ids.insert(setting.id), "duplicate id {}", setting.id);
                    if let Some(path) = setting.path {
                        assert!(
                            defaults.pointer(&pointer(path)).is_some(),
                            "{} has no settings key {path}",
                            setting.id
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn children_follow_their_parent_in_the_same_section() {
        for tab in tabs(false) {
            for section in &tab.sections {
                for (index, setting) in section.settings.iter().enumerate() {
                    let Some(parent) = setting.parent else {
                        continue;
                    };
                    let parent_index = section.settings.iter().position(|item| item.id == parent);
                    assert_eq!(
                        setting.advanced,
                        section.settings[parent_index.unwrap()].advanced
                    );
                    assert!(
                        parent_index.is_some_and(|parent_index| parent_index < index),
                        "{} must come after {parent}",
                        setting.id
                    );
                }
            }
        }
    }

    #[test]
    fn copy_sound_rows_live_on_paste_after_format_and_collapse_with_the_switch() {
        assert!(
            capture_sections()
                .iter()
                .flat_map(|section| &section.settings)
                .all(|setting| !setting.id.starts_with("copy.sound"))
        );
        let sections = paste_sections();
        let sound_index = sections
            .iter()
            .position(|section| section.id == "sound")
            .unwrap();
        assert_eq!(sections[sound_index - 1].id, "format");
        let sound = &sections[sound_index];
        assert_eq!(
            sound
                .settings
                .iter()
                .map(|setting| setting.id)
                .collect::<Vec<_>>(),
            ["copy.sound", "copy.sound.tone", "copy.sound.volume"]
        );
        assert_eq!(
            sound.settings[2].path,
            Some("clipboard.feedback.copySoundVolume")
        );
        assert!(matches!(
            sound.settings[2].control,
            Control::Slider { min: 0, max: 100 }
        ));
        let mut settings = Settings::default();
        assert!(!sound.settings[0].is_collapsed(&settings));
        for row in &sound.settings[1..] {
            assert_eq!(row.parent, Some("copy.sound"));
            assert!(row.is_collapsed(&settings));
        }
        settings.clipboard.feedback.copy_sound = true;
        assert!(
            sound
                .settings
                .iter()
                .all(|row| !row.is_collapsed(&settings))
        );
        for keyword in ["音效", "提示音", "声音"] {
            assert!(super::super::view::search_matches(
                keyword,
                "",
                sound.settings[0].keywords
            ));
        }
        for keyword in ["音量", "音效", "试听", "volume", "sound", "preview"] {
            assert!(super::super::view::search_matches(
                keyword,
                "",
                sound.settings[2].keywords
            ));
        }
    }
    #[test]
    fn image_text_status_lives_only_in_extensions() {
        let capture = capture_sections();
        let rows: Vec<_> = capture
            .iter()
            .flat_map(|section| &section.settings)
            .collect();
        assert!(rows.iter().all(|setting| setting.id != "ocr.enabled"));
        assert!(rows.iter().all(|setting| setting.id != "ocr.status"));
        let extensions = extensions_sections();
        assert!(
            extensions
                .iter()
                .flat_map(|section| &section.settings)
                .any(|setting| setting.id == "ocr.status"
                    && matches!(setting.control, Control::ImageTextStatus))
        );
        let tabs = tabs(false);
        let sync = tabs.iter().position(|tab| tab.id == TabId::Sync).unwrap();
        assert_eq!(tabs[sync + 1].id, TabId::Extensions);
        let manager = &extensions[0].settings[0];
        assert!(manager.control.full_width());
        for keyword in ["扩展", "插件", "extension", "plugin", "OCR", "文字识别"] {
            assert!(super::super::view::search_matches(
                keyword,
                "",
                manager.keywords
            ));
        }
    }
}
