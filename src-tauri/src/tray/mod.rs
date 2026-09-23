//! 系统托盘：Rust 侧实现。
//!
//! - icon 沿用 `assets/tray.ico`（Windows）/ `assets/tray-mac.ico`（macOS），作为 Tauri 资源打包。
//! - 菜单只有「偏好设置」「退出应用」两项，文案跟随 `Appearance.language` 即时切换（见 [`crate::i18n::tray`]）。
//! - 显隐跟随 `General.tray_icon`；语言或显隐变更后由 `commands/settings.rs` 调用 [`apply`] 同步。

use anyhow::Context;
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItem};
use tauri::path::BaseDirectory;
use tauri::tray::TrayIconBuilder;
#[cfg(target_os = "windows")]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::core::Result;
use crate::i18n::tray as tray_i18n;
use crate::i18n::tray::Key;
use crate::settings::{Language, Settings};
#[cfg(target_os = "windows")]
use crate::window::CLIPBOARD_WINDOW_LABEL;
use crate::window::{self, PREFERENCE_WINDOW_LABEL};

const TRAY_ID: &str = "app-tray";

const MENU_PREFERENCE: &str = "tray::preference";
const MENU_EXIT: &str = "tray::exit";

pub fn init(app: &AppHandle, settings: &Settings) -> Result<()> {
    let icon = load_icon(app)?;
    let menu = build_menu(app, settings.appearance.language)?;

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .tooltip("KwikPaste")
        .menu(&menu)
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            // macOS 左键已经走 show_menu_on_left_click，不在这里处理；
            // Windows 左键单击显剪贴板窗口。
            #[cfg(target_os = "windows")]
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle().clone();
                if let Err(err) = window::show_window(&app, CLIPBOARD_WINDOW_LABEL) {
                    log::error!("tray left-click show main failed: {err:?}");
                }
            }
            #[cfg(not(target_os = "windows"))]
            let _ = (tray, event);
        })
        .build(app)
        .context("build tray icon failed")?;

    if let Err(err) = tray.set_visible(settings.general.tray_icon) {
        log::warn!("tray set_visible on init failed: {err}");
    }

    Ok(())
}

/// 根据最新 settings 更新菜单和可见性。语言变了就重建菜单；显隐位变了就 set_visible。
pub fn apply(app: &AppHandle, settings: &Settings) -> Result<()> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let menu = build_menu(app, settings.appearance.language)?;
    tray.set_menu(Some(menu)).context("tray set_menu failed")?;
    tray.set_visible(settings.general.tray_icon)
        .context("tray set_visible failed")?;
    Ok(())
}

fn load_icon(app: &AppHandle) -> Result<Image<'static>> {
    let relative = if cfg!(target_os = "macos") {
        "assets/tray-mac.ico"
    } else {
        "assets/tray.ico"
    };
    let path = app
        .path()
        .resolve(relative, BaseDirectory::Resource)
        .with_context(|| format!("resolve tray icon resource {relative}"))?;
    Ok(Image::from_path(&path).with_context(|| format!("load tray icon from {path:?}"))?)
}

fn build_menu(app: &AppHandle, lang: Language) -> Result<Menu<tauri::Wry>> {
    let preference = MenuItem::with_id(
        app,
        MENU_PREFERENCE,
        tray_i18n::label(lang, Key::Preference),
        true,
        None::<&str>,
    )
    .context("build preference menu item")?;
    let exit = MenuItem::with_id(
        app,
        MENU_EXIT,
        tray_i18n::label(lang, Key::Exit),
        true,
        None::<&str>,
    )
    .context("build exit menu item")?;

    MenuBuilder::new(app)
        .items(&[&preference, &exit])
        .build()
        .context("build tray menu")
        .map_err(Into::into)
}

fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        MENU_PREFERENCE => {
            if let Err(err) = window::show_window(app, PREFERENCE_WINDOW_LABEL) {
                log::error!("tray open preference failed: {err:?}");
            }
        }
        MENU_EXIT => app.exit(0),
        _ => {}
    }
}
