//! 偏好窗用到的 lucide 图标：从 1.x 的 iconify 包导出到 `icons/`（见 `icons/export-icons.mjs`），
//! 内嵌进二进制，第一次用到时经 `kwikpaste_ui::register_svg` 登记成资源路径。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use gpui::{Hsla, IntoElement, Rems, SharedString, Styled as _, svg};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrefIcon {
    Settings,
    Keyboard,
    Palette,
    ClipboardPlus,
    PanelTop,
    ClipboardPaste,
    Layers,
    ChartPie,
    Database,
    Info,
    HardDrive,
    Search,
    Play,
    CheckCircle,
    Loader,
    FolderOpen,
    FolderSync,
    Puzzle,
    RotateCcw,
    Refresh,
    Grip,
    ArrowUp,
    ArrowDown,
    Pencil,
    Trash,
    Plus,
    CircleX,
    Alert,
    Sparkles,
    EyeOff,
    ChevronDown,
    Files,
    FileCode,
    FileImage,
    FileType,
    ClipboardType,
    ChartColumn,
    Shapes,
    AppWindow,
    CircleHelp,
    FolderTree,
    CalendarPlus,
    Repeat,
    History,
    Link,
    Mail,
    Star,
    Pin,
    NotebookPen,
    ShieldAlert,
    Inbox,
    PanelLeftClose,
    PanelLeftOpen,
    Mouse,
    MousePointer,
    ChevronRight,
    ArrowLeft,
    ScanText,
    Smartphone,
    Languages,
}

impl PrefIcon {
    fn markup(self) -> &'static str {
        match self {
            Self::Settings => include_str!("icons/lucide-settings.svg"),
            Self::Keyboard => include_str!("icons/lucide-keyboard.svg"),
            Self::Palette => include_str!("icons/lucide-palette.svg"),
            Self::ClipboardPlus => include_str!("icons/lucide-clipboard-plus.svg"),
            Self::PanelTop => include_str!("icons/lucide-panel-top.svg"),
            Self::ClipboardPaste => include_str!("icons/lucide-clipboard-paste.svg"),
            Self::Layers => include_str!("icons/lucide-layers.svg"),
            Self::ChartPie => include_str!("icons/lucide-chart-pie.svg"),
            Self::Database => include_str!("icons/lucide-database.svg"),
            Self::Info => include_str!("icons/lucide-info.svg"),
            Self::HardDrive => include_str!("icons/lucide-hard-drive.svg"),
            Self::Search => include_str!("icons/lucide-search.svg"),
            Self::Play => include_str!("icons/lucide-play.svg"),
            Self::CheckCircle => include_str!("icons/lucide-check-circle-2.svg"),
            Self::Loader => include_str!("icons/lucide-loader-circle.svg"),
            Self::FolderOpen => include_str!("icons/lucide-folder-open.svg"),
            Self::FolderSync => include_str!("icons/lucide-folder-sync.svg"),
            Self::Puzzle => include_str!("icons/lucide-puzzle.svg"),
            Self::RotateCcw => include_str!("icons/lucide-rotate-ccw.svg"),
            Self::Refresh => include_str!("icons/lucide-refresh-cw.svg"),
            Self::Grip => include_str!("icons/lucide-grip-vertical.svg"),
            Self::ArrowUp => include_str!("icons/lucide-arrow-up.svg"),
            Self::ArrowDown => include_str!("icons/lucide-arrow-down.svg"),
            Self::Pencil => include_str!("icons/lucide-pencil.svg"),
            Self::Trash => include_str!("icons/lucide-trash-2.svg"),
            Self::Plus => include_str!("icons/lucide-plus.svg"),
            Self::CircleX => include_str!("icons/lucide-circle-x.svg"),
            Self::Alert => include_str!("icons/lucide-triangle-alert.svg"),
            Self::Sparkles => include_str!("icons/lucide-sparkles.svg"),
            Self::EyeOff => include_str!("icons/lucide-eye-off.svg"),
            Self::ChevronDown => include_str!("icons/lucide-chevron-down.svg"),
            Self::Files => include_str!("icons/lucide-files.svg"),
            Self::FileCode => include_str!("icons/lucide-file-code-2.svg"),
            Self::FileImage => include_str!("icons/lucide-file-image.svg"),
            Self::FileType => include_str!("icons/lucide-file-type.svg"),
            Self::ClipboardType => include_str!("icons/lucide-clipboard-type.svg"),
            Self::ChartColumn => include_str!("icons/lucide-chart-column.svg"),
            Self::Shapes => include_str!("icons/lucide-shapes.svg"),
            Self::AppWindow => include_str!("icons/lucide-app-window.svg"),
            Self::CircleHelp => include_str!("icons/lucide-circle-help.svg"),
            Self::FolderTree => include_str!("icons/lucide-folder-tree.svg"),
            Self::CalendarPlus => include_str!("icons/lucide-calendar-plus.svg"),
            Self::Repeat => include_str!("icons/lucide-repeat.svg"),
            Self::History => include_str!("icons/lucide-history.svg"),
            Self::Link => include_str!("icons/lucide-link.svg"),
            Self::Mail => include_str!("icons/lucide-mail.svg"),
            Self::Star => include_str!("icons/lucide-star.svg"),
            Self::Pin => include_str!("icons/lucide-pin.svg"),
            Self::NotebookPen => include_str!("icons/lucide-notebook-pen.svg"),
            Self::ShieldAlert => include_str!("icons/lucide-shield-alert.svg"),
            Self::Inbox => include_str!("icons/lucide-inbox.svg"),
            Self::PanelLeftClose => include_str!("icons/lucide-panel-left-close.svg"),
            Self::PanelLeftOpen => include_str!("icons/lucide-panel-left-open.svg"),
            Self::Mouse => include_str!("icons/lucide-mouse.svg"),
            Self::MousePointer => include_str!("icons/lucide-mouse-pointer-2.svg"),
            Self::ChevronRight => include_str!("icons/lucide-chevron-right.svg"),
            Self::ArrowLeft => include_str!("icons/lucide-arrow-left.svg"),
            Self::ScanText => include_str!("icons/lucide-scan-text.svg"),
            Self::Smartphone => include_str!("icons/lucide-smartphone.svg"),
            Self::Languages => include_str!("icons/lucide-languages.svg"),
        }
    }

    /// 资源路径；第一次用到时登记。
    pub fn path(self) -> SharedString {
        static PATHS: LazyLock<Mutex<HashMap<PrefIcon, SharedString>>> =
            LazyLock::new(Mutex::default);
        let mut paths = PATHS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        paths
            .entry(self)
            .or_insert_with(|| kwikpaste_ui::register_svg(self.markup()))
            .clone()
    }

    /// 图标元素：`size` 见方，单色。
    pub fn view(self, size: Rems, color: Hsla) -> impl IntoElement {
        svg()
            .path(self.path())
            .size(size)
            .flex_none()
            .text_color(color)
    }
}
