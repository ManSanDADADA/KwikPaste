use super::*;
use gpui::{Hsla, Rgba};

fn rgb(color: Hsla) -> [f32; 3] {
    let rgba = Rgba::from(color);
    [rgba.r, rgba.g, rgba.b]
}

fn composite(foreground: Hsla, background: Hsla) -> [f32; 3] {
    let fg = rgb(foreground);
    let bg = rgb(background);
    let a = foreground.a;
    [
        fg[0] * a + bg[0] * (1. - a),
        fg[1] * a + bg[1] * (1. - a),
        fg[2] * a + bg[2] * (1. - a),
    ]
}

fn channel(value: f32) -> f32 {
    if value <= 0.03928 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

fn contrast(foreground: Hsla, background: Hsla) -> f32 {
    let foreground = luminance(composite(foreground, background));
    let background = luminance(rgb(background));
    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}

#[test]
fn palettes_define_roles_and_vary_by_appearance() {
    assert_ne!(
        palette::LIGHT.surfaces.window,
        palette::DARK.surfaces.window
    );
    assert_ne!(palette::LIGHT.text.primary, palette::DARK.text.primary);
    assert_ne!(palette::LIGHT.accent.solid, palette::DARK.accent.solid);
    assert_ne!(
        palette::LIGHT.shadows.overlay,
        palette::DARK.shadows.overlay
    );
    for appearance in [Appearance::Light, Appearance::Dark] {
        let semantic = semantic_for(appearance);
        let components = components_for(appearance);
        assert_eq!(components.input.border_focus, semantic.accent.solid);
    }
}

#[test]
fn text_contrast_is_readable_on_surfaces() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let semantic = semantic_for(appearance);
        for surface in [
            semantic.surface.window,
            semantic.surface.panel,
            semantic.surface.raised,
        ] {
            assert!(
                contrast(semantic.text.primary, surface) >= 7.,
                "primary contrast {appearance:?}: {}",
                contrast(semantic.text.primary, surface)
            );
            assert!(
                contrast(semantic.text.secondary, surface) >= 4.5,
                "secondary contrast {appearance:?}: {}",
                contrast(semantic.text.secondary, surface)
            );
            assert!(
                contrast(semantic.text.muted, surface) >= 3.,
                "muted contrast {appearance:?}: {}",
                contrast(semantic.text.muted, surface)
            );
        }
    }
}

#[test]
fn preference_resolves_against_the_system() {
    assert_eq!(
        resolve(ThemePreference::System, Appearance::Dark),
        Appearance::Dark
    );
    assert_eq!(
        resolve(ThemePreference::Light, Appearance::Dark),
        Appearance::Light
    );
    assert_eq!(
        resolve(ThemePreference::Dark, Appearance::Light),
        Appearance::Dark
    );
}

#[test]
fn text_scale_is_clamped() {
    assert_eq!(clamp_text_scale(0.5), 0.9);
    assert_eq!(clamp_text_scale(1.5), 1.5);
    assert_eq!(clamp_text_scale(3.), 2.25);
    assert_eq!(clamp_text_scale(f32::NAN), 1.);
}

#[test]
fn type_scale_matches_wind4() {
    let pairs: Vec<(f32, f32)> = TextSize::ALL
        .iter()
        .map(|size| (size.font_size().0 * 16., size.line_height().0 * 16.))
        .collect();
    assert_eq!(
        pairs,
        [(12., 16.), (14., 20.), (16., 24.), (18., 28.), (24., 32.)].to_vec()
    );
    assert_eq!(radius::MD.0 * 16., palette::RADIUS);
    assert_eq!(control_height::MD.0 * 16., 32.);
    assert_eq!(space(1.5).0, 0.375);
    assert_eq!(px_rems(24.).0, 1.5);
}

#[test]
fn frosted_scales_surfaces_and_keeps_overlaid_colors() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let plain = &intern(appearance, false).semantic;
        let frosted = &intern(appearance, true).semantic;
        let alpha = plain.materials.frosted_surface_alpha;
        assert_eq!(frosted.surface.panel.a, plain.surface.panel.a * alpha);
        assert_eq!(frosted.surface.raised, plain.surface.raised);
        assert_eq!(frosted.surface.overlay, plain.surface.overlay);
        assert_eq!(frosted.text, plain.text);
        assert_eq!(frosted.fill, plain.fill);
        assert_eq!(frosted.border, plain.border);
        assert_eq!(frosted.accent, plain.accent);
    }
}

#[test]
fn layers_stay_apart_on_any_backdrop() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let semantic = semantic_for(appearance);
        // 中性边框和主色淡底是叠加色：压在面板、浮起层、浮层上都看得出。
        for color in [
            semantic.border.default,
            semantic.border.subtle,
            semantic.accent.subtle,
        ] {
            assert!(color.a < 1., "{appearance:?}: {color:?} must be an overlay");
        }
        for surface in [
            semantic.surface.panel,
            semantic.surface.raised,
            semantic.surface.overlay,
        ] {
            assert!(
                contrast(semantic.border.default, surface) >= 1.2,
                "border contrast {appearance:?}: {}",
                contrast(semantic.border.default, surface)
            );
        }
    }
    // 深色的浮层比面板和浮起层都亮得出一档；浅色靠描边环和阴影。
    let dark = semantic_for(Appearance::Dark);
    assert!(contrast(dark.surface.overlay, dark.surface.panel) >= 1.25);
    assert!(contrast(dark.surface.overlay, dark.surface.raised) >= 1.15);
}

#[test]
fn token_sets_are_interned() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        assert!(std::ptr::eq(
            semantic_for(appearance),
            semantic_for(appearance)
        ));
        assert!(std::ptr::eq(
            components_for(appearance),
            components_for(appearance)
        ));
    }
}
