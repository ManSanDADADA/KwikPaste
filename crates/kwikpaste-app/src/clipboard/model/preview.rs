//! 预览窗的模型（1.x `window/preview.rs` 的几何与 `pages/Preview` 的排版规则）：面板尺寸按内容度量算，
//! 放在卡片的左右（按指针在主窗口的哪一半）或上下，夹在显示器内；文本按面板宽度和正文字体折成行；
//! 选词按起点到当前词连续勾选或取消。除了用文字系统量宽的几个函数，都是纯函数，可以 headless 单测。

use std::{collections::BTreeSet, ops::Range};

use gpui::{App, FontWeight, ScrollDelta, TextRun, Window, font, px};
use kwikpaste_core::{
    clipboard::WordSpan,
    presenter::{PreviewContentMetrics, PreviewWordChip},
    translate::{TranslationState, TranslationView},
};
use kwikpaste_ui::theme::{TextSize, fonts};

/// 面板离显示器边缘至少这么远。
const MARGIN: f64 = 32.;
const MIN_WIDTH: f64 = 288.;
const MAX_WIDTH: f64 = 480.;
const MIN_HEIGHT: f64 = 96.;
const MAX_HEIGHT: f64 = 480.;
/// 面板头部（标题、说明、类型标签）的高度。
pub const HEADER_HEIGHT: f64 = 48.;
const IMAGE_PADDING_X: f64 = 32.;
const IMAGE_PADDING_Y: f64 = 32.;
const EMPTY_HEIGHT: f64 = 96.;
/// 纯文本视图一行的高度（`leading-5.5`）。
pub const TEXT_ROW_HEIGHT: f64 = 22.;
const TEXT_VERTICAL_PADDING: f64 = 32.;
/// 纯文本视图左右各自的内边距。
pub const TEXT_PADDING_X: f64 = 16.;
/// 文件视图一行的高度。
pub const FILE_ROW_HEIGHT: f64 = 40.;
const FILE_VERTICAL_PADDING: f64 = 16.;
const FILE_MORE_HEIGHT: f64 = 40.;
const WORD_CHIP_HEIGHT: f64 = 24.;
const WORD_LINE_HEIGHT: f64 = 20.;
/// 词块之间的间距。
pub const WORD_GAP: f64 = 4.;
/// 词块区的内边距。
pub const WORDS_PADDING: f64 = 16.;
/// 选词视图底部操作栏的高度（1px 分隔线 + 上下各 8px + 小号按钮 24px），一直占位，选词前后布局不跳。
pub const WORDS_BAR_HEIGHT: f64 = 41.;
/// 选词区左右再扣掉的宽度：预览窗两侧各 1px 边框，加 1px 余量（GPUI 按设备像素取整，
/// 词块实际会比测得的略宽；宁可多留一行空白也不能裁掉最后一行）。
const WORDS_EDGE: f64 = 3.;
const FALLBACK_SIZE: (f64, f64) = (320., 240.);
/// 预览快捷信息区的上下内边距。
pub const SNIPPETS_PADDING_Y: f64 = 12.;
/// 快捷信息小块的高度、左右内边距、圆点直径，以及圆点、类型名、内容之间的间距。
pub const SNIPPET_CHIP_HEIGHT: f64 = 24.;
pub const SNIPPET_CHIP_PADDING_X: f64 = 8.;
pub const SNIPPET_DOT: f64 = 6.;
pub const SNIPPET_PART_GAP: f64 = 4.;
/// 快捷信息内容一行的高度（`text-xs` 的行高）。
pub const SNIPPET_LINE_HEIGHT: f64 = 16.;
/// 快捷信息区最高这么多（含分隔线）：三行整块再露出第四行的一半，看得出下面还有、可以滚动，
/// 片段再多也给正文留出位置。
pub const SNIPPETS_MAX_HEIGHT: f64 =
    1. + SNIPPETS_PADDING_Y + 3.5 * SNIPPET_CHIP_HEIGHT + 3. * WORD_GAP;
/// 预览图最多放大到适应预览窗时的几倍（实际还受显示器工作区限制）。
pub const MAX_ZOOM: f64 = 8.;

/// 预览窗译文区工具栏的高度（来源标签和小号按钮）。
pub const TRANSLATION_BAR_HEIGHT: f64 = 40.;
/// 译文正文最多显示的行数，超出在区内滚动。
pub const TRANSLATION_MAX_ROWS: usize = 8;
/// 译文正文下方的内边距。
pub const TRANSLATION_PADDING_BOTTOM: f64 = 12.;
/// 译文正文最高这么多（纯文本 [`TRANSLATION_MAX_ROWS`] 行），选词视图的词块区也一样，超出在区内滚动。
pub const TRANSLATION_BODY_MAX_HEIGHT: f64 =
    TRANSLATION_MAX_ROWS as f64 * TEXT_ROW_HEIGHT + TRANSLATION_PADDING_BOTTOM;

/// 逻辑像素的矩形。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl RectF {
    fn right(&self) -> f64 {
        self.left + self.width
    }

    fn bottom(&self) -> f64 {
        self.top + self.height
    }

    fn center_x(&self) -> f64 {
        self.left + self.width / 2.
    }

    fn center_y(&self) -> f64 {
        self.top + self.height / 2.
    }

    fn inset(&self, amount: f64) -> Self {
        Self {
            left: self.left + amount,
            top: self.top + amount,
            width: (self.width - amount * 2.).max(1.),
            height: (self.height - amount * 2.).max(1.),
        }
    }

    fn intersect(&self, other: &Self) -> Option<Self> {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > left && bottom > top).then_some(Self {
            left,
            top,
            width: right - left,
            height: bottom - top,
        })
    }

    fn clamp_into(&self, bounds: &Self) -> Self {
        let max_left = (bounds.right() - self.width).max(bounds.left);
        let max_top = (bounds.bottom() - self.height).max(bounds.top);
        Self {
            left: self.left.clamp(bounds.left, max_left),
            top: self.top.clamp(bounds.top, max_top),
            ..*self
        }
    }
}

/// 面板在卡片的哪一边。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Right,
    Left,
    Bottom,
    Top,
}

/// 预览窗的位置与大小（显示器上的逻辑像素）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub panel: RectF,
    pub placement: Placement,
}

impl Geometry {
    /// 同一条预览换视图（图片 / 文字、默认 / 选词）时沿用上一次的顶边，头部的切换按钮留在指针下面；
    /// 只在仍放在卡片同一侧的左右时生效，超出显示器照常夹回。
    pub fn keep_top(self, previous: &Geometry, monitor: RectF) -> Self {
        let side = matches!(self.placement, Placement::Left | Placement::Right);
        if !side || self.placement != previous.placement {
            return self;
        }
        let panel = RectF {
            top: previous.panel.top,
            ..self.panel
        }
        .clamp_into(&monitor.inset(MARGIN));

        Self { panel, ..self }
    }
}

/// 算预览窗的位置：`card` 是卡片、`window` 是卡片所在的主窗口、`monitor` 是显示器工作区（同一坐标系的
/// 逻辑像素），`metrics` 为 `None` 表示记录已经不在了（只剩空状态）。`prefer_left`：指针在主窗口左半边。
/// `gap` 是面板与卡片的距离。`scale` 是文本缩放，面板内容随它放大，尺寸上下限和间距也跟着放大。
/// `translation` 是正文下方译文区的高度。`zoom` 是图片在适应预览窗之上再放大的倍数（Mod+滚轮），
/// 只作用于图片。
#[allow(clippy::too_many_arguments)]
pub fn geometry(
    card: RectF,
    window: RectF,
    monitor: RectF,
    metrics: Option<&PreviewContentMetrics>,
    snippets: f64,
    translation: f64,
    prefer_left: bool,
    gap: f64,
    scale: f64,
    zoom: f64,
) -> Geometry {
    let available = monitor.inset(MARGIN);
    let source = card
        .intersect(&monitor)
        .unwrap_or_else(|| card.clamp_into(&monitor));
    let size = panel_size(metrics, snippets, translation, &available, scale, zoom);
    let gap = gap * scale;
    let placement = placement(&source, &window, &available, size, prefer_left, gap);
    let panel = raw_panel(&anchor(&source, &window, placement), placement, size, gap)
        .clamp_into(&available);

    Geometry { panel, placement }
}

/// 用 GPUI 实际使用的正文文字系统修正选词词块宽度。
///
/// core 在没有窗口的线程上只能用字符宽度估算；预览窗开在 UI 线程，
/// 这里用同一套字体、回退字体和字号重新排一遍，保证定尺寸和实际渲染一致。
pub fn measure_metrics(
    metrics: &PreviewContentMetrics,
    text: Option<&str>,
    words: &[WordSpan],
    scale: f64,
    window: &Window,
) -> PreviewContentMetrics {
    let PreviewContentMetrics::Words { chips } = metrics else {
        return metrics.clone();
    };
    let Some(text) = text else {
        return metrics.clone();
    };

    let font_size = px(TextSize::Sm.font_size().0 * 16. * scale as f32);
    let mut measured = chips.clone();
    for ((span, _chip), measured_chip) in words.iter().zip(chips).zip(&mut measured) {
        let range = utf16_range(text, span.0, span.1);
        let Some(word) = text.get(range) else {
            continue;
        };
        let mut ui_font = font(fonts::UI_FAMILY);
        ui_font.fallbacks = Some(fonts::ui_fallbacks());
        ui_font.weight = FontWeight::NORMAL;
        let run = TextRun {
            len: word.len(),
            font: ui_font,
            ..TextRun::default()
        };
        let width = window
            .text_system()
            .layout_line(word, font_size, &[run], None)
            .width
            .as_f32() as f64;
        measured_chip.width = (width / scale + 12.).ceil().max(24.);
    }

    PreviewContentMetrics::Words { chips: measured }
}

/// 选词视图的度量：一个词一块，前面隔着换行就另起一行（与预览窗的渲染一致）；宽度先估，
/// 再由 [`measure_metrics`] 按实际字体量。
pub fn words_metrics(text: &str, words: &[WordSpan]) -> PreviewContentMetrics {
    let mut previous_end = 0;
    let chips = words
        .iter()
        .enumerate()
        .map(|(index, span)| {
            let range = utf16_range(text, span.0, span.1);
            let line_break = index > 0
                && text
                    .get(previous_end.min(range.start)..range.start)
                    .is_some_and(|between| between.contains('\n'));
            previous_end = range.end;
            PreviewWordChip::new(text.get(range).unwrap_or_default(), line_break)
        })
        .collect();

    PreviewContentMetrics::Words { chips }
}

/// 按内容度量算面板尺寸，规则与面板的渲染一一对应。`snippets` 是纯文本下方快捷信息区的高度，
/// `translation` 是正文下方译文区的高度。
fn panel_size(
    metrics: Option<&PreviewContentMetrics>,
    snippets: f64,
    translation: f64,
    available: &RectF,
    scale: f64,
    zoom: f64,
) -> (f64, f64) {
    let zoom = match metrics {
        Some(PreviewContentMetrics::Image { .. }) => zoom,
        _ => 1.,
    };
    let max_width = (MAX_WIDTH * scale * zoom).min(available.width);
    let max_height = (MAX_HEIGHT * scale * zoom).min(available.height);
    let min_width = (MIN_WIDTH * scale).min(max_width);
    let min_height = (MIN_HEIGHT * scale).min(max_height);
    let header = HEADER_HEIGHT * scale;

    let (width, height) = match metrics {
        None => (max_width, header + EMPTY_HEIGHT * scale),
        Some(PreviewContentMetrics::Text { rows }) => (
            max_width,
            header + (text_height(*rows) + snippets + translation) * scale,
        ),
        Some(PreviewContentMetrics::Words { chips }) => {
            let widths: Vec<(f64, bool)> = chips
                .iter()
                .map(|chip| (chip.width, chip.line_break))
                .collect();
            let content = max_width / scale - WORDS_PADDING * 2. - WORDS_EDGE;
            (
                max_width,
                header + (words_height(&widths, content) + WORDS_BAR_HEIGHT + translation) * scale,
            )
        }
        Some(PreviewContentMetrics::Files { shown, total }) => {
            (max_width, header + files_height(*shown, *total) * scale)
        }
        Some(PreviewContentMetrics::Image { width, height }) => {
            image_size(*width, *height, max_width, max_height, scale, zoom)
        }
    };

    (
        width.clamp(min_width, max_width),
        height.clamp(min_height, max_height),
    )
}

/// 快捷信息小块和译文词块折行的内容宽度（设计 px）：与纯文本同宽，再扣掉两侧边框和取整余量。
pub fn chips_content_width(monitor: &RectF, scale: f64) -> f64 {
    (MAX_WIDTH * scale).min(monitor.inset(MARGIN).width) / scale - TEXT_PADDING_X * 2. - WORDS_EDGE
}

/// 一个快捷信息小块的宽度（设计 px）和内容行数：按 `text-xs` 量类型名和内容，`dot` 时前面多一个圆点。
/// 比 `content_width` 宽的小块（多为长链接）占满一行，内容在余下的宽度里折行。
pub fn snippet_chip(
    label: &str,
    text: &str,
    dot: bool,
    content_width: f64,
    scale: f64,
    window: &Window,
    cx: &App,
) -> (f64, usize) {
    let font_size = px(TextSize::Xs.font_size().0 * 16. * scale as f32);
    let measure = |line: &str| {
        let mut ui_font = font(fonts::UI_FAMILY);
        ui_font.fallbacks = Some(fonts::ui_fallbacks());
        let run = TextRun {
            len: line.len(),
            font: ui_font,
            ..TextRun::default()
        };
        window
            .text_system()
            .layout_line(line, font_size, &[run], None)
            .width
            .as_f32() as f64
            / scale
    };
    let dot = if dot {
        SNIPPET_DOT + SNIPPET_PART_GAP
    } else {
        0.
    };

    let fixed = SNIPPET_CHIP_PADDING_X * 2. + dot + measure(label) + SNIPPET_PART_GAP;
    let width = (fixed + measure(text) + 1.).ceil();
    // 带换行的内容按行画，同样要数行。
    if width <= content_width && !text.contains('\n') {
        return (width, 1);
    }

    let wrap_width = px(((content_width - fixed) * scale) as f32);
    (
        width,
        kwikpaste_ui::wrap_rows(text, wrap_width, font_size, cx).len(),
    )
}

/// 快捷信息区的高度（含顶部 1px 分隔线），`chips` 是每块的宽度和内容行数（见 [`snippet_chip`]）。
/// 小块按 `content_width` 折行，折成多行的小块独占一行；最高 [`SNIPPETS_MAX_HEIGHT`]，没有小块时为 0。
pub fn snippets_height(chips: &[(f64, usize)], content_width: f64) -> f64 {
    if chips.is_empty() {
        return 0.;
    }

    let mut rows = 1_u32;
    let mut extra_lines = 0;
    let mut line_width = 0.;
    for (index, &(chip, lines)) in chips.iter().enumerate() {
        extra_lines += lines.saturating_sub(1);
        let width = chip.min(content_width);
        if index == 0 {
            line_width = width;
        } else if line_width + WORD_GAP + width > content_width {
            rows += 1;
            line_width = width;
        } else {
            line_width += WORD_GAP + width;
        }
    }

    (1. + SNIPPETS_PADDING_Y * 2.
        + f64::from(rows) * SNIPPET_CHIP_HEIGHT
        + f64::from(rows - 1) * WORD_GAP
        + extra_lines as f64 * SNIPPET_LINE_HEIGHT)
        .min(SNIPPETS_MAX_HEIGHT)
}

/// 译文区的高度（含顶部 1px 分隔线）。`body` 是译文正文的高度（[`translation_rows_height`] 或
/// [`translation_words_height`]），没有译文时不算。没有译文、也不在翻译中或失败时没有译文区，高度为 0。
pub fn translation_height(view: &TranslationView, body: f64) -> f64 {
    if view.translation.is_none() && view.state == TranslationState::Idle {
        return 0.;
    }
    let body = if view.translation.is_some() { body } else { 0. };
    1. + TRANSLATION_BAR_HEIGHT + body
}

/// 纯文本译文的高度：`rows` 是译文按面板宽度折出的行数。
pub fn translation_rows_height(rows: usize) -> f64 {
    rows.clamp(1, TRANSLATION_MAX_ROWS) as f64 * TEXT_ROW_HEIGHT + TRANSLATION_PADDING_BOTTOM
}

/// 选词视图里译文词块区的高度：和正文词块一样折行，上方紧贴工具栏、下方留同样的内边距。
pub fn translation_words_height(chips: &[PreviewWordChip], content_width: f64) -> f64 {
    let chips: Vec<(f64, bool)> = chips
        .iter()
        .map(|chip| (chip.width, chip.line_break))
        .collect();
    (chip_rows_height(&chips, content_width) + TRANSLATION_PADDING_BOTTOM)
        .min(TRANSLATION_BODY_MAX_HEIGHT)
}

fn text_height(rows: u32) -> f64 {
    if rows == 0 {
        return EMPTY_HEIGHT;
    }
    f64::from(rows) * TEXT_ROW_HEIGHT + TEXT_VERTICAL_PADDING
}

/// 拆词视图按内容宽度模拟 flex 折行（1.x `words_content_height`）：`chips` 是（估算宽度、前面是否换段）。
fn words_height(chips: &[(f64, bool)], content_width: f64) -> f64 {
    if chips.is_empty() || content_width <= 0. {
        return EMPTY_HEIGHT;
    }
    chip_rows_height(chips, content_width) + WORDS_PADDING * 2.
}

/// 词块折行后的高度，不含内边距；没有词块时为 0。
fn chip_rows_height(chips: &[(f64, bool)], content_width: f64) -> f64 {
    if chips.is_empty() || content_width <= 0. {
        return 0.;
    }

    let mut rows = 0_u32;
    let mut breaks = 0_u32;
    let mut wrapped_lines = 0.;
    let mut line_width = 0.;
    for &(chip_width, line_break) in chips {
        let width = chip_width.min(content_width);
        wrapped_lines += (chip_width / content_width).ceil().max(1.) - 1.;
        if rows == 0 {
            rows = 1;
            line_width = width;
        } else if line_break {
            breaks += 1;
            rows += 1;
            line_width = width;
        } else if line_width + WORD_GAP + width > content_width {
            rows += 1;
            line_width = width;
        } else {
            line_width += WORD_GAP + width;
        }
    }

    f64::from(rows) * WORD_CHIP_HEIGHT
        + f64::from(rows - 1 + breaks) * WORD_GAP
        + wrapped_lines * WORD_LINE_HEIGHT
}

fn files_height(shown: u32, total: u32) -> f64 {
    if shown == 0 {
        return EMPTY_HEIGHT;
    }
    let footer = if total > shown { FILE_MORE_HEIGHT } else { 0. };
    f64::from(shown) * FILE_ROW_HEIGHT + FILE_VERTICAL_PADDING + footer
}

/// 图片按原始宽高等比缩到面板上限内，面板正好裹住缩放后的图加内边距。
fn image_size(
    width: Option<f64>,
    height: Option<f64>,
    max_width: f64,
    max_height: f64,
    scale: f64,
    zoom: f64,
) -> (f64, f64) {
    let fallback = (FALLBACK_SIZE.0 * scale, FALLBACK_SIZE.1 * scale);
    let (Some(width), Some(height)) = (width, height) else {
        return fallback;
    };
    if width <= 0. || height <= 0. {
        return fallback;
    }

    let padding_x = IMAGE_PADDING_X * scale;
    let padding_y = (HEADER_HEIGHT + IMAGE_PADDING_Y) * scale;
    let fit = zoom
        .min((max_width - padding_x).max(1.) / width)
        .min((max_height - padding_y).max(1.) / height);

    (
        (width * fit).ceil() + padding_x,
        (height * fit).ceil() + padding_y,
    )
}

/// Mod+滚轮一下的缩放倍数，与大图预览窗一致：滚轮一行 1.08 倍（Windows 一格 3 行约 1.26 倍），
/// 触控板按像素连续缩放。
pub fn wheel_zoom(delta: ScrollDelta) -> f64 {
    match delta {
        ScrollDelta::Lines(delta) => 1.08_f64.powf(f64::from(delta.y)),
        ScrollDelta::Pixels(delta) => (f64::from(delta.y.as_f32()) / 200.).exp(),
    }
}

/// 按偏好的一侧依次找放得下的方位：偏好侧、另一侧、主窗口下方、主窗口上方。都放不下时夹进显示器，
/// 取盖住主窗口最少的方位（一样多时按前面的顺序），比如主窗口居中在竖屏上时贴到屏幕边上。
fn placement(
    source: &RectF,
    window: &RectF,
    available: &RectF,
    size: (f64, f64),
    prefer_left: bool,
    gap: f64,
) -> Placement {
    let (preferred, opposite) = if prefer_left {
        (Placement::Left, Placement::Right)
    } else {
        (Placement::Right, Placement::Left)
    };
    let (width, height) = size;
    let order = [preferred, opposite, Placement::Bottom, Placement::Top];
    let fits = |placement: Placement| {
        let anchor = anchor(source, window, placement);
        match placement {
            Placement::Right => anchor.right() + gap + width <= available.right(),
            Placement::Left => anchor.left - gap - width >= available.left,
            Placement::Bottom => anchor.bottom() + gap + height <= available.bottom(),
            Placement::Top => anchor.top - gap - height >= available.top,
        }
    };
    if let Some(placement) = order.into_iter().find(|placement| fits(*placement)) {
        return placement;
    }

    let covered = |placement: Placement| {
        raw_panel(&anchor(source, window, placement), placement, size, gap)
            .clamp_into(available)
            .intersect(window)
            .map_or(0., |overlap| overlap.width * overlap.height)
    };
    order
        .into_iter()
        .min_by(|a, b| covered(*a).total_cmp(&covered(*b)))
        .unwrap_or(preferred)
}

/// 预览窗贴着放的矩形：左右贴卡片（与卡片垂直居中）；上下贴主窗口，不盖住卡片下面的列表。
fn anchor(source: &RectF, window: &RectF, placement: Placement) -> RectF {
    match placement {
        Placement::Left | Placement::Right => *source,
        Placement::Bottom | Placement::Top => RectF {
            top: window.top,
            height: window.height,
            ..*source
        },
    }
}

fn raw_panel(source: &RectF, placement: Placement, (width, height): (f64, f64), gap: f64) -> RectF {
    let centered_top = source.center_y() - height / 2.;
    let centered_left = source.center_x() - width / 2.;
    let (left, top) = match placement {
        Placement::Right => (source.right() + gap, centered_top),
        Placement::Left => (source.left - gap - width, centered_top),
        Placement::Bottom => (centered_left, source.bottom() + gap),
        Placement::Top => (centered_left, source.top - gap - height),
    };

    RectF {
        left,
        top,
        width,
        height,
    }
}

/// 纯文本视图的折行宽度：文本内容的面板与显示器能给的最大宽度一样，再减去两侧 1 px 边框和左右内边距。
pub fn text_wrap_width(monitor: &RectF, scale: f64) -> f64 {
    let panel = (MAX_WIDTH * scale).min(monitor.inset(MARGIN).width);
    panel - 2. - TEXT_PADDING_X * 2. * scale
}

/// 纯文本视图的行：按换行拆开，每行再按正文字体（`text-sm`）在 `width` 内折开，不把句末标点放到行首。
/// 返回每行的字节区间。
pub fn text_rows(text: &str, width: f64, scale: f64, cx: &App) -> Vec<Range<usize>> {
    let font_size = px(TextSize::Sm.font_size().0 * 16. * scale as f32);
    kwikpaste_ui::wrap_rows(text, px(width as f32), font_size, cx)
}

/// UTF-16 区间（core 的词区间）换成字节区间；越界时截到文本末尾。
pub fn utf16_range(text: &str, start: u32, end: u32) -> Range<usize> {
    let (start, end) = (start as usize, end as usize);
    let mut units = 0;
    let mut byte_start = text.len();
    let mut byte_end = text.len();
    for (offset, c) in text.char_indices() {
        if units >= start && byte_start == text.len() {
            byte_start = offset;
        }
        if units >= end {
            byte_end = offset;
            break;
        }
        units += c.len_utf16();
    }
    byte_start.min(byte_end)..byte_end
}

/// 字节数的显示（1.x `formatBytes`）：1024 进位，10 以下保留一位小数。
pub fn format_bytes(value: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value.max(0) as f64;
    let mut unit = 0;
    while size >= 1024. && unit < UNITS.len() - 1 {
        size /= 1024.;
        unit += 1;
    }
    let digits = if unit == 0 || size >= 10. { 0 } else { 1 };
    format!(
        "{size:.digits$} {}",
        UNITS.get(unit).copied().unwrap_or("B")
    )
}

/// 选词（1.x `useWordSelection`）：按下的词决定这一笔是勾选还是取消，拖过的区间跟着变。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordSelection {
    selected: BTreeSet<usize>,
    drag: Option<WordDrag>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WordDrag {
    anchor: usize,
    base: BTreeSet<usize>,
    select: bool,
}

impl WordSelection {
    pub fn selected(&self) -> &BTreeSet<usize> {
        &self.selected
    }

    pub fn is_selected(&self, index: usize) -> bool {
        self.selected.contains(&index)
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.drag = None;
    }

    /// 按下一个词：开始一笔。
    pub fn press(&mut self, index: usize) {
        let drag = WordDrag {
            anchor: index,
            base: self.selected.clone(),
            select: !self.selected.contains(&index),
        };
        self.drag = Some(drag);
        self.extend(index);
    }

    /// 拖到另一个词：从起点到这里的区间按这一笔的方向勾选或取消。
    pub fn extend(&mut self, index: usize) {
        let Some(drag) = &self.drag else {
            return;
        };
        let mut next = drag.base.clone();
        for current in drag.anchor.min(index)..=drag.anchor.max(index) {
            if drag.select {
                next.insert(current);
            } else {
                next.remove(&current);
            }
        }
        self.selected = next;
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

#[cfg(test)]
mod tests {
    use kwikpaste_core::clipboard::split_words;
    use kwikpaste_core::presenter::PreviewWordChip;

    use super::*;

    const GAP: f64 = 28.;

    const MONITOR: RectF = RectF {
        left: 0.,
        top: 0.,
        width: 1920.,
        height: 1080.,
    };

    fn card(left: f64) -> RectF {
        RectF {
            left,
            top: 300.,
            width: 340.,
            height: 80.,
        }
    }

    /// 卡片所在的主窗口：左右各留 8，上下各露出几张卡片。
    fn window(card: RectF) -> RectF {
        RectF {
            left: card.left - 8.,
            top: 100.,
            width: card.width + 16.,
            height: 600.,
        }
    }

    #[test]
    fn text_panels_take_the_full_width_and_grow_with_rows() {
        let geometry = geometry(
            card(800.),
            window(card(800.)),
            MONITOR,
            Some(&PreviewContentMetrics::Text { rows: 3 }),
            0.,
            0.,
            false,
            GAP,
            1.,
            1.,
        );
        assert_eq!(geometry.placement, Placement::Right);
        assert_eq!(geometry.panel.width, 480.);
        assert_eq!(geometry.panel.height, 48. + 3. * 22. + 32.);
        assert_eq!(geometry.panel.left, 800. + 340. + GAP);
        assert_eq!(geometry.panel.top, 340. - geometry.panel.height / 2.);

        let chips = [(200., 1), (200., 1), (120., 1)];
        let snippets = snippets_height(&chips, 445.);
        assert_eq!(snippets, 1. + 24. + 2. * 24. + 4.);
        let with_snippets = super::geometry(
            card(800.),
            window(card(800.)),
            MONITOR,
            Some(&PreviewContentMetrics::Text { rows: 3 }),
            snippets,
            0.,
            false,
            GAP,
            1.,
            1.,
        );
        assert_eq!(with_snippets.panel.height, geometry.panel.height + snippets);
        assert_eq!(snippets_height(&[], 445.), 0.);
        assert_eq!(
            snippets_height(&[(445., 1); 3], 445.),
            1. + 24. + 3. * 24. + 2. * 4.
        );
        assert_eq!(snippets_height(&[(445., 1); 32], 445.), SNIPPETS_MAX_HEIGHT);
        // 折成两行的长链接独占一行，比单行小块高一行字。
        assert_eq!(
            snippets_height(&[(900., 2), (120., 1)], 445.),
            1. + 24. + 2. * 24. + 4. + 16.
        );
    }

    #[test]
    fn switching_views_keeps_the_top_edge() {
        let text = Some(&PreviewContentMetrics::Text { rows: 12 });
        let image = Some(&PreviewContentMetrics::Image {
            width: Some(858.),
            height: Some(329.),
        });
        let tall = geometry(
            card(800.),
            window(card(800.)),
            MONITOR,
            text,
            0.,
            0.,
            true,
            GAP,
            1.,
            1.,
        );
        let short = geometry(
            card(800.),
            window(card(800.)),
            MONITOR,
            image,
            0.,
            0.,
            true,
            GAP,
            1.,
            1.,
        );
        assert_ne!(tall.panel.top, short.panel.top);

        let kept = short.keep_top(&tall, MONITOR);
        assert_eq!(kept.panel.top, tall.panel.top);
        assert_eq!(kept.panel.height, short.panel.height);
        assert_eq!(kept.panel.left, short.panel.left);
    }

    #[test]
    fn the_pointer_side_wins_and_falls_back_to_the_other_side() {
        let left = geometry(
            card(800.),
            window(card(800.)),
            MONITOR,
            None,
            0.,
            0.,
            true,
            GAP,
            1.,
            1.,
        );
        assert_eq!(left.placement, Placement::Left);

        // 贴着右边缘：右边放不下，换到左边。
        let edge = geometry(
            card(1500.),
            window(card(1500.)),
            MONITOR,
            None,
            0.,
            0.,
            false,
            GAP,
            1.,
            1.,
        );
        assert_eq!(edge.placement, Placement::Left);
    }

    #[test]
    fn a_centered_window_on_a_portrait_monitor_keeps_its_list_uncovered() {
        // 1080×1920 竖屏（工作区扣掉 48 的任务栏），500 宽的主窗口居中：左右都放不下，放到主窗口下面。
        let portrait = RectF {
            left: 0.,
            top: 0.,
            width: 1080.,
            height: 1872.,
        };
        let main = RectF {
            left: 290.,
            top: 460.,
            width: 500.,
            height: 570.,
        };
        let card = RectF {
            left: 298.,
            top: 520.,
            width: 484.,
            height: 100.,
        };
        let text = Some(&PreviewContentMetrics::Text { rows: 12 });
        let below = geometry(card, main, portrait, text, 0., 0., false, GAP, 1., 1.);
        assert_eq!(below.placement, Placement::Bottom);
        assert_eq!(below.panel.top, main.top + main.height + GAP);
        assert!(below.panel.intersect(&main).is_none());

        // 主窗口占满整个高度：哪里都放不下，贴到屏幕右边，只盖住主窗口的右边一条。
        let tall = RectF {
            top: 0.,
            height: 1872.,
            ..main
        };
        let edge = geometry(card, tall, portrait, text, 0., 0., false, GAP, 1., 1.);
        assert_eq!(edge.placement, Placement::Right);
        assert_eq!(edge.panel.right(), portrait.width - MARGIN);
    }

    #[test]
    fn images_fit_inside_the_limits() {
        let geometry = geometry(
            card(200.),
            window(card(200.)),
            MONITOR,
            Some(&PreviewContentMetrics::Image {
                width: Some(4000.),
                height: Some(1000.),
            }),
            0.,
            0.,
            false,
            GAP,
            1.,
            1.,
        );
        assert_eq!(geometry.panel.width, 480.);
        assert!(geometry.panel.height < 480.);

        let small = panel_size(
            Some(&PreviewContentMetrics::Image {
                width: Some(10.),
                height: Some(10.),
            }),
            0.,
            0.,
            &MONITOR.inset(MARGIN),
            1.,
            1.,
        );
        assert_eq!(small, (288., 96.));

        // Mod+滚轮放大：图片跟着放大，小图也能放大到原尺寸之上，最大到显示器工作区。
        let zoomed = |zoom: f64, size: f64| {
            panel_size(
                Some(&PreviewContentMetrics::Image {
                    width: Some(size * 4.),
                    height: Some(size),
                }),
                0.,
                0.,
                &MONITOR.inset(MARGIN),
                1.,
                zoom,
            )
        };
        assert_eq!(zoomed(2., 1000.).0, 960.);
        assert_eq!(zoomed(2., 50.), (432., 180.));
        assert_eq!(zoomed(MAX_ZOOM, 1000.).0, 1920. - 2. * MARGIN);
    }

    #[test]
    fn words_wrap_like_flex() {
        let chips = vec![
            PreviewWordChip {
                width: 100.,
                line_break: false,
            };
            9
        ];
        // 内容宽 448：一行放 4 个（4×100 + 3×4 = 412），9 个占 3 行。
        let height = panel_size(
            Some(&PreviewContentMetrics::Words { chips }),
            0.,
            0.,
            &MONITOR.inset(MARGIN),
            1.,
            1.,
        )
        .1;
        assert_eq!(height, 48. + 3. * 24. + 2. * 4. + 32. + 41.);
    }

    #[test]
    fn measured_order_preview_keeps_the_phone_suffix_visible() {
        let text = "订单 20260924 已发货，联系 138 1234 5678";
        let tokens = split_words(text).tokens;
        let estimated: Vec<_> = tokens
            .iter()
            .map(|token| PreviewWordChip::new(&token.text, token.line_break))
            .collect();
        let estimated_height = words_height(
            &estimated
                .iter()
                .map(|chip| (chip.width, chip.line_break))
                .collect::<Vec<_>>(),
            448.,
        );
        assert_eq!(estimated_height, 56.);

        // GPUI 的真实字形比 em 估算略宽；这组宽度让最后的 `5678` 落到第二行，
        // 面板尺寸因此必须多留一行，避免原来的裁切。
        let measured: Vec<_> = estimated
            .iter()
            .map(|chip| PreviewWordChip {
                width: chip.width + 2.,
                ..*chip
            })
            .collect();
        let measured_height = words_height(
            &measured
                .iter()
                .map(|chip| (chip.width, chip.line_break))
                .collect::<Vec<_>>(),
            448.,
        );
        assert_eq!(measured_height, 84.);
    }

    #[test]
    fn translation_zone_height_follows_its_state() {
        use kwikpaste_core::translate::{
            ItemTranslation, TranslateFailure, TranslationState, TranslationView,
        };

        let mut view = TranslationView {
            translatable: true,
            ..TranslationView::default()
        };
        // 可翻译但还没翻：没有译文区。
        assert_eq!(translation_height(&view, 50.), 0.);
        // 翻译中、失败：分隔线加一行工具栏。
        view.state = TranslationState::Pending;
        assert_eq!(translation_height(&view, 50.), 1. + TRANSLATION_BAR_HEIGHT);
        view.state = TranslationState::Failed {
            kind: TranslateFailure::Network,
            message: String::new(),
        };
        assert_eq!(translation_height(&view, 50.), 1. + TRANSLATION_BAR_HEIGHT);
        // 有译文：再加正文，最多 8 行。
        view.state = TranslationState::Idle;
        view.translation = Some(ItemTranslation {
            text: "译文".into(),
            source_lang: Some("en".into()),
            target_lang: "zh-Hans".into(),
            provider: "volc".into(),
        });
        assert_eq!(
            translation_height(&view, 50.),
            1. + TRANSLATION_BAR_HEIGHT + 50.
        );
        let body = |rows: f64| rows * TEXT_ROW_HEIGHT + TRANSLATION_PADDING_BOTTOM;
        assert_eq!(translation_rows_height(3), body(3.));
        assert_eq!(translation_rows_height(40), TRANSLATION_BODY_MAX_HEIGHT);
        // 选词视图：两个词块一行，词块多了同样封顶。
        let chip = |width: f64| PreviewWordChip {
            width,
            line_break: false,
        };
        assert_eq!(
            translation_words_height(&[chip(40.), chip(40.)], 300.),
            WORD_CHIP_HEIGHT + TRANSLATION_PADDING_BOTTOM
        );
        assert_eq!(
            translation_words_height(&vec![chip(40.); 200], 300.),
            TRANSLATION_BODY_MAX_HEIGHT
        );
    }

    #[test]
    fn panels_scale_with_text_size() {
        let size = panel_size(None, 0., 0., &MONITOR.inset(MARGIN), 1.25, 1.);
        assert_eq!(size, (600., (48. + 96.) * 1.25));
    }

    #[test]
    fn long_text_fills_the_panel_and_scrolls() {
        let (width, height) = panel_size(
            Some(&PreviewContentMetrics::Text { rows: 40 }),
            0.,
            0.,
            &MONITOR.inset(MARGIN),
            1.,
            1.,
        );
        assert_eq!(width, 480.);
        assert_eq!(height, 480.);
    }

    #[test]
    fn text_wraps_inside_the_borders_and_padding() {
        assert_eq!(text_wrap_width(&MONITOR, 1.), 480. - 2. - 32.);
        assert_eq!(text_wrap_width(&MONITOR, 1.25), 600. - 2. - 40.);
    }

    #[test]
    fn utf16_ranges_map_to_bytes() {
        let text = "a中😀b";
        assert_eq!(&text[utf16_range(text, 1, 2)], "中");
        assert_eq!(&text[utf16_range(text, 2, 4)], "😀");
        assert_eq!(&text[utf16_range(text, 4, 5)], "b");
        assert_eq!(utf16_range(text, 9, 12), text.len()..text.len());
    }

    #[test]
    fn bytes_read_like_1x() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(20 * 1024 * 1024), "20 MB");
    }

    #[test]
    fn word_drags_follow_the_first_word() {
        let mut selection = WordSelection::default();
        selection.press(2);
        selection.extend(4);
        selection.release();
        assert_eq!(
            selection.selected().iter().copied().collect::<Vec<_>>(),
            [2, 3, 4]
        );

        // 从已选的词开始拖：这一笔是取消。
        selection.press(3);
        selection.extend(5);
        selection.release();
        assert_eq!(
            selection.selected().iter().copied().collect::<Vec<_>>(),
            [2]
        );
    }
}
