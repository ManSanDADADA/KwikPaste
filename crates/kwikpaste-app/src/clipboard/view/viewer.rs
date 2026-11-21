//! 大图预览窗：与偏好窗一样的普通窗口（带到前台、能最小化最大化），同时只开一个。
//!
//! - 打开时拿列表里已加载的图片记录做快照，←/→ 在快照里换图；
//! - 舞台：滚轮（触控板的精确滚动要按住 Mod）以指针为锚点缩放，按住拖动平移，双击在适应窗口与 100%
//!   之间切换；
//! - 位图按显示尺寸解码（不超过原图、最长边 8192），缩放停下 [`SETTLE`] 后再按需重解码；旋转在解码时
//!   做；换下来的位图立刻从窗口图集里删掉，关窗时随窗口释放。
//! - 文字层：图片有识别文字时，各行按识别出的位置隐形地铺在图上，在文字上按住拖动跨行选字（双击
//!   选一整行），Ctrl+C 复制选中的文字；`T` 标出全部文字区域。还没识别过的图由 core 优先识别。

use std::{ops::Range, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, CursorStyle, Entity, FocusHandle,
    Global, Hsla, ImageSource, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, PathBuilder, Pixels, Render, RenderImage,
    ScrollDelta, ScrollWheelEvent, Styled as _, Subscription, Task, TextRun, Window, WindowBounds,
    WindowOptions, canvas, div, fill, font, img, point, prelude::FluentBuilder as _, px, size,
};
use kwikpaste_core::{CoreEvent, clipboard::ClipboardFragment, ocr::ImageTextLine};
use kwikpaste_ui::{
    Button, Icon, IconName, KpStyled as _, TITLE_BAR_HEIGHT, TitleBar,
    theme::{self, TextSize, fonts, radius, space},
    toast::{self, Toast},
};

use super::{
    host::ItemHost,
    image_cache::decode_rotated,
    list::{error_toast, save_image},
};
use crate::{
    clipboard::{
        model::{
            preview::format_bytes,
            shortcut,
            viewer::{self, Camera, decode_size, decoded_fits, rotated},
            viewer_text::{Placement, Point, TextLayer},
        },
        source::ClipboardSource,
    },
    i18n::t,
};

/// 缩放停下这么久再按新倍数重解码。
const SETTLE: Duration = Duration::from_millis(150);
/// 透明图下面垫的棋盘格边长。
const CHECKER: f32 = 12.;
/// 窗口初始大小的上下限（逻辑像素）。
const MIN_WINDOW: (f32, f32) = (560., 420.);
const MAX_WINDOW: (f32, f32) = (1280., 860.);
/// 量识别行里字符位置用的字号：只取比例，具体多大无所谓。
const MEASURE_FONT_SIZE: f32 = 16.;
/// 复制图片 / 选中文字的失败动作名与成功提示。
const COPY_LABELS: (&str, &str) = ("commands:labels.copy", "commands:messages.copied");

/// 快照里的一张图：列表载荷里的原图尺寸只用来定窗口初始大小，真实尺寸读文件头。
#[derive(Clone, Debug)]
pub struct ViewerImage {
    pub id: Arc<str>,
    pub size: Option<(u32, u32)>,
}

struct ViewerWindow {
    handle: AnyWindowHandle,
    view: Entity<ImageViewer>,
}

impl Global for ViewerWindow {}

/// 打开大图预览；已经开着就换内容并带到前台。`images` 不能为空。
pub fn open(
    source: Arc<dyn ClipboardSource>,
    host: Rc<dyn ItemHost>,
    images: Vec<ViewerImage>,
    index: usize,
    cx: &mut App,
) -> anyhow::Result<()> {
    anyhow::ensure!(index < images.len(), "no image to view");
    if let Some((handle, view)) = cx
        .try_global::<ViewerWindow>()
        .map(|viewer| (viewer.handle, viewer.view.clone()))
    {
        return handle.update(cx, |_, window, cx| {
            view.update(cx, |viewer, cx| viewer.show(images, index, window, cx));
            crate::preferences::view::bring_window_to_front(window, cx);
        });
    }

    let (width, height) = images
        .get(index)
        .and_then(|image| image.size)
        .map_or((960., 640.), |(width, height)| {
            (width as f32, height as f32)
        });
    let chrome = viewer::FIT_MARGIN * 2.;
    let window_size = size(
        px((width + chrome).clamp(MIN_WINDOW.0, MAX_WINDOW.0)),
        px((height + chrome + TITLE_BAR_HEIGHT.as_f32()).clamp(MIN_WINDOW.1, MAX_WINDOW.1)),
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(window_size, cx)),
        window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
        titlebar: Some(kwikpaste_ui::titlebar_options(t("preview:viewer.title"))),
        focus: false,
        ..Default::default()
    };
    let (handle, view) = crate::platform::open_window(options, cx, |window, cx| {
        let view = cx.new(|cx| ImageViewer::new(source, host, window, cx));
        view.update(cx, |viewer, cx| viewer.show(images, index, window, cx));
        crate::platform::reveal_after_first_frame(window, cx, |window, cx| {
            crate::preferences::view::bring_window_to_front(window, cx);
        });
        view
    })?;
    cx.set_global(ViewerWindow { handle, view });
    let window_id = handle.window_id();
    cx.on_window_closed(move |cx, closed_id| {
        if closed_id == window_id {
            let _ = cx.remove_global::<ViewerWindow>();
        }
    })
    .detach();
    Ok(())
}

/// 当前图的原图文件。
struct Original {
    path: PathBuf,
    pixels: (u32, u32),
    bytes: Option<u64>,
}

/// 已解码的位图。
struct Bitmap {
    image: Arc<RenderImage>,
    /// 转之前的方向上的解码尺寸。
    decoded: (u32, u32),
    turns: u8,
    transparent: bool,
}

/// 按下左键时的指针和图的位置。
struct Drag {
    pointer: (f32, f32),
    from: (f32, f32),
}

pub struct ImageViewer {
    source: Arc<dyn ClipboardSource>,
    host: Rc<dyn ItemHost>,
    images: Vec<ViewerImage>,
    index: usize,
    original: Option<Original>,
    missing: bool,
    turns: u8,
    camera: Camera,
    stage: viewer::Size,
    device: f32,
    bitmap: Option<Bitmap>,
    /// 每换一次图或请求一次解码加一，晚到的结果按它作废。
    generation: u64,
    loading: Option<Task<()>>,
    decoding: Option<Task<()>>,
    settle: Option<Task<()>>,
    drag: Option<Drag>,
    /// 当前图的文字层；没有识别文字（或还没识别好）时为 `None`。
    text: Option<TextLayer>,
    /// 在等 core 识别当前图：收到 `OcrChanged` 再取一次。
    text_pending: bool,
    text_loading: Option<Task<()>>,
    /// 选区的起点和终点（整段识别文字里的字符序号），终点跟着指针走。
    selection: Option<(usize, usize)>,
    /// 正在拖选文字。
    selecting: bool,
    /// 指针停在文字上。
    over_text: bool,
    /// 标出全部文字区域。
    reveal: bool,
    title: String,
    focus: FocusHandle,
    _subscription: Option<Subscription>,
}

impl ImageViewer {
    fn new(
        source: Arc<dyn ClipboardSource>,
        host: Rc<dyn ItemHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let subscription = crate::platform::core_events(cx).map(|events| {
            cx.subscribe_in(
                &events,
                window,
                |viewer, _, event: &CoreEvent, window, cx| {
                    match event {
                        CoreEvent::OcrChanged if viewer.text_pending => {
                            viewer.load_text(window, cx)
                        }
                        // 识别扩展装上、停用或卸载：文字层跟着出现或消失。
                        CoreEvent::ExtensionsChanged => viewer.load_text(window, cx),
                        _ => {}
                    }
                },
            )
        });
        Self {
            source,
            host,
            images: Vec::new(),
            index: 0,
            original: None,
            missing: false,
            turns: 0,
            camera: Camera::fit((1., 1.), (1., 1.)),
            stage: (1., 1.),
            device: window.scale_factor(),
            bitmap: None,
            generation: 0,
            loading: None,
            decoding: None,
            settle: None,
            drag: None,
            text: None,
            text_pending: false,
            text_loading: None,
            selection: None,
            selecting: false,
            over_text: false,
            reveal: false,
            title: String::new(),
            focus,
            _subscription: subscription,
        }
    }

    fn current_id(&self) -> Option<Arc<str>> {
        self.images.get(self.index).map(|image| image.id.clone())
    }

    /// 换成快照里的第 `index` 张：旧位图立刻释放，读完文件头再解码。
    fn show(
        &mut self,
        images: Vec<ViewerImage>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.images = images;
        self.go_to(index, window, cx);
    }

    fn go_to(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.images.get(index).map(|image| image.id.clone()) else {
            return;
        };
        self.index = index;
        self.generation += 1;
        self.original = None;
        self.missing = false;
        self.turns = 0;
        self.drag = None;
        self.decoding = None;
        self.settle = None;
        self.text = None;
        self.selection = None;
        self.selecting = false;
        self.over_text = false;
        self.release_bitmap(window, cx);
        self.load_text(window, cx);

        let generation = self.generation;
        let save = self.source.image_save(id);
        self.loading = Some(cx.spawn_in(window, async move |viewer, cx| {
            let original = match save.await {
                Ok(save) => {
                    cx.background_executor()
                        .spawn(async move {
                            let pixels =
                                kwikpaste_core::imaging::open(&save.source)?.into_dimensions()?;
                            let bytes = std::fs::metadata(&save.source).ok().map(|m| m.len());
                            anyhow::Ok(Original {
                                path: save.source,
                                pixels,
                                bytes,
                            })
                        })
                        .await
                }
                Err(err) => Err(err),
            };
            viewer
                .update_in(cx, |viewer, window, cx| {
                    if viewer.generation != generation {
                        return;
                    }
                    match original {
                        Ok(original) => {
                            viewer.original = Some(original);
                            viewer.camera = Camera::fit(viewer.image_size(), viewer.stage);
                            viewer.decode(window, cx);
                        }
                        Err(err) => {
                            log::warn!("image viewer could not open the original: {err:#}");
                            viewer.missing = true;
                        }
                    }
                    cx.notify();
                })
                .ok();
        }));
        cx.notify();
    }

    /// 取当前图的识别行；还没识别好就等 `OcrChanged`。
    fn load_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.current_id() else {
            return;
        };
        let lines = self.source.image_text_lines(id.clone());
        self.text_loading = Some(cx.spawn_in(window, async move |viewer, cx| {
            let result = lines.await;
            viewer
                .update_in(cx, |viewer, window, cx| {
                    if viewer.current_id().as_ref() != Some(&id) {
                        return;
                    }
                    let (text, pending) = match result {
                        Ok(Some(lines)) => (Some(text_layer(lines, window)), false),
                        Ok(None) => (None, true),
                        Err(err) => {
                            log::warn!("image viewer could not read the recognized text: {err:#}");
                            (None, false)
                        }
                    };
                    viewer.text_pending = pending;
                    if text != viewer.text {
                        viewer.text = text;
                        viewer.selection = None;
                        viewer.selecting = false;
                        cx.notify();
                    }
                })
                .ok();
        }));
    }

    /// 选中的文字区间；没有选区或选区为空时为 `None`。
    fn selected(&self) -> Option<Range<usize>> {
        let (anchor, focus) = self.selection?;
        let range = anchor.min(focus)..anchor.max(focus);
        (!range.is_empty()).then_some(range)
    }

    /// 有识别文字的话返回文字层。
    fn text_layer(&self) -> Option<&TextLayer> {
        self.text.as_ref().filter(|text| !text.is_empty())
    }

    /// 原图归一化坐标在舞台上的位置。
    fn placement(&self) -> Placement {
        let (width, height) = self.image_size();
        Placement {
            origin: (self.camera.x, self.camera.y),
            size: (width * self.camera.scale, height * self.camera.scale),
            turns: self.turns,
        }
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        if let Some(all) = self.text_layer().map(TextLayer::all) {
            self.selection = Some((all.start, all.end));
            cx.notify();
        }
    }

    fn toggle_reveal(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.text_layer().is_some() {
            self.reveal = !self.reveal;
            cx.notify();
        }
    }

    fn release_bitmap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bitmap) = self.bitmap.take() {
            cx.drop_image(bitmap.image, Some(window));
        }
    }

    /// 当前方向上的图片尺寸（逻辑像素）；原图还没读到时为 1×1。
    fn image_size(&self) -> viewer::Size {
        let Some(original) = &self.original else {
            return (1., 1.);
        };
        let (width, height) = original.pixels;
        rotated(
            (width as f32 / self.device, height as f32 / self.device),
            self.turns,
        )
    }

    /// 现有位图不合当前倍数或方向时重新解码。
    fn decode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(original) = &self.original else {
            return;
        };
        let needed = decode_size(original.pixels, self.camera.scale);
        if self.bitmap.as_ref().is_some_and(|bitmap| {
            bitmap.turns == self.turns && decoded_fits(bitmap.decoded, needed)
        }) {
            return;
        }

        self.generation += 1;
        let generation = self.generation;
        let (path, turns) = (original.path.clone(), self.turns);
        let task = cx
            .background_executor()
            .spawn(async move { decode_rotated(&path, needed.0, needed.1, turns) });
        self.decoding = Some(cx.spawn_in(window, async move |viewer, cx| {
            let result = task.await;
            viewer
                .update_in(cx, |viewer, window, cx| {
                    if viewer.generation != generation {
                        return;
                    }
                    match result {
                        Ok((image, transparent)) => {
                            viewer.release_bitmap(window, cx);
                            viewer.bitmap = Some(Bitmap {
                                image: Arc::new(image),
                                decoded: needed,
                                turns,
                                transparent,
                            });
                        }
                        Err(err) => {
                            log::warn!("image viewer could not decode the original: {err:#}");
                            viewer.missing = true;
                        }
                    }
                    cx.notify();
                })
                .ok();
        }));
    }

    /// 倍数变了：等缩放停下再看要不要重解码。
    fn camera_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settle = Some(cx.spawn_in(window, async move |viewer, cx| {
            cx.background_executor().timer(SETTLE).await;
            viewer
                .update_in(cx, |viewer, window, cx| viewer.decode(window, cx))
                .ok();
        }));
        cx.notify();
    }

    /// 舞台尺寸或屏幕缩放变了（改窗口大小、拖到别的屏）。
    fn sync_stage(
        &mut self,
        stage: viewer::Size,
        device: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if stage == self.stage && device == self.device {
            return;
        }
        self.stage = stage;
        self.device = device;
        self.camera.resize(self.image_size(), stage);
        self.camera_changed(window, cx);
    }

    fn zoom_at(
        &mut self,
        scale: f32,
        anchor: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.camera
            .zoom_at(scale, anchor, self.image_size(), self.stage);
        self.camera_changed(window, cx);
    }

    fn step(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.camera.step(up, self.image_size(), self.stage);
        self.camera_changed(window, cx);
    }

    fn fit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.camera = Camera::fit(self.image_size(), self.stage);
        self.camera_changed(window, cx);
    }

    fn actual_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let center = (self.stage.0 / 2., self.stage.1 / 2.);
        self.zoom_at(1., center, window, cx);
    }

    fn rotate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.original.is_none() {
            return;
        }
        self.turns = (self.turns + 1) % 4;
        self.camera = Camera::fit(self.image_size(), self.stage);
        self.decode(window, cx);
        cx.notify();
    }

    fn switch(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.images.len();
        if count < 2 {
            return;
        }
        let index = if forward {
            (self.index + 1) % count
        } else {
            (self.index + count - 1) % count
        };
        self.go_to(index, window, cx);
    }

    fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.current_id() else {
            return;
        };
        let task = self.host.copy(id, false, None, cx);
        Self::report_copy(task, COPY_LABELS, window, cx);
    }

    /// 复制选中的文字。
    fn copy_text(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.current_id() else {
            return;
        };
        let fragment = ClipboardFragment::ImageTextRange {
            start: range.start,
            end: range.end,
        };
        let task = self.host.copy_fragment(id, fragment, cx);
        Self::report_copy(task, COPY_LABELS, window, cx);
    }

    /// 识别当前图里的二维码并复制内容；与复制图中文字一样，复制的内容会记成一条新的文本记录。
    fn copy_qr_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.current_id() else {
            return;
        };
        let task = self
            .host
            .copy_fragment(id, ClipboardFragment::ImageQrCode, cx);
        Self::report_copy(
            task,
            (
                "commands:labels.scanQrCode",
                "commands:messages.qrCodeCopied",
            ),
            window,
            cx,
        );
    }

    /// 复制完成后的提示：`(label, done)` 是失败时的动作名和成功提示的文案键。
    fn report_copy(
        task: Task<anyhow::Result<()>>,
        (label, done): (&'static str, &'static str),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await;
            cx.update(|window, cx| match result {
                Ok(()) => toast::show(Toast::success(t(done)), window, cx),
                Err(err) => error_toast(label, &err, window, cx),
            })
            .ok();
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.current_id() {
            save_image(self.source.image_save(id), window, cx);
        }
    }

    fn pin_to_screen(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.current_id() {
            super::screen_pin::open_image(self.source.clone(), self.host.clone(), id, cx);
        }
    }

    fn open_external(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(original) = &self.original {
            cx.open_with_system(&original.path);
        }
    }

    // ------------------------------------------------------------ 输入

    /// 窗口坐标换成舞台坐标。
    fn stage_point(position: gpui::Point<Pixels>) -> (f32, f32) {
        (
            position.x.as_f32(),
            (position.y - TITLE_BAR_HEIGHT).as_f32(),
        )
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.original.is_none() {
            return;
        }
        let anchor = Self::stage_point(event.position);
        match event.delta {
            // Windows 上滚轮一格按系统设置是 3 行：一格约放大 1.26 倍。
            ScrollDelta::Lines(delta) => {
                self.zoom_at(
                    self.camera.scale * 1.08_f32.powf(delta.y),
                    anchor,
                    window,
                    cx,
                );
            }
            ScrollDelta::Pixels(delta) if event.modifiers.secondary() => {
                let factor = (delta.y.as_f32() / 200.).exp();
                self.zoom_at(self.camera.scale * factor, anchor, window, cx);
            }
            ScrollDelta::Pixels(delta) => {
                self.camera.pan(
                    delta.x.as_f32(),
                    delta.y.as_f32(),
                    self.image_size(),
                    self.stage,
                );
                cx.notify();
            }
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.original.is_none() {
            return;
        }
        let pointer = Self::stage_point(event.position);
        let placement = self.placement();
        if let Some(text) = self.text_layer() {
            if event.click_count >= 2
                && let Some(line) = text.line_at(&placement, pointer)
            {
                let range = text.line_range(line);
                self.selection = Some((range.start, range.end));
                self.selecting = false;
                cx.notify();
                return;
            }
            if event.click_count == 1
                && let Some(caret) = text.caret_at(&placement, pointer)
            {
                self.selection = Some((caret, caret));
                self.selecting = true;
                cx.notify();
                return;
            }
        }
        if self.selection.take().is_some() {
            cx.notify();
        }
        if event.click_count >= 2 {
            self.drag = None;
            if self.camera.fitted {
                self.zoom_at(1., pointer, window, cx);
            } else {
                self.fit(window, cx);
            }
            return;
        }
        if self.camera.pannable(self.image_size(), self.stage) {
            self.drag = Some(Drag {
                pointer,
                from: (self.camera.x, self.camera.y),
            });
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let pointer = Self::stage_point(event.position);
        let placement = self.placement();
        if self.selecting {
            if event.pressed_button != Some(MouseButton::Left) {
                self.selecting = false;
                return;
            }
            let caret = self
                .text_layer()
                .and_then(|text| text.nearest_caret(&placement, pointer));
            if let (Some(caret), Some((_, focus))) = (caret, self.selection.as_mut())
                && *focus != caret
            {
                *focus = caret;
                cx.notify();
            }
            return;
        }
        let over_text = self.drag.is_none()
            && self
                .text_layer()
                .is_some_and(|text| text.line_at(&placement, pointer).is_some());
        if over_text != self.over_text {
            self.over_text = over_text;
            cx.notify();
        }

        let Some(drag) = &self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            cx.notify();
            return;
        }
        let (dx, dy) = (pointer.0 - drag.pointer.0, pointer.1 - drag.pointer.1);
        self.camera.x = drag.from.0;
        self.camera.y = drag.from.1;
        self.camera.pan(dx, dy, self.image_size(), self.stage);
        cx.notify();
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let plain = !modifiers.secondary() && !modifiers.alt;
        match keystroke.key.as_str() {
            "escape" if self.selected().is_some() => {
                self.selection = None;
                cx.notify();
            }
            "escape" => window.remove_window(),
            // 与面板上打开大图的 Shift+空格成对：再按一次关上。
            "space" if plain && modifiers.shift => window.remove_window(),
            "left" if plain => self.switch(false, window, cx),
            "right" if plain => self.switch(true, window, cx),
            "=" | "+" if plain => self.step(true, window, cx),
            "-" if plain => self.step(false, window, cx),
            "0" if plain => self.fit(window, cx),
            "1" if plain => self.actual_size(window, cx),
            "r" if plain => self.rotate(window, cx),
            "t" if plain => self.toggle_reveal(window, cx),
            "q" if plain => self.copy_qr_code(window, cx),
            "a" if modifiers.secondary() => self.select_all(cx),
            "c" if modifiers.secondary() => match self.selected() {
                Some(range) => self.copy_text(range, window, cx),
                None => self.copy(window, cx),
            },
            "s" if modifiers.secondary() => self.save(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // ------------------------------------------------------------ 渲染

    fn window_title(&self) -> String {
        let Some(original) = &self.original else {
            return t("preview:viewer.title").to_string();
        };
        let (width, height) = rotated(
            (original.pixels.0 as f32, original.pixels.1 as f32),
            self.turns,
        );
        let mut parts = vec![format!("{width} × {height}")];
        if let Some(extension) = original.path.extension() {
            parts.push(extension.to_string_lossy().to_uppercase());
        }
        if let Some(bytes) = original.bytes {
            parts.push(format_bytes(i64::try_from(bytes).unwrap_or(i64::MAX)));
        }
        if self.images.len() > 1 {
            parts.push(format!("{} / {}", self.index + 1, self.images.len()));
        }
        parts.join(" · ")
    }

    /// 图片层：位图、透明图下面的棋盘格。
    fn picture(&self, cx: &App) -> Option<gpui::AnyElement> {
        let bitmap = self
            .bitmap
            .as_ref()
            .filter(|bitmap| bitmap.turns == self.turns)?;
        let (width, height) = self.image_size();
        let bounds = Bounds::new(
            point(px(self.camera.x), px(self.camera.y)),
            size(
                px(width * self.camera.scale),
                px(height * self.camera.scale),
            ),
        );
        let stage = Bounds::new(
            point(px(0.), px(0.)),
            size(px(self.stage.0), px(self.stage.1)),
        );
        let visible = bounds.intersect(&stage);
        let tokens = theme::semantic(cx);
        let (light, dark) = (tokens.surface.raised, tokens.fill.default);
        let marks = self.text_marks(cx);

        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .when(bitmap.transparent, |layer| {
                    layer.child(
                        canvas(
                            |_, _, _| {},
                            move |origin, _, window, _| {
                                let offset = origin.origin;
                                window.paint_quad(fill(visible + offset, light));
                                let columns = (visible.size.width.as_f32() / CHECKER).ceil() as i32;
                                let rows = (visible.size.height.as_f32() / CHECKER).ceil() as i32;
                                for row in 0..rows {
                                    for column in (row % 2..columns).step_by(2) {
                                        let square = Bounds::new(
                                            visible.origin
                                                + offset
                                                + point(
                                                    px(column as f32 * CHECKER),
                                                    px(row as f32 * CHECKER),
                                                ),
                                            size(px(CHECKER), px(CHECKER)),
                                        )
                                        .intersect(&(visible + offset));
                                        window.paint_quad(fill(square, dark));
                                    }
                                }
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                })
                .child(
                    img(ImageSource::Render(bitmap.image.clone()))
                        .absolute()
                        .left(bounds.origin.x)
                        .top(bounds.origin.y)
                        .w(bounds.size.width)
                        .h(bounds.size.height),
                )
                .when(!marks.is_empty(), |layer| {
                    layer.child(
                        canvas(
                            |_, _, _| {},
                            move |origin, _, window, _| {
                                for (quad, color) in &marks {
                                    if let Some(path) = quad_path(quad, origin.origin) {
                                        window.paint_path(path, *color);
                                    }
                                }
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                })
                .into_any_element(),
        )
    }

    /// 文字层要画的块（舞台坐标）：标出的文字区域在下，选区在上；舞台外的不画。
    fn text_marks(&self, cx: &App) -> Vec<([Point; 4], Hsla)> {
        let Some(text) = self.text_layer() else {
            return Vec::new();
        };
        let colors = theme::components(cx).image_text;
        let placement = self.placement();
        let regions = if self.reveal {
            text.regions(&placement)
        } else {
            Vec::new()
        };
        let regions = regions.into_iter().map(|quad| (quad, colors.region));
        let selection = self
            .selected()
            .map(|range| text.highlights(&placement, &range))
            .unwrap_or_default()
            .into_iter()
            .map(|quad| (quad, colors.selection));
        let stage = self.stage;
        regions
            .chain(selection)
            .filter(|(quad, _)| {
                quad.iter().any(|corner| corner.0 >= 0.)
                    && quad.iter().any(|corner| corner.0 <= stage.0)
                    && quad.iter().any(|corner| corner.1 >= 0.)
                    && quad.iter().any(|corner| corner.1 <= stage.1)
            })
            .collect()
    }

    fn icon_button(
        &self,
        id: &'static str,
        icon: IconName,
        label: &str,
        key: Option<&str>,
        action: fn(&mut Self, &mut Window, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> Button {
        let tooltip = match key {
            Some(key) => format!("{} ({})", t(label), shortcut::display_keystroke(key)),
            None => t(label).to_string(),
        };
        let viewer = cx.entity().downgrade();
        Button::icon(id, icon, tooltip).on_click(move |_, window, cx| {
            viewer
                .update(cx, |viewer, cx| action(viewer, window, cx))
                .ok();
        })
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let viewer = cx.entity().downgrade();
        let scale = Button::new("viewer-scale", format!("{:.0}%", self.camera.scale * 100.))
            .ghost()
            .tooltip(format!(
                "{} ({})",
                t("preview:viewer.fit"),
                shortcut::display_keystroke("0")
            ))
            .on_click(move |_, window, cx| {
                viewer.update(cx, |viewer, cx| viewer.fit(window, cx)).ok();
            });
        let viewer = cx.entity().downgrade();
        let actual = Button::new("viewer-actual", "1:1")
            .ghost()
            .tooltip(format!(
                "{} ({})",
                t("preview:viewer.actualSize"),
                shortcut::display_keystroke("1")
            ))
            .on_click(move |_, window, cx| {
                viewer
                    .update(cx, |viewer, cx| viewer.actual_size(window, cx))
                    .ok();
            });
        let separator = || {
            div()
                .w(px(1.))
                .h(space(4.))
                .mx(space(1.))
                .bg(tokens.border.divider)
        };

        div()
            .id("viewer-toolbar")
            .flex()
            .items_center()
            .gap(space(0.5))
            .p(space(1.))
            .rounded(radius::LG)
            .bg(tokens.surface.overlay)
            .shadow(tokens.shadow.float())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(self.icon_button(
                "viewer-zoom-out",
                IconName::ZoomOut,
                "preview:viewer.zoomOut",
                Some("-"),
                |viewer, window, cx| viewer.step(false, window, cx),
                cx,
            ))
            .child(scale)
            .child(self.icon_button(
                "viewer-zoom-in",
                IconName::ZoomIn,
                "preview:viewer.zoomIn",
                Some("="),
                |viewer, window, cx| viewer.step(true, window, cx),
                cx,
            ))
            .child(separator())
            .child(self.icon_button(
                "viewer-fit",
                IconName::Shrink,
                "preview:viewer.fit",
                Some("0"),
                Self::fit,
                cx,
            ))
            .child(actual)
            .child(self.icon_button(
                "viewer-rotate",
                IconName::RotateCw,
                "preview:viewer.rotate",
                Some("r"),
                Self::rotate,
                cx,
            ))
            .when(self.text_layer().is_some(), |toolbar| {
                toolbar.child(
                    self.icon_button(
                        "viewer-text",
                        IconName::ScanText,
                        "preview:viewer.showText",
                        Some("t"),
                        Self::toggle_reveal,
                        cx,
                    )
                    .selected(self.reveal),
                )
            })
            .child(self.icon_button(
                "viewer-qr-code",
                IconName::ScanQrCode,
                "preview:viewer.scanQrCode",
                Some("q"),
                Self::copy_qr_code,
                cx,
            ))
            .child(separator())
            .child(self.icon_button(
                "viewer-copy",
                IconName::Copy,
                "preview:viewer.copy",
                Some("secondary-c"),
                Self::copy,
                cx,
            ))
            .child(self.icon_button(
                "viewer-save",
                IconName::Download,
                "preview:viewer.save",
                Some("secondary-s"),
                Self::save,
                cx,
            ))
            .child(self.icon_button(
                "viewer-pin",
                IconName::PictureInPicture,
                "preview:viewer.pinToScreen",
                None,
                Self::pin_to_screen,
                cx,
            ))
            .child(self.icon_button(
                "viewer-open",
                IconName::ExternalLink,
                "preview:viewer.openExternal",
                None,
                Self::open_external,
                cx,
            ))
    }

    fn side_button(&self, forward: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let (id, icon, label, key) = if forward {
            (
                "viewer-next",
                IconName::ChevronRight,
                "preview:viewer.next",
                "right",
            )
        } else {
            (
                "viewer-previous",
                IconName::ChevronLeft,
                "preview:viewer.previous",
                "left",
            )
        };
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .when(forward, |side| side.right(space(3.)))
            .when(!forward, |side| side.left(space(3.)))
            .flex()
            .items_center()
            .child(
                div()
                    .rounded(radius::MD)
                    .bg(tokens.surface.overlay)
                    .shadow(tokens.shadow.float())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.icon_button(
                        id,
                        icon,
                        label,
                        Some(key),
                        if forward {
                            |viewer, window, cx| viewer.switch(true, window, cx)
                        } else {
                            |viewer, window, cx| viewer.switch(false, window, cx)
                        },
                        cx,
                    )),
            )
    }

    fn placeholder(&self, cx: &App) -> Option<gpui::AnyElement> {
        let tokens = theme::semantic(cx);
        let (icon, key) = if self.missing {
            (Some(IconName::ImageOff), "preview:empty.imageMissing")
        } else if self
            .bitmap
            .as_ref()
            .is_none_or(|bitmap| bitmap.turns != self.turns)
        {
            (None, "preview:title.loading")
        } else {
            return None;
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(space(2.))
                .text_color(tokens.text.muted)
                .when_some(icon, |column, icon| {
                    column.child(Icon::new(icon).size(gpui::rems(2.)))
                })
                .child(div().kp_text(TextSize::Sm).child(t(key)))
                .into_any_element(),
        )
    }
}

impl Render for ImageViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let stage = (
            viewport.width.as_f32().max(1.),
            (viewport.height - TITLE_BAR_HEIGHT).as_f32().max(1.),
        );
        self.sync_stage(stage, window.scale_factor(), window, cx);
        let title = self.window_title();
        if title != self.title {
            window.set_window_title(&title);
            self.title = title.clone();
        }

        let tokens = theme::semantic(cx);
        let cursor = if self.drag.is_some() {
            CursorStyle::ClosedHand
        } else if self.selecting || self.over_text {
            CursorStyle::IBeam
        } else if self.camera.pannable(self.image_size(), self.stage) {
            CursorStyle::OpenHand
        } else {
            CursorStyle::Arrow
        };
        let picture = self.picture(cx);
        let placeholder = self.placeholder(cx);
        let switchable = self.images.len() > 1;

        div()
            .key_context("ImageViewer")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(crate::platform::material::shell_surface(
                cx,
                tokens.surface.panel,
            ))
            .text_color(tokens.text.primary)
            .child(TitleBar::new().title(self.title.clone()))
            .child(
                div()
                    .id("viewer-stage")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .bg(tokens.surface.window)
                    .cursor(cursor)
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|viewer, _, _, cx| viewer.end_drag(cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|viewer, _, _, cx| viewer.end_drag(cx)),
                    )
                    .children(picture)
                    .children(placeholder)
                    .when(switchable, |stage| {
                        stage
                            .child(self.side_button(false, cx))
                            .child(self.side_button(true, cx))
                    })
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(space(4.))
                            .flex()
                            .justify_center()
                            .child(self.toolbar(cx)),
                    ),
            )
    }
}

/// 按界面字体量出每行字符的位置，排成文字层。
fn text_layer(lines: Vec<ImageTextLine>, window: &Window) -> TextLayer {
    let mut ui_font = font(fonts::UI_FAMILY);
    ui_font.fallbacks = Some(fonts::ui_fallbacks());
    TextLayer::new(
        lines
            .into_iter()
            .map(|line| (line.text, line.quad.map(|[x, y]| (x, y)))),
        |text| {
            let run = TextRun {
                len: text.len(),
                font: ui_font.clone(),
                ..TextRun::default()
            };
            let layout =
                window
                    .text_system()
                    .layout_line(text, px(MEASURE_FONT_SIZE), &[run], None);
            text.char_indices()
                .map(|(index, _)| layout.x_for_index(index).as_f32())
                .chain(std::iter::once(layout.width.as_f32()))
                .collect()
        },
    )
}

/// 舞台坐标的四边形，`origin` 是舞台在窗口里的左上角。
fn quad_path(quad: &[Point; 4], origin: gpui::Point<Pixels>) -> Option<gpui::Path<Pixels>> {
    let corner = |(x, y): Point| origin + point(px(x), px(y));
    let mut builder = PathBuilder::fill();
    builder.move_to(corner(quad[0]));
    for &next in &quad[1..] {
        builder.line_to(corner(next));
    }
    builder.close();
    builder.build().ok()
}
