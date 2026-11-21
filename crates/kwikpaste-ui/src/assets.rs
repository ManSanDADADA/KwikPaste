//! 资源源：gpui-kit-assets 的默认图标集（组件自带的勾选、下拉箭头等）加上 `icons/` 下从 1.x
//! iconify 包导出的图标（见 `icons/export-icons.mjs`），以及运行时登记的 SVG（用户导入的分组图标）。
//! 只内嵌用到的 SVG，不整套打包 lucide。

use std::{
    borrow::Cow,
    collections::HashMap,
    hash::{Hash as _, Hasher as _},
    sync::{Arc, LazyLock, Mutex},
};

use gpui::{AssetSource, Result, SharedString};

/// 自带图标的路径前缀，与 gpui-kit-assets 的 `icons/` 区分开。
pub(crate) const PREFIX: &str = "kp-icons/";
/// 运行时登记的 SVG 的路径前缀。
const DYNAMIC_PREFIX: &str = "kp-svg/";

macro_rules! embedded {
    ($($file:literal),* $(,)?) => {
        &[$((concat!("kp-icons/", $file), include_bytes!(concat!("../icons/", $file)))),*]
    };
}

/// `icons/` 目录下内嵌的 SVG：`(资源路径, 内容)`。
const EMBEDDED: &[(&str, &[u8])] = embedded![
    "lets-icons-bell.svg",
    "lets-icons-book.svg",
    "lets-icons-bookmark.svg",
    "lets-icons-box.svg",
    "lets-icons-calendar.svg",
    "lets-icons-code.svg",
    "lets-icons-database.svg",
    "lets-icons-file-dock.svg",
    "lets-icons-folder-file-alt.svg",
    "lets-icons-folder.svg",
    "lets-icons-img-box.svg",
    "lets-icons-link.svg",
    "lets-icons-notebook.svg",
    "lets-icons-pin.svg",
    "lets-icons-setting-line.svg",
    "lets-icons-star.svg",
    "lets-icons-widget.svg",
    "lucide-app-window.svg",
    "lucide-chevron-left.svg",
    "lucide-chevron-right.svg",
    "lucide-circle-check.svg",
    "lucide-clipboard-paste.svg",
    "lucide-clipboard-type.svg",
    "lucide-clock.svg",
    "lucide-copy-check.svg",
    "lucide-download.svg",
    "lucide-eye-off.svg",
    "lucide-file-symlink.svg",
    "lucide-filter-x.svg",
    "lucide-folder-open.svg",
    "lucide-grip-vertical.svg",
    "lucide-image-off.svg",
    "lucide-key-round.svg",
    "lucide-keyboard.svg",
    "lucide-languages.svg",
    "lucide-laptop.svg",
    "lucide-link.svg",
    "lucide-list-checks.svg",
    "lucide-list-filter.svg",
    "lucide-mail.svg",
    "lucide-monitor.svg",
    "lucide-notebook-pen.svg",
    "lucide-pencil.svg",
    "lucide-picture-in-picture-2.svg",
    "lucide-rotate-cw.svg",
    "lucide-scan-qr-code.svg",
    "lucide-scan-text.svg",
    "lucide-settings-2.svg",
    "lucide-shapes.svg",
    "lucide-shrink.svg",
    "lucide-smartphone.svg",
    "lucide-square-arrow-out-up-right.svg",
    "lucide-text-select.svg",
    "lucide-trash-2.svg",
    "lucide-trash.svg",
    "lucide-triangle-alert.svg",
    "lucide-type.svg",
    "lucide-zoom-in.svg",
    "lucide-zoom-out.svg",
    "ph-push-pin-bold.svg",
];

/// 运行时登记的 SVG：路径 → 内容。只增不删，条数等于用过的不同自定义图标数。
static DYNAMIC: LazyLock<Mutex<HashMap<String, Arc<[u8]>>>> = LazyLock::new(Mutex::default);

/// 自定义分组图标的默认值（1.x `DEFAULT_GROUP_ICON`）。
const DEFAULT_GROUP_ICON: &str = "kp-icons/lets-icons-folder.svg";

/// 自定义分组图标的资源路径。`icon` 是分组记录里存的值：预设图标的类名（`i-lets-icons:book`）
/// 或用户导入的 SVG 源码；SVG 当场登记，同一份源码总是得到同一个路径。认不出的预设回退到文件夹。
pub fn group_icon_path(icon: &str) -> SharedString {
    let icon = icon.trim();
    if icon.starts_with("<svg") {
        return register_svg(icon);
    }

    let file = icon
        .strip_prefix("i-")
        .map(|name| format!("{PREFIX}{}.svg", name.replace(':', "-")));
    match file {
        Some(path) if EMBEDDED.iter().any(|(embedded, _)| *embedded == path) => path.into(),
        _ => DEFAULT_GROUP_ICON.into(),
    }
}

/// 登记一段 SVG 源码，返回可交给 `svg().path(..)` 的资源路径。
pub fn register_svg(markup: &str) -> SharedString {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    markup.hash(&mut hasher);
    let path = format!("{DYNAMIC_PREFIX}{:016x}.svg", hasher.finish());

    if let Ok(mut dynamic) = DYNAMIC.lock() {
        dynamic
            .entry(path.clone())
            .or_insert_with(|| Arc::from(markup.as_bytes()));
    }

    path.into()
}

/// 应用的资源源。创建 `Application` 时用 `.with_assets(kwikpaste_ui::Assets)` 装上，
/// 否则组件和卡片里的图标是空的。
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with(PREFIX) {
            return Ok(EMBEDDED
                .iter()
                .find(|(embedded, _)| *embedded == path)
                .map(|(_, bytes)| Cow::Borrowed(*bytes)));
        }
        if path.starts_with(DYNAMIC_PREFIX) {
            let bytes = DYNAMIC
                .lock()
                .ok()
                .and_then(|dynamic| dynamic.get(path).cloned());
            return Ok(bytes.map(|bytes| Cow::Owned(bytes.to_vec())));
        }

        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit_assets::Assets.list(path)?;
        paths.extend(
            EMBEDDED
                .iter()
                .filter(|(embedded, _)| embedded.starts_with(path))
                .map(|(embedded, _)| SharedString::from(*embedded)),
        );

        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icons_load_and_parse_as_svg() {
        for (path, _) in EMBEDDED {
            let bytes = Assets
                .load(path)
                .ok()
                .flatten()
                .expect("embedded icon loads");
            let text = std::str::from_utf8(&bytes).expect("svg is utf-8");
            assert!(text.starts_with("<svg"), "{path} is an svg");
        }
    }

    #[test]
    fn kit_icons_still_load() {
        let bytes = Assets.load("icons/check.svg").ok().flatten();
        assert!(bytes.is_some_and(|bytes| !bytes.is_empty()));
    }

    #[test]
    fn group_icons_resolve_presets_and_custom_svg() {
        assert_eq!(
            group_icon_path("i-lets-icons:book"),
            SharedString::from("kp-icons/lets-icons-book.svg")
        );
        assert_eq!(
            group_icon_path("i-lets-icons:not-a-preset"),
            SharedString::from(DEFAULT_GROUP_ICON)
        );
        assert_eq!(group_icon_path(""), SharedString::from(DEFAULT_GROUP_ICON));

        let markup = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1"/></svg>"#;
        let path = group_icon_path(markup);
        assert!(path.starts_with(DYNAMIC_PREFIX));
        assert_eq!(group_icon_path(markup), path, "same markup, same path");
        let bytes = Assets
            .load(&path)
            .ok()
            .flatten()
            .expect("registered svg loads");
        assert_eq!(&*bytes, markup.as_bytes());
    }
}
