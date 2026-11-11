//! Windows 面板胶水：从 GPUI 窗口取 HWND，几何计算和原生调用交给 `kwikpaste_os::win`。

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use gpui::Window;
use kwikpaste_core::settings::{ListLayout, WindowPosition};
use kwikpaste_core::window_state::WindowGeometry;
use kwikpaste_os::geometry::{Point, Rect, Size, center_in, follow_cursor, scale_size};
use kwikpaste_os::win::monitor::{self, BASE_DPI, MonitorInfo};
use kwikpaste_os::win::panel::{self as win_panel, PanelOptions};
use kwikpaste_os::win::{self as os, keyboard, mouse};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::editing::{EditReport, EditTrigger};
use super::panel::{PanelCommand, panel_default_size, panel_min_size};
use super::window_state::{PanelLayout, shift_layout_width};

/// 钩子确认吞掉带标记的 Alt 的最长等待（实测约 1.5 ms）。
const MARKED_ALT_TIMEOUT: Duration = Duration::from_millis(50);

/// 一次显示算出并写入的几何。
pub struct Placement {
    pub client: Rect,
    pub outer: Rect,
    pub dpi: u32,
}

pub struct NativePanel {
    panel: win_panel::Panel,
    /// 本次运行里上次隐藏时的几何；比存档新，存档在后台写盘。
    last: Cell<Option<WindowGeometry>>,
    /// 系统「文本大小」系数。
    text_scale: Cell<f64>,
    layout: Cell<ListLayout>,
    /// 编辑态：进入前的前台窗口；`None` 表示不在编辑态。
    editing: Cell<Option<isize>>,
    /// 拖动区、缩放边上的按下要收起弹出菜单，经它发给面板任务。
    commands: RefCell<Option<async_channel::Sender<PanelCommand>>>,
}

impl NativePanel {
    pub fn set_command_sender(&self, sender: async_channel::Sender<PanelCommand>) {
        *self.commands.borrow_mut() = Some(sender);
    }

    pub fn attach(window: &Window, text_scale: f64, layout: ListLayout) -> anyhow::Result<Self> {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|err| anyhow!("panel window handle: {err:?}"))?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            bail!("the panel is not a Win32 window");
        };

        Ok(Self {
            // GPUI 在主线程创建窗口，面板永不销毁。
            panel: unsafe { win_panel::Panel::from_raw(handle.hwnd.get()) },
            last: Cell::new(None),
            text_scale: Cell::new(text_scale),
            layout: Cell::new(layout),
            editing: Cell::new(None),
            commands: RefCell::new(None),
        })
    }

    pub fn install(&self) -> anyhow::Result<()> {
        self.panel.install(PanelOptions {
            min_logical_size: panel_min_size(self.layout.get()),
            text_scale: self.text_scale.get(),
        })?;
        if let Some(sender) = self.commands.borrow().clone() {
            self.panel.set_drag_sink(Box::new(move || {
                let _ = sender.try_send(PanelCommand::DismissPopup);
            }));
        }
        Ok(())
    }

    pub fn is_visible(&self) -> bool {
        self.panel.is_visible()
    }

    pub fn raw_handle(&self) -> isize {
        self.panel.raw()
    }

    /// 文本大小变了：最小尺寸立即按新系数算，默认尺寸下次显示时补偿。
    pub fn set_text_scale(&self, text_scale: f64) {
        self.text_scale.set(text_scale);
        self.panel.set_text_scale(text_scale);
    }

    /// 切换布局：先更新原生最小尺寸，再移动记住的宽度；可见时就地调整，不重新跟随光标。
    pub fn set_layout(
        &self,
        layout: ListLayout,
        saved: Option<WindowGeometry>,
    ) -> Option<WindowGeometry> {
        let old = self.layout.replace(layout);
        if old == layout {
            return None;
        }
        self.panel.set_min_logical_size(panel_min_size(layout));
        let visible = self.is_visible();
        let geometry = if visible {
            self.geometry()
        } else {
            self.last.get().or(saved)
        }?;
        let mut geometry = shift_layout_width(geometry, old, layout, self.text_scale.get());
        self.last.set(Some(geometry));
        if visible {
            match self.panel.resize_content((geometry.width, geometry.height)) {
                Ok(()) => {
                    geometry = self.geometry().unwrap_or(geometry);
                    self.last.set(Some(geometry));
                }
                Err(err) => log::warn!("panel layout size could not be applied: {err}"),
            }
        }
        Some(geometry)
    }

    /// 按设置的摆放方式定位：跟随光标（光标所在显示器、光标附近）、居中（光标所在显示器的工作区正中）
    /// 或回到记住的位置（落在最近的显示器上并夹回工作区）。取不到光标时用主显示器。
    pub fn place(&self, layout: &PanelLayout) -> anyhow::Result<Placement> {
        let cursor_monitor = monitor::at_cursor()
            .or_else(|err| {
                log::warn!("cursor monitor unavailable ({err}); using the primary monitor");
                monitor::primary()
            })
            .context("no monitor to show the panel on")?;
        let saved = self.last.get().or(layout.saved);
        let remembered = match layout.position {
            WindowPosition::Remember => saved.and_then(|geometry| self.remembered_origin(geometry)),
            WindowPosition::FollowCursor | WindowPosition::Center => None,
        };

        let (monitor, client) = match (layout.position, remembered) {
            (WindowPosition::FollowCursor, _) => {
                let size = self.target_size(saved, &cursor_monitor);
                let client = follow_cursor(cursor_monitor.cursor, cursor_monitor.work_area, size);
                (cursor_monitor, client)
            }
            (_, Some((monitor, origin))) => {
                let size = self.target_size(saved, &monitor);
                (monitor, follow_cursor(origin, monitor.work_area, size))
            }
            (WindowPosition::Center | WindowPosition::Remember, None) => {
                let size = self.target_size(saved, &cursor_monitor);
                (cursor_monitor, center_in(cursor_monitor.work_area, size))
            }
        };
        let outer = self.panel.place(client, monitor.dpi)?;

        Ok(Placement {
            client,
            outer,
            dpi: monitor.dpi,
        })
    }

    /// 内容区尺寸：没有存档用布局的默认尺寸，有存档就沿用，但不小于最小尺寸（都乘文本缩放）。
    fn target_size(&self, saved: Option<WindowGeometry>, monitor: &MonitorInfo) -> Size {
        let text_scale = self.text_scale.get();
        let layout = self.layout.get();
        let default = panel_default_size(layout);
        let min = panel_min_size(layout);
        let logical = saved.map_or(
            (default.0 * text_scale, default.1 * text_scale),
            |geometry| {
                (
                    geometry.width.max(min.0 * text_scale),
                    geometry.height.max(min.1 * text_scale),
                )
            },
        );

        scale_size(logical, monitor.scale())
    }

    /// 离记住的外框左上角最近的显示器和对应的内容区左上角；拖到屏幕上沿、左沿后外框会出屏，
    /// 落点交给 `follow_cursor` 夹回工作区。没有任何显示器时为 `None`。
    fn remembered_origin(&self, geometry: WindowGeometry) -> Option<(MonitorInfo, Point)> {
        let outer = Point {
            x: (geometry.x * geometry.scale).round() as i32,
            y: (geometry.y * geometry.scale).round() as i32,
        };
        let monitor = monitor::all().into_iter().min_by_key(|info| {
            let rect = info.monitor;
            let dx = i64::from((rect.left - outer.x).max(outer.x - (rect.right - 1)).max(0));
            let dy = i64::from((rect.top - outer.y).max(outer.y - (rect.bottom - 1)).max(0));
            dx * dx + dy * dy
        })?;
        let insets = self
            .panel
            .client_rect()
            .and_then(|client| Ok(client.insets_within(self.panel.window_rect()?)))
            .ok()?;

        Some((
            monitor,
            Point {
                x: outer.x + insets.left,
                y: outer.y + insets.top,
            },
        ))
    }

    pub fn show(&self) {
        self.panel.show_without_activating();
    }

    /// 面板显示后装上键盘、鼠标钩子：非编辑态的按键、窗外点击隐藏。
    pub fn start_hooks(&self) {
        if let Err(err) = keyboard::start_with_target(self.panel.raw(), os::foreground_window()) {
            log::error!("keyboard hook is unavailable: {err}");
        }
        if let Err(err) = mouse::start_outside_click() {
            log::error!("mouse hook is unavailable: {err}");
        }
    }

    pub fn stop_hooks(&self) {
        keyboard::stop();
        mouse::stop_outside_click();
    }

    /// 唤起输入落在已显示、但按键已还给目标应用的面板上：应拉回按键而不是隐藏。
    pub fn should_recapture_on_toggle(&self, visible: bool, summon: bool) -> bool {
        keyboard::toggle_requires_recapture(visible, keyboard::is_captured(), summon)
    }

    /// 按键交给面板（目标记为当前前台窗口）或还给目标应用。
    pub fn set_input_capture(&self, captured: bool) {
        if captured {
            keyboard::capture(os::foreground_window());
        } else {
            keyboard::release();
        }
    }

    /// 显示后外框必须等于写入的矩形；不等（例如 DPI 变化改了尺寸）就记错误并重放一次。
    pub fn verify(&self, placement: &Placement) {
        match self.panel.window_rect() {
            Ok(rect) if rect == placement.outer => {}
            Ok(rect) => {
                log::error!(
                    "panel rect {rect:?} differs from the target {:?}; placing it again",
                    placement.outer
                );
                if let Err(err) = self.panel.place(placement.client, placement.dpi) {
                    log::error!("panel could not be placed again: {err}");
                }
            }
            Err(err) => log::warn!("panel rect is unreadable after show: {err}"),
        }
    }

    pub fn raise(&self) {
        if let Err(err) = self.panel.raise() {
            log::warn!("panel could not be raised: {err}");
        }
    }

    /// 隐藏面板，返回隐藏前的几何（外框左上角与内容区尺寸，逻辑像素），供存档；与上次相同时为 `None`。
    pub fn hide(&self) -> Option<WindowGeometry> {
        let geometry = self.geometry();
        let changed = geometry != self.last.get();
        if geometry.is_some() {
            self.last.set(geometry);
        }
        self.panel.hide();
        geometry.filter(|_| changed)
    }

    fn geometry(&self) -> Option<WindowGeometry> {
        let outer = self.panel.window_rect().ok()?;
        let client = self.panel.client_rect().ok()?;
        let scale = f64::from(self.panel.dpi()) / f64::from(BASE_DPI);

        Some(WindowGeometry {
            x: f64::from(outer.left) / scale,
            y: f64::from(outer.top) / scale,
            width: f64::from(client.width()) / scale,
            height: f64::from(client.height()) / scale,
            scale,
        })
    }

    pub fn is_editing(&self) -> bool {
        self.editing.get().is_some()
    }

    /// 进入编辑态，见 [`super::editing`]。失败时已回滚到非编辑态。
    pub fn begin_editing(&self, trigger: EditTrigger) -> anyhow::Result<EditReport> {
        let started = Instant::now();
        let hwnd = self.panel.raw();
        let previous = os::foreground_window();

        keyboard::set_navigation(false);
        self.panel.set_activatable(true);

        let marked_alt_swallowed = match trigger {
            EditTrigger::Mouse => false,
            EditTrigger::Keyboard => {
                if !keyboard::swallow_marked_alt(MARKED_ALT_TIMEOUT) {
                    self.roll_back_editing();
                    bail!("the keyboard hook did not confirm the marked Alt");
                }
                true
            }
        };
        if !os::set_foreground(hwnd) {
            self.roll_back_editing();
            bail!(
                "Windows refused to make the panel the foreground window (foreground is 0x{:X})",
                os::foreground_window()
            );
        }

        self.editing
            .set(Some(if previous == hwnd { 0 } else { previous }));
        Ok(EditReport {
            previous_foreground: previous,
            marked_alt_swallowed,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    fn roll_back_editing(&self) {
        self.panel.set_activatable(false);
        keyboard::set_navigation(true);
    }

    /// 退出编辑态。`restore_foreground` 为真且面板仍是前台时，把前台还给进入前的窗口。
    pub fn end_editing(&self, restore_foreground: bool) {
        let Some(previous) = self.editing.take() else {
            return;
        };
        let foreground_is_panel = os::foreground_window() == self.panel.raw();

        self.panel.set_activatable(false);
        keyboard::set_navigation(true);
        keyboard::capture(previous);
        if restore_foreground
            && foreground_is_panel
            && os::is_window(previous)
            && !os::set_foreground(previous)
        {
            log::debug!("Windows refused to give the foreground back to 0x{previous:X}");
        }
    }

    /// 自测探针用的原生状态（JSON 对象的字段片段，以逗号开头）。
    pub fn probe_fields(&self, placement: Option<&Placement>) -> String {
        let counters = win_panel::counters();
        let mut fields = format!(
            r#","hwnd":{},"visible":{},"foreground":{},"editing":{},"non_activating":{},"dpi":{},"text_scale":{},"activations":{},"phantom_activations":{},"mouse_activate_replies":{:?},"mouse_activate_overrides":{}"#,
            self.panel.raw(),
            self.panel.is_visible(),
            os::foreground_window(),
            self.is_editing(),
            self.panel.is_non_activating(),
            self.panel.dpi(),
            self.text_scale.get(),
            counters.activations,
            counters.phantom_activations,
            counters.mouse_activate_replies,
            counters.mouse_activate_overrides,
        );
        if let Ok(rect) = self.panel.window_rect() {
            fields.push_str(&format!(r#","window_rect":{}"#, json_rect(rect)));
        }
        if let Ok(rect) = self.panel.client_rect() {
            fields.push_str(&format!(r#","client_rect":{}"#, json_rect(rect)));
        }
        if let Some(placement) = placement {
            fields.push_str(&format!(
                r#","target_client":{},"target_outer":{},"target_dpi":{}"#,
                json_rect(placement.client),
                json_rect(placement.outer),
                placement.dpi,
            ));
        }

        fields
    }
}

fn json_rect(rect: Rect) -> String {
    format!(
        "[{},{},{},{}]",
        rect.left, rect.top, rect.right, rect.bottom
    )
}
