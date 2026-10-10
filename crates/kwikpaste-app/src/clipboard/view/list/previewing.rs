//! 预览窗的开合（1.x `useClipboardPreviewController`）。
//!
//! - 悬停：指针停在卡片上（显示后指针真的动过）`hoverDelayMs` 后打开；已有悬停预览时换卡片立即换内容；
//!   离开卡片 240 ms 后关，期间指针进了预览窗就不关。
//! - 空格（设置打开时）：按住预览当前项，↑/↓ 换当前项时跟着换，松开关；松开时指针在预览窗里就转成
//!   悬停式（离开预览窗再关）。
//! - 关：面板隐藏、换筛选条件、滚动（悬停式）、Esc、对这一条做了粘贴 / 复制 / 删除等操作。
//!
//! 预览窗的几何在打开时按卡片在屏幕上的位置算（见 [`crate::clipboard::model::preview::geometry`]）：
//! 卡片的位置由列表渲染时挂在卡片上的 canvas 记下。

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use futures::channel::oneshot;
use gpui::{
    AnyElement, AnyWindowHandle, AsyncApp, Bounds, Context, IntoElement as _, Pixels, Point,
    Styled as _, Subscription, Task, Window, canvas,
};
use kwikpaste_core::{clipboard::ClipboardFragment, settings::PreviewHoverDelayMs};
use kwikpaste_ui::theme;

use super::{ClipboardList, Pointer, ops::error_toast};
use crate::{
    clipboard::{
        model::preview::{self, HEADER_HEIGHT, RectF},
        source::{Preview, PreviewContentMetrics, PreviewTextView},
        view::preview::{
            ImageTextView, PreviewEvent, PreviewWindow,
            native::{self, NativePreview},
        },
    },
    i18n::t,
};

/// 离开卡片后等这么久再关（1.x `HOVER_HIDE_BUFFER_MS`），留时间把指针挪进预览窗。
const HIDE_BUFFER: Duration = Duration::from_millis(240);
/// 键盘预览换当前项后等卡片布局稳定再打开。
const FOLLOW_DELAY: Duration = Duration::from_millis(32);

/// 预览是怎么打开的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewTrigger {
    Hover,
    /// 按住空格。
    Keyboard,
    /// 松开空格时指针在预览窗里：之后与悬停一样，离开预览窗再关。
    Held,
}

impl PreviewTrigger {
    fn pointer(self) -> bool {
        matches!(self, Self::Hover | Self::Held)
    }
}

/// 卡片在面板窗口里的位置（渲染时由 canvas 记下）。
pub(super) type Anchors = Rc<RefCell<Vec<(Arc<str>, Bounds<Pixels>)>>>;

/// 列表里的预览状态。
#[derive(Default)]
pub(super) struct Previewing {
    window: Option<PreviewWindow>,
    /// 这台机器上建不了预览窗（macOS 还没做，或建窗失败）。
    unavailable: bool,
    session: Option<(Arc<str>, PreviewTrigger)>,
    /// 正在看识别文字（而不是图片本身）的图片记录；换了记录或关上预览就回到图片。
    image_text: Option<Arc<str>>,
    /// 每次打开、关闭都加一，晚到的结果按它作废。
    request: u64,
    hover_timer: Option<Task<()>>,
    hide_timer: Option<Task<()>>,
    follow_timer: Option<Task<()>>,
    pointer_inside: bool,
    pub(super) anchors: Anchors,
    /// 面板里最后的指针位置（悬停预览朝指针所在的一半弹出）。
    pub(super) pointer: Option<Point<Pixels>>,
    _subscription: Option<Subscription>,
}

impl Previewing {
    fn anchor(&self, id: &str) -> Option<Bounds<Pixels>> {
        self.anchors
            .borrow()
            .iter()
            .find(|(anchor, _)| **anchor == *id)
            .map(|(_, bounds)| *bounds)
    }
}

/// 挂在卡片上、记下卡片位置的 canvas。
pub(super) fn anchor_canvas(id: Arc<str>, anchors: Anchors) -> AnyElement {
    canvas(
        move |bounds, _, _| {
            let mut anchors = anchors.borrow_mut();
            anchors.retain(|(anchor, _)| *anchor != id);
            anchors.push((id.clone(), bounds));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

fn hover_delay(delay: PreviewHoverDelayMs) -> Duration {
    Duration::from_millis(match delay {
        PreviewHoverDelayMs::Ms300 => 300,
        PreviewHoverDelayMs::Ms500 => 500,
        PreviewHoverDelayMs::Ms1000 => 1000,
    })
}

impl ClipboardList {
    /// 直接打开现有预览，并切到可选择词语的模式。
    pub fn split_words(&mut self, id: Arc<str>, cx: &mut Context<Self>) {
        self.settings.clipboard.preview.text_view = PreviewTextView::Words;
        self.open_preview(id, PreviewTrigger::Keyboard, cx);
    }

    /// 正在预览的记录与打开方式。
    pub fn preview_session(&self) -> Option<(Arc<str>, PreviewTrigger)> {
        self.previewing.session.clone()
    }

    /// 预览窗当前是否可见（自测用）。
    pub fn preview_visible(&self) -> bool {
        self.previewing
            .window
            .as_ref()
            .is_some_and(|window| window.native.is_visible())
    }

    /// 预览窗是不是前台窗口（自测核对“不抢前台”）。
    pub fn preview_foreground(&self) -> bool {
        self.previewing
            .window
            .as_ref()
            .is_some_and(|window| window.native.is_foreground())
    }

    /// 要记下位置的卡片：悬停的、当前项、正在预览的。
    pub(super) fn wants_anchor(&self, id: &Arc<str>, active: bool) -> bool {
        active
            || self.hovered.as_ref() == Some(id)
            || self
                .previewing
                .session
                .as_ref()
                .is_some_and(|(session, _)| session == id)
    }

    fn ensure_preview_window(&mut self, cx: &mut Context<Self>) -> bool {
        if self.previewing.window.is_some() {
            return true;
        }
        if self.previewing.unavailable {
            return false;
        }

        match PreviewWindow::open(cx) {
            Ok(window) => {
                let native = window.native.clone();
                cx.spawn(async move |_, _| native.install()).detach();
                self.previewing._subscription = Some(
                    cx.subscribe(&window.panel, |list, _, event: &PreviewEvent, cx| {
                        list.on_preview_event(event, cx)
                    }),
                );
                self.previewing.window = Some(window);
                true
            }
            Err(err) => {
                log::warn!("the preview window is unavailable: {err:#}");
                self.previewing.unavailable = true;
                false
            }
        }
    }

    fn on_preview_event(&mut self, event: &PreviewEvent, cx: &mut Context<Self>) {
        match event {
            PreviewEvent::Pointer(inside) => {
                self.previewing.pointer_inside = *inside;
                if *inside {
                    self.previewing.hide_timer = None;
                } else {
                    self.schedule_preview_hide(cx);
                }
            }
            PreviewEvent::TextView(view) => self.switch_preview_text_view(*view, cx),
            PreviewEvent::Words { paste } => self.use_preview_words(*paste, cx),
            PreviewEvent::ImageText(view) => self.switch_preview_image_text(*view, cx),
        }
    }

    /// 指针进出卡片（悬停预览）。
    pub(super) fn preview_hover(&mut self, id: &Arc<str>, hovered: bool, cx: &mut Context<Self>) {
        if !hovered {
            self.schedule_preview_hide(cx);
            return;
        }
        if !self.visible || self.pointer != Pointer::Moved || self.selection.active() {
            return;
        }

        let preview = &self.settings.clipboard.preview;
        self.previewing.hide_timer = None;
        match self.previewing.session.clone() {
            Some((session, PreviewTrigger::Keyboard)) => {
                if session != *id {
                    self.open_preview(id.clone(), PreviewTrigger::Keyboard, cx);
                }
                return;
            }
            Some((session, _)) if preview.hover_enabled => {
                if session != *id {
                    self.open_preview(id.clone(), PreviewTrigger::Hover, cx);
                }
                return;
            }
            _ => {}
        }
        if !preview.hover_enabled {
            return;
        }

        let delay = hover_delay(preview.hover_delay_ms);
        let target = id.clone();
        self.previewing.hover_timer = Some(cx.spawn(async move |list, cx| {
            cx.background_executor().timer(delay).await;
            list.update(cx, |list, cx| {
                list.previewing.hover_timer = None;
                let still_here = list.hovered.as_ref() == Some(&target);
                if still_here && list.visible && list.settings.clipboard.preview.hover_enabled {
                    list.open_preview(target, PreviewTrigger::Hover, cx);
                }
            })
            .ok();
        }));
    }

    /// 悬停式预览在指针离开卡片和预览窗 240 ms 后关上。
    ///
    /// 不取消悬停计时器：换到相邻卡片时 GPUI 可能先发新卡片的进入、再发旧卡片的离开，
    /// 计时器到点时自己核对指针还在不在目标卡片上。
    fn schedule_preview_hide(&mut self, cx: &mut Context<Self>) {
        let pointer_session = self
            .previewing
            .session
            .as_ref()
            .is_some_and(|(_, trigger)| trigger.pointer());
        if !pointer_session {
            return;
        }

        self.previewing.hide_timer = Some(cx.spawn(async move |list, cx| {
            cx.background_executor().timer(HIDE_BUFFER).await;
            list.update(cx, |list, cx| {
                list.previewing.hide_timer = None;
                if list.previewing.pointer_inside {
                    return;
                }
                let over_session_card = match (&list.hovered, &list.previewing.session) {
                    (Some(hovered), Some((session, _))) => hovered == session,
                    _ => false,
                };
                if !over_session_card {
                    list.close_preview(cx);
                }
            })
            .ok();
        }));
    }

    /// 空格按下 / 松开（设置“按住空格预览”打开时）。
    pub(super) fn preview_space(&mut self, down: bool, cx: &mut Context<Self>) {
        if !self.settings.clipboard.preview.space_enabled {
            return;
        }
        if down {
            if matches!(self.previewing.session, Some((_, PreviewTrigger::Keyboard))) {
                return;
            }
            if let Some(item) = self.active_item() {
                self.open_preview(item.id.clone(), PreviewTrigger::Keyboard, cx);
            }
            return;
        }

        let Some((id, PreviewTrigger::Keyboard)) = self.previewing.session.clone() else {
            return;
        };
        if self.previewing.pointer_inside {
            self.previewing.session = Some((id, PreviewTrigger::Held));
        } else {
            self.close_preview(cx);
        }
    }

    /// 键盘预览时换了当前项：等卡片布局稳定后换内容。
    pub(super) fn preview_follow_active(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.previewing.session, Some((_, PreviewTrigger::Keyboard))) {
            return;
        }

        self.previewing.follow_timer = Some(cx.spawn(async move |list, cx| {
            cx.background_executor().timer(FOLLOW_DELAY).await;
            list.update(cx, |list, cx| {
                list.previewing.follow_timer = None;
                let keyboard =
                    matches!(list.previewing.session, Some((_, PreviewTrigger::Keyboard)));
                if let (true, Some(item)) = (keyboard, list.active_item()) {
                    list.open_preview(item.id.clone(), PreviewTrigger::Keyboard, cx);
                }
            })
            .ok();
        }));
    }

    /// 打开（或换成）`id` 的预览：取数据，按卡片位置算几何，再在借用之外显示原生窗口。
    pub fn open_preview(&mut self, id: Arc<str>, trigger: PreviewTrigger, cx: &mut Context<Self>) {
        if !self.ensure_preview_window(cx) {
            return;
        }
        self.previewing.request += 1;
        let request = self.previewing.request;
        self.previewing.session = Some((id.clone(), trigger));
        self.previewing.hover_timer = None;
        if self.previewing.image_text.as_ref() != Some(&id) {
            self.previewing.image_text = None;
        }
        // 识别出文字的图片给「图片 / 文字」切换；看文字时取识别文字的文本预览，取不到就退回图片。
        let has_text = self.model.find(&id).is_some_and(|item| item.has_image_text)
            && crate::core_host::core(cx).is_none_or(|core| core.ocr_enabled());
        let mut image_text = has_text.then(|| {
            if self.previewing.image_text.is_some() {
                ImageTextView::Text
            } else {
                ImageTextView::Image
            }
        });
        let (future, fallback) = if image_text == Some(ImageTextView::Text) {
            (
                self.source.image_text_preview(id.clone()),
                Some(self.source.preview(id.clone())),
            )
        } else {
            (self.source.preview(id.clone()), None)
        };
        let window = self.window;

        cx.spawn(async move |list, cx| {
            let mut preview = match future.await {
                Ok(preview) => preview,
                Err(err) => {
                    log::warn!("preview of {id} is unavailable: {err:#}");
                    None
                }
            };
            if preview.is_none()
                && let Some(fallback) = fallback
            {
                image_text = Some(ImageTextView::Image);
                preview = fallback.await.unwrap_or_else(|err| {
                    log::warn!("preview of {id} is unavailable: {err:#}");
                    None
                });
            }
            // 指针刚换到这张卡片时，它的位置要等面板画完下一帧才记下；数据往往先到。
            let anchored = list
                .read_with(cx, |list, _| list.previewing.anchor(&id).is_some())
                .unwrap_or(true);
            if !anchored {
                after_next_paint(window, cx).await;
            }
            let placed = window
                .update(cx, |_, window, cx| {
                    list.update(cx, |list, cx| {
                        if list.previewing.request != request {
                            return None;
                        }
                        if image_text != Some(ImageTextView::Text) {
                            list.previewing.image_text = None;
                        }
                        list.place_preview(&id, trigger, preview, image_text, window, cx)
                    })
                    .ok()
                    .flatten()
                })
                .ok()
                .flatten();
            if let Some((native, client, dpi)) = placed {
                native.show(client, dpi);
            }
        })
        .detach();
    }

    /// 关掉预览（返回是否有预览开着）。
    pub fn close_preview(&mut self, cx: &mut Context<Self>) -> bool {
        self.previewing.hover_timer = None;
        self.previewing.hide_timer = None;
        self.previewing.follow_timer = None;
        self.previewing.pointer_inside = false;
        self.previewing.image_text = None;
        self.previewing.request += 1;
        let was_open = self.previewing.session.take().is_some();
        if !was_open {
            return false;
        }

        if let Some(window) = &self.previewing.window {
            let native = window.native.clone();
            let panel = window.panel.clone();
            window
                .handle
                .update(cx, |_, window, cx| {
                    panel.update(cx, |panel, cx| panel.release(window, cx));
                })
                .ok();
            let request = self.previewing.request;
            cx.spawn(async move |list, cx| {
                native.hide();
                // 隐藏后再丢正文，免得隐藏前多画一帧空白；期间又打开了新预览就不动。
                let reopened = list
                    .read_with(cx, |list, _| list.previewing.request != request)
                    .unwrap_or(true);
                if !reopened {
                    panel.update(cx, |panel, _| panel.forget());
                }
            })
            .detach();
        }
        true
    }

    /// 对这一条做了操作（粘贴、复制、删除……）：正在预览它就关掉（1.x 各处的 `closePreview`）。
    pub(super) fn close_preview_of(&mut self, id: &str, cx: &mut Context<Self>) {
        let previewing = self
            .previewing
            .session
            .as_ref()
            .is_some_and(|(session, _)| &**session == id);
        if previewing {
            self.close_preview(cx);
        }
    }

    /// 悬停式预览随滚动关上（键盘预览不关）。
    pub(super) fn close_pointer_preview(&mut self, cx: &mut Context<Self>) {
        self.previewing.hover_timer = None;
        let pointer = self
            .previewing
            .session
            .as_ref()
            .is_some_and(|(_, trigger)| trigger.pointer());
        if pointer {
            self.close_preview(cx);
        }
    }

    /// 预览里选中的词（Enter / Mod+C 作用于它们，1.x `getPreviewWordsFragment`）。
    pub fn preview_words(&self, cx: &gpui::App) -> Option<(Arc<str>, Vec<usize>)> {
        let (id, _) = self.previewing.session.as_ref()?;
        let panel = self.previewing.window.as_ref()?.panel.read(cx);
        if panel.item_id() != Some(&**id) {
            return None;
        }
        let words = panel.selected_words();
        (!words.is_empty()).then(|| (id.clone(), words))
    }

    /// 粘贴或复制预览里选中的词，然后关掉预览（1.x `pasteSelection` / `copySelection`）。
    pub fn use_preview_words(&mut self, paste: bool, cx: &mut Context<Self>) {
        let Some((id, indices)) = self.preview_words(cx) else {
            return;
        };
        let fragment = if self.previewing.image_text.as_ref() == Some(&id) {
            ClipboardFragment::ImageWords { indices }
        } else {
            ClipboardFragment::Words { indices }
        };
        if paste {
            log::info!("preview words paste requested for {id}");
            self.close_preview(cx);
            let task = self.host.paste_fragment(id, fragment, cx);
            self.report_host_failure("commands:labels.paste", task, cx);
            return;
        }

        let task = self.host.copy_fragment(id, fragment, cx);
        let window = self.window;
        cx.spawn(async move |_, cx| {
            let result = task.await;
            window
                .update(cx, |_, window, cx| match result {
                    Ok(()) => kwikpaste_ui::toast::show(
                        kwikpaste_ui::toast::Toast::success(t("commands:messages.copied")),
                        window,
                        cx,
                    ),
                    Err(err) => error_toast("commands:labels.copy", &err, window, cx),
                })
                .ok();
        })
        .detach();
    }

    /// 预览设置变了：关掉了悬停 / 空格预览就关掉对应的预览，其余（文本方式）按新设置重开。
    pub(super) fn preview_settings_changed(&mut self, cx: &mut Context<Self>) {
        let preview = &self.settings.clipboard.preview;
        match self.previewing.session.clone() {
            Some((_, PreviewTrigger::Hover)) if !preview.hover_enabled => {
                self.close_preview(cx);
            }
            Some((_, PreviewTrigger::Keyboard)) if !preview.space_enabled => {
                self.close_preview(cx);
            }
            Some((id, trigger)) => self.open_preview(id, trigger, cx),
            None => {}
        }
    }

    /// 切换预览的文本方式：写进设置，当前预览按新方式重开（尺寸也跟着变）。
    fn switch_preview_text_view(&mut self, view: PreviewTextView, cx: &mut Context<Self>) {
        self.settings.clipboard.preview.text_view = view;
        let future = self.source.set_preview_text_view(view);
        cx.spawn(async move |list, cx| {
            let result = future.await;
            list.update(cx, |list, cx| {
                if let Err(err) = result {
                    log::warn!("preview text view could not be saved: {err:#}");
                }
                if let Some((id, trigger)) = list.previewing.session.clone() {
                    list.open_preview(id, trigger, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 图片在「图片」和「图中文字」之间切换：按选中的一面重开当前预览（尺寸也跟着变）。
    fn switch_preview_image_text(&mut self, view: ImageTextView, cx: &mut Context<Self>) {
        let Some((id, trigger)) = self.previewing.session.clone() else {
            return;
        };
        self.previewing.image_text = (view == ImageTextView::Text).then(|| id.clone());
        self.open_preview(id, trigger, cx);
    }

    /// 按卡片位置算出预览窗在屏幕上的位置，换好内容，返回要显示的原生窗口与位置。
    fn place_preview(
        &mut self,
        id: &Arc<str>,
        trigger: PreviewTrigger,
        preview: Option<Preview>,
        image_text: Option<ImageTextView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(Rc<NativePreview>, kwikpaste_os::geometry::Rect, u32)> {
        let Some(bounds) = self.previewing.anchor(id) else {
            log::debug!("preview of {id} has no card on screen");
            self.close_preview(cx);
            return None;
        };
        let place = native::screen_place(window, bounds)?;
        let prefer_left = trigger.pointer()
            && self.previewing.pointer.is_some_and(|pointer| {
                pointer.x.as_f32() < window.viewport_size().width.as_f32() / 2.
            });
        let text_scale = f64::from(theme::text_scale(cx));
        let wrap_width = preview::text_wrap_width(&place.monitor, text_scale);
        let rows = preview
            .as_ref()
            .and_then(|preview| preview.payload.text.as_deref())
            .map(|text| preview::text_rows(text, wrap_width, text_scale, cx))
            .unwrap_or_default();
        let measured_metrics = preview.as_ref().map(|preview| match preview.metrics {
            // core 只能按字数估行数，这里换成按正文字体实际折出的行数。
            PreviewContentMetrics::Text { .. } => PreviewContentMetrics::Text {
                rows: u32::try_from(rows.len()).unwrap_or(u32::MAX),
            },
            _ => preview::measure_metrics(
                &preview.metrics,
                preview.payload.text.as_deref(),
                &preview.payload.words,
                text_scale,
                window,
            ),
        });
        let geometry = preview::geometry(
            place.card,
            place.monitor,
            measured_metrics.as_ref(),
            prefer_left,
            text_scale,
        );
        let image_box = preview
            .as_ref()
            .and_then(|preview| image_box(preview, geometry.panel, text_scale));
        let text_view = self.settings.clipboard.preview.text_view;
        let preview_window = self.previewing.window.as_ref()?;
        preview_window.panel.update(cx, |panel, cx| {
            panel.set(preview, rows, text_view, image_box, image_text, cx)
        });

        Some((
            preview_window.native.clone(),
            place.to_screen(geometry.panel),
            place.dpi,
        ))
    }
}

/// 等 `window` 画完一帧。GPUI 在帧开头、绘制之前调用 next-frame 回调，所以要等到再下一帧的开头；
/// 窗口隐藏期间一直挂着，窗口关掉时随回调一起丢弃。
async fn after_next_paint(window: AnyWindowHandle, cx: &mut AsyncApp) {
    let (painted, wait) = oneshot::channel();
    let scheduled = window.update(cx, |_, window, _| {
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |_, _| {
                painted.send(()).ok();
            });
        });
    });
    if scheduled.is_ok() {
        wait.await.ok();
    }
}

/// 图片在面板里的显示尺寸：等比缩进面板内容区（左右各 16、上下各 16 的内边距，头部 48）。
fn image_box(preview: &Preview, panel: RectF, scale: f64) -> Option<(f32, f32)> {
    let PreviewContentMetrics::Image {
        width: Some(width),
        height: Some(height),
    } = preview.metrics
    else {
        return None;
    };
    if width <= 0. || height <= 0. {
        return None;
    }

    let room_width = (panel.width - 32. * scale).max(1.);
    let room_height = (panel.height - (HEADER_HEIGHT + 32.) * scale).max(1.);
    let fit = 1_f64.min(room_width / width).min(room_height / height);
    Some(((width * fit) as f32, (height * fit) as f32))
}
