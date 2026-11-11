//! macOS 面板：从 GPUI 窗口取得 NSPanel，并负责 AppKit 状态同步。

use std::cell::{Cell, RefCell};
use std::time::Instant;

use anyhow::{Context as _, anyhow, bail};
use async_channel::Sender;
use gpui::Window;
use kwikpaste_core::settings::{ListLayout, Material, WindowPosition};
use kwikpaste_core::window_state::WindowGeometry;
use kwikpaste_os::geometry::Size;
use kwikpaste_os::mac::panel as mac_panel;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::editing::{EditReport, EditTrigger};
use super::panel::{PanelCommand, Trigger, TriggerSource, panel_default_size, panel_min_size};
use super::window_state::{PanelLayout, shift_layout_width};

/// macOS 上原生定位已经直接写入 NSWindow，供 GPUI 层记录一次放置结果。
pub struct Placement;

pub struct NativePanel {
    panel: mac_panel::Panel,
    last: Cell<Option<WindowGeometry>>,
    layout: Cell<ListLayout>,
    editing: Cell<bool>,
    commands: RefCell<Option<Sender<PanelCommand>>>,
}

impl NativePanel {
    pub fn attach(window: &Window, _text_scale: f64, layout: ListLayout) -> anyhow::Result<Self> {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|err| anyhow!("panel window handle: {err:?}"))?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            bail!("the panel is not an AppKit window");
        };

        Ok(Self {
            // GPUI 在主线程创建窗口，面板永久不销毁。
            panel: unsafe { mac_panel::Panel::from_raw(handle.ns_view) },
            last: Cell::new(None),
            layout: Cell::new(layout),
            editing: Cell::new(false),
            commands: RefCell::new(None),
        })
    }

    pub fn set_command_sender(&self, sender: Sender<PanelCommand>) {
        *self.commands.borrow_mut() = Some(sender);
    }

    pub fn install(&self) -> anyhow::Result<()> {
        let min = panel_min_size(self.layout.get());
        self.panel
            .install(Size {
                width: min.0 as i32,
                height: min.1 as i32,
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

    /// 切换布局：立即更新内容区最小尺寸与记住的宽度，可见时通过 AppKit 就地调整外框。
    pub fn set_layout(
        &self,
        layout: ListLayout,
        saved: Option<WindowGeometry>,
    ) -> Option<WindowGeometry> {
        let old = self.layout.replace(layout);
        if old == layout {
            return None;
        }
        let min = panel_min_size(layout);
        if let Err(err) = self.panel.set_min_content_size((min.0, min.1)) {
            log::warn!("macOS panel minimum size could not be updated: {err}");
        }
        let visible = self.is_visible();
        let geometry = if visible {
            self.panel.geometry()
        } else {
            self.last.get().or(saved)
        }?;
        let mut geometry = shift_layout_width(geometry, old, layout, 1.);
        self.last.set(Some(geometry));
        if visible {
            match self.panel.resize_content((geometry.width, geometry.height)) {
                Ok(()) => {
                    geometry = self.panel.geometry().unwrap_or(geometry);
                    self.last.set(Some(geometry));
                }
                Err(err) => log::warn!("macOS panel layout size could not be applied: {err}"),
            }
        }
        Some(geometry)
    }

    /// 按设置的 FollowCursor、Center 或 Remember 规则定位面板。
    pub fn place(&self, layout: &PanelLayout) -> anyhow::Result<Placement> {
        let min = panel_min_size(self.layout.get());
        let saved = self.last.get().or(layout.saved).map(|mut geometry| {
            geometry.width = geometry.width.max(min.0);
            geometry.height = geometry.height.max(min.1);
            geometry
        });
        let size = saved.map_or(panel_default_size(self.layout.get()), |geometry| {
            (geometry.width, geometry.height)
        });
        self.panel.set_content_size(size)?;
        match layout.position {
            WindowPosition::FollowCursor => self.panel.place_near_cursor()?,
            WindowPosition::Center => self.panel.place_center()?,
            WindowPosition::Remember => {
                if let Some(saved) = saved {
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
        if let Err(err) = self.panel.start_global_mouse_monitor(move || {
            let _ = sender.try_send(PanelCommand::Hide(Trigger::now(
                TriggerSource::OutsideClick,
            )));
        }) {
            log::error!("global mouse monitor is unavailable: {err}");
        }
    }

    pub fn stop_hooks(&self) {
        self.panel.stop_global_mouse_monitor();
    }

    /// macOS 由 NSPanel 的 key window 分发按键，没有 Windows 的按键捕获。
    pub fn should_recapture_on_toggle(&self, _visible: bool, _summon: bool) -> bool {
        false
    }

    /// 只处理释放：固定面板粘贴、复制后把按键还给目标应用。重新捕获靠点面板时 NSPanel 自己成为 key。
    pub fn set_input_capture(&self, captured: bool) {
        if !captured {
            self.panel.resign_key();
        }
    }

    pub fn verify(&self, _: &Placement) {}

    pub fn raise(&self) {
        self.panel.show_without_activating();
    }

    pub fn hide(&self) -> Option<WindowGeometry> {
        let geometry = self.panel.geometry();
        let changed = geometry != self.last.get();
        if geometry.is_some() {
            self.last.set(geometry);
        }
        self.panel.hide();
        geometry.filter(|_| changed)
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
        if restore_foreground {
            self.panel.restore_previous_foreground();
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
