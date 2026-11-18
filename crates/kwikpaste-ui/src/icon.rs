//! 图标。
//!
//! 组件常用的 lucide 图标借用 gpui-kit-assets 的默认图标集；默认集里没有的，从 1.x 用的 iconify
//! 包导出到 `icons/`（见 `icons/export-icons.mjs`），由 [`crate::Assets`] 一起提供。自定义分组的
//! 图标（预设或用户导入的 SVG）不在这里，见 [`crate::group_icon_path`]。

use gpui::{
    App, Hsla, IntoElement, Radians, Rems, RenderOnce, Styled, Window, prelude::FluentBuilder as _,
};

use crate::assets::PREFIX;

/// 应用可用的图标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconName {
    /// `lucide:app-window`：搜索框筛选的来源应用。
    AppWindow,
    Check,
    ChevronDown,
    /// `lucide:chevron-left` / `chevron-right`：大图预览换图。
    ChevronLeft,
    ChevronRight,
    /// 1.x `i-lucide:circle-check`：复制成功。
    CircleCheck,
    /// 1.x `i-lucide:clipboard-paste`：粘贴。
    ClipboardPaste,
    /// 1.x `i-lucide:clipboard-type`：纯文本粘贴。
    ClipboardType,
    /// `lucide:clock`：搜索框筛选的时间。
    Clock,
    Close,
    Copy,
    /// 1.x `i-lucide:copy-check`：复制为纯文本。
    CopyCheck,
    Delete,
    /// `lucide:download`：大图预览另存为。
    Download,
    Ellipsis,
    ExternalLink,
    Eye,
    /// 1.x `i-lucide:eye-off`：隐藏分组。
    EyeOff,
    /// 1.x `i-lucide:file-symlink`：粘贴为路径。
    FileSymlink,
    /// `lucide:filter-x`：清除搜索框筛选。
    FilterX,
    Folder,
    /// 1.x `i-lucide:folder-open`：在文件管理器中显示。
    FolderOpen,
    Globe,
    /// 1.x 管理分组里可拖动行的把手（antd Tree `draggable` 的 holder）。
    GripVertical,
    /// 1.x `i-lets-icons:widget`：分组栏“全部”。
    GroupAll,
    /// 1.x `i-lets-icons:star`：分组栏“收藏”。
    GroupFavorite,
    /// 1.x `i-lets-icons:folder-file-alt`：分组栏“文件”。
    GroupFiles,
    /// 1.x `i-lets-icons:img-box`：分组栏“图片”。
    GroupImage,
    /// 1.x `i-lets-icons:file-dock`：分组栏“文本”。
    GroupText,
    Heart,
    /// 1.x `i-lucide:image-off`：缩略图解码失败。
    ImageOff,
    Inbox,
    Info,
    /// 1.x `i-lucide:key-round`：敏感内容标记。
    KeyRound,
    /// 1.x `i-lucide:keyboard`：快捷键列表。
    Keyboard,
    /// `lucide:languages`：译文、翻译按钮。
    Languages,
    /// 1.x `i-lucide:laptop`：来自 macOS 设备的同步记录。
    Laptop,
    /// `lucide:link`：链接类型。
    Link,
    /// 1.x `i-lucide:list-checks`：多选。
    ListChecks,
    /// `lucide:list-filter`：搜索框筛选入口。
    ListFilter,
    /// 1.x `i-lucide:mail`：发送邮件。
    Mail,
    /// 1.x `i-lucide:monitor`：来自 Windows 设备的同步记录。
    Monitor,
    /// `lucide:smartphone`：来自手机微信输入法的同步记录。
    Smartphone,
    Moon,
    /// 1.x `i-lucide:notebook-pen`：便签。
    NotebookPen,
    Palette,
    /// 1.x `i-lucide:pencil`：编辑分组。
    Pencil,
    /// 1.x `i-lets-icons:pin`：固定窗口。
    PinWindow,
    /// 1.x `i-ph:push-pin-bold`：置顶标记。
    PushPin,
    Plus,
    /// `lucide:rotate-cw`：大图预览向右旋转。
    RotateCw,
    /// `lucide:scan-qr-code`：大图预览识别二维码。
    ScanQrCode,
    /// `i-lucide:scan-text`：图片里识别出的文字。
    ScanText,
    Search,
    Settings,
    /// 1.x `i-lucide:settings-2`：管理分组。
    Settings2,
    /// 1.x `i-lets-icons:setting-line`：偏好设置。
    SettingLine,
    /// `lucide:shapes`：搜索框筛选的类型。
    Shapes,
    /// `lucide:shrink`：大图预览适应窗口。
    Shrink,
    /// 1.x `i-lucide:square-arrow-out-up-right`：打开链接。
    SquareArrowOutUpRight,
    Star,
    Sun,
    /// 1.x `i-lucide:text-select`：拆词。
    TextSelect,
    TextSize,
    /// `i-lucide:triangle-alert`：需要用户处理的提示。
    TriangleAlert,
    /// 1.x `i-lucide:trash`：删除记录。
    Trash,
    /// 1.x `i-lucide:trash-2`：删除分组。
    Trash2,
    /// `lucide:type`：富文本类型。
    Type,
    /// 自绘标题栏的窗口按钮（Fluent 字形）。
    WindowClose,
    WindowMaximize,
    WindowMinimize,
    WindowRestore,
    /// `lucide:zoom-in` / `zoom-out`：大图预览缩放。
    ZoomIn,
    ZoomOut,
}

impl IconName {
    /// 从 1.x iconify 包导出的图标的资源文件名；其余用 gpui-kit-assets 的默认集。
    fn exported(self) -> Option<&'static str> {
        Some(match self {
            Self::AppWindow => "lucide-app-window.svg",
            Self::Clock => "lucide-clock.svg",
            Self::ChevronLeft => "lucide-chevron-left.svg",
            Self::ChevronRight => "lucide-chevron-right.svg",
            Self::Download => "lucide-download.svg",
            Self::RotateCw => "lucide-rotate-cw.svg",
            Self::ScanQrCode => "lucide-scan-qr-code.svg",
            Self::Shrink => "lucide-shrink.svg",
            Self::ZoomIn => "lucide-zoom-in.svg",
            Self::ZoomOut => "lucide-zoom-out.svg",
            Self::FilterX => "lucide-filter-x.svg",
            Self::Link => "lucide-link.svg",
            Self::ListFilter => "lucide-list-filter.svg",
            Self::Shapes => "lucide-shapes.svg",
            Self::Type => "lucide-type.svg",
            Self::CircleCheck => "lucide-circle-check.svg",
            Self::ClipboardPaste => "lucide-clipboard-paste.svg",
            Self::ClipboardType => "lucide-clipboard-type.svg",
            Self::CopyCheck => "lucide-copy-check.svg",
            Self::EyeOff => "lucide-eye-off.svg",
            Self::FileSymlink => "lucide-file-symlink.svg",
            Self::FolderOpen => "lucide-folder-open.svg",
            Self::GripVertical => "lucide-grip-vertical.svg",
            Self::GroupAll => "lets-icons-widget.svg",
            Self::GroupFavorite => "lets-icons-star.svg",
            Self::GroupFiles => "lets-icons-folder-file-alt.svg",
            Self::GroupImage => "lets-icons-img-box.svg",
            Self::GroupText => "lets-icons-file-dock.svg",
            Self::ImageOff => "lucide-image-off.svg",
            Self::KeyRound => "lucide-key-round.svg",
            Self::Keyboard => "lucide-keyboard.svg",
            Self::Languages => "lucide-languages.svg",
            Self::Laptop => "lucide-laptop.svg",
            Self::ListChecks => "lucide-list-checks.svg",
            Self::Mail => "lucide-mail.svg",
            Self::Monitor => "lucide-monitor.svg",
            Self::Smartphone => "lucide-smartphone.svg",
            Self::NotebookPen => "lucide-notebook-pen.svg",
            Self::Pencil => "lucide-pencil.svg",
            Self::PinWindow => "lets-icons-pin.svg",
            Self::PushPin => "ph-push-pin-bold.svg",
            Self::ScanText => "lucide-scan-text.svg",
            Self::Settings2 => "lucide-settings-2.svg",
            Self::SettingLine => "lets-icons-setting-line.svg",
            Self::SquareArrowOutUpRight => "lucide-square-arrow-out-up-right.svg",
            Self::TextSelect => "lucide-text-select.svg",
            Self::TriangleAlert => "lucide-triangle-alert.svg",
            Self::Trash => "lucide-trash.svg",
            Self::Trash2 => "lucide-trash-2.svg",
            _ => return None,
        })
    }

    /// gpui-kit-assets 默认集里的图标；导出的图标返回 `None`。
    fn kit(self) -> Option<gpui_component::IconName> {
        use gpui_component::IconName as Kit;

        Some(match self {
            Self::Check => Kit::Check,
            Self::ChevronDown => Kit::ChevronDown,
            Self::Close => Kit::Close,
            Self::Copy => Kit::Copy,
            Self::Delete => Kit::Delete,
            Self::Ellipsis => Kit::Ellipsis,
            Self::ExternalLink => Kit::ExternalLink,
            Self::Eye => Kit::Eye,
            Self::Folder => Kit::Folder,
            Self::Globe => Kit::Globe,
            Self::Heart => Kit::Heart,
            Self::Inbox => Kit::Inbox,
            Self::Info => Kit::Info,
            Self::Moon => Kit::Moon,
            Self::Palette => Kit::Palette,
            Self::Plus => Kit::Plus,
            Self::Search => Kit::Search,
            Self::Settings => Kit::Settings,
            Self::Star => Kit::Star,
            Self::Sun => Kit::Sun,
            Self::TextSize => Kit::ALargeSmall,
            Self::WindowClose => Kit::WindowClose,
            Self::WindowMaximize => Kit::WindowMaximize,
            Self::WindowMinimize => Kit::WindowMinimize,
            Self::WindowRestore => Kit::WindowRestore,
            _ => return None,
        })
    }

    pub(crate) fn kit_icon(self) -> gpui_component::Icon {
        match (self.exported(), self.kit()) {
            (Some(file), _) => gpui_component::Icon::empty().path(format!("{PREFIX}{file}")),
            (None, Some(kit)) => gpui_component::Icon::new(kit),
            (None, None) => gpui_component::Icon::new(gpui_component::IconName::Info),
        }
    }
}

/// 单色图标，颜色默认继承文字色。
#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: Option<Rems>,
    color: Option<Hsla>,
    rotation: Option<Radians>,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self {
            name,
            size: None,
            color: None,
            rotation: None,
        }
    }

    pub fn size(mut self, size: Rems) -> Self {
        self.size = Some(size);
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// 旋转（1.x 置顶按钮的 `-rotate-45`）。
    pub fn rotate(mut self, radians: Radians) -> Self {
        self.rotation = Some(radians);
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.name
            .kit_icon()
            .when_some(self.size, |icon, size| icon.size(size))
            .when_some(self.color, |icon, color| icon.text_color(color))
            .when_some(self.rotation, |icon, radians| icon.rotate(radians))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个图标要么是导出的、要么在默认集里，导出的文件都内嵌了。
    #[test]
    fn every_icon_has_a_source() {
        use gpui::AssetSource as _;

        let all = [
            IconName::Check,
            IconName::CircleCheck,
            IconName::ClipboardPaste,
            IconName::ClipboardType,
            IconName::CopyCheck,
            IconName::EyeOff,
            IconName::FileSymlink,
            IconName::FolderOpen,
            IconName::GripVertical,
            IconName::GroupAll,
            IconName::GroupFavorite,
            IconName::GroupFiles,
            IconName::GroupImage,
            IconName::GroupText,
            IconName::Keyboard,
            IconName::Languages,
            IconName::ListChecks,
            IconName::Mail,
            IconName::Pencil,
            IconName::PinWindow,
            IconName::Settings2,
            IconName::SettingLine,
            IconName::Smartphone,
            IconName::SquareArrowOutUpRight,
            IconName::TextSelect,
            IconName::Trash,
            IconName::Trash2,
            IconName::PushPin,
        ];
        for icon in all {
            match (icon.exported(), icon.kit()) {
                (Some(file), None) => {
                    let path = format!("{PREFIX}{file}");
                    assert!(
                        crate::Assets.load(&path).ok().flatten().is_some(),
                        "{path} is embedded"
                    );
                }
                (None, Some(_)) => {}
                (exported, kit) => panic!(
                    "{icon:?} must have exactly one source (exported {exported:?}, kit {})",
                    kit.is_some()
                ),
            }
        }
    }
}
