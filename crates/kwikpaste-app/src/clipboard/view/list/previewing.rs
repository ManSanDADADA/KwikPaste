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

use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc, time::Duration};

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
        model::{
            actions::OpenTarget,
            preview::{self, HEADER_HEIGHT, RectF},
        },
        source::{Preview, PreviewContentMetrics, PreviewTextView, TranslationView},
        view::preview::{
            ImageTextView, PreviewEvent, PreviewWindow, TranslationZone,
            native::{self, NativePreview},
            snippet_label,
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
    /// “拆词”：不跟随悬停（菜单关上时指针下的是别的卡片），只随键盘换当前项；指针进过预览窗后转成
    /// [`Self::Held`]。
    Words,
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
    /// 上一次摆好的预览窗：同一条换视图时沿用它的顶边；关上预览就清掉。
    placed: Option<(Arc<str>, preview::Geometry)>,
    /// 「拆词」临时换的文本方式，只管这一次预览（键盘换当前项时跟着走），不写进设置；关上预览就清掉。
    text_view: Option<PreviewTextView>,
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
    /// 拆词：打开这一条的预览并切到选词；正在选词预览这一条时再拆一次就退回原文。只管这一次预览，
    /// 不改「预览方式」设置。
    pub fn split_words(&mut self, id: Arc<str>, cx: &mut Context<Self>) {
        let session = self
            .previewing
            .session
            .clone()
            .filter(|(session, _)| *session == id);
        if session.is_none() {
            self.close_preview(cx);
        }
        let (view, trigger) = match session {
            Some((_, trigger)) if self.preview_text_view() == PreviewTextView::Words => {
                (PreviewTextView::Plain, trigger)
            }
            _ => (PreviewTextView::Words, PreviewTrigger::Words),
        };
        self.previewing.text_view = Some(view);
        self.open_preview(id, trigger, cx);
    }

    /// 当前预览用的文本方式：拆词临时换的优先，否则按设置。
    fn preview_text_view(&self) -> PreviewTextView {
        self.previewing
            .text_view
            .unwrap_or(self.settings.clipboard.preview.text_view)
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
            // 关上后（异步隐藏前后）预览窗还可能收到指针移动，不能把下一次预览的状态带偏。
            PreviewEvent::Pointer(_) if self.previewing.session.is_none() => {}
            PreviewEvent::Pointer(inside) => {
                self.previewing.pointer_inside = *inside;
                if *inside {
                    self.previewing.hide_timer = None;
                    // 拆词预览不跟随悬停，指针进过预览窗后与悬停一样，离开预览窗再关。
                    if let Some((_, trigger @ PreviewTrigger::Words)) = &mut self.previewing.session
                    {
                        *trigger = PreviewTrigger::Held;
                    }
                } else {
                    self.schedule_preview_hide(cx);
                }
            }
            PreviewEvent::TextView(view) => self.switch_preview_text_view(*view, cx),
            PreviewEvent::Words { paste } => self.use_preview_words(*paste, cx),
            PreviewEvent::ImageText(view) => self.switch_preview_image_text(*view, cx),
            PreviewEvent::Snippet(text) => self.copy_preview_snippet(text.clone(), cx),
            PreviewEvent::PasteSnippet(text) => {
                if let Some((id, _)) = self.previewing.session.clone() {
                    self.paste_snippet(&id, text.clone(), cx);
                }
            }
            PreviewEvent::OpenSnippet(text, kind) => {
                if let Some((id, _)) = self.previewing.session.clone() {
                    self.open_snippet(&id, text, *kind, cx);
                }
            }
            PreviewEvent::Open(target) => self.open_preview_target(*target, cx),
            PreviewEvent::ViewImage => {
                if let Some((id, _)) = self.previewing.session.clone() {
                    self.view_image(id, cx);
                }
            }
            PreviewEvent::Reveal(path) => {
                self.close_preview(cx);
                cx.reveal_path(Path::new(path.as_ref()));
            }
            PreviewEvent::EditText => self.edit_preview_text(cx),
            PreviewEvent::Translate => {
                self.act_on_previewed(cx, |list, id, window, cx| list.translate(id, window, cx))
            }
            PreviewEvent::CopyTranslation => self.act_on_previewed(cx, |list, id, window, cx| {
                list.copy_translation(id, window, cx)
            }),
            PreviewEvent::PasteTranslation => {
                if let Some((id, _)) = self.previewing.session.clone() {
                    self.paste_translation(id, cx);
                }
            }
        }
    }

    /// 双击了预览里的正文：关掉预览，打开这一条的内容编辑框。
    fn edit_preview_text(&mut self, cx: &mut Context<Self>) {
        let Some((id, _)) = self.previewing.session.clone() else {
            return;
        };
        let window = self.window;
        cx.spawn(async move |list, cx| {
            window
                .update(cx, |_, window, cx| {
                    list.update(cx, |list, cx| list.edit_content(id, window, cx))
                })
                .ok();
        })
        .detach();
    }

    /// 对正在预览的记录执行一个要用面板窗口的操作（提示显示在面板上）。
    fn act_on_previewed(
        &mut self,
        cx: &mut Context<Self>,
        action: impl FnOnce(&mut Self, Arc<str>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some((id, _)) = self.previewing.session.clone() else {
            return;
        };
        let window = self.window;
        cx.spawn(async move |list, cx| {
            window
                .update(cx, |_, window, cx| {
                    list.update(cx, |list, cx| action(list, id, window, cx))
                })
                .ok();
        })
        .detach();
    }

    /// 正在预览的记录的翻译有了变化：按新状态重开预览，尺寸跟着变，位置沿用上一次的顶边。
    pub(super) fn refresh_preview_translation(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        if let Some((current, trigger)) = self.previewing.session.clone()
            && id.is_none_or(|id| *current == *id)
        {
            self.open_preview(current, trigger, cx);
        }
    }

    /// 点了预览里的链接 / 邮箱：关掉预览，交给浏览器或邮件客户端。
    fn open_preview_target(&mut self, target: OpenTarget, cx: &mut Context<Self>) {
        let Some((id, _)) = self.previewing.session.clone() else {
            return;
        };
        let window = self.window;
        cx.spawn(async move |list, cx| {
            window
                .update(cx, |_, window, cx| {
                    list.update(cx, |list, cx| list.open(id, target, window, cx))
                })
                .ok();
        })
        .detach();
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
            Some((_, PreviewTrigger::Words)) => return,
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

    /// 键盘预览或拆词时换了当前项：等卡片布局稳定后换内容。
    pub(super) fn preview_follow_active(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.previewing.session,
            Some((_, PreviewTrigger::Keyboard | PreviewTrigger::Words))
        ) {
            return;
        }

        self.previewing.follow_timer = Some(cx.spawn(async move |list, cx| {
            cx.background_executor().timer(FOLLOW_DELAY).await;
            list.update(cx, |list, cx| {
                list.previewing.follow_timer = None;
                let trigger = match list.previewing.session {
                    Some((_, trigger @ (PreviewTrigger::Keyboard | PreviewTrigger::Words))) => {
                        trigger
                    }
                    _ => return,
                };
                if let Some(item) = list.active_item() {
                    list.open_preview(item.id.clone(), trigger, cx);
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
        let translation = self.source.translation(id.clone());
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
            // 图片记录（包括正在看图中文字时）没有翻译。
            let translation = match translation.await {
                Ok(view) if image_text.is_none() => view,
                Ok(_) => TranslationView::default(),
                Err(err) => {
                    log::warn!("translation of {id} is unavailable: {err:#}");
                    TranslationView::default()
                }
            };
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
                        list.place_preview(
                            &id,
                            trigger,
                            preview,
                            image_text,
                            translation,
                            window,
                            cx,
                        )
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

    /// 丢弃列表前销毁它独占的预览窗，连隐藏窗口也一起释放。
    pub fn dispose_preview(&mut self, cx: &mut Context<Self>) {
        let mut previewing = std::mem::take(&mut self.previewing);
        self.previewing.request = previewing.request + 1;
        self.previewing.unavailable = true;
        if let Some(preview) = previewing.window.take() {
            preview
                .handle
                .update(cx, |_, window, cx| {
                    preview.panel.update(cx, |panel, cx| {
                        panel.release(window, cx);
                        panel.forget();
                    });
                    window.remove_window();
                })
                .ok();
        }
    }

    /// 关掉预览（返回是否有预览开着）。
    pub fn close_preview(&mut self, cx: &mut Context<Self>) -> bool {
        self.previewing.hover_timer = None;
        self.previewing.hide_timer = None;
        self.previewing.follow_timer = None;
        self.previewing.pointer_inside = false;
        self.previewing.image_text = None;
        self.previewing.placed = None;
        self.previewing.text_view = None;
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

    /// 按住 / 松开修饰键：预览快捷信息里的链接、邮箱跟着变成可点的链接或复原。
    pub(super) fn set_preview_links(&self, on: bool, cx: &mut Context<Self>) {
        if let Some(preview) = &self.previewing.window {
            preview.panel.update(cx, |panel, cx| {
                panel.links = on;
                cx.notify();
            });
        }
    }

    /// 复制预览快捷信息区点中的一段，预览保持打开。
    fn copy_preview_snippet(&mut self, text: Arc<str>, cx: &mut Context<Self>) {
        let Some((id, _)) = self.previewing.session.clone() else {
            return;
        };
        let fragment = ClipboardFragment::Snippet {
            text: text.to_string(),
        };
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
        self.previewing.text_view = None;
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
    #[allow(clippy::too_many_arguments)]
    fn place_preview(
        &mut self,
        id: &Arc<str>,
        trigger: PreviewTrigger,
        preview: Option<Preview>,
        image_text: Option<ImageTextView>,
        translation: TranslationView,
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
        let text_view = self.preview_text_view();
        let measured_metrics = preview.as_ref().map(|preview| {
            let payload = &preview.payload;
            let metrics = match (&preview.metrics, payload.text.as_deref()) {
                // core 按设置里的预览方式量；拆词可能临时换了方式，文本按这次实际的视图重新量。
                (PreviewContentMetrics::Text { .. } | PreviewContentMetrics::Words { .. }, _)
                    if text_view == PreviewTextView::Plain || payload.words.is_empty() =>
                {
                    // core 只能按字数估行数，这里换成按正文字体实际折出的行数。
                    return PreviewContentMetrics::Text {
                        rows: u32::try_from(rows.len()).unwrap_or(u32::MAX),
                    };
                }
                (PreviewContentMetrics::Text { .. }, Some(text)) => {
                    preview::words_metrics(text, &payload.words)
                }
                (metrics, _) => metrics.clone(),
            };
            preview::measure_metrics(
                &metrics,
                payload.text.as_deref(),
                &payload.words,
                text_scale,
                window,
            )
        });
        let snippet_colors = self.settings.clipboard.display.quick_snippet_colors;
        let snippets_height = match &preview {
            Some(preview)
                if matches!(measured_metrics, Some(PreviewContentMetrics::Text { .. })) =>
            {
                let content_width = preview::snippets_content_width(&place.monitor, text_scale);
                let chips: Vec<(f64, usize)> = preview
                    .payload
                    .snippets
                    .iter()
                    .map(|snippet| {
                        preview::snippet_chip(
                            &snippet
                                .label
                                .clone()
                                .map(gpui::SharedString::from)
                                .unwrap_or_else(|| t(snippet_label(snippet.kind))),
                            &snippet.text,
                            snippet_colors,
                            content_width,
                            text_scale,
                            window,
                            cx,
                        )
                    })
                    .collect();
                preview::snippets_height(&chips, content_width)
            }
            _ => 0.,
        };
        // 译文和正文同宽、同字号，折行用同一个函数；高度按面板实际收到的译文区算，两边一致。
        let zone = (translation.translatable || translation.translation.is_some()).then(|| {
            let rows = translation
                .translation
                .as_ref()
                .map(|done| preview::text_rows(&done.text, wrap_width, text_scale, cx))
                .unwrap_or_default();
            TranslationZone {
                view: translation,
                rows,
            }
        });
        let translation_height = zone.as_ref().map_or(0., |zone| {
            preview::translation_height(&zone.view, zone.rows.len())
        });
        let geometry = preview::geometry(
            place.card,
            place.window,
            place.monitor,
            measured_metrics.as_ref(),
            snippets_height,
            translation_height,
            prefer_left,
            f64::from(self.settings.clipboard.preview.gap),
            text_scale,
        );
        let geometry = match &self.previewing.placed {
            Some((placed, previous)) if placed == id => geometry.keep_top(previous, place.monitor),
            _ => geometry,
        };
        self.previewing.placed = Some((id.clone(), geometry));
        let image_box = preview
            .as_ref()
            .and_then(|preview| image_box(preview, geometry.panel, text_scale));
        let preview_window = self.previewing.window.as_ref()?;
        let links = self.key_hints;
        preview_window.panel.update(cx, |panel, cx| {
            panel.snippet_colors = snippet_colors;
            panel.links = links;
            panel.set(preview, rows, text_view, image_box, image_text, zone, cx)
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
