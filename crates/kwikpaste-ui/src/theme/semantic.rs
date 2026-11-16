//! 角色化语义 token：视图描述用途，而不是选择某个原始色阶。

use super::palette::{self, Palette};
use gpui::{BoxShadow, Hsla, point, px};

/// 窗口、面板、抬高层、浮层、聚光层和遮罩表面。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceTokens {
    pub window: Hsla,
    pub panel: Hsla,
    pub raised: Hsla,
    pub overlay: Hsla,
    pub spotlight: Hsla,
    pub mask: Hsla,
}
/// 正文、次级、弱化、占位、禁用、强调和链接文字。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextTokens {
    pub primary: Hsla,
    pub secondary: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub placeholder: Hsla,
    pub disabled: Hsla,
    pub on_accent: Hsla,
    pub link: Hsla,
}
/// 默认、细微、分隔和强调边框。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderTokens {
    pub default: Hsla,
    pub subtle: Hsla,
    pub divider: Hsla,
    pub strong: Hsla,
}
/// 强、默认、细微、微弱、悬停和按下填充。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillTokens {
    pub strong: Hsla,
    pub default: Hsla,
    pub subtle: Hsla,
    pub faint: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
}
/// 主色的常态、悬停、按下、细微底、细微悬停、描边和文字。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccentTokens {
    pub solid: Hsla,
    pub hover: Hsla,
    pub active: Hsla,
    pub subtle: Hsla,
    pub subtle_hover: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub text_hover: Hsla,
    pub text_active: Hsla,
}
/// 一个状态的常态、悬停、按下、细微底、描边和当文字用的颜色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusTokens {
    pub solid: Hsla,
    pub hover: Hsla,
    pub active: Hsla,
    pub subtle: Hsla,
    pub border: Hsla,
    pub text: Hsla,
}

/// 单项的悬停、选中、选中悬停及文字按钮状态。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemTokens {
    pub hover: Hsla,
    pub selected: Hsla,
    pub selected_hover: Hsla,
    pub text_hover: Hsla,
    pub text_active: Hsla,
}
/// 组件库仍会用到的少量独立色相。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HueTokens {
    pub blue_1: Hsla,
    pub blue_6: Hsla,
    pub cyan_1: Hsla,
    pub cyan_6: Hsla,
    pub green_1: Hsla,
    pub green_6: Hsla,
    pub red_1: Hsla,
    pub red_6: Hsla,
    pub yellow_1: Hsla,
    pub yellow_6: Hsla,
    pub magenta_1: Hsla,
    pub magenta_6: Hsla,
    pub orange_6: Hsla,
    pub purple_6: Hsla,
    pub gold_3: Hsla,
}
/// 识别出的文本子类型（链接、邮箱、颜色、路径）的标识色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KindTokens {
    pub link: Hsla,
    pub email: Hsla,
    pub color: Hsla,
    pub path: Hsla,
}
/// 成功、警告、危险和信息语义。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusGroup {
    pub success: StatusTokens,
    pub warning: StatusTokens,
    pub danger: StatusTokens,
    pub info: StatusTokens,
}
/// 浮层的描边环与阴影、卡片阴影。
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowTokens {
    pub overlay_ring: Hsla,
    pub overlay: [BoxShadow; 3],
    pub card: [BoxShadow; 3],
}
impl ShadowTokens {
    /// 浮层的完整外沿：1 px 描边环加阴影，与菜单、下拉一致。
    pub fn float(&self) -> Vec<BoxShadow> {
        let ring = BoxShadow {
            color: self.overlay_ring,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: false,
        };

        std::iter::once(ring)
            .chain(self.overlay.iter().cloned())
            .collect()
    }
}

/// 从角色色板直接生成的视图语义层。
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticTokens {
    pub surface: SurfaceTokens,
    pub text: TextTokens,
    pub border: BorderTokens,
    pub fill: FillTokens,
    pub item: ItemTokens,
    pub accent: AccentTokens,
    pub status: StatusGroup,
    pub hues: HueTokens,
    pub kinds: KindTokens,
    pub shadow: ShadowTokens,
    pub materials: palette::Materials,
    pub radii: palette::Radii,
    pub border_disabled: Hsla,
    pub white: Hsla,
}
fn status(s: palette::StatusPalette) -> StatusTokens {
    StatusTokens {
        solid: s.solid.hsla(),
        hover: s.hover.hsla(),
        active: s.active.hsla(),
        subtle: s.subtle.hsla(),
        border: s.border.hsla(),
        text: s.text.hsla(),
    }
}
impl SemanticTokens {
    /// 从角色色板建立语义层。
    pub fn from_palette(p: &Palette) -> Self {
        Self {
            surface: SurfaceTokens {
                window: p.surfaces.window.hsla(),
                panel: p.surfaces.panel.hsla(),
                raised: p.surfaces.raised.hsla(),
                overlay: p.surfaces.overlay.hsla(),
                spotlight: p.surfaces.spotlight.hsla(),
                mask: p.surfaces.mask.hsla(),
            },
            text: TextTokens {
                primary: p.text.primary.hsla(),
                secondary: p.text.secondary.hsla(),
                muted: p.text.muted.hsla(),
                faint: p.text.faint.hsla(),
                placeholder: p.text.placeholder.hsla(),
                disabled: p.text.disabled.hsla(),
                on_accent: p.text.on_accent.hsla(),
                link: p.accent.text.hsla(),
            },
            border: BorderTokens {
                default: p.borders.default.hsla(),
                subtle: p.borders.subtle.hsla(),
                divider: p.borders.divider.hsla(),
                strong: p.accent.border.hsla(),
            },
            fill: FillTokens {
                strong: p.fills.strong.hsla(),
                default: p.fills.default.hsla(),
                subtle: p.fills.subtle.hsla(),
                faint: p.fills.faint.hsla(),
                hover: p.items.text_hover.hsla(),
                pressed: p.items.text_active.hsla(),
            },
            item: ItemTokens {
                hover: p.items.hover.hsla(),
                selected: p.items.selected.hsla(),
                selected_hover: p.items.selected_hover.hsla(),
                text_hover: p.items.text_hover.hsla(),
                text_active: p.items.text_active.hsla(),
            },
            accent: AccentTokens {
                solid: p.accent.solid.hsla(),
                hover: p.accent.hover.hsla(),
                active: p.accent.active.hsla(),
                subtle: p.accent.subtle.hsla(),
                subtle_hover: p.accent.subtle_hover.hsla(),
                border: p.accent.border.hsla(),
                text: p.accent.text.hsla(),
                text_hover: p.accent.text_hover.hsla(),
                text_active: p.accent.text_active.hsla(),
            },
            status: StatusGroup {
                success: status(p.success),
                warning: status(p.warning),
                danger: status(p.danger),
                info: status(p.info),
            },
            hues: HueTokens {
                blue_1: p.extras.blue_1.hsla(),
                blue_6: p.extras.blue_6.hsla(),
                cyan_1: p.extras.cyan_1.hsla(),
                cyan_6: p.extras.cyan_6.hsla(),
                green_1: p.extras.green_1.hsla(),
                green_6: p.extras.green_6.hsla(),
                red_1: p.extras.red_1.hsla(),
                red_6: p.extras.red_6.hsla(),
                yellow_1: p.extras.yellow_1.hsla(),
                yellow_6: p.extras.yellow_6.hsla(),
                magenta_1: p.extras.magenta_1.hsla(),
                magenta_6: p.extras.magenta_6.hsla(),
                orange_6: p.extras.orange_6.hsla(),
                purple_6: p.extras.purple_6.hsla(),
                gold_3: p.extras.gold_3.hsla(),
            },
            kinds: KindTokens {
                link: p.kinds.link.hsla(),
                email: p.kinds.email.hsla(),
                color: p.kinds.color.hsla(),
                path: p.kinds.path.hsla(),
            },
            shadow: ShadowTokens {
                overlay_ring: p.shadows.overlay_ring.hsla(),
                overlay: p.shadows.overlay.map(palette::ShadowSpec::box_shadow),
                card: p.shadows.card.map(palette::ShadowSpec::box_shadow),
            },
            materials: p.materials,
            radii: p.radii,
            border_disabled: p.borders.disabled.hsla(),
            white: p.surfaces.white.hsla(),
        }
    }
}
