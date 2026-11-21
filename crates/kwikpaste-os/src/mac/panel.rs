//! macOS NSPanel 配置、定位和非激活显示。

use std::cell::RefCell;
use std::ffi::c_void;
use std::io;
use std::ptr::NonNull;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use kwikpaste_core::settings::Material;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSEvent,
    NSEventMask, NSRunningApplication, NSScreen, NSView, NSWindow, NSWindowButton,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::geometry::{Point, Rect, Size, follow_cursor};
use kwikpaste_core::window_state::WindowGeometry;

/// 材质（透明）面板的圆角，与 1.x 面板相同。
const MATERIAL_CORNER_RADIUS: f64 = 16.;

/// 切成不进 Dock、也不出现在 Cmd-Tab 中的辅助应用策略。
pub fn set_dock_icon_visible(visible: bool) -> io::Result<()> {
    let main_thread = main_thread()?;
    let app = NSApplication::sharedApplication(main_thread);
    let policy = if visible {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    if !app.setActivationPolicy(policy) {
        return Err(io::Error::other("NSApp refused the activation policy"));
    }
    Ok(())
}

/// GPUI 创建的 NSView 对应的非激活 NSPanel。
pub struct Panel {
    view: NonNull<c_void>,
    outside_monitor: RefCell<Option<Retained<AnyObject>>>,
    previous_foreground: RefCell<Option<Retained<NSRunningApplication>>>,
}

impl Panel {
    /// 包装 GPUI 窗口的 `NSView`。
    ///
    /// # Safety
    /// `ns_view` 必须仍然存活，并且所有调用都在主线程执行。
    pub unsafe fn from_raw(ns_view: NonNull<c_void>) -> Self {
        Self {
            view: ns_view,
            outside_monitor: RefCell::new(None),
            previous_foreground: RefCell::new(None),
        }
    }

    fn window(&self) -> Option<Retained<NSWindow>> {
        let view = unsafe { self.view.cast::<NSView>().as_ref() };
        view.window()
    }

    pub fn raw_view_handle(&self) -> isize {
        self.view.as_ptr() as isize
    }

    /// 补上 GPUI WindowKind::PopUp 缺少的 NSPanel 约束。
    pub fn install(&self, min_size: Size) -> io::Result<()> {
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        let mask = window.styleMask() | NSWindowStyleMask::NonactivatingPanel;
        window.setStyleMask(mask | NSWindowStyleMask::Resizable);
        hide_window_buttons(&window);
        window.setLevel(20);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        window.setContentSize(NSSize::new(min_size.width as f64, min_size.height as f64));
        self.set_min_content_size((min_size.width as f64, min_size.height as f64))?;
        self.schedule_unregister_dragged_types();
        Ok(())
    }

    /// 更新内容区最小尺寸；屏幕较小时，扣除标题栏后限制到可见工作区。
    pub fn set_min_content_size(&self, size: (f64, f64)) -> io::Result<()> {
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        let screen = window
            .screen()
            .ok_or_else(|| io::Error::other("the panel has no screen"))?;
        let available = window.contentRectForFrameRect(screen.visibleFrame()).size;
        window.setContentMinSize(NSSize::new(
            size.0.min(available.width).max(1.),
            size.1.min(available.height).max(1.),
        ));
        Ok(())
    }

    /// 显示前按存档恢复内容区尺寸，位置随后由摆放规则决定。
    pub fn set_content_size(&self, size: (f64, f64)) -> io::Result<()> {
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        window.setContentSize(NSSize::new(size.0.max(1.), size.1.max(1.)));
        Ok(())
    }

    /// 保留外框左上角调整内容区大小，必要时夹回当前屏幕工作区；不激活窗口。
    pub fn resize_content(&self, size: (f64, f64)) -> io::Result<()> {
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        let screen = window
            .screen()
            .ok_or_else(|| io::Error::other("the panel has no screen"))?;
        let work = screen.visibleFrame();
        let old = window.frame();
        let mut frame = window.frameRectForContentRect(NSRect::new(
            NSPoint::new(0., 0.),
            NSSize::new(size.0.max(1.), size.1.max(1.)),
        ));
        frame.size.width = frame.size.width.min(work.size.width).max(1.);
        frame.size.height = frame.size.height.min(work.size.height).max(1.);
        frame.origin.x = old
            .origin
            .x
            .min(work.origin.x + work.size.width - frame.size.width)
            .max(work.origin.x);
        frame.origin.y = (old.origin.y + old.size.height - frame.size.height)
            .min(work.origin.y + work.size.height - frame.size.height)
            .max(work.origin.y);
        window.setFrame_display(frame, true);
        Ok(())
    }

    /// GPUI 建窗时可能暂时注册拖放类型；独立的下一轮主队列 turn 清掉它，避免自拖回面板触发投放。
    pub fn schedule_unregister_dragged_types(&self) {
        let view = self.view.as_ptr();
        unsafe {
            DispatchQueue::main().exec_async_f(view, unregister_dragged_types);
        }
    }

    /// 在窗口下放置 AppKit 材质层（[`crate::mac::material`]）；默认材质移除它并恢复不透明窗口。
    ///
    /// 不透明窗口由系统按窗口自己的圆角裁切，内容层不再另加圆角：系统圆角比 16 小，
    /// 两道圆角之间会露出 GPUI 给不透明窗口铺的黑底。材质窗口是透明的，圆角由内容层给。
    pub fn set_material(&self, material: Material) -> io::Result<()> {
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        let content = window
            .contentView()
            .ok_or_else(|| io::Error::other("the panel window has no content view"))?;
        let view = unsafe { self.view.cast::<NSView>().as_ref() };
        match crate::mac::material::apply_to_view(view, material)? {
            None => {
                set_corner_radius(&content, 0.);
                window.setOpaque(true);
            }
            Some(effect) => {
                set_corner_radius(&content, MATERIAL_CORNER_RADIUS);
                window.setOpaque(false);
                set_corner_radius(&effect, MATERIAL_CORNER_RADIUS);
            }
        }
        Ok(())
    }

    /// 配置预览 NSPanel：不允许调整大小，并使用 Status level。
    pub fn install_preview(&self, min_size: Size) -> io::Result<()> {
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the preview view has no window"))?;
        window.setStyleMask(window.styleMask() | NSWindowStyleMask::NonactivatingPanel);
        hide_window_buttons(&window);
        window.setLevel(25);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        window.setContentSize(NSSize::new(min_size.width as f64, min_size.height as f64));
        Ok(())
    }

    /// 配置贴图窗：浮动层级（普通窗口之上、面板和预览之下），应用失活时也不隐藏。
    pub fn install_pin(&self) -> io::Result<()> {
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the pin view has no window"))?;
        hide_window_buttons(&window);
        window.setLevel(3);
        window.setHidesOnDeactivate(false);
        Ok(())
    }

    /// 窗口外框，换成以主屏左上角为原点、向下为正的 point（见 [`cursor_screen`]）。
    pub fn flipped_frame(&self) -> Option<Rect> {
        self.window().map(|window| flip(window.frame()))
    }

    /// 把外框放到 `rect`（[`Panel::flipped_frame`] 的坐标），不激活。
    pub fn set_flipped_frame(&self, rect: Rect) -> io::Result<()> {
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        window.setFrame_display(
            NSRect::new(
                NSPoint::new(f64::from(rect.left), -f64::from(rect.bottom)),
                NSSize::new(
                    f64::from(rect.width().max(1)),
                    f64::from(rect.height().max(1)),
                ),
            ),
            true,
        );
        Ok(())
    }

    /// 放到最前但不激活应用、也不成为 key window。
    pub fn order_front_without_key(&self) {
        if let Some(window) = self.window() {
            window.orderFrontRegardless();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.window().is_some_and(|window| window.isVisible())
    }

    pub fn is_key(&self) -> bool {
        self.window().is_some_and(|window| window.isKeyWindow())
    }

    pub fn is_panel(&self) -> bool {
        self.window()
            .is_some_and(|window| unsafe { msg_send![&*window, isKindOfClass: class!(NSPanel)] })
    }

    pub fn can_become_main(&self) -> bool {
        self.window()
            .is_some_and(|window| window.canBecomeMainWindow())
    }

    pub fn can_become_key(&self) -> bool {
        self.window()
            .is_some_and(|window| window.canBecomeKeyWindow())
    }

    pub fn style_mask(&self) -> Option<NSWindowStyleMask> {
        self.window().map(|window| window.styleMask())
    }

    pub fn collection_behavior(&self) -> Option<NSWindowCollectionBehavior> {
        self.window().map(|window| window.collectionBehavior())
    }

    pub fn frame(&self) -> Option<NSRect> {
        self.window().map(|window| window.frame())
    }

    pub fn backing_scale(&self) -> f64 {
        self.window()
            .and_then(|window| window.screen().map(|screen| screen.backingScaleFactor()))
            .unwrap_or(1.)
    }

    /// 把面板外框定位到鼠标附近，必要时限制到当前屏幕的可见工作区。
    pub fn place_near_cursor(&self) -> io::Result<()> {
        let main_thread = main_thread()?;
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        let mouse = NSEvent::mouseLocation();
        let screen = NSScreen::screens(main_thread)
            .iter()
            .find(|screen| contains(screen.frame(), mouse))
            .or_else(|| NSScreen::mainScreen(main_thread))
            .ok_or_else(|| io::Error::other("no screen to place the panel on"))?;
        let visible = screen.visibleFrame();
        let work_area = Rect {
            left: visible.origin.x.round() as i32,
            top: (-(visible.origin.y + visible.size.height)).round() as i32,
            right: (visible.origin.x + visible.size.width).round() as i32,
            bottom: (-visible.origin.y).round() as i32,
        };
        let frame = window.frame();
        let size = Size {
            width: frame.size.width.round() as i32,
            height: frame.size.height.round() as i32,
        };
        let cursor = Point {
            x: mouse.x.round() as i32,
            y: (-mouse.y).round() as i32,
        };
        let target = follow_cursor(cursor, work_area, size);
        window.setFrameOrigin(NSPoint::new(
            f64::from(target.left),
            f64::from(-target.bottom),
        ));
        Ok(())
    }

    pub fn place_center(&self) -> io::Result<()> {
        let main_thread = main_thread()?;
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        let screen =
            NSScreen::mainScreen(main_thread).ok_or_else(|| io::Error::other("no main screen"))?;
        let visible = screen.visibleFrame();
        let frame = window.frame();
        let x = visible.origin.x + (visible.size.width - frame.size.width) / 2.;
        let y = visible.origin.y + (visible.size.height - frame.size.height) / 2.;
        window.setFrameOrigin(NSPoint::new(x, y));
        Ok(())
    }

    pub fn place_saved(&self, geometry: WindowGeometry) -> io::Result<()> {
        let main_thread = main_thread()?;
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        window.setContentSize(NSSize::new(geometry.width.max(1.), geometry.height.max(1.)));
        let target_y = -geometry.y - window.frame().size.height;
        let on_screen = NSScreen::screens(main_thread)
            .iter()
            .any(|screen| contains(screen.frame(), NSPoint::new(geometry.x, target_y)));
        if !on_screen {
            return Err(io::Error::other(
                "saved panel position is no longer on a screen",
            ));
        }
        window.setFrameOrigin(NSPoint::new(geometry.x, target_y));
        Ok(())
    }

    /// 把窗口放到 `rect`：主屏左上角为原点、向下为正（与 [`crate::mac::monitor::screens`] 相同），
    /// 单位是按 `dpi` 放大过的像素，这里除回 point，再翻成 AppKit 以主屏左下角为原点、向上为正的坐标。
    pub fn place_rect(&self, rect: Rect, dpi: u32) -> io::Result<()> {
        let main_thread = main_thread()?;
        let window = self
            .window()
            .ok_or_else(|| io::Error::other("the panel view has no window"))?;
        let primary_height = NSScreen::screens(main_thread)
            .iter()
            .next()
            .map(|screen| screen.frame().size.height)
            .ok_or_else(|| io::Error::other("no screen to place the window on"))?;
        let scale = f64::from(dpi.max(1)) / 96.;
        window.setContentSize(NSSize::new(
            f64::from(rect.width().max(1)) / scale,
            f64::from(rect.height().max(1)) / scale,
        ));
        window.setFrameOrigin(NSPoint::new(
            f64::from(rect.left) / scale,
            primary_height - f64::from(rect.bottom) / scale,
        ));
        Ok(())
    }

    pub fn geometry(&self) -> Option<WindowGeometry> {
        let window = self.window()?;
        let frame = window.frame();
        let content = window.contentRectForFrameRect(frame);
        Some(WindowGeometry {
            x: frame.origin.x,
            y: -(frame.origin.y + frame.size.height),
            width: content.size.width,
            height: content.size.height,
            scale: window
                .screen()
                .map_or(1., |screen| screen.backingScaleFactor()),
        })
    }

    /// 不激活应用，但让 NSPanel 成为 key window 以接收编辑键盘。
    pub fn show_without_activating(&self) {
        let Some(window) = self.window() else {
            return;
        };
        *self.previous_foreground.borrow_mut() =
            NSWorkspace::sharedWorkspace().frontmostApplication();
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        window.orderFrontRegardless();
        window.makeKeyWindow();
    }

    pub fn hide(&self) {
        let Some(window) = self.window() else {
            return;
        };
        window.orderOut(None);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        self.restore_previous_foreground();
    }

    pub fn restore_previous_foreground(&self) {
        let previous = self.previous_foreground.borrow_mut().take();
        // 刚打开的偏好设置 / 引导窗已经是 key window：随后才处理的面板隐藏不能把前台还给外部应用。
        let own_window_key = MainThreadMarker::new().is_some_and(|marker| {
            NSApplication::sharedApplication(marker)
                .keyWindow()
                .is_some_and(|window| {
                    !window
                        .styleMask()
                        .contains(NSWindowStyleMask::NonactivatingPanel)
                })
        });
        if own_window_key {
            return;
        }
        if let Some(previous) = previous {
            #[allow(deprecated)]
            let _ = previous
                .activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
        }
    }

    pub fn start_global_mouse_monitor(&self, callback: impl Fn() + 'static) -> io::Result<()> {
        self.stop_global_mouse_monitor();
        let Some(window) = self.window() else {
            return Err(io::Error::other("the panel view has no window"));
        };
        // global monitor 本该只收到发给别的应用的按下，但按住面板顶部（系统标题栏区域）拖动时面板会被隐藏，
        // 说明这类按下也会进来；`locationInWindow` 对别的窗口的事件也不是屏幕坐标。
        // 所以按窗口号认出自己的窗口，位置改用屏幕坐标。
        let monitor = RcBlock::new(move |event: NonNull<NSEvent>| {
            if is_own_window(unsafe { event.as_ref() }.windowNumber()) {
                return;
            }
            if !contains(window.frame(), NSEvent::mouseLocation()) {
                callback();
            }
        });
        let mask =
            NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
        let token = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &monitor)
            .ok_or_else(|| io::Error::other("global mouse monitor could not be installed"))?;
        *self.outside_monitor.borrow_mut() = Some(token);
        Ok(())
    }

    pub fn stop_global_mouse_monitor(&self) {
        if let Some(token) = self.outside_monitor.borrow_mut().take() {
            unsafe { NSEvent::removeMonitor(&token) };
        }
    }

    /// 保持可见但放弃 key window：固定面板粘贴时 panel 仍是 key 会吞掉 ⌘V，让出后键焦点回到前台应用
    /// （1.x `resign_clipboard_panel_key`）。之后点一下面板就重新成为 key。
    pub fn resign_key(&self) {
        if let Some(window) = self.window() {
            window.resignKeyWindow();
        }
    }

    pub fn begin_editing(&self) {
        if let Some(window) = self.window() {
            window.makeKeyWindow();
        }
    }

    pub fn activation_policy(&self) -> Option<NSApplicationActivationPolicy> {
        let marker = MainThreadMarker::new()?;
        Some(NSApplication::sharedApplication(marker).activationPolicy())
    }

    pub fn application_is_active(&self) -> bool {
        MainThreadMarker::new()
            .is_some_and(|marker| NSApplication::sharedApplication(marker).isActive())
    }

    pub fn foreground_unchanged(&self) -> Option<bool> {
        let previous = self.previous_foreground.borrow();
        let previous = previous.as_ref()?.processIdentifier();
        let current = NSWorkspace::sharedWorkspace().frontmostApplication();
        Some(current.is_some_and(|application| application.processIdentifier() == previous))
    }
}

/// 光标位置、它所在屏幕的可见工作区和这块屏幕的 backing scale。坐标是以主屏左上角为原点、
/// 向下为正的 point（AppKit 的 y 取反）。
pub fn cursor_screen() -> io::Result<(Point, Rect, f64)> {
    let main_thread = main_thread()?;
    let mouse = NSEvent::mouseLocation();
    let screen = NSScreen::screens(main_thread)
        .iter()
        .find(|screen| contains(screen.frame(), mouse))
        .or_else(|| NSScreen::mainScreen(main_thread))
        .ok_or_else(|| io::Error::other("no screen under the cursor"))?;
    let cursor = Point {
        x: mouse.x.round() as i32,
        y: (-mouse.y).round() as i32,
    };
    Ok((
        cursor,
        flip(screen.visibleFrame()),
        screen.backingScaleFactor(),
    ))
}

/// AppKit 矩形（左下角原点、向上为正）换成向下为正的整数 point。
fn flip(rect: NSRect) -> Rect {
    Rect {
        left: rect.origin.x.round() as i32,
        top: (-(rect.origin.y + rect.size.height)).round() as i32,
        right: (rect.origin.x + rect.size.width).round() as i32,
        bottom: (-rect.origin.y).round() as i32,
    }
}

fn main_thread() -> io::Result<MainThreadMarker> {
    MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("AppKit calls must run on the main thread"))
}

fn set_corner_radius(view: &NSView, radius: f64) {
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setCornerRadius(radius);
    }
}

/// GPUI 的无标题栏窗口仍是 Titled 窗口（能成为 key、有系统阴影和圆角），AppKit 照样放红绿灯，
/// 面板加了 Resizable 后缩放按钮还是可点的：三个按钮都藏起来。
fn hide_window_buttons(window: &NSWindow) {
    for kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = window.standardWindowButton(kind) {
            button.setHidden(true);
        }
    }
}

/// 窗口号是不是本应用的窗口（面板、预览窗、偏好设置等）。
fn is_own_window(number: isize) -> bool {
    number > 0
        && MainThreadMarker::new().is_some_and(|marker| {
            NSApplication::sharedApplication(marker)
                .windowWithWindowNumber(number)
                .is_some()
        })
}

fn contains(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.x < rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y < rect.origin.y + rect.size.height
}

extern "C" fn unregister_dragged_types(context: *mut c_void) {
    if context.is_null() {
        return;
    }
    let view = unsafe { &*(context.cast::<NSView>()) };
    view.unregisterDraggedTypes();
    crate::mac::drag_out::record_unregistered();
}
