//! 反复出现的控件状态 token，由语义 token 生成；视图与控件只读这些角色，不再自己拼色阶或透明度。

use gpui::Hsla;

/// 图标按钮在常态、悬停、按下和选中时的颜色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconButtonTokens {
    pub foreground: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub selected: Hsla,
    pub selected_hover: Hsla,
    pub chip_foreground: Hsla,
    pub chip_background: Hsla,
    pub chip_hover: Hsla,
}

/// 空状态快捷操作的禁用透明度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuickActionTokens {
    pub disabled_opacity: f32,
}

/// 不存在的预览文件行的透明度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewFileTokens {
    pub missing_opacity: f32,
}

/// 面板内排序时源卡片与跟随指针的幽灵卡片透明度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReorderTokens {
    pub source_opacity: f32,
    pub ghost_opacity: f32,
}

/// 引导完成状态的图标背景。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnboardingTokens {
    pub done_background: Hsla,
}

/// 偏好设置导航的常态、悬停和选中状态，禁用的分段控件，以及材质磁贴示意图的着色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreferencesTokens {
    pub nav_selected: Hsla,
    pub nav_hover: Hsla,
    pub choice_disabled_opacity: f32,
    pub material_wallpaper: Hsla,
    pub material_blob: Hsla,
    pub material_mica_backdrop: Hsla,
    pub material_mica_panel: Hsla,
    pub material_frosted_panel: Hsla,
}

/// 输入和选择器的表面、边框、聚焦边框及占位符颜色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputTokens {
    pub background: Hsla,
    pub border: Hsla,
    pub border_focus: Hsla,
    pub placeholder: Hsla,
}

/// 分组胶囊的常态、悬停、选中和文字颜色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupChipTokens {
    pub background: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    pub foreground: Hsla,
    pub selected_foreground: Hsla,
}

/// 卡片或列表行的常态、悬停、激活、勾选和边框颜色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardRowTokens {
    pub background: Hsla,
    pub hover: Hsla,
    pub active: Hsla,
    pub checked: Hsla,
    pub border: Hsla,
}

/// 标签与快捷键提示的底色和文字色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagTokens {
    pub background: Hsla,
    pub foreground: Hsla,
    pub key_background: Hsla,
    pub key_foreground: Hsla,
    pub separator: Hsla,
    pub variants: [TagStateTokens; 5],
}

/// 标签的表面、文字和边框。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TagStateTokens {
    pub background: Hsla,
    pub foreground: Hsla,
    pub border: Hsla,
}

/// 卡片类型标签：识别出的子类型画成淡色底的小胶囊。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KindTag {
    pub background: Hsla,
    pub foreground: Hsla,
}

/// 链接、邮箱、颜色、路径四种类型标签。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KindTagTokens {
    pub link: KindTag,
    pub email: KindTag,
    pub color: KindTag,
    pub path: KindTag,
}

/// 快捷信息按类型的颜色：卡片和预览里的圆点、预览正文里的淡色底。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnippetTokens {
    pub custom: KindTag,
    pub number: KindTag,
    pub phone: KindTag,
    pub email: KindTag,
    pub time: KindTag,
    pub link: KindTag,
}

/// 菜单项的常态、悬停、按下、危险和禁用文字色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuItemTokens {
    pub foreground: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub danger: Hsla,
    pub disabled: Hsla,
}

/// Tooltip 的表面、文字和阴影。
#[derive(Clone, Debug, PartialEq)]
pub struct TooltipTokens {
    pub background: Hsla,
    pub foreground: Hsla,
    pub shadow: [gpui::BoxShadow; 3],
}

/// Toast 的表面、文字、状态图标和阴影。
#[derive(Clone, Debug, PartialEq)]
pub struct ToastTokens {
    pub background: Hsla,
    pub foreground: Hsla,
    pub info: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub shadow: [gpui::BoxShadow; 3],
}

/// Dialog 的表面、正文、遮罩和阴影。
#[derive(Clone, Debug, PartialEq)]
pub struct DialogTokens {
    pub background: Hsla,
    pub foreground: Hsla,
    pub mask: Hsla,
    pub shadow: [gpui::BoxShadow; 3],
}

/// Switch 的轨道、手柄、选中和禁用状态。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwitchTokens {
    pub track: Hsla,
    pub thumb: Hsla,
    pub checked: Hsla,
    pub disabled: Hsla,
}

/// Scrollbar 的滑块常态和悬停色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarTokens {
    pub thumb: Hsla,
    pub thumb_hover: Hsla,
}

/// 自绘标题栏的窗口按钮：普通按钮悬停、按下是中性填充，关闭按钮悬停变红（Windows 惯例）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptionButtonTokens {
    pub foreground: Hsla,
    pub inactive_foreground: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub close_hover: Hsla,
    pub close_pressed: Hsla,
    pub close_foreground: Hsla,
}

/// 大图预览文字层：标出的文字区域和选中的文字，叠在图片上所以是半透明的。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageTextTokens {
    pub region: Hsla,
    pub selection: Hsla,
}

/// 淡色底跟着文字色走，材质窗口上也不会变成一块实色。
/// 材质示意图的壁纸：暗色的色阶带透明度，先压到窗口底上，壁纸才是不透明的。
fn material_wallpaper(s: &super::semantic::SemanticTokens) -> Hsla {
    s.surface.window.blend(s.hues.blue_6.opacity(0.35))
}

fn kind_tag(foreground: Hsla) -> KindTag {
    KindTag {
        background: foreground.opacity(0.12),
        foreground,
    }
}

/// 由角色语义 token 生成组件状态，避免视图临时拼接透明度。
pub fn icon_button(tokens: &super::semantic::SemanticTokens) -> IconButtonTokens {
    IconButtonTokens {
        foreground: tokens.text.secondary,
        hover: tokens.fill.default,
        pressed: tokens.fill.strong,
        selected: tokens.accent.solid.opacity(0.13),
        selected_hover: tokens.accent.solid.opacity(0.2),
        chip_foreground: tokens.text.primary,
        chip_background: tokens.fill.default,
        chip_hover: tokens.fill.strong,
    }
}

/// 由角色语义 token 生成输入状态。
pub fn input(tokens: &super::semantic::SemanticTokens) -> InputTokens {
    InputTokens {
        background: tokens.fill.subtle,
        border: tokens.border.default,
        border_focus: tokens.accent.solid,
        placeholder: tokens.text.placeholder,
    }
}

/// 反复出现的组件状态集合，由语义层一次构建并驻留。
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentTokens {
    pub icon_button: IconButtonTokens,
    pub quick_action: QuickActionTokens,
    pub preview_file: PreviewFileTokens,
    pub reorder: ReorderTokens,
    pub onboarding: OnboardingTokens,
    pub preferences: PreferencesTokens,
    pub input: InputTokens,
    pub group_chip: GroupChipTokens,
    pub card_row: CardRowTokens,
    pub tag: TagTokens,
    pub kind_tag: KindTagTokens,
    pub snippet: SnippetTokens,
    pub menu_item: MenuItemTokens,
    pub tooltip: TooltipTokens,
    pub toast: ToastTokens,
    pub dialog: DialogTokens,
    pub switch: SwitchTokens,
    pub scrollbar: ScrollbarTokens,
    pub caption_button: CaptionButtonTokens,
    pub image_text: ImageTextTokens,
}

impl ComponentTokens {
    /// 从角色语义层生成全部组件状态，保留现有像素值。
    pub fn from_semantic(s: &super::semantic::SemanticTokens) -> Self {
        Self {
            icon_button: icon_button(s),
            quick_action: QuickActionTokens {
                disabled_opacity: 0.4,
            },
            preview_file: PreviewFileTokens {
                missing_opacity: 0.5,
            },
            reorder: ReorderTokens {
                source_opacity: 0.42,
                ghost_opacity: 0.96,
            },
            onboarding: OnboardingTokens {
                done_background: s.status.success.solid.opacity(0.12),
            },
            preferences: PreferencesTokens {
                nav_selected: s.fill.default,
                nav_hover: s.fill.subtle,
                choice_disabled_opacity: 0.5,
                material_wallpaper: material_wallpaper(s),
                material_blob: s.hues.orange_6,
                material_mica_backdrop: material_wallpaper(s).blend(s.hues.orange_6.opacity(0.2)),
                material_mica_panel: s.surface.panel.opacity(0.55),
                material_frosted_panel: s.surface.panel.opacity(0.45),
            },
            input: input(s),
            group_chip: GroupChipTokens {
                background: s.fill.faint,
                hover: s.fill.default,
                selected: s.accent.subtle,
                foreground: s.text.secondary,
                selected_foreground: s.accent.solid,
            },
            card_row: CardRowTokens {
                background: s.surface.panel,
                hover: s.item.text_hover,
                active: s.accent.subtle,
                checked: s.accent.subtle,
                border: s.border.subtle,
            },
            tag: TagTokens {
                background: s.fill.default,
                foreground: s.text.secondary,
                key_background: s.surface.spotlight,
                key_foreground: s.text.on_accent,
                separator: s.text.muted,
                variants: [
                    TagStateTokens {
                        background: s.fill.faint,
                        foreground: s.text.primary,
                        border: s.border.default,
                    },
                    TagStateTokens {
                        background: s.accent.subtle,
                        foreground: s.accent.solid,
                        border: s.accent.border,
                    },
                    TagStateTokens {
                        background: s.status.success.subtle,
                        foreground: s.status.success.text,
                        border: s.status.success.border,
                    },
                    TagStateTokens {
                        background: s.status.warning.subtle,
                        foreground: s.status.warning.text,
                        border: s.status.warning.border,
                    },
                    TagStateTokens {
                        background: s.status.danger.subtle,
                        foreground: s.status.danger.text,
                        border: s.status.danger.border,
                    },
                ],
            },
            kind_tag: KindTagTokens {
                link: kind_tag(s.kinds.link),
                email: kind_tag(s.kinds.email),
                color: kind_tag(s.kinds.color),
                path: kind_tag(s.kinds.path),
            },
            // 链接、邮箱与类型标签同色；手机号、时间借用路径的绿和颜色的橙，与官网一致。
            snippet: SnippetTokens {
                custom: kind_tag(s.accent.solid),
                number: kind_tag(s.hues.magenta_6),
                phone: kind_tag(s.kinds.path),
                email: kind_tag(s.kinds.email),
                time: kind_tag(s.kinds.color),
                link: kind_tag(s.kinds.link),
            },
            menu_item: MenuItemTokens {
                foreground: s.text.primary,
                hover: s.item.text_hover,
                pressed: s.fill.strong,
                danger: s.status.danger.solid,
                disabled: s.text.muted,
            },
            tooltip: TooltipTokens {
                background: s.surface.spotlight,
                foreground: s.text.on_accent,
                shadow: s.shadow.overlay.clone(),
            },
            toast: ToastTokens {
                background: s.surface.raised,
                foreground: s.text.primary,
                info: s.status.info.solid,
                success: s.status.success.solid,
                warning: s.status.warning.solid,
                danger: s.status.danger.solid,
                shadow: s.shadow.overlay.clone(),
            },
            dialog: DialogTokens {
                background: s.surface.raised,
                foreground: s.text.primary,
                mask: s.surface.mask,
                shadow: s.shadow.overlay.clone(),
            },
            switch: SwitchTokens {
                track: s.text.faint,
                thumb: s.white,
                checked: s.accent.solid,
                disabled: s.border_disabled,
            },
            scrollbar: ScrollbarTokens {
                thumb: s.text.faint,
                thumb_hover: s.text.muted,
            },
            caption_button: CaptionButtonTokens {
                foreground: s.text.primary,
                inactive_foreground: s.text.muted,
                hover: s.fill.default,
                pressed: s.fill.strong,
                close_hover: s.status.danger.solid,
                close_pressed: s.status.danger.active,
                close_foreground: s.white,
            },
            image_text: ImageTextTokens {
                region: s.accent.solid.opacity(0.16),
                selection: s.accent.solid.opacity(0.38),
            },
        }
    }
}
