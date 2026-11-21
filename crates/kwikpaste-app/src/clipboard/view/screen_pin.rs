//! 贴到屏幕：把一张图片钉成置顶的无边框小窗，可以同时钉多张。
//!
//! - 打开时以光标为中心，按原图 1:1 的设备像素显示（截图贴回去和原来一样大），放不下就缩到光标所在
//!   显示器的工作区里；
//! - 窗口从不激活，点它、拖它都不抢目标应用的前台和输入法；层级在普通窗口之上，面板、预览显示时
//!   压在它上面；
//! - 按住任意处拖动移动，滚轮以指针为锚点缩放（不超过工作区，最小边 [`MIN_SIDE`]），双击关闭；
//!   悬停时右上角出现缩放比例（点了回到原始大小）、复制和关闭；
//! - 窗口可能只有几十像素，放不下窗口内的右键菜单，原生菜单又要抢前台，所以不做右键菜单。
//!
//! 几何都用「屏幕单位」：Windows 是物理像素，macOS 是 point（y 向下）。原生移动、缩放在任务体里、
//! GPUI 借用之外进行。

use std::{path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use futures::future::BoxFuture;
use gpui::{
    App, AppContext as _, AsyncApp, Bounds, Context, ImageSource, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Render, RenderImage, ScrollDelta,
    ScrollWheelEvent, StatefulInteractiveElement as _, Styled as _, Task, Window, WindowBounds,
    WindowKind, WindowOptions, div, img, point, prelude::FluentBuilder as _, px, size,
};
use kwikpaste_os::geometry::{Point, Rect, Size, follow_cursor};
use kwikpaste_ui::{
    Button, IconName,
    theme::{self, radius, space},
};

use super::{host::ItemHost, image_cache::decode_rotated, list::error_toast};
use crate::{
    clipboard::{
        model::viewer::{decode_size, decoded_fits},
        source::{ClipboardSource, ImageSave},
    },
    i18n::t,
};

/// 缩放停下这么久再按新尺寸重解码。
const SETTLE: Duration = Duration::from_millis(150);
/// 图片短边至少这么多逻辑像素，比它还小的原图打开时就放大到这里：Windows 给窗口加了最小高度
/// （175% 下内容区约 52 物理像素），更小会被系统拉变形。
const MIN_SIDE: f64 = 40.;
/// 最大放大倍数（设备像素 / 原图像素）。
const MAX_ZOOM: f64 = 8.;
/// 悬停按钮条要求的最小窗口（逻辑像素）；更小的窗口只靠双击关闭。
const BAR_ROOM: (f32, f32) = (132., 44.);
/// 复制成功后对勾显示多久。
const COPIED_FOR: Duration = Duration::from_secs(1);

/// 光标和它所在显示器的几何。
#[derive(Clone, Copy, Debug)]
pub struct Spot {
    pub cursor: Point,
    pub work_area: Rect,
    /// 屏幕单位 / 逻辑像素：Windows 是 DPI 缩放，macOS 是 1。
    pub scale: f64,
    /// 设备像素 / 逻辑像素。
    pub device: f64,
}

impl Spot {
    /// 一个设备像素等于几个屏幕单位。
    fn unit(&self) -> f64 {
        self.scale / self.device
    }
}

/// 把这条图片记录钉到屏幕上。失败只记日志：入口只在原图还在时出现。
pub fn open(source: Arc<dyn ClipboardSource>, host: Rc<dyn ItemHost>, id: Arc<str>, cx: &mut App) {
    let save = source.image_save(id.clone());
    cx.spawn(async move |cx| {
        if let Err(err) = open_pin(save, host, id, cx).await {
            log::warn!("could not pin the image to the screen: {err:#}");
        }
    })
    .detach();
}

async fn open_pin(
    save: BoxFuture<'static, anyhow::Result<ImageSave>>,
    host: Rc<dyn ItemHost>,
    id: Arc<str>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let path = save.await?.source;
    let spot = native::spot().ok_or_else(|| anyhow::anyhow!("no screen to pin the image on"))?;
    let decoding = path.clone();
    let (pixels, bitmap) = cx
        .background_executor()
        .spawn(async move {
            let pixels = kwikpaste_core::imaging::open(&decoding)?.into_dimensions()?;
            let zoom = initial_zoom(pixels, &spot);
            let needed = decode_size(pixels, zoom as f32);
            let (image, _) = decode_rotated(&decoding, needed.0, needed.1, 0)?;
            anyhow::Ok((
                pixels,
                Bitmap {
                    image: Arc::new(image),
                    decoded: needed,
                },
            ))
        })
        .await?;

    let zoom = initial_zoom(pixels, &spot);
    let rect = centered_at(spot.cursor, spot.work_area, frame_size(pixels, zoom, &spot));
    let logical = size(
        px((f64::from(rect.width()) / spot.scale) as f32),
        px((f64::from(rect.height()) / spot.scale) as f32),
    );
    let native = cx.update(|cx| -> anyhow::Result<Rc<native::NativePin>> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                logical,
            ))),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_resizable: false,
            is_minimizable: false,
            inactive_frame_interval: None,
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            ..Default::default()
        };
        let mut built = None;
        kwikpaste_ui::open_window(options, cx, |window, cx| {
            let native = native::NativePin::attach(window).map(Rc::new);
            let kept = native.as_ref().ok().cloned();
            built = Some(native);
            crate::platform::reveal_after_first_frame(window, cx, |_, _| {});
            cx.new(|_| ScreenPin {
                id,
                host,
                path,
                pixels,
                bitmap,
                native: kept,
                pending: None,
                wanted: None,
                decoding: None,
                hovered: false,
                copied: false,
                copied_reset: None,
                #[cfg(target_os = "windows")]
                drag: None,
            })
        })?;
        built.ok_or_else(|| anyhow::anyhow!("the pin window was not built"))?
    })?;
    native.show(rect, &spot);
    Ok(())
}

/// 已解码的位图。
struct Bitmap {
    image: Arc<RenderImage>,
    decoded: (u32, u32),
}

struct ScreenPin {
    id: Arc<str>,
    host: Rc<dyn ItemHost>,
    path: PathBuf,
    /// 原图像素尺寸。
    pixels: (u32, u32),
    bitmap: Bitmap,
    native: Option<Rc<native::NativePin>>,
    /// 还没交给原生窗口的目标矩形：连续的拖动、滚轮只留最新的一个。
    pending: Option<Rect>,
    /// 正在等着重解码的尺寸。
    wanted: Option<(u32, u32)>,
    decoding: Option<Task<()>>,
    hovered: bool,
    copied: bool,
    copied_reset: Option<Task<()>>,
    /// 按下左键时的光标和窗口矩形（Windows 自己移动窗口；macOS 交给系统拖动）。
    #[cfg(target_os = "windows")]
    drag: Option<(Point, Rect)>,
}

impl ScreenPin {
    /// 当前（或即将生效）的窗口矩形。
    fn rect(&self) -> Option<Rect> {
        self.pending
            .or_else(|| self.native.as_ref().and_then(|native| native.rect()))
    }

    /// 在任务里把窗口放到 `rect`。
    fn move_to(&mut self, rect: Rect, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.replace(rect).is_some() {
            return;
        }
        cx.spawn_in(window, async move |pin, cx| {
            let Ok((Some(native), Some(rect))) =
                pin.update(cx, |pin, _| (pin.native.clone(), pin.pending.take()))
            else {
                return;
            };
            native.set_rect(rect);
        })
        .detach();
    }

    /// 以光标为锚点缩放到 `zoom`（设备像素 / 原图像素），夹在允许的范围里。
    fn zoom_to(
        &mut self,
        zoom: impl FnOnce(f64) -> f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(rect), Some(spot)) = (self.rect(), native::spot()) else {
            return;
        };
        let current = f64::from(rect.width()) / (f64::from(self.pixels.0.max(1)) * spot.unit());
        let (min, max) = zoom_limits(self.pixels, &spot);
        let size = frame_size(self.pixels, zoom(current).max(min).min(max), &spot);
        if size != rect.size() {
            self.move_to(zoomed(rect, spot.cursor, size), window, cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        // 与大图预览相同的手感：滚轮一格（Windows 默认 3 行）约 1.26 倍。
        let factor = match event.delta {
            ScrollDelta::Lines(delta) => 1.08_f64.powf(f64::from(delta.y)),
            ScrollDelta::Pixels(delta) => (f64::from(delta.y.as_f32()) / 200.).exp(),
        };
        if factor != 1. {
            self.zoom_to(|current| current * factor, window, cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            window.remove_window();
            return;
        }
        #[cfg(target_os = "macos")]
        window.start_window_move();
        #[cfg(target_os = "windows")]
        {
            self.drag = native::spot()
                .zip(self.rect())
                .map(|(spot, rect)| (spot.cursor, rect));
        }
    }

    /// Windows：GPUI 按下时已经 `SetCapture`，指针移出窗口也收得到移动。
    #[cfg(target_os = "windows")]
    fn on_mouse_move(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start, from)) = self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let (Some(spot), Some(current)) = (native::spot(), self.rect()) else {
            return;
        };
        // 尺寸取当前的：拖过 DPI 不同的显示器时系统已按新 DPI 改过大小。
        let origin = Point {
            x: from.left + spot.cursor.x - start.x,
            y: from.top + spot.cursor.y - start.y,
        };
        let rect = Rect::from_origin_size(origin, current.size());
        if rect != current {
            self.move_to(rect, window, cx);
        }
    }

    fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.host.copy(self.id.clone(), false, None, cx);
        cx.spawn_in(window, async move |pin, cx| {
            let result = task.await;
            pin.update_in(cx, |pin, window, cx| match result {
                Ok(()) => {
                    pin.copied = true;
                    pin.copied_reset = Some(cx.spawn_in(window, async move |pin, cx| {
                        cx.background_executor().timer(COPIED_FOR).await;
                        pin.update(cx, |pin, cx| {
                            pin.copied = false;
                            cx.notify();
                        })
                        .ok();
                    }));
                    cx.notify();
                }
                Err(err) => error_toast("commands:labels.copy", &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 显示尺寸变了：等缩放停下再按新尺寸解码，失败时留着旧位图。
    fn redecode(&mut self, needed: (u32, u32), window: &mut Window, cx: &mut Context<Self>) {
        self.wanted = Some(needed);
        let path = self.path.clone();
        self.decoding = Some(cx.spawn_in(window, async move |pin, cx| {
            cx.background_executor().timer(SETTLE).await;
            let result = cx
                .background_executor()
                .spawn(async move { decode_rotated(&path, needed.0, needed.1, 0) })
                .await;
            pin.update_in(cx, |pin, window, cx| match result {
                Ok((image, _)) => {
                    let old = std::mem::replace(
                        &mut pin.bitmap,
                        Bitmap {
                            image: Arc::new(image),
                            decoded: needed,
                        },
                    );
                    cx.drop_image(old.image, Some(window));
                    cx.notify();
                }
                Err(err) => log::warn!("pinned image could not be decoded again: {err:#}"),
            })
            .ok();
        }));
    }

    fn toolbar(&self, zoom: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let reset = cx.entity().downgrade();
        let copy = reset.clone();
        div()
            .absolute()
            .top(space(1.))
            .right(space(1.))
            .flex()
            .items_center()
            .gap(space(0.5))
            .p(space(0.5))
            .rounded(radius::MD)
            .bg(tokens.surface.overlay)
            .shadow(tokens.shadow.float())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("pin-zoom", format!("{:.0}%", zoom * 100.))
                    .ghost()
                    .small()
                    .tooltip(t("preview:pin.resetZoom"))
                    .on_click(move |_, window, cx| {
                        reset
                            .update(cx, |pin, cx| pin.zoom_to(|_| 1., window, cx))
                            .ok();
                    }),
            )
            .child(
                Button::icon(
                    "pin-copy",
                    if self.copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    },
                    t("preview:pin.copy"),
                )
                .small()
                .on_click(move |_, window, cx| {
                    copy.update(cx, |pin, cx| pin.copy(window, cx)).ok();
                }),
            )
            .child(
                Button::icon("pin-close", IconName::Close, t("preview:pin.close"))
                    .small()
                    .on_click(|_, window, _| window.remove_window()),
            )
    }
}

impl Render for ScreenPin {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let zoom = viewport.width.as_f32() * window.scale_factor() / self.pixels.0.max(1) as f32;
        let needed = decode_size(self.pixels, zoom);
        if !decoded_fits(self.bitmap.decoded, needed) && self.wanted != Some(needed) {
            self.redecode(needed, window, cx);
        }

        let tokens = theme::semantic(cx);
        let bar = self.hovered
            && viewport.width.as_f32() >= BAR_ROOM.0
            && viewport.height.as_f32() >= BAR_ROOM.1;
        let pin = div()
            .id("screen-pin")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(tokens.surface.window)
            .text_color(tokens.text.primary)
            .on_hover(cx.listener(|pin, hovered: &bool, _, cx| {
                pin.hovered = *hovered;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll));
        #[cfg(target_os = "windows")]
        let pin = pin
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|pin, _, _, _| pin.drag = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|pin, _, _, _| pin.drag = None),
            );

        pin.child(img(ImageSource::Render(self.bitmap.image.clone())).size_full())
            // 细描边盖在图片边缘上，不占位置：1:1 时位图正好对齐设备像素。
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .border_1()
                    .border_color(tokens.border.divider),
            )
            .when(bar, |pin| pin.child(self.toolbar(zoom, cx)))
    }
}

/// 允许的缩放范围（设备像素 / 原图像素）：短边不小于 [`MIN_SIDE`]，整张不超过工作区，
/// 最多 [`MAX_ZOOM`] 倍（小图为了够到最小边可以超过它）。
fn zoom_limits((width, height): (u32, u32), spot: &Spot) -> (f64, f64) {
    let width = f64::from(width.max(1)) * spot.unit();
    let height = f64::from(height.max(1)) * spot.unit();
    let fit = (f64::from(spot.work_area.width().max(1)) / width)
        .min(f64::from(spot.work_area.height().max(1)) / height);
    let min = (MIN_SIDE * spot.scale / width.min(height)).min(fit);
    (min, MAX_ZOOM.max(min).min(fit))
}

/// 打开时的倍数：原图 1:1，放不下就缩到工作区里，太小就放大到最小边。
fn initial_zoom(pixels: (u32, u32), spot: &Spot) -> f64 {
    let (min, max) = zoom_limits(pixels, spot);
    1_f64.max(min).min(max)
}

/// `zoom` 倍时的窗口尺寸（屏幕单位）。
fn frame_size((width, height): (u32, u32), zoom: f64, spot: &Spot) -> Size {
    let side = |pixels: u32| ((f64::from(pixels) * zoom * spot.unit()).round() as i32).max(1);
    Size {
        width: side(width),
        height: side(height),
    }
}

/// 以 `cursor` 为中心放一个 `size` 的窗口，夹回工作区。
fn centered_at(cursor: Point, work_area: Rect, size: Size) -> Rect {
    let corner = Point {
        x: cursor.x - size.width / 2,
        y: cursor.y - size.height / 2,
    };
    follow_cursor(corner, work_area, size)
}

/// 把 `rect` 缩放到 `size`，`anchor` 在窗口里的相对位置不变。
fn zoomed(rect: Rect, anchor: Point, size: Size) -> Rect {
    let along = |start: i32, extent: i32, anchor: i32, next: i32| {
        let offset = i64::from(anchor - start) * i64::from(next) / i64::from(extent.max(1));
        anchor - offset as i32
    };
    Rect::from_origin_size(
        Point {
            x: along(rect.left, rect.width(), anchor.x, size.width),
            y: along(rect.top, rect.height(), anchor.y, size.height),
        },
        size,
    )
}

/// Windows：GPUI 的 `PopUp` 已是 topmost 工具窗口，补上 `WS_EX_NOACTIVATE`，几何都是物理像素。
#[cfg(target_os = "windows")]
mod native {
    use anyhow::{anyhow, bail};
    use gpui::Window;
    use kwikpaste_os::geometry::Rect;
    use kwikpaste_os::win::{monitor, panel::Panel};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::Spot;

    pub fn spot() -> Option<Spot> {
        let info = monitor::at_cursor()
            .or_else(|_| monitor::primary())
            .map_err(|err| log::warn!("no monitor for the pinned image: {err}"))
            .ok()?;
        Some(Spot {
            cursor: info.cursor,
            work_area: info.work_area,
            scale: info.scale(),
            device: info.scale(),
        })
    }

    pub struct NativePin {
        window: Panel,
    }

    impl NativePin {
        pub fn attach(window: &Window) -> anyhow::Result<Self> {
            let handle = HasWindowHandle::window_handle(window)
                .map_err(|err| anyhow!("pin window handle: {err:?}"))?;
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                bail!("the pin window is not a Win32 window");
            };
            // GPUI 在主线程建窗；窗口关掉后句柄失效，之后的调用只会失败并记日志。
            Ok(Self {
                window: unsafe { Panel::from_raw(handle.hwnd.get()) },
            })
        }

        pub fn rect(&self) -> Option<Rect> {
            self.window.client_rect().ok()
        }

        pub fn set_rect(&self, rect: Rect) {
            if let Err(err) = self.window.place(rect, self.window.dpi()) {
                log::warn!("pinned image window could not be moved: {err}");
            }
        }

        pub fn show(&self, rect: Rect, spot: &Spot) {
            self.window.set_activatable(false);
            let dpi = (spot.scale * f64::from(monitor::BASE_DPI)).round() as u32;
            if let Err(err) = self.window.place(rect, dpi) {
                log::warn!("pinned image window could not be placed: {err}");
            }
            self.window.show_without_activating();
        }
    }
}

/// macOS：GPUI 的 `PopUp` 已是不激活的 NSPanel，改成浮动层级；几何是 y 向下的 point。
#[cfg(target_os = "macos")]
mod native {
    use anyhow::{anyhow, bail};
    use gpui::Window;
    use kwikpaste_os::geometry::Rect;
    use kwikpaste_os::mac::panel::{self as mac_panel, Panel};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::Spot;

    pub fn spot() -> Option<Spot> {
        let (cursor, work_area, device) = mac_panel::cursor_screen()
            .map_err(|err| log::warn!("no screen for the pinned image: {err}"))
            .ok()?;
        Some(Spot {
            cursor,
            work_area,
            scale: 1.,
            device,
        })
    }

    pub struct NativePin {
        window: Panel,
    }

    impl NativePin {
        pub fn attach(window: &Window) -> anyhow::Result<Self> {
            let handle = HasWindowHandle::window_handle(window)
                .map_err(|err| anyhow!("pin window handle: {err:?}"))?;
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                bail!("the pin window is not an AppKit window");
            };
            Ok(Self {
                window: unsafe { Panel::from_raw(handle.ns_view) },
            })
        }

        pub fn rect(&self) -> Option<Rect> {
            self.window.flipped_frame()
        }

        pub fn set_rect(&self, rect: Rect) {
            if let Err(err) = self.window.set_flipped_frame(rect) {
                log::warn!("pinned image window could not be moved: {err}");
            }
        }

        pub fn show(&self, rect: Rect, _: &Spot) {
            if let Err(err) = self.window.install_pin() {
                log::warn!("pinned image window setup failed: {err}");
            }
            self.set_rect(rect);
            self.window.order_front_without_key();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };

    fn spot(scale: f64, device: f64) -> Spot {
        Spot {
            cursor: Point { x: 960, y: 520 },
            work_area: WORK,
            scale,
            device,
        }
    }

    #[test]
    fn opens_one_to_one_in_device_pixels_and_shrinks_to_the_work_area() {
        // Windows 175%：800×600 的截图就占 800×600 物理像素。
        let windows = spot(1.75, 1.75);
        assert_eq!(
            frame_size((800, 600), initial_zoom((800, 600), &windows), &windows),
            Size {
                width: 800,
                height: 600
            }
        );
        // macOS Retina：1600×1200 像素是 800×600 point。
        let mac = spot(1., 2.);
        assert_eq!(
            frame_size((1600, 1200), initial_zoom((1600, 1200), &mac), &mac),
            Size {
                width: 800,
                height: 600
            }
        );
        // 比工作区大：等比缩到放得下。
        assert_eq!(
            frame_size((3840, 2080), initial_zoom((3840, 2080), &windows), &windows),
            Size {
                width: 1920,
                height: 1040
            }
        );
    }

    #[test]
    fn zoom_keeps_a_minimum_side_and_tiny_images_open_at_it() {
        let (min, max) = zoom_limits((400, 100), &spot(1., 1.));
        assert!((min - 0.4).abs() < 1e-9);
        // 放大到和工作区一样宽为止。
        assert!((max - 4.8).abs() < 1e-9);
        assert_eq!(zoom_limits((100, 50), &spot(1., 1.)).1, MAX_ZOOM);
        // 12×12 的图在 175% 下打开成 70×70 物理像素（40 逻辑像素）。
        let windows = spot(1.75, 1.75);
        assert_eq!(
            frame_size((12, 12), initial_zoom((12, 12), &windows), &windows),
            Size {
                width: 70,
                height: 70
            }
        );
    }

    #[test]
    fn centered_window_stays_inside_the_work_area() {
        let size = Size {
            width: 400,
            height: 300,
        };
        let rect = centered_at(Point { x: 960, y: 520 }, WORK, size);
        assert_eq!((rect.left, rect.top), (760, 370));
        let corner = centered_at(Point { x: 10, y: 1030 }, WORK, size);
        assert_eq!((corner.left, corner.bottom), (0, 1040));
    }

    #[test]
    fn zoom_keeps_the_anchor_in_place() {
        let rect = Rect::from_origin_size(
            Point { x: 100, y: 100 },
            Size {
                width: 200,
                height: 100,
            },
        );
        let grown = zoomed(
            rect,
            Point { x: 150, y: 150 },
            Size {
                width: 400,
                height: 200,
            },
        );
        assert_eq!((grown.left, grown.top, grown.width()), (50, 50, 400));
    }
}
