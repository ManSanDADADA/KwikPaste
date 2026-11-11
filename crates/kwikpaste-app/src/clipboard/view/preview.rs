//! 预览窗（1.x `pages/Preview` 与 `window/preview.rs`）：卡片旁边的一个独立的不激活窗口，头部是标题、
//! 说明、文本方式切换和类型标签，内容是大图、长文本（纯文本或选词）、HTML 的结构化文档、RTF 的纯文本、
//! 文件列表。
//!
//! 窗口启动后按需建一次、永不销毁；显示、隐藏、定位只走原生调用（Windows 与主面板一样是
//! `WS_EX_NOACTIVATE` 的 topmost 工具窗口），点它、滚它、选词都不抢前台。原生调用都放在任务里、
//! GPUI 的借用之外（见 `platform::panel` 的说明）。macOS 使用原生不激活 NSPanel。

use std::{ops::Range, rc::Rc, sync::Arc};

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, ClickEvent, Context, DispatchPhase,
    Div, ElementId, Entity, EventEmitter, FontStyle, FontWeight, HighlightStyle, ImageSource,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, ParentElement as _, Render, ScrollHandle, SharedString, Stateful,
    StatefulInteractiveElement as _, StrikethroughStyle, Styled as _, StyledText, UnderlineStyle,
    UniformListScrollHandle, WeakEntity, Window, WindowBounds, WindowKind, WindowOptions, canvas,
    div, img, point, prelude::FluentBuilder as _, px, size, uniform_list,
};
use kwikpaste_core::{
    clipboard::SnippetKind,
    db::models::{ClipboardKind, ClipboardSubKind},
    presenter::{PreviewBlock, PreviewDocument, PreviewInline},
};
use kwikpaste_ui::{
    Button, Icon, IconName, KpStyled as _, MenuEntry, MenuItem, MenuTrigger, dismiss_menu,
    theme::{self, TextSize, px_rems, radius, space},
};

use super::{
    card,
    image_cache::{ImageKey, ImageState, KpImageCache, ResizeMode, path_of},
};
use crate::{
    clipboard::{
        model::{
            actions::OpenTarget,
            preview::{
                FILE_ROW_HEIGHT, HEADER_HEIGHT, RectF, SNIPPET_CHIP_HEIGHT, SNIPPET_CHIP_PADDING_X,
                SNIPPET_PART_GAP, SNIPPETS_MAX_HEIGHT, SNIPPETS_PADDING_Y, TEXT_PADDING_X,
                TEXT_ROW_HEIGHT, WORDS_BAR_HEIGHT, WordSelection, format_bytes, utf16_range,
            },
        },
        source::{Preview, PreviewTextView},
    },
    i18n::{t, t_args, t_count},
    platform::window_drag::WindowDragArea as _,
};

/// 预览窗发给列表的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewEvent {
    /// 指针进出预览窗（悬停预览离开卡片后，指针在预览窗里就不关）。
    Pointer(bool),
    /// 切换文本方式（写进设置）。
    TextView(PreviewTextView),
    /// 选词后点了复制或粘贴。
    Words { paste: bool },
    /// 图片记录在「图片」和「图中文字」之间切换。
    ImageText(ImageTextView),
    /// 点了快捷信息区的一块：只复制这一段。
    Snippet(Arc<str>),
    /// 快捷信息右键菜单里选了粘贴。
    PasteSnippet(Arc<str>),
    /// 中键或按住修饰键点了链接 / 邮箱快捷信息，或在右键菜单里选了打开。
    OpenSnippet(Arc<str>, SnippetKind),
    /// 点了链接、邮箱记录的正文：打开链接或写邮件。
    Open(OpenTarget),
    /// 点了文件记录里的一行：在文件管理器中定位这个文件。
    Reveal(Arc<str>),
    /// 点了图片：在大图预览窗里看。
    ViewImage,
    /// 双击了文本记录的正文：打开内容编辑框。
    EditText,
}

/// 有识别文字的图片记录正在看的那一面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageTextView {
    Image,
    Text,
}

/// 预览窗的内容视图。
pub struct PreviewPanel {
    preview: Option<Preview>,
    /// 记录已经不在了：只显示空状态。
    missing: bool,
    text_view: PreviewTextView,
    rows: Rc<Vec<Range<usize>>>,
    selection: WordSelection,
    /// 图片在面板里的显示尺寸（逻辑像素），由列表按面板尺寸算好。
    image_box: Option<(f32, f32)>,
    /// 图片有识别文字时的「图片 / 文字」切换；`Text` 时 `preview` 是识别文字的文本预览。
    image_text: Option<ImageTextView>,
    /// 快捷信息按类型着色（「快捷信息颜色」设置），列表在 [Self::set] 前更新。
    pub snippet_colors: bool,
    /// 按住修饰键：快捷信息里的链接、邮箱显示成链接，点了打开。列表的数字角标开关时同步。
    pub links: bool,
    images: Entity<KpImageCache>,
    /// 预览文件图标的独立有界缓存；关闭预览时不清空，避免再次打开时闪烁。
    icons: Entity<KpImageCache>,
    text_scroll: UniformListScrollHandle,
    scroll: ScrollHandle,
}

impl EventEmitter<PreviewEvent> for PreviewPanel {}

impl PreviewPanel {
    fn new(cx: &mut Context<Self>) -> Self {
        let images = cx.new(|_| KpImageCache::new());
        let icons = cx.new(|_| KpImageCache::with_capacity(128));
        cx.observe(&images, |_, _, cx| cx.notify()).detach();
        cx.observe(&icons, |_, _, cx| cx.notify()).detach();

        Self {
            preview: None,
            missing: false,
            text_view: PreviewTextView::default(),
            rows: Rc::default(),
            selection: WordSelection::default(),
            image_box: None,
            image_text: None,
            snippet_colors: true,
            links: false,
            images,
            icons,
            text_scroll: UniformListScrollHandle::new(),
            scroll: ScrollHandle::new(),
        }
    }

    /// 换内容：选词清空，滚动回到顶部。`rows` 是纯文本视图按面板宽度折好的行。
    pub fn set(
        &mut self,
        preview: Option<Preview>,
        rows: Vec<Range<usize>>,
        text_view: PreviewTextView,
        image_box: Option<(f32, f32)>,
        image_text: Option<ImageTextView>,
        cx: &mut Context<Self>,
    ) {
        let same_item = self.image_text == image_text
            && matches!(
                (&self.preview, &preview),
                (Some(old), Some(new)) if old.payload.id == new.payload.id
            );
        if !same_item {
            self.selection.clear();
            self.scroll.set_offset(point(px(0.), px(0.)));
            self.text_scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
        self.rows = Rc::new(rows);
        self.missing = preview.is_none();
        self.preview = preview;
        self.text_view = text_view;
        self.image_box = image_box;
        self.image_text = image_text;
        cx.notify();
    }

    pub fn item_id(&self) -> Option<&str> {
        self.preview
            .as_ref()
            .map(|preview| preview.payload.id.as_str())
    }

    /// 选词视图里选中的词（序号升序）；不在选词视图或没选时为空。
    pub fn selected_words(&self) -> Vec<usize> {
        if !self.words_view() {
            return Vec::new();
        }
        self.selection.selected().iter().copied().collect()
    }

    /// 释放图片（预览关上时），关掉开着的快捷信息菜单。
    pub fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        dismiss_menu(window, cx);
        self.selection.clear();
        self.images
            .update(cx, |images, cx| images.clear(Some(window), cx));
    }

    /// 窗口隐藏后丢掉正文：预览窗口只隐藏不关，留着的正文最多可到文本上限（默认 4 MB）。
    /// 不 `notify`，隐藏的窗口不用重画，下次 [`Self::set`] 会换上新内容。
    pub fn forget(&mut self) {
        self.preview = None;
        self.rows = Rc::default();
    }

    fn can_pick_words(&self) -> bool {
        self.preview.as_ref().is_some_and(|preview| {
            preview.payload.kind == ClipboardKind::Text && !preview.payload.words.is_empty()
        })
    }

    fn words_view(&self) -> bool {
        self.text_view == PreviewTextView::Words && self.can_pick_words()
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let payload = self.preview.as_ref().map(|preview| &preview.payload);
        let (title, meta) = match payload {
            None => (t("preview:title.loading"), t("preview:meta.contentViewer")),
            Some(payload) => match payload.kind {
                ClipboardKind::Files => (
                    t_count(
                        "preview:title.files",
                        i64::try_from(payload.total_files).unwrap_or(i64::MAX),
                        &[],
                    ),
                    t_count(
                        "preview:meta.filesLoaded",
                        i64::try_from(payload.files.len()).unwrap_or(i64::MAX),
                        &[],
                    ),
                ),
                ClipboardKind::Image => {
                    let dimensions = match (payload.image_width, payload.image_height) {
                        (Some(width), Some(height)) => format!("{width} x {height}"),
                        _ => t("preview:meta.unknownSize").to_string(),
                    };
                    let size = payload
                        .size
                        .map(|size| format!(" · {}", format_bytes(size)))
                        .unwrap_or_default();
                    (
                        t("preview:title.image"),
                        SharedString::from(format!("{dimensions}{size}")),
                    )
                }
                ClipboardKind::Text if self.image_text == Some(ImageTextView::Text) => {
                    let units = payload
                        .text
                        .as_deref()
                        .map_or(0, |text| text.encode_utf16().count());
                    (
                        t("preview:title.imageText"),
                        t_count(
                            "preview:meta.characters",
                            i64::try_from(units).unwrap_or(i64::MAX),
                            &[],
                        ),
                    )
                }
                ClipboardKind::Text => {
                    let count = payload.size.unwrap_or_else(|| {
                        let units = payload
                            .text
                            .as_deref()
                            .map_or(0, |text| text.encode_utf16().count());
                        i64::try_from(units).unwrap_or(i64::MAX)
                    });
                    (
                        t("preview:title.text"),
                        t_count("preview:meta.characters", count, &[]),
                    )
                }
            },
        };

        div()
            .flex()
            .flex_none()
            .h(space((HEADER_HEIGHT as f32) / 4.))
            .items_center()
            .justify_between()
            .gap(space((12.) / 4.))
            .px(space((16.) / 4.))
            .border_b_1()
            .border_color(tokens.border.divider)
            .child(
                div()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .kp_text(TextSize::Sm)
                            .font_weight(FontWeight::MEDIUM)
                            .window_drag_area()
                            .child(title),
                    )
                    .child(
                        div()
                            .truncate()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .child(meta),
                    ),
            )
            .when_some(payload, |header, payload| {
                header.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(space((8.) / 4.))
                        .when_some(self.image_text, |row, current| {
                            row.child(self.image_text_switch(current, cx))
                        })
                        .when(self.can_pick_words(), |row| row.child(self.view_switch(cx)))
                        // 有「图片 / 文字」切换时它已经说明了类型，不再放类型标签。
                        .when(self.image_text.is_none(), |row| {
                            row.child(
                                div()
                                    .flex()
                                    .h(space((24.) / 4.))
                                    .items_center()
                                    .rounded(radius::SM)
                                    .bg(tokens.fill.subtle)
                                    .px(space((8.) / 4.))
                                    .kp_text(TextSize::Xs)
                                    .text_color(tokens.text.secondary)
                                    .child(t(type_key(payload.kind, payload.sub_kind))),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    /// 文本方式切换（1.x antd `Segmented size="small"`）。
    fn view_switch(&self, cx: &mut Context<Self>) -> AnyElement {
        let segment = |view: PreviewTextView, key: &str, cx: &mut Context<Self>| {
            segment(key, self.text_view == view, cx).on_click(cx.listener(
                move |panel, _, _, cx| {
                    if panel.text_view != view {
                        panel.text_view = view;
                        panel.selection.clear();
                        cx.emit(PreviewEvent::TextView(view));
                        cx.notify();
                    }
                },
            ))
        };

        segmented(
            [
                segment(PreviewTextView::Plain, "preview:view.plain", cx),
                segment(PreviewTextView::Words, "preview:view.words", cx),
            ],
            cx,
        )
    }

    /// 「图片 / 文字」切换：列表按选中的一面重新取内容、重排预览窗。
    fn image_text_switch(&self, current: ImageTextView, cx: &mut Context<Self>) -> AnyElement {
        let segment = |view: ImageTextView, key: &str, cx: &mut Context<Self>| {
            segment(key, current == view, cx).on_click(cx.listener(move |panel, _, _, cx| {
                if panel.image_text != Some(view) {
                    panel.selection.clear();
                    cx.emit(PreviewEvent::ImageText(view));
                }
            }))
        };

        segmented(
            [
                segment(ImageTextView::Image, "preview:view.image", cx),
                segment(ImageTextView::Text, "preview:view.text", cx),
            ],
            cx,
        )
    }

    fn empty(key: &str, cx: &App) -> AnyElement {
        let tokens = theme::semantic(cx);

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h(space((96.) / 4.))
            .items_center()
            .justify_center()
            .gap(space((8.) / 4.))
            .child(
                Icon::new(IconName::Inbox)
                    .size(space((32.) / 4.))
                    .color(tokens.text.faint),
            )
            .child(
                div()
                    .kp_text(TextSize::Sm)
                    .text_color(tokens.text.secondary)
                    .child(t(key)),
            )
            .into_any_element()
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(preview) = self.preview.clone() else {
            return if self.missing {
                Self::empty("preview:empty.content", cx)
            } else {
                div().into_any_element()
            };
        };
        let payload = &preview.payload;

        match payload.kind {
            ClipboardKind::Image => self.image(&preview, window, cx),
            ClipboardKind::Files => self.files(&preview, window, cx),
            ClipboardKind::Text => {
                let text = payload.text.as_deref().unwrap_or_default();
                if text.is_empty() {
                    Self::empty("preview:empty.text", cx)
                } else if self.words_view() {
                    self.words(&preview, cx)
                } else if let Some(document) = &payload.document {
                    self.document(&preview, document, cx)
                } else {
                    self.plain_text(&preview, cx)
                }
            }
        }
    }

    fn image(
        &mut self,
        preview: &Preview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let payload = &preview.payload;
        let (Some(path), true, Some((width, height))) = (
            payload.image_path.as_deref(),
            payload.image_exists,
            self.image_box,
        ) else {
            return Self::empty("preview:empty.imageMissing", cx);
        };
        let scale = window.scale_factor();
        let key = ImageKey {
            modified: None,
            path: path_of(path),
            width: (width * scale).round().max(1.) as u32,
            height: (height * scale).round().max(1.) as u32,
            resize: ResizeMode::Exact,
        };
        let state = self
            .images
            .update(cx, |images, cx| images.request(key, window, cx));

        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .p(space((16.) / 4.))
            .child(match state {
                // 同列表缩略图：圆角，叠一圈不占位置的细描边。点图片看大图。
                ImageState::Ready(image) => div()
                    .id("preview-image")
                    .cursor_pointer()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::ViewImage)))
                    .relative()
                    .flex_none()
                    .w(px(width))
                    .h(px(height))
                    .child(
                        img(ImageSource::Render(image))
                            .size_full()
                            .rounded(radius::SM),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .rounded(radius::SM)
                            .border_1()
                            .border_color(theme::semantic(cx).border.divider),
                    )
                    .into_any_element(),
                ImageState::Loading => div().w(px(width)).h(px(height)).into_any_element(),
                ImageState::Failed => Self::empty("preview:empty.imageMissing", cx),
            })
            .into_any_element()
    }

    fn plain_text(&self, preview: &Preview, cx: &mut Context<Self>) -> AnyElement {
        let text: Arc<str> = Arc::from(preview.payload.text.as_deref().unwrap_or_default());
        let rows = self.rows.clone();
        let colors = theme::components(cx).snippet;
        let marks: Rc<Vec<(Range<usize>, HighlightStyle)>> = Rc::new(if self.snippet_colors {
            preview
                .payload
                .snippets
                .iter()
                .map(|snippet| {
                    let color = card::snippet_color(&colors, snippet.kind);
                    let style = HighlightStyle {
                        background_color: Some(color.background),
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            color: Some(color.foreground),
                            wavy: false,
                        }),
                        ..HighlightStyle::default()
                    };
                    (snippet.range.clone(), style)
                })
                .collect()
        } else {
            Vec::new()
        });

        let list = uniform_list(
            "preview-text",
            rows.len(),
            cx.processor(move |_, range: Range<usize>, _, _| {
                range
                    .map(|index| {
                        let row = rows.get(index).cloned().unwrap_or_default();
                        let line = text
                            .get(row.clone())
                            .filter(|line| !line.is_empty())
                            .unwrap_or(" ");
                        // 高亮按行切开；区间越出这一行实际渲染的文字时不画，免得 StyledText 越界。
                        let highlights: Vec<_> = marks
                            .iter()
                            .filter_map(|(mark, style)| {
                                let start = mark.start.max(row.start);
                                let end = mark.end.min(row.end);
                                (start < end && end - row.start <= line.len())
                                    .then(|| (start - row.start..end - row.start, *style))
                            })
                            .collect();
                        div()
                            .h(space((TEXT_ROW_HEIGHT as f32) / 4.))
                            .px(space((TEXT_PADDING_X as f32) / 4.))
                            .kp_text(TextSize::Sm)
                            .line_height(space((TEXT_ROW_HEIGHT as f32) / 4.))
                            .whitespace_nowrap()
                            .child(
                                StyledText::new(SharedString::from(line.to_owned()))
                                    .with_highlights(highlights),
                            )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.text_scroll)
        .flex_1()
        .min_h_0()
        .py(space((16.) / 4.));

        let link = match preview.payload.sub_kind {
            _ if self.image_text.is_some() => None,
            Some(ClipboardSubKind::Url) => Some(OpenTarget::Link),
            Some(ClipboardSubKind::Email) => Some(OpenTarget::Email),
            _ => None,
        };
        let editable = self.image_text.is_none();
        let list = div()
            .id("preview-text-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(list)
            .map(|body| match link {
                Some(target) => body
                    .cursor_pointer()
                    .text_color(theme::semantic(cx).text.link)
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(PreviewEvent::Open(target)))),
                None if editable => body.on_click(cx.listener(edit_on_double_click)),
                None => body,
            });

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(list)
            .children(self.snippets(preview, cx))
            .into_any_element()
    }

    /// HTML 记录的结构化预览（段落、标题、列表、表格、引用、代码块），下方仍是快捷信息区。
    fn document(
        &self,
        preview: &Preview,
        document: &PreviewDocument,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("preview-document")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_click(cx.listener(edit_on_double_click))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space(2.))
                            .px(space((TEXT_PADDING_X as f32) / 4.))
                            .py(space((16.) / 4.))
                            .kp_text(TextSize::Sm)
                            .children(
                                document
                                    .blocks
                                    .iter()
                                    .map(|block| document_block(block, cx)),
                            ),
                    ),
            )
            .children(self.snippets(preview, cx))
            .into_any_element()
    }

    /// 纯文本下方的快捷信息区：列出全文识别到的每一段，点一下只复制这一段（按住修饰键时链接、
    /// 邮箱是打开），右键在它上方弹出菜单；一行放不下的（多为长链接）折行显示全文。
    /// 片段多时封顶，区内滚动。
    fn snippets(&self, preview: &Preview, cx: &mut Context<Self>) -> Option<AnyElement> {
        let snippets = &preview.payload.snippets;
        if snippets.is_empty() {
            return None;
        }
        let tokens = theme::semantic(cx);
        let colors = self.snippet_colors.then(|| theme::components(cx).snippet);
        // 小块的 id 带上记录：换了记录，上一条开着的右键菜单随旧小块一起消失，不会留着旧内容。
        let chip_id: SharedString = format!("preview-snippet-{}", preview.payload.id).into();

        Some(
            div()
                .id("preview-snippets")
                .flex_none()
                .max_h(space((SNIPPETS_MAX_HEIGHT as f32) / 4.))
                .overflow_y_scroll()
                .py(space((SNIPPETS_PADDING_Y as f32) / 4.))
                .px(space((TEXT_PADDING_X as f32) / 4.))
                .border_t_1()
                .border_color(tokens.border.divider)
                .child(div().flex().flex_wrap().gap(space(1.)).children(
                    snippets.iter().enumerate().map(|(index, snippet)| {
                        let text: Arc<str> = Arc::from(snippet.text.as_str());
                        let kind = snippet.kind;
                        let openable = card::snippet_open_url(kind, &text).is_some();
                        let open = self.links && openable;
                        let open_text = text.clone();
                        let menu_text = text.clone();
                        let panel = cx.entity().downgrade();
                        MenuTrigger::new(ElementId::NamedInteger(chip_id.clone(), index as u64))
                            .flex()
                            .items_start()
                            .gap(space((SNIPPET_PART_GAP as f32) / 4.))
                            .max_w_full()
                            .min_h(space((SNIPPET_CHIP_HEIGHT as f32) / 4.))
                            .py(space(1.))
                            .px(space((SNIPPET_CHIP_PADDING_X as f32) / 4.))
                            .rounded(radius::MD)
                            .bg(tokens.fill.subtle)
                            .kp_text(TextSize::Xs)
                            .hover(|style| style.bg(tokens.fill.default))
                            .when_some(colors, |chip, colors| {
                                chip.child(
                                    card::snippet_dot(card::snippet_color(&colors, snippet.kind))
                                        .mt(space(1.25)),
                                )
                            })
                            .child(
                                div().flex_none().text_color(tokens.text.secondary).child(
                                    snippet
                                        .label
                                        .clone()
                                        .map(SharedString::from)
                                        .unwrap_or_else(|| t(snippet_label(snippet.kind))),
                                ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .when(open, |text| {
                                        text.text_color(tokens.text.link).underline()
                                    })
                                    .child(SharedString::from(snippet.text.clone())),
                            )
                            .when(openable, |chip| {
                                chip.on_mouse_down(
                                    MouseButton::Middle,
                                    cx.listener(move |_, _, _, cx| {
                                        cx.emit(PreviewEvent::OpenSnippet(open_text.clone(), kind));
                                    }),
                                )
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if open {
                                    cx.emit(PreviewEvent::OpenSnippet(text.clone(), kind));
                                } else {
                                    cx.emit(PreviewEvent::Snippet(text.clone()));
                                }
                            }))
                            .right_click_menu(move |_, _| snippet_menu(&panel, &menu_text, kind))
                    }),
                ))
                .into_any_element(),
        )
    }

    fn words(&self, preview: &Preview, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let payload = &preview.payload;
        let text = payload.text.as_deref().unwrap_or_default();
        let mut chips: Vec<AnyElement> = Vec::with_capacity(payload.words.len() + 8);
        let mut previous_end = 0;

        for (index, span) in payload.words.iter().enumerate() {
            let range = utf16_range(text, span.0, span.1);
            let between = text.get(previous_end.min(range.start)..range.start);
            if index > 0 && between.is_some_and(|between| between.contains('\n')) {
                chips.push(div().w_full().h(px(0.)).into_any_element());
            }
            previous_end = range.end;
            let selected = self.selection.is_selected(index);
            let word = SharedString::from(text.get(range).unwrap_or_default().to_owned());

            chips.push(
                div()
                    .id(("preview-word", index))
                    .min_w(space((24.) / 4.))
                    .max_w_full()
                    .px(space((6.) / 4.))
                    .py(space((2.) / 4.))
                    .rounded(radius::MD)
                    .kp_text(TextSize::Sm)
                    .cursor_pointer()
                    .map(|chip| {
                        if selected {
                            chip.bg(tokens.accent.solid)
                                .text_color(tokens.text.on_accent)
                        } else {
                            chip.bg(tokens.fill.subtle)
                                .hover(|style| style.bg(tokens.fill.default))
                        }
                    })
                    .child(div().text_center().child(word))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |panel, _: &MouseDownEvent, _, cx| {
                            panel.selection.press(index);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |panel, event: &MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left)
                            && panel.selection.dragging()
                        {
                            panel.selection.extend(index);
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }

        let count = self.selection.selected().len();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("preview-words")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .content_start()
                            .gap(space(1.))
                            .p(space((16.) / 4.))
                            .children(chips),
                    )
                    .when(payload.words_truncated, |area| {
                        area.child(
                            div()
                                .px(space((16.) / 4.))
                                .pb(space((16.) / 4.))
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.secondary)
                                .child(t("preview:words.truncated")),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(space((8.) / 4.))
                    .h(space((WORDS_BAR_HEIGHT as f32) / 4.))
                    .border_t_1()
                    .border_color(tokens.border.divider)
                    .pr(space((12.) / 4.))
                    .pl(space((16.) / 4.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .child(if count > 0 {
                                t_count(
                                    "preview:words.selected",
                                    i64::try_from(count).unwrap_or(i64::MAX),
                                    &[],
                                )
                            } else {
                                t("preview:words.hint")
                            }),
                    )
                    .when(count > 0, |bar| {
                        bar.child(
                            Button::new("preview-words-clear", t("preview:words.clear"))
                                .small()
                                .ghost()
                                .on_click(cx.listener(|panel, _, _, cx| {
                                    panel.selection.clear();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("preview-words-copy", t("preview:words.copy"))
                                .small()
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(PreviewEvent::Words { paste: false });
                                })),
                        )
                        .child(
                            Button::new("preview-words-paste", t("preview:words.paste"))
                                .small()
                                .primary()
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(PreviewEvent::Words { paste: true });
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn icon(
        &mut self,
        path: &str,
        logical_size: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ImageState {
        let physical = (px_rems(logical_size).to_pixels(window.rem_size()).as_f32()
            * window.scale_factor())
        .ceil()
        .max(1.) as u32;
        let key = ImageKey {
            modified: None,
            path: path_of(path),
            width: physical,
            height: physical,
            resize: ResizeMode::Contain,
        };
        self.icons
            .update(cx, |icons, cx| icons.request(key, window, cx))
    }

    fn files(
        &mut self,
        preview: &Preview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = theme::semantic(cx);
        let payload = &preview.payload;
        if payload.files.is_empty() {
            return Self::empty("preview:empty.files", cx);
        }

        let icon_states: Vec<Option<ImageState>> = payload
            .files
            .iter()
            .map(|file| {
                file.icon_path
                    .as_deref()
                    .map(|path| self.icon(path, 24., window, cx))
            })
            .collect();
        let rows = payload
            .files
            .iter()
            .zip(icon_states)
            .map(|(file, icon_state)| {
                let kind = if file.is_dir {
                    t("preview:file.folder")
                } else {
                    t("preview:file.item")
                };
                let size_label = file
                    .size
                    .map(|size| SharedString::from(format_bytes(size)))
                    .unwrap_or(kind);
                let path = if file.exists {
                    SharedString::from(file.path.clone())
                } else {
                    t("preview:file.missingPath")
                };

                let target: Arc<str> = file.path.as_str().into();
                let reveal = file.exists.then(|| {
                    let target = target.clone();
                    cx.listener(move |_, _, _, cx| cx.emit(PreviewEvent::Reveal(target.clone())))
                });

                div().px(space((8.) / 4.)).child(
                    div()
                        .id(ElementId::Name(target.into()))
                        .flex()
                        .min_h(space((FILE_ROW_HEIGHT as f32) / 4.))
                        .items_center()
                        .gap(space((8.) / 4.))
                        .rounded(radius::MD)
                        .px(space((8.) / 4.))
                        .py(space((6.) / 4.))
                        .when(!file.exists, |row| {
                            row.opacity(theme::components(cx).preview_file.missing_opacity)
                        })
                        .when_some(reveal, |row, reveal| {
                            row.cursor_pointer()
                                .hover(|style| style.bg(tokens.fill.subtle))
                                .on_click(reveal)
                        })
                        .child(match (file.icon_path.is_some(), icon_state) {
                            (true, Some(ImageState::Ready(image))) => {
                                img(ImageSource::Render(image))
                                    .flex_none()
                                    .size(space((24.) / 4.))
                                    .into_any_element()
                            }
                            (true, Some(ImageState::Loading)) => {
                                div().size(space((24.) / 4.)).flex_none().into_any_element()
                            }
                            (true, Some(ImageState::Failed)) | (true, None) => {
                                div().size(space((24.) / 4.)).flex_none().into_any_element()
                            }
                            (false, _) => Icon::new(IconName::Folder)
                                .size(space((20.) / 4.))
                                .color(tokens.text.secondary)
                                .into_any_element(),
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .kp_text(TextSize::Xs)
                                        .when(!file.exists, |name| name.line_through())
                                        .child(file.name.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .kp_text(TextSize::Xs)
                                        .text_color(tokens.text.secondary)
                                        .child(path),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.muted)
                                .child(size_label),
                        ),
                )
            });

        div()
            .id("preview-files")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .py(space((8.) / 4.))
            .children(rows)
            .when(payload.total_files > payload.files.len(), |list| {
                list.child(
                    div()
                        .px(space((16.) / 4.))
                        .py(space((8.) / 4.))
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.secondary)
                        .child(t_args(
                            "preview:file.shownCount",
                            &[
                                ("shown", &payload.files.len().to_string()),
                                ("total", &payload.total_files.to_string()),
                            ],
                        )),
                )
            })
            .into_any_element()
    }
}

/// 类型标签的文案 key（1.x `clipboard:types.{subKind ?? kind}`）。
/// 头部小号分段切换里的一段：选中段在亮色里是浮起的白块（带一层极淡的影），暗色里是亮一档的填充。
/// 双击文本正文进入编辑（图片的单击看大图对应）；单击留给滚动和链接。
fn edit_on_double_click(
    _: &mut PreviewPanel,
    event: &ClickEvent,
    _: &mut Window,
    cx: &mut Context<PreviewPanel>,
) {
    if event.click_count() >= 2 {
        cx.emit(PreviewEvent::EditText);
    }
}

fn segment(key: &str, selected: bool, cx: &App) -> Stateful<Div> {
    let tokens = theme::semantic(cx);
    let thumb = match theme::appearance(cx) {
        theme::Appearance::Light => tokens.surface.panel,
        theme::Appearance::Dark => tokens.fill.default,
    };

    div()
        .id(SharedString::from(format!("preview-view-{key}")))
        .flex()
        .items_center()
        .h(space((20.) / 4.))
        .px(space((8.) / 4.))
        .rounded(radius::XS)
        .kp_text(TextSize::Xs)
        .cursor_pointer()
        .map(|segment| {
            if selected {
                segment
                    .bg(thumb)
                    .shadow(tokens.shadow.card.to_vec())
                    .text_color(tokens.text.primary)
            } else {
                segment
                    .text_color(tokens.text.secondary)
                    .hover(|style| style.text_color(tokens.text.primary))
            }
        })
        .child(t(key))
}

/// 分段切换的底框（1.x antd `Segmented size="small"`）。
fn segmented(segments: impl IntoIterator<Item = Stateful<Div>>, cx: &App) -> AnyElement {
    let tokens = theme::semantic(cx);

    div()
        .flex()
        .gap(space((2.) / 4.))
        .p(space((2.) / 4.))
        .rounded(radius::SM)
        .bg(tokens.fill.subtle)
        .children(segments)
        .into_any_element()
}

/// 快捷信息类型名的文案 key。
pub fn snippet_label(kind: SnippetKind) -> &'static str {
    match kind {
        SnippetKind::Custom => "preview:snippets.kinds.custom",
        SnippetKind::Number => "preview:snippets.kinds.number",
        SnippetKind::Phone => "preview:snippets.kinds.phone",
        SnippetKind::Email => "preview:snippets.kinds.email",
        SnippetKind::Time => "preview:snippets.kinds.time",
        SnippetKind::Link => "preview:snippets.kinds.link",
    }
}

/// 快捷信息的右键菜单：粘贴、复制这一段，链接 / 邮箱还能直接打开。
fn snippet_menu(
    panel: &WeakEntity<PreviewPanel>,
    text: &Arc<str>,
    kind: SnippetKind,
) -> Vec<MenuEntry> {
    let item = |label: &str, event: PreviewEvent| -> MenuEntry {
        let panel = panel.clone();
        MenuItem::new(t(label), move |_, cx| {
            panel.update(cx, |_, cx| cx.emit(event.clone())).ok();
        })
        .into()
    };
    let mut entries = vec![
        item(
            "clipboard:menu.paste",
            PreviewEvent::PasteSnippet(text.clone()),
        ),
        item("clipboard:menu.copy", PreviewEvent::Snippet(text.clone())),
    ];
    let open_label = match kind {
        SnippetKind::Link => Some("clipboard:menu.openLink"),
        SnippetKind::Email => Some("clipboard:menu.sendEmail"),
        _ => None,
    };
    if let Some(label) = open_label.filter(|_| card::snippet_open_url(kind, text).is_some()) {
        entries.push(MenuEntry::Separator);
        entries.push(item(label, PreviewEvent::OpenSnippet(text.clone(), kind)));
    }

    entries
}

/// 预览文档里的一块；列表项和引用里的块递归渲染。
fn document_block(block: &PreviewBlock, cx: &App) -> AnyElement {
    let tokens = theme::semantic(cx);
    let blocks = |blocks: &[PreviewBlock]| {
        div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .children(blocks.iter().map(|block| document_block(block, cx)))
    };

    match block {
        PreviewBlock::Paragraph(runs) => div().child(document_runs(runs, cx)).into_any_element(),
        PreviewBlock::Heading { level, runs } => div()
            .kp_text(if *level == 1 {
                TextSize::Lg
            } else {
                TextSize::Base
            })
            .font_weight(FontWeight::SEMIBOLD)
            .child(document_runs(runs, cx))
            .into_any_element(),
        PreviewBlock::List { ordered, items } => div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .children(items.iter().enumerate().map(|(index, item)| {
                let marker = if *ordered {
                    format!("{}.", index + 1)
                } else {
                    "•".to_owned()
                };
                div()
                    .flex()
                    .gap(space(1.5))
                    .child(
                        div()
                            .flex_none()
                            .text_color(tokens.text.secondary)
                            .child(marker),
                    )
                    .child(blocks(item).flex_1().min_w_0())
            }))
            .into_any_element(),
        PreviewBlock::Table { rows } => div()
            .flex()
            .flex_col()
            .rounded(radius::SM)
            .border_1()
            .border_color(tokens.border.divider)
            .children(rows.iter().enumerate().map(|(index, row)| {
                div()
                    .flex()
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(tokens.border.divider)
                    })
                    .children(row.iter().enumerate().map(|(column, cell)| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(space(2.))
                            .py(space(1.))
                            .when(column > 0, |cell| {
                                cell.border_l_1().border_color(tokens.border.divider)
                            })
                            .child(document_runs(cell, cx))
                    }))
            }))
            .into_any_element(),
        PreviewBlock::Blockquote(quoted) => blocks(quoted)
            .pl(space(3.))
            .border_l_2()
            .border_color(tokens.border.divider)
            .text_color(tokens.text.secondary)
            .into_any_element(),
        PreviewBlock::CodeBlock(code) => div()
            .p(space(2.))
            .rounded(radius::SM)
            .bg(tokens.fill.subtle)
            .kp_mono()
            .child(SharedString::from(code.clone()))
            .into_any_element(),
    }
}

/// 一段行内文字，样式按区间叠加；链接只上色加下划线，不可点、不加载。
fn document_runs(runs: &[PreviewInline], cx: &App) -> StyledText {
    let tokens = theme::semantic(cx);
    let mut text = String::new();
    let mut highlights = Vec::new();

    for run in runs {
        let start = text.len();
        text.push_str(&run.text);
        let style = &run.style;
        let link = style.href.is_some();
        let highlight = HighlightStyle {
            font_weight: style.bold.then_some(FontWeight::BOLD),
            font_style: style.italic.then_some(FontStyle::Italic),
            underline: (style.underline || link).then(|| UnderlineStyle {
                thickness: px(1.),
                ..UnderlineStyle::default()
            }),
            strikethrough: style.strikethrough.then(|| StrikethroughStyle {
                thickness: px(1.),
                ..StrikethroughStyle::default()
            }),
            color: link.then_some(tokens.text.link),
            background_color: style.code.then_some(tokens.fill.subtle),
            ..HighlightStyle::default()
        };
        if highlight != HighlightStyle::default() {
            highlights.push((start..text.len(), highlight));
        }
    }

    StyledText::new(SharedString::from(text)).with_highlights(highlights)
}

fn type_key(kind: ClipboardKind, sub_kind: Option<ClipboardSubKind>) -> &'static str {
    match (sub_kind, kind) {
        (Some(ClipboardSubKind::Rtf), _) => "clipboard:types.rtf",
        (Some(ClipboardSubKind::Html), _) => "clipboard:types.html",
        (Some(ClipboardSubKind::Url), _) => "clipboard:types.url",
        (Some(ClipboardSubKind::Email), _) => "clipboard:types.email",
        (Some(ClipboardSubKind::Color), _) => "clipboard:types.color",
        (Some(ClipboardSubKind::Path), _) => "clipboard:types.path",
        (None, ClipboardKind::Text) => "clipboard:types.text",
        (None, ClipboardKind::Image) => "clipboard:types.image",
        (None, ClipboardKind::Files) => "clipboard:types.files",
    }
}

impl Render for PreviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let content = self.content(window, cx);
        let panel = cx.entity().downgrade();

        div()
            .id("preview-panel")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(crate::platform::material::shell_surface(
                cx,
                tokens.surface.raised,
            ))
            .border_1()
            .border_color(tokens.border.subtle)
            .text_color(tokens.text.primary)
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|panel, _, _, _| panel.selection.release()),
            )
            .child(self.header(cx))
            .child(div().flex_1().min_h_0().relative().child(content))
            .child(pointer_tracker(panel))
    }
}

/// 按整个窗口报告指针进出（[`PreviewEvent::Pointer`]），不看元素命中：快捷信息的右键菜单会挡住
/// 下面的命中，指针在菜单上仍在预览窗里。
fn pointer_tracker(panel: WeakEntity<PreviewPanel>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let moved = panel.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let inside = Bounds::new(point(px(0.), px(0.)), window.viewport_size())
                    .contains(&event.position);
                moved
                    .update(cx, |_, cx| cx.emit(PreviewEvent::Pointer(inside)))
                    .ok();
            });
            let exited = panel.clone();
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    exited
                        .update(cx, |_, cx| cx.emit(PreviewEvent::Pointer(false)))
                        .ok();
                }
            });
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// 预览窗：GPUI 窗口、内容视图和原生句柄。
pub struct PreviewWindow {
    pub handle: AnyWindowHandle,
    pub panel: Entity<PreviewPanel>,
    pub native: Rc<native::NativePreview>,
}

impl PreviewWindow {
    /// 以隐藏状态建窗。原生样式（不激活、topmost）要在借用之外补上，见 [`native::NativePreview::install`]。
    pub fn open(cx: &mut App) -> anyhow::Result<Self> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(320.), px(240.)),
            ))),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            inactive_frame_interval: None,
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            ..Default::default()
        };
        let mut native = None;
        let (handle, panel) = crate::platform::open_window(options, cx, |window, cx| {
            native = Some(native::NativePreview::attach(window));
            cx.new(PreviewPanel::new)
        })?;
        let native =
            native.ok_or_else(|| anyhow::anyhow!("the preview window was not built"))??;

        Ok(Self {
            handle,
            panel,
            native: Rc::new(native),
        })
    }
}

/// 原生预览窗（Windows）：与主面板同一套不激活的工具窗口。
#[cfg(target_os = "windows")]
pub mod native {
    use anyhow::{anyhow, bail};
    use gpui::{Bounds, Pixels, Window};
    use kwikpaste_os::geometry::{Point, Rect};
    use kwikpaste_os::win::{
        monitor::{self, MonitorInfo},
        panel::{self as win_panel, PanelOptions},
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::ScreenPlace;
    use crate::clipboard::model::preview::RectF;

    pub struct NativePreview {
        panel: win_panel::Panel,
    }

    fn hwnd(window: &Window) -> anyhow::Result<isize> {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|err| anyhow!("window handle: {err:?}"))?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            bail!("not a Win32 window");
        };
        Ok(handle.hwnd.get())
    }

    impl NativePreview {
        pub fn attach(window: &Window) -> anyhow::Result<Self> {
            // GPUI 在主线程建窗，预览窗永不销毁。
            Ok(Self {
                panel: unsafe { win_panel::Panel::from_raw(hwnd(window)?) },
            })
        }

        /// 补上不激活、topmost 的样式。要在 GPUI 的借用之外调用。
        pub fn install(&self) {
            if let Err(err) = self.panel.install(PanelOptions {
                min_logical_size: (1., 1.),
                text_scale: 1.,
            }) {
                log::error!("preview window setup failed: {err}");
            }
        }

        /// 把内容区放到 `client`（物理像素）并不激活地显示。要在 GPUI 的借用之外调用。
        pub fn show(&self, client: Rect, dpi: u32) {
            if let Err(err) = self.panel.place(client, dpi) {
                log::warn!("preview window could not be placed: {err}");
                return;
            }
            self.panel.show_without_activating();
        }

        /// 隐藏。要在 GPUI 的借用之外调用。
        pub fn hide(&self) {
            self.panel.hide();
        }

        pub fn is_visible(&self) -> bool {
            self.panel.is_visible()
        }

        /// 预览窗是不是前台窗口（自测核对“不抢前台”）。
        pub fn is_foreground(&self) -> bool {
            kwikpaste_os::win::foreground_window() == self.panel.raw()
        }
    }

    /// 窗口内容区左上角的屏幕坐标（物理像素）。
    fn client_origin(window: &Window) -> Option<Point> {
        let panel = unsafe { win_panel::Panel::from_raw(hwnd(window).ok()?) };
        let rect = panel.client_rect().ok()?;
        Some(Point {
            x: rect.left,
            y: rect.top,
        })
    }

    /// 包含这个点（物理像素）的显示器；都不包含时取第一块。
    fn monitor_at(point: Point) -> Option<MonitorInfo> {
        let monitors = monitor::all();
        monitors
            .iter()
            .copied()
            .find(|info| {
                let rect = info.monitor;
                point.x >= rect.left
                    && point.x < rect.right
                    && point.y >= rect.top
                    && point.y < rect.bottom
            })
            .or_else(|| monitors.first().copied())
    }

    /// 面板窗口里 `card`（逻辑像素）所在显示器工作区上的几何（不含任务栏，预览窗不被它挡住）。
    pub fn screen_place(window: &Window, card: Bounds<Pixels>) -> Option<ScreenPlace> {
        let origin = client_origin(window)?;
        let scale = f64::from(window.scale_factor());
        // 面板里的逻辑矩形换成屏幕物理像素 (left, top, width, height)。
        let to_screen = |bounds: Bounds<Pixels>| {
            let physical = |value: Pixels| f64::from(value.as_f32()) * scale;
            (
                f64::from(origin.x) + physical(bounds.origin.x),
                f64::from(origin.y) + physical(bounds.origin.y),
                physical(bounds.size.width),
                physical(bounds.size.height),
            )
        };
        let (left, top, width, height) = to_screen(card);
        let monitor = monitor_at(Point {
            x: (left + width / 2.) as i32,
            y: (top + height / 2.) as i32,
        })?;
        let work = monitor.work_area;
        let monitor_scale = monitor.scale();
        let to_monitor = |(left, top, width, height): (f64, f64, f64, f64)| RectF {
            left: (left - f64::from(work.left)) / monitor_scale,
            top: (top - f64::from(work.top)) / monitor_scale,
            width: width / monitor_scale,
            height: height / monitor_scale,
        };

        Some(ScreenPlace {
            card: to_monitor((left, top, width, height)),
            window: to_monitor(to_screen(Bounds::new(
                gpui::Point::default(),
                window.viewport_size(),
            ))),
            monitor: RectF {
                left: 0.,
                top: 0.,
                width: f64::from(work.width()) / monitor_scale,
                height: f64::from(work.height()) / monitor_scale,
            },
            origin: (work.left, work.top),
            scale: monitor_scale,
            dpi: monitor.dpi,
        })
    }
}

/// 卡片所在显示器上的几何：卡片、主窗口与显示器都是以显示器（Windows 上是工作区）左上角为原点的
/// 逻辑像素。
pub struct ScreenPlace {
    pub card: RectF,
    /// 卡片所在的主窗口（内容区）。
    pub window: RectF,
    pub monitor: RectF,
    /// 显示器（Windows 上是工作区）左上角的屏幕坐标（物理像素）。
    pub origin: (i32, i32),
    /// 显示器的缩放比例。
    pub scale: f64,
    pub dpi: u32,
}

impl ScreenPlace {
    /// 显示器上的逻辑矩形换成屏幕上的物理矩形。
    pub fn to_screen(&self, rect: RectF) -> kwikpaste_os::geometry::Rect {
        let to_physical = |value: f64| (value * self.scale).round() as i32;
        let left = self.origin.0 + to_physical(rect.left);
        let top = self.origin.1 + to_physical(rect.top);
        kwikpaste_os::geometry::Rect {
            left,
            top,
            right: left + to_physical(rect.width),
            bottom: top + to_physical(rect.height),
        }
    }
}

#[cfg(target_os = "macos")]
pub mod native {
    use anyhow::{anyhow, bail};
    use gpui::{Bounds, Pixels, Window};
    use kwikpaste_os::geometry::{Rect, Size};
    use kwikpaste_os::mac::{monitor, panel as mac_panel};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{RectF, ScreenPlace};

    pub struct NativePreview {
        panel: mac_panel::Panel,
    }

    /// 面板窗口里 `card`（逻辑像素，macOS 上就是 point）所在显示器上的几何。
    ///
    /// 全程用 point、以主屏左上角为原点（与 [`monitor::screens`] 相同）；只有交给
    /// [`ScreenPlace::to_screen`] 的显示器原点按缩放乘成像素，`place_rect` 再按同一缩放除回来。
    pub fn screen_place(window: &Window, card: Bounds<Pixels>) -> Option<ScreenPlace> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        let panel = unsafe { mac_panel::Panel::from_raw(handle.ns_view) };
        let frame = panel.frame()?;
        let screens = monitor::screens();
        let primary_height = screens.first()?.height;
        let left = frame.origin.x + f64::from(card.origin.x.as_f32());
        let top = primary_height - (frame.origin.y + frame.size.height)
            + f64::from(card.origin.y.as_f32());
        let width = f64::from(card.size.width.as_f32());
        let height = f64::from(card.size.height.as_f32());
        let monitor = screens
            .iter()
            .find(|screen| {
                left + width / 2. >= screen.x
                    && left + width / 2. < screen.x + screen.width
                    && top + height / 2. >= screen.y
                    && top + height / 2. < screen.y + screen.height
            })
            .or_else(|| screens.first())?;
        let monitor_scale = monitor.scale;
        Some(ScreenPlace {
            card: RectF {
                left: left - monitor.x,
                top: top - monitor.y,
                width,
                height,
            },
            window: RectF {
                left: frame.origin.x - monitor.x,
                top: primary_height - (frame.origin.y + frame.size.height) - monitor.y,
                width: frame.size.width,
                height: frame.size.height,
            },
            monitor: RectF {
                left: 0.,
                top: 0.,
                width: monitor.width,
                height: monitor.height,
            },
            origin: (
                (monitor.x * monitor_scale).round() as i32,
                (monitor.y * monitor_scale).round() as i32,
            ),
            scale: monitor_scale,
            dpi: (monitor_scale * 96.).round() as u32,
        })
    }

    impl NativePreview {
        pub fn attach(window: &Window) -> anyhow::Result<Self> {
            let handle = HasWindowHandle::window_handle(window)
                .map_err(|err| anyhow!("preview window handle: {err:?}"))?;
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                bail!("not an AppKit preview window");
            };
            Ok(Self {
                panel: unsafe { mac_panel::Panel::from_raw(handle.ns_view) },
            })
        }

        pub fn install(&self) {
            if let Err(err) = self.panel.install_preview(Size {
                width: 1,
                height: 1,
            }) {
                log::error!("preview window setup failed: {err}");
            }
        }

        pub fn show(&self, rect: Rect, dpi: u32) {
            if let Err(err) = self.panel.place_rect(rect, dpi) {
                log::warn!("preview window could not be placed: {err}");
                return;
            }
            self.panel.show_without_activating();
        }

        pub fn hide(&self) {
            self.panel.hide();
        }

        pub fn is_visible(&self) -> bool {
            self.panel.is_visible()
        }

        pub fn is_foreground(&self) -> bool {
            self.panel.is_key() && self.panel.application_is_active()
        }
    }
}
