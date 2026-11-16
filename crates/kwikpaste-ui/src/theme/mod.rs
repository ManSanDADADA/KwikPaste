//! 主题：2.0 的角色色板、语义 token、组件状态 token 与公共度量，以及到 gpui-component `Theme` 的映射。
//!
//! 亮 / 暗由 [`ThemePreference`] 决定，默认跟随系统；系统切换明暗时由 [`crate::open_window`]
//! 装的观察者转发到这里。gpui-component 的主题只有 App 级一份，所有窗口同时切换。

pub mod components;
mod css_color;
pub mod fonts;
mod kit;
pub mod palette;
pub mod semantic;
mod tokens;

use gpui::{App, Global, Hsla, Window, WindowAppearance, px};
use gpui_component::{Theme, ThemeMode};

use components::ComponentTokens;
pub use css_color::css_color;
pub use palette::{MATERIAL_FROSTED_ALPHA, MATERIAL_MICA_ALPHA, MaterialKind};
pub use semantic::SemanticTokens;
use std::sync::OnceLock;
pub use tokens::{TextSize, control_height, motion, px_rems, radius, space};

/// 用户的主题设置，对应 1.x `appearance.theme` 的 `auto` / `light` / `dark`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

/// 实际生效的明暗。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

impl From<WindowAppearance> for Appearance {
    fn from(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
        }
    }
}

/// rem 基准：浏览器默认 16 px，乘以文本缩放系数后交给 gpui-component 的 `Root` 每帧设置。
const REM_BASE: f32 = 16.;
/// gpui-component 代码类控件的等宽字号基准（它的默认值）。
const MONO_BASE: f32 = 13.;
/// 文本缩放系数的范围：下限 0.9 是“小”档，上限是 Windows“文本大小”的最大值。
const TEXT_SCALE_RANGE: (f32, f32) = (0.9, 2.25);

struct KpTheme {
    preference: ThemePreference,
    system: Appearance,
    text_scale: f32,
    /// 窗口材质是不是磨砂：磨砂用表面整体变透明的那套 token。
    frosted: bool,
    /// 当前明暗对应的运行时 token；切换外观时替换而不是修改静态色板。
    set: &'static ThemeSet,
}

impl Global for KpTheme {}

impl KpTheme {
    fn appearance(&self) -> Appearance {
        resolve(self.preference, self.system)
    }

    fn intern(&self) -> &'static ThemeSet {
        intern(self.appearance(), self.frosted)
    }
}

struct ThemeSet {
    palette: &'static palette::Palette,
    semantic: SemanticTokens,
    components: ComponentTokens,
}

/// 每个色板、明暗与磨砂组合只构建一次，所有访问器共享同一个驻留集合。
fn intern(appearance: Appearance, frosted: bool) -> &'static ThemeSet {
    static LIGHT: OnceLock<&'static ThemeSet> = OnceLock::new();
    static DARK: OnceLock<&'static ThemeSet> = OnceLock::new();
    static LIGHT_FROSTED: OnceLock<&'static ThemeSet> = OnceLock::new();
    static DARK_FROSTED: OnceLock<&'static ThemeSet> = OnceLock::new();
    let (slot, palette) = match (appearance, frosted) {
        (Appearance::Light, false) => (&LIGHT, &palette::LIGHT),
        (Appearance::Dark, false) => (&DARK, &palette::DARK),
        (Appearance::Light, true) => (&LIGHT_FROSTED, &palette::LIGHT),
        (Appearance::Dark, true) => (&DARK_FROSTED, &palette::DARK),
    };
    slot.get_or_init(|| {
        let mut semantic = SemanticTokens::from_palette(palette);
        if frosted {
            frost(&mut semantic);
        }
        let components = ComponentTokens::from_semantic(&semantic);
        Box::leak(Box::new(ThemeSet {
            palette,
            semantic,
            components,
        }))
    })
}

/// 磨砂：窗口里铺开的不透明表面（窗口、面板、状态淡底）统一乘上 `frosted_surface_alpha`，文字、主色不变。
/// 浮起层（`raised` / `overlay`：下拉、提示、对话框、悬浮按钮）保持不透明：它们浮在同一窗口的内容上，
/// 背后没有模糊，半透明会直接透出下面的字。中性边框、填充、悬停色和主色淡底在色板里就是叠在表面上的
/// 半透明色，压在面板、卡片、浮层或系统材质上都保持同样的对比，不再缩放。
fn frost(s: &mut SemanticTokens) {
    let alpha = s.materials.frosted_surface_alpha;
    if alpha >= 1. {
        return;
    }
    for color in [
        &mut s.surface.window,
        &mut s.surface.panel,
        &mut s.status.success.subtle,
        &mut s.status.warning.subtle,
        &mut s.status.danger.subtle,
        &mut s.status.info.subtle,
    ] {
        color.a *= alpha;
    }
}

/// 完全透明。有系统材质的窗口让 `Root` 不画底色时用。
pub fn transparent() -> Hsla {
    gpui::transparent_black()
}

/// 设置的明暗与系统明暗合成实际生效的明暗。
pub fn resolve(preference: ThemePreference, system: Appearance) -> Appearance {
    match preference {
        ThemePreference::System => system,
        ThemePreference::Light => Appearance::Light,
        ThemePreference::Dark => Appearance::Dark,
    }
}

/// 把文本缩放系数钳到 0.9–2.25；非有限值按 1.0 处理。
pub fn clamp_text_scale(scale: f32) -> f32 {
    if !scale.is_finite() {
        return 1.;
    }

    scale.clamp(TEXT_SCALE_RANGE.0, TEXT_SCALE_RANGE.1)
}

pub(crate) fn init(cx: &mut App) {
    cx.set_global(KpTheme {
        preference: ThemePreference::System,
        system: cx.window_appearance().into(),
        text_scale: 1.,
        frosted: false,
        set: intern(Appearance::from(cx.window_appearance()), false),
    });
    apply(cx);
}

/// 指定明暗的角色语义 token，测试和无全局主题的初始化路径使用。
pub fn semantic_for(appearance: Appearance) -> &'static SemanticTokens {
    &intern(appearance, false).semantic
}

/// 指定明暗的组件状态 token，测试和无全局主题的初始化路径使用。
pub fn components_for(appearance: Appearance) -> &'static ComponentTokens {
    &intern(appearance, false).components
}

/// 当前角色语义 token，新界面只通过这个入口读取颜色。
pub fn semantic(cx: &App) -> &'static SemanticTokens {
    cx.try_global::<KpTheme>()
        .map_or_else(|| semantic_for(appearance(cx)), |theme| &theme.set.semantic)
}

/// 当前组件状态 token，避免视图临时调制颜色。
pub fn components(cx: &App) -> &'static ComponentTokens {
    cx.try_global::<KpTheme>().map_or_else(
        || components_for(appearance(cx)),
        |theme| &theme.set.components,
    )
}
pub fn appearance(cx: &App) -> Appearance {
    cx.try_global::<KpTheme>()
        .map_or(Appearance::Light, KpTheme::appearance)
}

pub fn preference(cx: &App) -> ThemePreference {
    cx.try_global::<KpTheme>()
        .map_or(ThemePreference::System, |theme| theme.preference)
}

pub fn text_scale(cx: &App) -> f32 {
    cx.try_global::<KpTheme>()
        .map_or(1., |theme| theme.text_scale)
}

/// 切换主题设置并刷新所有窗口。
pub fn set_preference(preference: ThemePreference, cx: &mut App) {
    let Some(theme) = cx.try_global::<KpTheme>() else {
        return;
    };
    if theme.preference == preference {
        return;
    }

    cx.global_mut::<KpTheme>().preference = preference;
    cx.global_mut::<KpTheme>().set = cx.global::<KpTheme>().intern();
    apply(cx);
}

/// 窗口材质切到或切离磨砂时由平台层调用，换成对应的 token 并刷新所有窗口。
pub fn set_frosted(frosted: bool, cx: &mut App) {
    let Some(theme) = cx.try_global::<KpTheme>() else {
        return;
    };
    if theme.frosted == frosted {
        return;
    }

    cx.global_mut::<KpTheme>().frosted = frosted;
    cx.global_mut::<KpTheme>().set = cx.global::<KpTheme>().intern();
    apply(cx);
}

/// 设置文本缩放系数（Windows“文本大小”，由平台层读取并推送；macOS 恒为 1）。
///
/// rem 基准变成 `16 × scale`，只用 rem 的界面整体放大，窗口尺寸补偿由平台层负责。
pub fn set_text_scale(scale: f32, cx: &mut App) {
    let scale = clamp_text_scale(scale);
    let Some(theme) = cx.try_global::<KpTheme>() else {
        return;
    };
    if theme.text_scale == scale {
        return;
    }

    cx.global_mut::<KpTheme>().text_scale = scale;
    apply(cx);
}

/// 系统明暗变化时由窗口观察者调用；设置为跟随系统时立即切换。
pub(crate) fn sync_system_appearance(window: &Window, cx: &mut App) {
    gpui_base::apply_system_reduce_motion(cx);

    let system = Appearance::from(window.appearance());
    let Some(theme) = cx.try_global::<KpTheme>() else {
        return;
    };
    if theme.system == system {
        return;
    }

    let follows_system = theme.preference == ThemePreference::System;
    cx.global_mut::<KpTheme>().system = system;
    if follows_system {
        cx.global_mut::<KpTheme>().set = cx.global::<KpTheme>().intern();
        apply(cx);
    }
}

/// 把当前明暗与文本缩放写进 gpui-component 的全局主题，并刷新所有窗口。
fn apply(cx: &mut App) {
    let Some(theme) = cx.try_global::<KpTheme>() else {
        return;
    };
    let scale = theme.text_scale;
    let set = theme.set;
    let mode = match theme.appearance() {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };

    // `change` 会先装上 gpui-component 自带的主题，随后的 `update` 再整体覆盖成快贴的配色。
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.colors = kit::theme_color(set.palette, &set.semantic);
        theme.font_family = fonts::UI_FAMILY.into();
        theme.mono_font_family = fonts::MONO_FAMILY.into();
        theme.font_size = px(REM_BASE * scale);
        theme.mono_font_size = px(MONO_BASE * scale);
        theme.radius = px(palette::RADIUS * scale);
        theme.radius_lg = px(palette::RADIUS_LG * scale);
        // 输入框、选择器、按钮、开关聚焦时不画外圈光晕，只换边框色（见 `kit` 的 `ring`）。
        theme.focus_ring = false;
    });
    // 菜单、子菜单、下拉的描边环与阴影：gpui-component 自带的黑 10% 在深色上看不出来。
    gpui_component::set_popover_elevation(Some(gpui_component::PopoverElevation {
        ring: set.semantic.shadow.overlay_ring,
        shadows: set.semantic.shadow.overlay.to_vec(),
    }));
}

#[cfg(test)]
mod tests;
