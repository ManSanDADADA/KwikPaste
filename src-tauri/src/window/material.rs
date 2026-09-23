use serde::Serialize;
use tauri::{
    window::{Effect, EffectsBuilder},
    AppHandle, Manager,
};

#[cfg(target_os = "macos")]
use tauri::window::EffectState;

use crate::core::Result;
use crate::settings::{Appearance, Material, Theme};

/// 不套原生材质的窗口：WebView 底色不透明时效果不可见，只会白白触发 DWM 调用。
const NO_NATIVE_MATERIAL_LABELS: &[&str] = &[super::UPDATE_WINDOW_LABEL];

/// 当前系统对各原生材质的支持情况；前端据此禁用不可用选项并回退纯色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialSupport {
    pub mica: bool,
    pub acrylic: bool,
}

impl MaterialSupport {
    fn allows(self, material: Material) -> bool {
        match material {
            Material::Default => true,
            Material::Mica => self.mica,
            Material::Acrylic => self.acrylic,
        }
    }

    /// 把设置里的材质收敛为当前系统真正能生效的材质。
    fn resolve(self, material: Material) -> Material {
        if self.allows(material) {
            material
        } else {
            Material::Default
        }
    }
}

/// Windows 门槛与 window-vibrancy 一致：Mica 需要 Windows 11（build 22000），
/// Acrylic 需要 Windows 10 1809（build 17763）。
#[cfg(target_os = "windows")]
pub fn support() -> MaterialSupport {
    let build = windows_version::OsVersion::current().build;

    MaterialSupport {
        mica: build >= 22000,
        acrylic: build >= 17763,
    }
}

#[cfg(target_os = "macos")]
pub fn support() -> MaterialSupport {
    MaterialSupport {
        mica: true,
        acrylic: true,
    }
}

/// Applies the configured native backdrop to one existing window.
///
/// The web layer keeps an opaque token fallback for unsupported systems and
/// reduced-transparency preferences; native effects are limited to the window shell.
pub(super) fn apply(app_handle: &AppHandle, label: &str, appearance: &Appearance) -> Result<()> {
    if NO_NATIVE_MATERIAL_LABELS.contains(&label) {
        return Ok(());
    }

    let Some(window) = app_handle.get_webview_window(label) else {
        return Ok(());
    };
    let material = support().resolve(appearance.material);
    let effects = effects_for(material, appearance.theme);

    // Clear first so switching between Acrylic and Mica cannot leave the previous
    // platform backdrop active underneath the new one.
    window.set_effects(None).map_err(|e| anyhow::anyhow!(e))?;
    if let Some(effects) = effects {
        window
            .set_effects(effects)
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    Ok(())
}

/// Applies the configured backdrop to every window that currently exists.
pub(super) fn apply_existing(app_handle: &AppHandle, appearance: &Appearance) {
    for label in app_handle.webview_windows().into_keys() {
        let result = if label == super::CLIPBOARD_WINDOW_LABEL {
            super::apply_clipboard_window_material(app_handle, appearance)
        } else {
            apply(app_handle, &label, appearance)
        };
        if let Err(err) = result {
            log::warn!("apply native material failed for {label}: {err}");
        }
    }
}

#[cfg(target_os = "windows")]
fn effects_for(
    material: Material,
    theme: Theme,
) -> Option<tauri::utils::config::WindowEffectsConfig> {
    let effect = match material {
        Material::Default => return None,
        Material::Mica => match theme {
            Theme::Auto => Effect::Mica,
            Theme::Light => Effect::MicaLight,
            Theme::Dark => Effect::MicaDark,
        },
        Material::Acrylic => Effect::Acrylic,
    };

    Some(EffectsBuilder::new().effect(effect).build())
}

#[cfg(target_os = "macos")]
fn effects_for(
    material: Material,
    _theme: Theme,
) -> Option<tauri::utils::config::WindowEffectsConfig> {
    let effect = match material {
        Material::Default => return None,
        Material::Mica => Effect::UnderWindowBackground,
        Material::Acrylic => Effect::Popover,
    };

    Some(
        EffectsBuilder::new()
            .effect(effect)
            .state(EffectState::FollowsWindowActiveState)
            .build(),
    )
}

#[cfg(test)]
mod support_tests {
    use super::*;

    const NONE: MaterialSupport = MaterialSupport {
        mica: false,
        acrylic: false,
    };
    const ACRYLIC_ONLY: MaterialSupport = MaterialSupport {
        mica: false,
        acrylic: true,
    };
    const ALL: MaterialSupport = MaterialSupport {
        mica: true,
        acrylic: true,
    };

    #[test]
    fn default_material_is_always_allowed() {
        assert_eq!(NONE.resolve(Material::Default), Material::Default);
        assert_eq!(ALL.resolve(Material::Default), Material::Default);
    }

    #[test]
    fn unsupported_materials_fall_back_to_default() {
        assert_eq!(NONE.resolve(Material::Mica), Material::Default);
        assert_eq!(NONE.resolve(Material::Acrylic), Material::Default);
        assert_eq!(ACRYLIC_ONLY.resolve(Material::Mica), Material::Default);
    }

    #[test]
    fn supported_materials_pass_through() {
        assert_eq!(ACRYLIC_ONLY.resolve(Material::Acrylic), Material::Acrylic);
        assert_eq!(ALL.resolve(Material::Mica), Material::Mica);
        assert_eq!(ALL.resolve(Material::Acrylic), Material::Acrylic);
    }

    // 只有底色不透明的窗口跳过原生材质；预览窗口已经是面板等大的透明小窗，要套材质。
    #[test]
    fn only_opaque_windows_skip_native_material() {
        use crate::window::{
            CLIPBOARD_PREVIEW_WINDOW_LABEL, CLIPBOARD_WINDOW_LABEL, UPDATE_WINDOW_LABEL,
        };

        assert!(NO_NATIVE_MATERIAL_LABELS.contains(&UPDATE_WINDOW_LABEL));
        assert!(!NO_NATIVE_MATERIAL_LABELS.contains(&CLIPBOARD_PREVIEW_WINDOW_LABEL));
        assert!(!NO_NATIVE_MATERIAL_LABELS.contains(&CLIPBOARD_WINDOW_LABEL));
    }

    #[test]
    fn support_serializes_camel_case_flags() {
        let json = serde_json::to_string(&ACRYLIC_ONLY).unwrap();
        assert_eq!(json, r#"{"mica":false,"acrylic":true}"#);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn mica_tracks_the_explicit_theme_and_auto_uses_system_mica() {
        assert_eq!(
            effects_for(Material::Mica, Theme::Auto).unwrap().effects,
            vec![Effect::Mica]
        );
        assert_eq!(
            effects_for(Material::Mica, Theme::Light).unwrap().effects,
            vec![Effect::MicaLight]
        );
        assert_eq!(
            effects_for(Material::Mica, Theme::Dark).unwrap().effects,
            vec![Effect::MicaDark]
        );
    }

    #[test]
    fn default_material_clears_native_effects() {
        assert!(effects_for(Material::Default, Theme::Auto).is_none());
    }

    #[test]
    fn acrylic_is_independent_from_color_theme() {
        assert_eq!(
            effects_for(Material::Acrylic, Theme::Light)
                .unwrap()
                .effects,
            vec![Effect::Acrylic]
        );
        assert_eq!(
            effects_for(Material::Acrylic, Theme::Dark).unwrap().effects,
            vec![Effect::Acrylic]
        );
    }
}
