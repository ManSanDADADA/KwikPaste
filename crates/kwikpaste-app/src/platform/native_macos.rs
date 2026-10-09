//! macOS 面板：从 GPUI 窗口取得 NSPanel，并负责 AppKit 状态同步。

use std::cell::{Cell, RefCell};
use std::time::Instant;

use anyhow::{Context as _, anyhow, bail};
use async_channel::Sender;
use gpui::Window;
use kwikpaste_core::settings::Material;
use kwikpaste_core::settings::WindowPosition;
use kwikpaste_core::window_state::WindowGeometry;
use kwikpaste_os::geometry::Size;
use kwikpaste_os::mac::panel as mac_panel;
use kwikpaste_os::paste_target::PasteTarget;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::editing::{EditReport, EditTrigger};
use super::panel::{PanelCommand, Trigger, TriggerSource};
use super::window_state::PanelLayout;

/// macOS 上原生定位已经直接写入 NSWindow，供 GPUI 层记录一次放置结果。
pub struct Placement;

pub struct NativePanel {
    panel: mac_panel::Panel,
    editing: Cell<bool>,
    commands: RefCell<Option<Sender<PanelCommand>>>,
}

impl NativePanel {
    pub fn attach(window: &Window, _text_scale: f64) -> anyhow::Result<Self> {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|err| anyhow!("panel window handle: {err:?}"))?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            bail!("the panel is not an AppKit window");
        };

        Ok(Self {
            // GPUI 在主线程创建窗口，面板永久不销毁。
            panel: unsafe { mac_panel::Panel::from_raw(handle.ns_view) },
            editing: Cell::new(false),
            commands: RefCell::new(None),
        })
    }

    pub fn set_command_sender(&self, sender: Sender<PanelCommand>) {
        *self.commands.borrow_mut() = Some(sender);
    }

    pub fn install(&self) -> anyhow::Result<()> {
        self.panel
            .install(Size {
                width: super::panel::PANEL_SIZE.0 as i32,
                height: super::panel::PANEL_SIZE.1 as i32,
            })
            .context("macOS panel setup")?;
        Ok(())
    }

    pub fn set_material(&self, material: Material) -> anyhow::Result<()> {
        self.panel
            .set_material(material)
            .context("macOS window material")?;
        Ok(())
    }

    pub fn is_visible(&self) -> bool {
        self.panel.is_visible()
    }

    pub fn raw_handle(&self) -> isize {
        self.panel.raw_view_handle()
    }

    pub fn set_text_scale(&self, _text_scale: f64) {}

    /// 按设置的 FollowCursor、Center 或 Remember 规则定位面板。
    pub fn place(&self, layout: &PanelLayout) -> anyhow::Result<Placement> {
        match layout.position {
            WindowPosition::FollowCursor => self.panel.place_near_cursor()?,
            WindowPosition::Center => self.panel.place_center()?,
            WindowPosition::Remember => {
                if let Some(saved) = layout.saved {
                    if self.panel.place_saved(saved).is_err() {
                        self.panel.place_center()?;
                    }
                } else {
                    self.panel.place_center()?;
                }
            }
        }
        Ok(Placement)
    }

    pub fn show(&self) {
        self.panel.show_without_activating();
    }

    pub fn start_hooks(&self) {
        let Some(sender) = self.commands.borrow().clone() else {
            log::warn!("macOS outside-click hook has no panel command channel");
            return;
        };
        let inside_sender = sender.clone();
        if let Err(err) = self.panel.start_global_mouse_monitor(
            move || {
                let _ = sender.try_send(
                    PanelCommand::Hide(Trigger::now(TriggerSource::OutsideClick)).observed(),
                );
            },
            move || {
                let _ = inside_sender.try_send(PanelCommand::SetInputCapture(true).observed());
            },
        ) {
            log::error!("global mouse monitor is unavailable: {err}");
        }
    }

    pub fn stop_hooks(&self) {
        self.panel.stop_global_mouse_monitor();
    }

    /// macOS 由 NSPanel 的 key window 分发按键，没有 Windows 的按键捕获。
    pub fn should_recapture_on_toggle(&self, visible: bool, summon: bool) -> bool {
        visible && summon && !self.panel.is_key()
    }

    pub fn set_input_capture(&self, captured: bool) {
        if captured {
            self.panel.begin_editing();
        } else if let Err(err) = self.panel.release_input_capture() {
            log::warn!("macOS panel keyboard handoff: {err}");
        }
    }

    pub fn begin_paste_handoff(&self) -> anyhow::Result<PasteTarget> {
        self.panel
            .begin_paste_handoff()
            .context("macOS paste target")
    }

    pub fn paste_handoff_ready(&self, target: PasteTarget) -> anyhow::Result<bool> {
        self.panel
            .paste_handoff_ready(target)
            .context("macOS paste handoff")
    }

    pub fn cancel_paste_handoff(&self) {
        self.panel.cancel_paste_handoff();
    }

    pub fn cancel_paste_handoff_if(&self, target: PasteTarget) {
        self.panel.cancel_paste_handoff_if(target);
    }

    /// Mac readiness only reads identity/focus; false means keyboard release is still pending.
    pub fn validate_paste_handoff(&self, target: PasteTarget) -> anyhow::Result<()> {
        self.panel
            .paste_handoff_ready(target)
            .map(|_| ())
            .context("macOS paste target")
    }

    pub fn verify(&self, _: &Placement) {}

    pub fn raise(&self) {
        self.panel.show_without_activating();
    }

    pub fn hide(&self) -> Option<WindowGeometry> {
        let geometry = self.panel.geometry();
        self.panel.hide();
        geometry
    }

    pub fn is_editing(&self) -> bool {
        self.editing.get()
    }

    pub fn begin_editing(&self, _trigger: EditTrigger) -> anyhow::Result<EditReport> {
        let started = Instant::now();
        self.panel.begin_editing();
        self.editing.set(true);
        Ok(EditReport {
            previous_foreground: 0,
            marked_alt_swallowed: false,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
        })
    }

    pub fn end_editing(&self, restore_foreground: bool) {
        if !self.editing.replace(false) {
            return;
        }
        if restore_foreground && let Err(err) = self.panel.release_input_capture() {
            log::warn!("macOS editing keyboard handoff: {err}");
        }
    }

    /// 自测探针用的 NSPanel 不变量字段。
    pub fn probe_fields(&self, _: Option<&Placement>) -> String {
        let style = self.panel.style_mask();
        let behavior = self.panel.collection_behavior();
        format!(
            r#",visible":{},"key":{},"editing":{},"is_ns_panel":{},"foreground_unchanged":{},"non_activating":{},"can_become_main":{},"can_become_key":{},"style_mask":{},"collection_behavior":{},"activation_policy":"{}""#,
            self.panel.is_visible(),
            self.panel.is_key(),
            self.is_editing(),
            self.panel.is_panel(),
            self.panel.foreground_unchanged().unwrap_or(false),
            style.is_some_and(
                |value| value.contains(objc2_app_kit::NSWindowStyleMask::NonactivatingPanel)
            ),
            self.panel.can_become_main(),
            self.panel.can_become_key(),
            style.map_or(0, |value| value.bits()),
            behavior.map_or(0, |value| value.bits()),
            self.panel
                .activation_policy()
                .map_or("unknown", |value| match value {
                    objc2_app_kit::NSApplicationActivationPolicy::Regular => "regular",
                    objc2_app_kit::NSApplicationActivationPolicy::Accessory => "accessory",
                    objc2_app_kit::NSApplicationActivationPolicy::Prohibited => "prohibited",
                    _ => "unknown",
                }),
        )
    }

    pub fn check_invariants(&self) -> anyhow::Result<()> {
        let style = self.panel.style_mask().context("panel has no style mask")?;
        anyhow::ensure!(self.panel.is_panel(), "panel is not an NSPanel");
        anyhow::ensure!(
            self.panel.foreground_unchanged().unwrap_or(true),
            "showing panel changed the frontmost application"
        );
        anyhow::ensure!(
            style.contains(objc2_app_kit::NSWindowStyleMask::NonactivatingPanel),
            "panel is missing NSNonactivatingPanel"
        );
        anyhow::ensure!(
            !self.panel.can_become_main(),
            "panel can become main window"
        );
        anyhow::ensure!(
            self.panel.can_become_key(),
            "panel cannot become key window"
        );
        anyhow::ensure!(
            self.panel.activation_policy()
                == Some(objc2_app_kit::NSApplicationActivationPolicy::Accessory),
            "application activation policy is not accessory"
        );
        Ok(())
    }
}
