//! 偏好窗的结构：分类（侧栏）→ 分组 → 设置项，照 1.x `src/pages/Preference/config/preferenceSchema.ts`。
//!
//! 设置项 id 是 i18n key（`preferences:schema.settings.<id>.*`）与搜索跳转的目标，挪动分类时保持不变；
//! `path` 是 `settings.json` 的点分 camelCase 路径，读值和写补丁都按它走。

use super::icons::PrefIcon;
use kwikpaste_core::settings::{ListDensity, ListStyle, Settings};

/// 侧栏分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabId {
    General,
    Shortcuts,
    Appearance,
    Capture,
    Window,
    Paste,
    Items,
    Sync,
    Overview,
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
            Self::Items => "items",
            Self::Sync => "sync",
            Self::Overview => "overview",
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

/// 外观磁贴的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TilesKind {
    Theme,
    Material,
}

impl TilesKind {
    pub fn values(self) -> &'static [&'static str] {
        match self {
            Self::Theme => &["auto", "light", "dark"],
            Self::Material => &["default", "mica", "acrylic"],
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

/// 下拉选项：值的文案取 `schema.settings.<id>.options.<value>`；快捷键选项直接显示按键。
#[derive(Clone, Copy, Debug)]
pub enum Options {
    Values(&'static [&'static str]),
    /// 数字取值（列表间距、内边距），文案同样按值取。
    Numbers(&'static [u64]),
    /// `(值, 快捷键字面量)`：快速粘贴的修饰键。
    Shortcuts(&'static [(&'static str, &'static str)]),
}

/// 设置项右侧的控件。1.x 的 `select` 与 `segmented` 视觉统一为下拉，这里合成一种。
#[derive(Clone, Copy, Debug)]
pub enum Control {
    Switch,
    Select(Options),
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
    /// 清理状态与「立即清理」。
    CleanupStatus,
    /// 数据概览。
    StorageOverview,
    /// 局域网同步的设备、配对与连接控制面板。
    LanSync,
    /// 图片文字识别的进度、结果和系统能力（开关下面的一行，按状态换标题和按钮）。
    ImageTextStatus,
    ShortcutRecorder,
}

impl Control {
    /// 采集类型、采集顺序和清理规则需要整行宽度，换到标题下方单独一行；外观磁贴现在画成
    /// 下拉框，和其他选项一样靠右。
    pub fn full_width(self) -> bool {
        matches!(
            self,
            Self::CaptureKinds | Self::CaptureOrder | Self::RetentionRules | Self::LanSync
        )
    }
}

pub struct Setting {
    pub id: &'static str,
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
            keywords: &[],
            control,
            path: None,
            parent: None,
            disabled_when: None,
        }
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
const LIST_DENSITIES: &[&str] = &["comfortable", "standard", "compact", "custom"];
/// 自定义密度可选的条目间距与上下内边距（px），与列表的映射一一对应。
const LIST_ITEM_GAPS: &[u64] = &[0, 2, 4, 6, 8, 12];
const LIST_PADDINGS_Y: &[u64] = &[2, 4, 6, 8];
const WINDOW_POSITIONS: &[&str] = &["followCursor", "center", "remember"];
const OPEN_RANGES: &[&str] = &["preserve", "all", "favorite"];
const OPEN_CATEGORIES: &[&str] = &["preserve", "all", "text", "image", "files"];
const HOVER_DELAYS: &[&str] = &["ms300", "ms500", "ms1000"];
const TEXT_VIEWS: &[&str] = &["plain", "words"];
const UPDATE_FREQUENCIES: &[&str] = &["daily", "weekly", "monthly"];
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
pub fn tabs(portable: bool) -> Vec<Tab> {
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
            sections: vec![Section {
                id: "globalShortcuts",
                settings: shortcut_settings(),
            }],
        },
        Tab {
            id: TabId::Appearance,
            group: Group::App,
            icon: PrefIcon::Palette,
            sections: appearance_sections(),
        },
        Tab {
            id: TabId::Capture,
            group: Group::Clipboard,
            icon: PrefIcon::ClipboardPlus,
            sections: capture_sections(),
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
            id: TabId::Items,
            group: Group::Clipboard,
            icon: PrefIcon::Layers,
            sections: item_sections(),
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
            id: TabId::Overview,
            group: Group::Data,
            icon: PrefIcon::ChartPie,
            sections: vec![Section {
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
            }],
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

fn general_sections(portable: bool) -> Vec<Section> {
    let mut startup = vec![
        Setting::new("control.autoStart", Control::Switch)
            .path("general.autoStart")
            .keywords(&["startup", "login", "autostart"]),
        Setting::new("control.trayIcon", Control::Switch)
            .path("general.trayIcon")
            .keywords(&["tray", "menu bar", "system"]),
    ];
    // macOS 单击菜单栏图标弹出菜单，没有可选的单击行为。
    #[cfg(target_os = "windows")]
    startup.push(
        Setting::new(
            "control.trayClick",
            Control::Select(Options::Values(TRAY_CLICK)),
        )
        .path("general.trayClick")
        .keywords(&["tray", "click", "preferences", "clipboard"])
        .child_of("control.trayIcon", |settings| !settings.general.tray_icon),
    );
    #[cfg(target_os = "macos")]
    startup.push(
        Setting::new("control.dockIcon", Control::Switch)
            .path("general.dockIcon")
            .keywords(&["dock", "taskbar", "icon"]),
    );
    startup.extend([Setting::new(
        "control.reopenOnboarding",
        Control::Action { danger: false },
    )
    .keywords(&["onboarding", "guide", "welcome", "help"])]);

    let mut sections = vec![Section {
        id: "startup",
        settings: startup,
    }];

    let mut permissions = Vec::new();
    #[cfg(target_os = "macos")]
    permissions.extend([
        Setting::new(
            "permissions.accessibility",
            Control::Permission(PermissionKind::Accessibility),
        )
        .keywords(&["accessibility", "permission", "paste", "macos"]),
        Setting::new(
            "permissions.fullDiskAccess",
            Control::Permission(PermissionKind::FullDiskAccess),
        )
        .keywords(&["full disk", "permission", "privacy", "macos"]),
    ]);
    if cfg!(target_os = "windows") && !portable {
        permissions.push(
            Setting::new(
                "permissions.runAsAdministrator",
                Control::Permission(PermissionKind::RunAsAdministrator),
            )
            .keywords(&["administrator", "admin", "permission", "windows", "uac"]),
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
                .keywords(&["system", "performance", "memory", "idle"]),
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
            }),
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

pub(super) fn shortcut_settings() -> Vec<Setting> {
    let mut settings = vec![
        Setting::new("shortcuts.openClipboard", Control::ShortcutRecorder)
            .path("shortcuts.openClipboard")
            .keywords(&["shortcut", "hotkey", "open"]),
        Setting::new("shortcuts.openPreference", Control::ShortcutRecorder)
            .path("shortcuts.openPreference")
            .keywords(&["shortcut", "hotkey", "preference"]),
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
    ];
    #[cfg(target_os = "windows")]
    settings.extend([
        Setting::new("shortcuts.winV", Control::Switch)
            .path("shortcuts.winV")
            .keywords(&["win", "winv", "windows", "clipboard history", "super"]),
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
        ]),
    ]);
    settings.extend([
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
    ]);
    // TODO: macOS 还没有全屏判断（见 `kwikpaste_os::mac::trigger_pause`），先只在 Windows 显示。
    #[cfg(target_os = "windows")]
    settings.push(
        Setting::new("shortcuts.pauseInFullscreen", Control::Switch)
            .path("shortcuts.pauseInFullscreen")
            .keywords(PAUSE_KEYWORDS),
    );
    settings.push(
        Setting::new("shortcuts.pauseApps", Control::AppExclusion)
            .path("shortcuts.pauseAppIds")
            .keywords(PAUSE_KEYWORDS),
    );
    settings
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
                    .keywords(&["material", "mica", "acrylic", "transparency"]),
                Setting::new(
                    "appearance.language",
                    Control::Select(Options::Values(LANGUAGES)),
                )
                .path("appearance.language")
                .keywords(&["language", "locale", "english"]),
            ],
        },
        Section {
            id: "cards",
            settings: vec![
                Setting::new(
                    "appearance.listStyle",
                    Control::Select(Options::Values(LIST_STYLES)),
                )
                .path("clipboard.display.listStyle")
                .keywords(&["list", "card", "seamless", "divider", "style"]),
                Setting::new(
                    "appearance.listDensity",
                    Control::Select(Options::Values(LIST_DENSITIES)),
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
                    Control::Select(Options::Numbers(LIST_ITEM_GAPS)),
                )
                .path("clipboard.display.customLayout.itemGap")
                .keywords(&["density", "gap", "spacing", "margin"])
                .child_of("appearance.listDensity", |settings| {
                    not_custom_density(settings)
                        || settings.clipboard.display.list_style == ListStyle::Seamless
                }),
                Setting::new(
                    "appearance.itemPadding",
                    Control::Select(Options::Numbers(LIST_PADDINGS_Y)),
                )
                .path("clipboard.display.customLayout.paddingY")
                .keywords(&["density", "padding", "height"])
                .child_of("appearance.listDensity", not_custom_density),
                Setting::new(
                    "appearance.textMaxLines",
                    Control::Number {
                        min: 1,
                        max: 5,
                        suffix: Some("lines"),
                    },
                )
                .path("clipboard.display.textMaxLines")
                .keywords(&["density", "text", "line", "compact"]),
                Setting::new(
                    "appearance.imageMaxHeight",
                    Control::Number {
                        min: 20,
                        max: 100,
                        suffix: Some("px"),
                    },
                )
                .path("clipboard.display.imageMaxHeight")
                .keywords(&["density", "image", "height", "thumbnail"]),
                Setting::new(
                    "appearance.fileMaxCount",
                    Control::Number {
                        min: 1,
                        max: 5,
                        suffix: Some("files"),
                    },
                )
                .path("clipboard.display.fileMaxCount")
                .keywords(&["density", "file", "count", "array"]),
                Setting::new("paste.quickSnippets", Control::Switch)
                    .path("clipboard.display.quickSnippets")
                    .keywords(&["quick", "snippet", "extract", "number", "code"]),
                Setting::new("appearance.showOriginalPreview", Control::Switch)
                    .path("clipboard.content.showOriginalPreview")
                    .keywords(&["note", "hover", "original", "preview"]),
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
                    .keywords(&["priority", "order", "format", "rich text"]),
            ],
        },
        Section {
            id: "captureRules",
            settings: vec![
                Setting::new(
                    "capture.maxTextMb",
                    Control::Number {
                        min: 0,
                        max: u64::from(u32::MAX),
                        suffix: Some("mb"),
                    },
                )
                .path("clipboard.capture.maxTextMb")
                .keywords(&["text", "size", "limit", "mb"]),
                Setting::new(
                    "capture.maxImageMb",
                    Control::Number {
                        min: 0,
                        max: u64::from(u32::MAX),
                        suffix: Some("mb"),
                    },
                )
                .path("clipboard.capture.maxImageMb")
                .keywords(&["image", "picture", "size", "limit", "mb"]),
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
                ]),
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
    ]
}

fn window_sections() -> Vec<Section> {
    vec![
        Section {
            id: "window",
            settings: vec![
                Setting::new(
                    "window.position",
                    Control::Select(Options::Values(WINDOW_POSITIONS)),
                )
                .path("clipboard.window.position")
                .keywords(&["window", "position", "cursor"]),
                Setting::new("window.scrollToTopOnOpen", Control::Switch)
                    .path("clipboard.window.scrollToTopOnOpen")
                    .keywords(&["window", "scroll", "top", "open"]),
                Setting::new(
                    "window.selectRangeOnOpen",
                    Control::Select(Options::Values(OPEN_RANGES)),
                )
                .path("clipboard.window.selectRangeOnOpen")
                .keywords(&["window", "range", "all", "favorite", "open"]),
                Setting::new(
                    "window.selectCategoryOnOpen",
                    Control::Select(Options::Values(OPEN_CATEGORIES)),
                )
                .path("clipboard.window.selectCategoryOnOpen")
                .keywords(&["window", "category", "kind", "all", "open"]),
                Setting::new("window.selectGroupOnOpen", Control::GroupSelect)
                    .path("clipboard.window.selectGroupOnOpen")
                    .keywords(&["window", "group", "folder", "all", "open"]),
                Setting::new("search.defaultFocus", Control::Switch)
                    .path("clipboard.search.defaultFocus")
                    .keywords(&["search", "focus", "open"]),
                Setting::new("search.clearOnHide", Control::Switch)
                    .path("clipboard.search.clearOnHide")
                    .keywords(&["search", "clear", "hide"]),
            ],
        },
        Section {
            id: "sort",
            settings: vec![
                Setting::new("search.sort", Control::Select(Options::Values(SORTS)))
                    .path("clipboard.content.sort")
                    .keywords(&["sort", "frequency", "usage", "created", "updated"]),
                Setting::new("copy.updateOnReuse", Control::Switch)
                    .path("clipboard.content.updateOnReuse")
                    .keywords(&["copy", "paste", "reuse", "sort", "frequency"]),
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
                    Control::Select(Options::Values(HOVER_DELAYS)),
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
                    Control::Select(Options::Values(TEXT_VIEWS)),
                )
                .path("clipboard.preview.textView")
                .keywords(&["preview", "text", "words", "split"]),
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
                    .keywords(&["file", "path", "paste"]),
                Setting::new("copy.plainDefault", Control::Switch)
                    .path("clipboard.content.copyPlain")
                    .keywords(&["copy", "plain", "format"]),
            ],
        },
        Section {
            id: "sound",
            settings: vec![
                Setting::new("copy.sound", Control::Switch)
                    .path("clipboard.feedback.copySound")
                    .keywords(&["sound", "feedback", "copy", "音效", "提示音", "声音"]),
                Setting::new("copy.sound.volume", Control::Slider { min: 0, max: 100 })
                    .path("clipboard.feedback.copySoundVolume")
                    .keywords(&["音量", "音效", "试听", "volume", "sound", "preview"])
                    .child_of("copy.sound", |settings| {
                        !settings.clipboard.feedback.copy_sound
                    }),
                Setting::new("copy.sound.preview", Control::Action { danger: false })
                    .keywords(&["音量", "音效", "试听", "volume", "sound", "preview"])
                    .child_of("copy.sound", |settings| {
                        !settings.clipboard.feedback.copy_sound
                    }),
            ],
        },
    ]
}

fn item_sections() -> Vec<Section> {
    vec![
        Section {
            id: "actions",
            settings: vec![
                Setting::new("actions.visible", Control::ActionVisibility)
                    .path("clipboard.content.itemActions")
                    .keywords(&["action", "hover", "buttons"]),
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
                    .keywords(&["delete", "favorite", "allow"]),
                Setting::new("actions.deleteFavoriteConfirm", Control::Switch)
                    .path("clipboard.content.deleteFavoriteConfirm")
                    .keywords(&["delete", "favorite", "confirm"])
                    .child_of("actions.deleteFavoriteItems", |settings| {
                        !settings.clipboard.content.delete_favorite_items
                    }),
                Setting::new(
                    "actions.deleteFavoriteItemsOnlyInFavoriteGroup",
                    Control::Switch,
                )
                .path("clipboard.content.deleteFavoriteItemsOnlyInFavoriteGroup")
                .keywords(&["delete", "favorite", "group"])
                .child_of("actions.deleteFavoriteItems", |settings| {
                    !settings.clipboard.content.delete_favorite_items
                }),
                Setting::new("actions.deletePinnedItems", Control::Switch)
                    .path("clipboard.content.deletePinnedItems")
                    .keywords(&["delete", "pinned", "pin", "allow"]),
                Setting::new("actions.deletePinnedConfirm", Control::Switch)
                    .path("clipboard.content.deletePinnedConfirm")
                    .keywords(&["delete", "pinned", "pin", "confirm"])
                    .child_of("actions.deletePinnedItems", |settings| {
                        !settings.clipboard.content.delete_pinned_items
                    }),
            ],
        },
        Section {
            id: "organizing",
            settings: vec![
                Setting::new("organizing.customGroups", Control::Action { danger: false })
                    .keywords(&["group", "folder", "organize"]),
                Setting::new("organizing.autoFavorite", Control::Switch)
                    .path("clipboard.content.autoFavorite")
                    .keywords(&["note", "favorite", "auto"]),
            ],
        },
    ]
}

fn data_sections() -> Vec<Section> {
    vec![
        Section {
            id: "cleanup",
            settings: vec![
                Setting::new("history.retention", Control::Retention)
                    .path("clipboard.history.retention")
                    .keywords(&["retention", "cleanup", "history", "expire"]),
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
                    ]),
                Setting::new(
                    "history.maxCount",
                    Control::Number {
                        min: 0,
                        max: u64::from(u32::MAX),
                        suffix: Some("items"),
                    },
                )
                .path("clipboard.history.maxCount")
                .keywords(&["max", "count", "limit"]),
                Setting::new(
                    "localData.storageLimit",
                    Control::Number {
                        min: STORAGE_LIMIT_MIN_MB,
                        max: STORAGE_LIMIT_MAX_MB,
                        suffix: Some("mb"),
                    },
                )
                .path("clipboard.history.storageLimitMb")
                .keywords(&["storage", "limit", "size", "quota", "disk"]),
                Setting::new(
                    "localData.storageLimitAction",
                    Control::Select(Options::Values(STORAGE_LIMIT_ACTIONS)),
                )
                .path("clipboard.history.storageLimitAction")
                .keywords(&["storage", "limit", "cleanup", "remind"]),
                Setting::new("history.cleanupStatus", Control::CleanupStatus).keywords(&[
                    "cleanup",
                    "status",
                    "protected",
                    "now",
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
            ],
        },
        Section {
            id: "updates",
            settings: vec![
                Setting::new("about.checkUpdates", Control::Action { danger: false })
                    .keywords(&["update", "version"]),
                Setting::new("updates.autoCheck", Control::Switch)
                    .path("update.autoCheck")
                    .keywords(&["update", "version"]),
                Setting::new(
                    "updates.frequency",
                    Control::Select(Options::Values(UPDATE_FREQUENCIES)),
                )
                .path("update.frequency")
                .keywords(&["update", "frequency", "schedule"])
                .child_of("updates.autoCheck", |settings| !settings.update.auto_check),
            ],
        },
        Section {
            id: "diagnostics",
            settings: vec![
                Setting::new("localData.logDirectory", Control::Action { danger: false })
                    .keywords(&["log", "diagnostic"]),
                Setting::new(
                    "diagnostics.resetPreferences",
                    Control::Action { danger: true },
                )
                .keywords(&["reset", "preferences"]),
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
            ["copy.sound", "copy.sound.volume", "copy.sound.preview"]
        );
        assert_eq!(
            sound.settings[1].path,
            Some("clipboard.feedback.copySoundVolume")
        );
        assert!(matches!(
            sound.settings[1].control,
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
                sound.settings[1].keywords
            ));
        }
    }
    #[test]
    fn capture_retains_image_text_status_but_not_the_removed_ocr_switch() {
        let capture = capture_sections();
        let rows: Vec<_> = capture
            .iter()
            .flat_map(|section| &section.settings)
            .collect();
        assert!(rows.iter().all(|setting| setting.id != "ocr.enabled"));
        assert!(rows.iter().any(|setting| setting.id == "ocr.status"
            && matches!(setting.control, Control::ImageTextStatus)));
    }
}
