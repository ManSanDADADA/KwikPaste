//! 托盘：tray-icon + muda（1.x 经 Tauri 用的同一系 crate），在主线程创建，由 GPUI 的消息循环派发。
//!
//! 与 1.x 相同：菜单是「偏好设置」「退出应用」，文案取 `kwikpaste_core::i18n::tray`，跟随
//! `appearance.language`；显隐跟随 `general.trayIcon`；Windows 左键单击按 `general.trayClick`
//! 打开窗口，macOS 左键弹菜单。事件经专用线程阻塞 `recv()` 转进 `async_channel`，
//! 线程里只转发、不碰 GPUI。
//!
//! 「偏好设置」和 `trayClick = preference` 发 [`HostRequest::OpenPreferences`]，由 UI 打开偏好窗
//! （见 [`super::host`]；UI 还没注册时回退为唤起面板）。

use async_channel::Sender;
use gpui::{App, AsyncApp, Global};
use kwikpaste_core::i18n::tray::{self as tray_i18n, Key};
use kwikpaste_core::settings::{Settings, TrayClick};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::host::{self, HostRequest, RequestSource};
use super::panel::{PanelCommand, Trigger, TriggerSource};
use crate::core_host;

const TRAY_ID: &str = "app-tray";
const MENU_PREFERENCE: &str = "tray::preference";
const MENU_EXIT: &str = "tray::exit";
const ICON: &[u8] = include_bytes!("../../assets/tray.ico");

/// 持有托盘图标：退出前丢弃，任务栏上不留残影。
struct Tray {
    icon: Option<TrayIcon>,
}

impl Global for Tray {}

enum TrayAction {
    #[cfg_attr(
        target_os = "macos",
        expect(dead_code, reason = "macOS 左键弹菜单，不单独处理左键")
    )]
    LeftClick,
    Preference,
    Exit,
}

/// 创建托盘图标和菜单，并启动事件桥。
pub fn create(cx: &mut App, commands: Sender<PanelCommand>) -> anyhow::Result<()> {
    let settings = core_host::core(cx)
        .map(|core| core.settings())
        .unwrap_or_default();
    #[cfg(target_os = "windows")]
    kwikpaste_os::win::menu_theme::allow_dark_menus();
    let icon = TrayIconBuilder::new()
        .with_id(TRAY_ID)
        .with_icon(load_icon()?)
        .with_icon_as_template(cfg!(target_os = "macos"))
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .with_tooltip(crate::identity::display_name())
        .with_menu(Box::new(build_menu(&settings)?))
        .build()?;
    if let Err(err) = icon.set_visible(settings.general.tray_icon) {
        log::warn!("tray icon visibility could not be set: {err}");
    }

    cx.set_global(Tray { icon: Some(icon) });
    cx.on_app_quit(|cx| {
        remove(cx);
        async {}
    })
    .detach();

    let (actions, receiver) = async_channel::unbounded();
    #[cfg(target_os = "windows")]
    bridge_left_click(actions.clone())?;
    std::thread::Builder::new()
        .name("tray-menu-bridge".to_owned())
        .spawn(move || {
            let events = MenuEvent::receiver();
            while let Ok(event) = events.recv() {
                let action = match event.id.as_ref() {
                    MENU_PREFERENCE => TrayAction::Preference,
                    MENU_EXIT => TrayAction::Exit,
                    _ => continue,
                };
                let ticks = kwikpaste_os::clock::now_ticks();
                super::paste_coordinator::observe_control(ticks);
                if actions.send_blocking((action, ticks)).is_err() {
                    break;
                }
            }
        })?;

    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok((action, ticks)) = receiver.recv().await {
            if !matches!(action, TrayAction::Exit)
                && cx.update(|cx| super::paste::control_is_stale(cx, ticks))
            {
                continue;
            }
            match action {
                TrayAction::Exit => cx.update(|cx| cx.quit()),
                TrayAction::Preference => cx.update(|cx| {
                    host::dispatch_observed(
                        cx,
                        HostRequest::OpenPreferences {
                            source: RequestSource::TrayMenu,
                        },
                        ticks,
                    );
                }),
                TrayAction::LeftClick => {
                    let click = cx.update(|cx| {
                        core_host::core(cx).map(|core| core.settings().general.tray_click)
                    });
                    if click == Some(TrayClick::Preference) {
                        cx.update(|cx| {
                            host::dispatch_observed(
                                cx,
                                HostRequest::OpenPreferences {
                                    source: RequestSource::TrayClick,
                                },
                                ticks,
                            );
                        });
                        continue;
                    }
                    let trigger = Trigger {
                        source: TriggerSource::Tray,
                        ticks,
                    };
                    let _ = commands
                        .send(PanelCommand::Show(trigger).observed_at(ticks))
                        .await;
                }
            }
        }
    })
    .detach();

    Ok(())
}

/// 删掉托盘图标：退出前、更新交接时调用，任务栏上不留残影。
pub fn remove(cx: &mut App) {
    if cx.has_global::<Tray>() {
        cx.global_mut::<Tray>().icon = None;
    }
}

/// 设置变了：按语言重建菜单，按 `general.trayIcon` 显隐。
pub fn apply(settings: &Settings, cx: &mut App) {
    let Some(icon) = cx.try_global::<Tray>().and_then(|tray| tray.icon.as_ref()) else {
        return;
    };
    match build_menu(settings) {
        Ok(menu) => icon.set_menu(Some(Box::new(menu))),
        Err(err) => log::warn!("tray menu could not be rebuilt: {err}"),
    }
    if let Err(err) = icon.set_visible(settings.general.tray_icon) {
        log::warn!("tray icon visibility could not be set: {err}");
    }
}

fn build_menu(settings: &Settings) -> anyhow::Result<Menu> {
    let language = settings.appearance.language;
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id(
            MENU_PREFERENCE,
            tray_i18n::label(language, Key::Preference),
            true,
            None,
        ),
        &MenuItem::with_id(MENU_EXIT, tray_i18n::label(language, Key::Exit), true, None),
    ])?;

    Ok(menu)
}

/// Windows 左键单击托盘（松开）；macOS 左键弹菜单，不走这里。
#[cfg(target_os = "windows")]
fn bridge_left_click(actions: Sender<(TrayAction, i64)>) -> std::io::Result<()> {
    use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

    std::thread::Builder::new()
        .name("tray-bridge".to_owned())
        .spawn(move || {
            let events = TrayIconEvent::receiver();
            while let Ok(event) = events.recv() {
                let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                else {
                    continue;
                };
                let ticks = kwikpaste_os::clock::now_ticks();
                super::paste_coordinator::observe_control(ticks);
                if actions
                    .send_blocking((TrayAction::LeftClick, ticks))
                    .is_err()
                {
                    break;
                }
            }
        })?;

    Ok(())
}

fn load_icon() -> anyhow::Result<Icon> {
    let image = image::load_from_memory_with_format(ICON, image::ImageFormat::Ico)?.into_rgba8();
    let (width, height) = image.dimensions();

    Ok(Icon::from_rgba(image.into_raw(), width, height)?)
}
