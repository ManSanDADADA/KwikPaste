//! 2.0 的原始配色。这里按视觉角色分组，亮暗两套值都是显式的。

use gpui::{BoxShadow, Hsla, Rgba, point, px};

/// 一个带透明度的 RGB 原色；保留原始值，转换发生在语义层构建时。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub rgb: u32,
    pub alpha: f32,
}
impl Color {
    pub const fn opaque(rgb: u32) -> Self {
        Self { rgb, alpha: 1. }
    }
    pub const fn translucent(rgb: u32, alpha: f32) -> Self {
        Self { rgb, alpha }
    }
    pub fn hsla(self) -> Hsla {
        let [_, r, g, b] = self.rgb.to_be_bytes();
        Rgba {
            r: f32::from(r) / 255.,
            g: f32::from(g) / 255.,
            b: f32::from(b) / 255.,
            a: self.alpha,
        }
        .into()
    }
}
const fn hex(rgb: u32) -> Color {
    Color::opaque(rgb)
}
const fn rgba(rgb: u32, alpha: f32) -> Color {
    Color::translucent(rgb, alpha)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePalette {
    pub window: Color,
    pub panel: Color,
    pub raised: Color,
    /// 浮层（菜单、下拉、toast）：比 `raised` 再亮一档，压在面板或系统材质上都分得开。
    pub overlay: Color,
    pub spotlight: Color,
    pub mask: Color,
    pub white: Color,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextPalette {
    pub primary: Color,
    pub secondary: Color,
    pub muted: Color,
    pub faint: Color,
    pub placeholder: Color,
    pub disabled: Color,
    pub on_accent: Color,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderPalette {
    pub default: Color,
    pub subtle: Color,
    pub disabled: Color,
    pub divider: Color,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillPalette {
    pub strong: Color,
    pub default: Color,
    pub subtle: Color,
    pub faint: Color,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemPalette {
    pub hover: Color,
    pub selected: Color,
    pub selected_hover: Color,
    pub text_hover: Color,
    pub text_active: Color,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccentPalette {
    pub solid: Color,
    pub hover: Color,
    pub active: Color,
    pub subtle: Color,
    pub subtle_hover: Color,
    pub border: Color,
    pub text: Color,
    pub text_hover: Color,
    pub text_active: Color,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusPalette {
    pub solid: Color,
    pub hover: Color,
    pub active: Color,
    pub subtle: Color,
    pub border: Color,
    /// 当文字用的状态色：浅色下比 `solid` 深一档，压在白底或自己的淡底上才读得清。
    pub text: Color,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtraHues {
    pub blue_1: Color,
    pub blue_6: Color,
    pub cyan_1: Color,
    pub cyan_6: Color,
    pub green_1: Color,
    pub green_6: Color,
    pub red_1: Color,
    pub red_6: Color,
    pub yellow_1: Color,
    pub yellow_6: Color,
    pub magenta_1: Color,
    pub magenta_6: Color,
    pub orange_6: Color,
    pub purple_6: Color,
    pub gold_3: Color,
}
/// 卡片类型标签里识别出的子类型的文字色；底色由组件层取它的淡化。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KindPalette {
    pub link: Color,
    pub email: Color,
    pub color: Color,
    pub path: Color,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowSpec {
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}
impl ShadowSpec {
    pub fn box_shadow(self) -> BoxShadow {
        BoxShadow {
            color: self.color.hsla(),
            offset: point(px(0.), px(self.y)),
            blur_radius: px(self.blur),
            spread_radius: px(self.spread),
            inset: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowPalette {
    /// 浮层外沿 1 px 的描边环，叠在浮层下面的内容上。
    pub overlay_ring: Color,
    pub overlay: [ShadowSpec; 3],
    pub card: [ShadowSpec; 3],
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Radii {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Materials {
    pub panel_mica_alpha: f32,
    pub panel_frosted_alpha: f32,
    pub chrome_mica_alpha: f32,
    pub chrome_frosted_alpha: f32,
    /// 磨砂下所有不透明表面（窗口、面板、浮起层、淡底）统一乘上的不透明度，边框和文字不变。
    pub frosted_surface_alpha: f32,
}

/// 一个完整的明暗原始色板；语义层和组件 token 都只能从这里读取。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub surfaces: SurfacePalette,
    pub text: TextPalette,
    pub borders: BorderPalette,
    pub fills: FillPalette,
    pub items: ItemPalette,
    pub accent: AccentPalette,
    pub success: StatusPalette,
    pub warning: StatusPalette,
    pub danger: StatusPalette,
    pub info: StatusPalette,
    pub extras: ExtraHues,
    pub kinds: KindPalette,
    pub shadows: ShadowPalette,
    pub radii: Radii,
    pub materials: Materials,
}
const LIGHT_EXTRAS: ExtraHues = ExtraHues {
    blue_1: hex(0xeef2fe),
    blue_6: hex(0x3268ed),
    cyan_1: hex(0xe6fffb),
    cyan_6: hex(0x13c2c2),
    green_1: hex(0xf6ffed),
    green_6: hex(0x52c41a),
    red_1: hex(0xfff1f0),
    red_6: hex(0xf5222d),
    yellow_1: hex(0xfeffe6),
    yellow_6: hex(0xfadb14),
    magenta_1: hex(0xfff0f6),
    magenta_6: hex(0xeb2f96),
    orange_6: hex(0xfa8c16),
    purple_6: hex(0x722ed1),
    gold_3: hex(0xffe58f),
};
const LIGHT_KINDS: KindPalette = KindPalette {
    link: hex(0x0958d9),
    email: hex(0x531dab),
    color: hex(0xb45309),
    path: hex(0x237804),
};
const DARK_KINDS: KindPalette = KindPalette {
    link: hex(0x5a9cf0),
    email: hex(0xa985e8),
    color: hex(0xe89a3c),
    path: hex(0x6abe39),
};
const DARK_EXTRAS: ExtraHues = ExtraHues {
    blue_1: rgba(0x5b8cf7, 0.14),
    blue_6: hex(0x5b8cf7),
    cyan_1: hex(0x112123),
    cyan_6: hex(0x13a8a8),
    green_1: hex(0x162312),
    green_6: hex(0x49aa19),
    red_1: hex(0x2a1215),
    red_6: hex(0xd32029),
    yellow_1: hex(0x2b2611),
    yellow_6: hex(0xd8bd14),
    magenta_1: hex(0x291321),
    magenta_6: hex(0xcb2b83),
    orange_6: hex(0xd87a16),
    purple_6: hex(0x642ab5),
    gold_3: hex(0x594214),
};
const RADII: Radii = Radii {
    xs: 2.,
    sm: 4.,
    md: 5.,
    lg: 6.,
};
/// 材质下面板色的不透明度。Windows 的 Mica 本身不透明，不再铺底色；磨砂的表面已经按
/// `frosted_surface_alpha` 整体变透明（与 QcClip 的做法相同），根底色直接用它，不再二次缩放。
#[cfg(not(target_os = "macos"))]
const MATERIALS: Materials = Materials {
    panel_mica_alpha: 0.,
    panel_frosted_alpha: 1.,
    chrome_mica_alpha: 0.,
    chrome_frosted_alpha: 1.,
    frosted_surface_alpha: 0.3,
};
/// macOS 所有窗口都是系统 `NSVisualEffectView`：自带色调，但花哨的桌面仍会透出来，两种材质都要铺底色。
/// 磨砂沿用只给根底色半透明的做法，表面不整体缩放。
#[cfg(target_os = "macos")]
const MATERIALS: Materials = Materials {
    panel_mica_alpha: 0.58,
    panel_frosted_alpha: 0.34,
    chrome_mica_alpha: 0.34,
    chrome_frosted_alpha: 0.2,
    frosted_surface_alpha: 1.,
};
const fn shadow(y: f32, blur: f32, spread: f32, color: Color) -> ShadowSpec {
    ShadowSpec {
        y,
        blur,
        spread,
        color,
    }
}

/// 亮色原始色板。
pub const LIGHT: Palette = Palette {
    surfaces: SurfacePalette {
        window: hex(0xf6f6f7),
        panel: hex(0xffffff),
        raised: hex(0xffffff),
        overlay: hex(0xffffff),
        spotlight: rgba(0x1d1d21, 0.92),
        mask: rgba(0x000000, 0.32),
        white: hex(0xffffff),
    },
    text: TextPalette {
        primary: hex(0x18181b),
        secondary: hex(0x55555e),
        muted: hex(0x8b8b95),
        faint: hex(0xb4b4bb),
        placeholder: hex(0xa1a1aa),
        disabled: hex(0xb4b4bb),
        on_accent: hex(0xffffff),
    },
    borders: BorderPalette {
        default: rgba(0x000000, 0.1),
        subtle: rgba(0x000000, 0.06),
        disabled: rgba(0x000000, 0.16),
        divider: rgba(0x000000, 0.055),
    },
    fills: FillPalette {
        strong: rgba(0x000000, 0.08),
        default: rgba(0x000000, 0.05),
        subtle: rgba(0x000000, 0.035),
        faint: rgba(0x000000, 0.03),
    },
    items: ItemPalette {
        hover: rgba(0x000000, 0.04),
        selected: rgba(0x000000, 0.06),
        selected_hover: rgba(0x000000, 0.08),
        text_hover: rgba(0x000000, 0.04),
        text_active: rgba(0x000000, 0.07),
    },
    accent: AccentPalette {
        solid: hex(0x3268ed),
        hover: hex(0x4c7ef0),
        active: hex(0x2552c9),
        subtle: rgba(0x3268ed, 0.085),
        subtle_hover: rgba(0x3268ed, 0.16),
        border: hex(0xb3c6f9),
        text: hex(0x3268ed),
        text_hover: hex(0x4c7ef0),
        text_active: hex(0x2552c9),
    },
    success: StatusPalette {
        solid: hex(0x16a34a),
        hover: hex(0x22c55e),
        active: hex(0x15803d),
        subtle: hex(0xf0fdf4),
        border: hex(0xbbf7d0),
        text: hex(0x15803d),
    },
    warning: StatusPalette {
        solid: hex(0xd97706),
        hover: hex(0xf59e0b),
        active: hex(0xb45309),
        subtle: hex(0xfffbeb),
        border: hex(0xfde68a),
        text: hex(0xb45309),
    },
    danger: StatusPalette {
        solid: hex(0xe5484d),
        hover: hex(0xeb6b6f),
        active: hex(0xc93a3f),
        subtle: hex(0xfff1f1),
        border: hex(0xfbcaca),
        text: hex(0xc93a3f),
    },
    info: StatusPalette {
        solid: hex(0x3268ed),
        hover: hex(0x4c7ef0),
        active: hex(0x2552c9),
        subtle: hex(0xeef2fe),
        border: hex(0xb3c6f9),
        text: hex(0x2552c9),
    },
    extras: LIGHT_EXTRAS,
    kinds: LIGHT_KINDS,
    shadows: ShadowPalette {
        overlay_ring: rgba(0x000000, 0.1),
        overlay: [
            shadow(1., 2., 0., rgba(0, 0.06)),
            shadow(4., 12., 0., rgba(0, 0.08)),
            shadow(12., 32., -4., rgba(0, 0.1)),
        ],
        card: [
            shadow(1., 2., 0., rgba(0, 0.04)),
            shadow(1., 1., 0., rgba(0, 0.02)),
            shadow(0., 0., 0., rgba(0, 0.)),
        ],
    },
    radii: RADII,
    materials: MATERIALS,
};
/// 暗色原始色板。
pub const DARK: Palette = Palette {
    surfaces: SurfacePalette {
        window: hex(0x0f0f11),
        panel: hex(0x161618),
        raised: hex(0x1f1f23),
        overlay: hex(0x2c2c32),
        spotlight: hex(0x2a2a2f),
        mask: rgba(0x000000, 0.5),
        white: hex(0xffffff),
    },
    text: TextPalette {
        primary: hex(0xededef),
        secondary: hex(0xa0a0ab),
        muted: hex(0x85858f),
        faint: hex(0x55555e),
        placeholder: hex(0x70707a),
        disabled: hex(0x55555e),
        on_accent: hex(0xffffff),
    },
    borders: BorderPalette {
        default: rgba(0xffffff, 0.12),
        subtle: rgba(0xffffff, 0.07),
        disabled: rgba(0xffffff, 0.17),
        divider: rgba(0xffffff, 0.055),
    },
    fills: FillPalette {
        strong: rgba(0xffffff, 0.11),
        default: rgba(0xffffff, 0.07),
        subtle: rgba(0xffffff, 0.045),
        faint: rgba(0xffffff, 0.03),
    },
    items: ItemPalette {
        hover: rgba(0xffffff, 0.05),
        selected: rgba(0xffffff, 0.08),
        selected_hover: rgba(0xffffff, 0.1),
        text_hover: rgba(0xffffff, 0.05),
        text_active: rgba(0xffffff, 0.08),
    },
    accent: AccentPalette {
        solid: hex(0x5b8cf7),
        hover: hex(0x74a0f9),
        active: hex(0x4677e0),
        subtle: rgba(0x5b8cf7, 0.14),
        subtle_hover: rgba(0x5b8cf7, 0.2),
        border: rgba(0x5b8cf7, 0.4),
        text: hex(0x74a0f9),
        text_hover: hex(0x8db2fa),
        text_active: hex(0x5b8cf7),
    },
    success: StatusPalette {
        solid: hex(0x2fbf62),
        hover: hex(0x4ade80),
        active: hex(0x16a34a),
        subtle: rgba(0x2fbf62, 0.14),
        border: rgba(0x2fbf62, 0.4),
        text: hex(0x2fbf62),
    },
    warning: StatusPalette {
        solid: hex(0xf0a020),
        hover: hex(0xfbbf24),
        active: hex(0xd97706),
        subtle: rgba(0xf0a020, 0.14),
        border: rgba(0xf0a020, 0.4),
        text: hex(0xf0a020),
    },
    danger: StatusPalette {
        solid: hex(0xf2555a),
        hover: hex(0xf5777b),
        active: hex(0xd8434a),
        subtle: rgba(0xf2555a, 0.14),
        border: rgba(0xf2555a, 0.4),
        text: hex(0xf2555a),
    },
    info: StatusPalette {
        solid: hex(0x5b8cf7),
        hover: hex(0x74a0f9),
        active: hex(0x4677e0),
        subtle: rgba(0x5b8cf7, 0.14),
        border: rgba(0x5b8cf7, 0.4),
        text: hex(0x74a0f9),
    },
    extras: DARK_EXTRAS,
    kinds: DARK_KINDS,
    shadows: ShadowPalette {
        overlay_ring: rgba(0xffffff, 0.12),
        overlay: [
            shadow(1., 2., 0., rgba(0, 0.3)),
            shadow(4., 12., 0., rgba(0, 0.35)),
            shadow(12., 32., -4., rgba(0, 0.45)),
        ],
        card: [
            shadow(1., 2., 0., rgba(0, 0.2)),
            shadow(0., 0., 0., rgba(0, 0.)),
            shadow(0., 0., 0., rgba(0, 0.)),
        ],
    },
    radii: RADII,
    materials: MATERIALS,
};

pub const RADIUS_XS: f32 = RADII.xs;
pub const RADIUS_SM: f32 = RADII.sm;
pub const RADIUS: f32 = RADII.md;
pub const RADIUS_LG: f32 = RADII.lg;
pub const MATERIAL_MICA_ALPHA: f32 = MATERIALS.panel_mica_alpha;
pub const MATERIAL_FROSTED_ALPHA: f32 = MATERIALS.panel_frosted_alpha;

/// 材质类型；由窗口层映射到当前主题的 surface 角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialKind {
    None,
    Mica,
    Frosted,
}
pub fn material_surface(tokens: &super::semantic::SemanticTokens, kind: MaterialKind) -> Hsla {
    match kind {
        MaterialKind::None => tokens.surface.panel,
        MaterialKind::Mica => tokens
            .surface
            .panel
            .opacity(tokens.materials.panel_mica_alpha),
        MaterialKind::Frosted => tokens
            .surface
            .panel
            .opacity(tokens.materials.panel_frosted_alpha),
    }
}
pub fn material_chrome_surface(
    tokens: &super::semantic::SemanticTokens,
    kind: MaterialKind,
) -> Hsla {
    match kind {
        MaterialKind::None => tokens.surface.window,
        MaterialKind::Mica => tokens
            .surface
            .panel
            .opacity(tokens.materials.chrome_mica_alpha),
        MaterialKind::Frosted => tokens
            .surface
            .panel
            .opacity(tokens.materials.chrome_frosted_alpha),
    }
}
