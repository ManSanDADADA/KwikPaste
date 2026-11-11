//! 列表卡片与占位骨架，按 1.x `ClipboardCard.tsx` / `cards/*` 的样子画（卡片风格、标准密度为默认）。
//!
//! 尺寸全部来自 [`LayoutSpec`] 的设计 px，经 `space(px / 4)` 换成 rem，随文本缩放；颜色全部取角色 token。
//! 交互（悬停、点击）由列表视图挂在返回的元素上；悬停快捷动作、多选复选框这类带回调的部件也由
//! 列表画好后经 [`CardState`] 交进来，这里只管摆放。

use std::{rc::Rc, sync::Arc, time::Duration};

use chrono::{DateTime, Local};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Div, ElementId, HighlightStyle, Image,
    ImageSource, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    RenderImage, SharedString, StatefulInteractiveElement as _, Styled, StyledText, Window, div,
    img, prelude::FluentBuilder as _, pulsating_between, relative,
};
use kwikpaste_core::clipboard::SnippetKind;
use kwikpaste_ui::{
    Icon, IconName, KeyHint, KpStyled as _, TooltipExt as _,
    theme::{
        TextSize,
        components::{KindTag, KindTagTokens, SnippetTokens},
        css_color, radius,
        semantic::SemanticTokens,
        space,
    },
};

use crate::{
    clipboard::model::{
        item::{
            FileRow, FilesPreview, ItemKind, ListItem, Platform, SubKind, TextSnippet, TypeKey,
        },
        layout::{ImageBox, LayoutSpec, predict_image_box},
        time_label::time_label,
    },
    i18n::{t, t_args},
};

/// 骨架脉动的帧率上限（附录 D §2.1：循环动画限 10 fps）。
const PULSE_MAX_FPS: f32 = 10.;
/// Tailwind `animate-pulse` 的周期。
const PULSE_PERIOD: Duration = Duration::from_secs(2);

/// 渲染卡片需要的环境。
pub struct CardEnv<'a> {
    pub tokens: &'static SemanticTokens,
    pub kind_tags: KindTagTokens,
    /// 快捷信息按类型的颜色；关掉「快捷信息颜色」时为 `None`。
    pub snippet_colors: Option<SnippetTokens>,
    pub reorder_source_opacity: f32,
    pub layout: &'a LayoutSpec,
    pub now: DateTime<Local>,
    pub reduce_motion: bool,
}

/// 图片区的状态（缩略图或单图文件）。
#[derive(Clone)]
pub enum Visual {
    Ready(Arc<RenderImage>),
    Loading,
    Failed,
}

/// 卡片的状态标记。
#[derive(Default)]
pub struct CardState {
    /// 当前项：中性灰底。
    pub active: bool,
    /// 指针在卡片上：铺一层悬停底色。
    pub hovered: bool,
    pub image: Option<Visual>,
    /// 来源应用图标；有路径时由列表里的独立图标缓存提供。
    pub app_icon: Option<Visual>,
    /// 文件行图标，顺序与 `ListItem::file_rows()` 相同。
    pub file_icons: Vec<Option<Visual>>,
    /// 指针在卡片上：有备注且开了“悬停显示原文”时显示原文。
    pub show_original: bool,
    /// 按住修饰键时来源图标上的数字角标（1–9、0）。
    pub hint: Option<SharedString>,
    /// 多选时已勾上：卡片铺一层淡主色。
    pub checked: bool,
    /// 上一行铺了底色（当前项、悬停或勾选）：这一行上方的分隔线不画，免得贴着色块多出一道线。
    pub after_highlight: bool,
    /// 悬停快捷动作（列表画好的按钮行）；有它时头部行不显示时间。
    pub actions: Option<AnyElement>,
    /// 多选的复选框。
    pub checkbox: Option<AnyElement>,
    /// 点卡片下方的快捷信息；多选时为空，快捷信息不响应点击。
    pub on_snippet: Option<SnippetHandler>,
    /// 按住修饰键时链接、邮箱卡片的正文是可点的链接（1.x `isLinkActive`）：点了打开。
    /// 有它时快捷信息里的链接、邮箱也一样。
    pub on_link: Option<LinkHandler>,
    /// 当前条目在无障碍列表中的 1-based 位置。
    pub position: usize,
    /// 无障碍列表的总条数。
    pub set_size: usize,
    /// 面板内排序时源卡片留在原位但变暗。
    pub dragged: bool,
    /// 面板内排序时跟随指针的幽灵卡片：不透明的浮起底色加投影，盖住下面的行。
    pub lifted: bool,
}

/// 卡片下方快捷信息的交互。
pub enum SnippetEvent {
    /// 左键点了：按「点击」设置复制或粘贴这一段。
    Pick(Arc<str>),
    /// 中键点了链接 / 邮箱，或按住修饰键左键点了：直接打开。
    Open(Arc<str>, SnippetKind),
    /// 指针进出这一块：右键菜单按指针下的那一块给出动作。
    Hover {
        text: Arc<str>,
        kind: SnippetKind,
        hovered: bool,
    },
}

/// 快捷信息的交互处理。
pub type SnippetHandler = Rc<dyn Fn(SnippetEvent, &mut Window, &mut App)>;

/// 链接正文被点了。
pub type LinkHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// 这一行会不会铺底色（当前项、悬停或勾选）。
pub fn is_highlighted(active: bool, hovered: bool, checked: bool) -> bool {
    active || hovered || checked
}

/// 超过这个长度的片段（多为链接）大概率会被截断，悬停时补一个完整内容的提示（1.x 同）。
const SNIPPET_TOOLTIP_MIN_CHARS: usize = 32;

/// 一行卡片（含外层间距）。返回的元素带 id，调用方再挂交互。
pub fn card(
    env: &CardEnv<'_>,
    item: &ListItem,
    index: usize,
    state: CardState,
) -> gpui::Stateful<Div> {
    let tokens = env.tokens;
    let layout = env.layout;
    let links = state.on_link.is_some();
    let body = content(
        env,
        item,
        index,
        state.image,
        &state.file_icons,
        state.show_original,
        state.on_link,
    );
    let pinned = item.is_pinned;
    let sensitive = item.shows_sensitive_mark();
    let hint = state.hint;
    let actions = state.actions;
    let checkbox = state.checkbox;
    let on_snippet = state.on_snippet;
    let highlighted = is_highlighted(state.active, state.hovered, state.checked);
    let divider = !layout.seamless && state.position > 1 && !highlighted && !state.after_highlight;
    // 分隔线左端与正文对齐：紧凑密度的正文在 16 px 来源图标右侧。
    let divider_left = layout.item_padding_x
        + layout.card_padding_x
        + if layout.header_row {
            0.
        } else {
            16. + layout.body_gap
        };

    // 条目是扁平的行：平时没有底色和描边，悬停、当前项（中性灰）、勾选（淡主色）时才铺一层
    // 底色。卡片风格是圆角块、行间画细分隔线；无间风格贴边，用底边线分隔。
    let lifted = state.lifted;
    let highlight = if lifted {
        Some(tokens.surface.raised)
    } else if state.checked {
        Some(tokens.accent.subtle)
    } else if state.active {
        Some(tokens.fill.default)
    } else if state.hovered {
        // 比当前项浅一档，指针扫过时不会和当前项混淆。
        Some(tokens.fill.faint)
    } else {
        None
    };
    let mut frame = div()
        .relative()
        .flex()
        .overflow_hidden()
        .py(space((layout.card_padding_y) / 4.))
        .px(space((layout.card_padding_x) / 4.))
        .when(layout.seamless, |frame| {
            frame.border_b_1().border_color(tokens.border.divider)
        })
        // 卡片风格保留 1 px 的透明描边：行高估算里算了这圈描边。
        .when(!layout.seamless, |frame| {
            frame
                .border_1()
                .border_color(kwikpaste_ui::theme::transparent())
                .rounded(radius::LG)
        })
        .when_some(highlight, |frame, highlight| frame.bg(highlight))
        .when(lifted, |frame| {
            frame
                .rounded(radius::LG)
                .shadow(tokens.shadow.overlay.to_vec())
        });

    frame = if layout.header_row {
        // 来源、类型、时间这行小号灰字在正文上方。
        frame
            .flex_col()
            .gap(space((layout.body_gap) / 4.))
            .child(meta(
                env,
                item,
                hint,
                state.app_icon.clone(),
                actions,
                checkbox,
                status_marks(tokens, pinned, sensitive, true),
            ))
            .child(body)
            .children(snippets(env, item, 0, links, on_snippet))
    } else {
        frame
            .items_start()
            .gap(space((layout.body_gap) / 4.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h(space((20.) / 4.))
                    .items_center()
                    .child(hinted_icon(env, item, hint, state.app_icon)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(space((2.) / 4.))
                    .child(body)
                    .children(snippets(env, item, 0, links, on_snippet)),
            )
            .when(pinned || sensitive, |frame| {
                frame.child(status_marks(tokens, pinned, sensitive, true))
            })
            .when_some(checkbox, |frame, checkbox| {
                frame.child(
                    div()
                        .flex()
                        .flex_none()
                        .h(space((20.) / 4.))
                        .items_center()
                        .child(checkbox),
                )
            })
            // 没有头部行时动作按钮浮在卡片右上角，不占行高（1.x `floating`）。
            .when_some(actions, |frame, actions| {
                frame.child(
                    div()
                        .absolute()
                        .top(space(1.))
                        .right(space(1.))
                        .p(space((2.) / 4.))
                        .rounded(radius::MD)
                        .border_1()
                        .border_color(tokens.border.subtle)
                        .bg(tokens.surface.raised)
                        .shadow(tokens.shadow.card.to_vec())
                        .child(actions),
                )
            })
    };

    div()
        // 按记录 id 而不是行号标识：刷新后新记录挪到光标下时，悬停、点击状态不会继承给它。
        .id(ElementId::Name(SharedString::from(item.id.clone())))
        .role(gpui::Role::ListBoxOption)
        .aria_label(accessibility_name(item, &env.now))
        .aria_selected(state.active)
        .aria_position_in_set(state.position)
        .aria_size_of_set(state.set_size)
        .relative()
        .when(state.dragged, |row| row.opacity(env.reorder_source_opacity))
        .px(space((layout.item_padding_x) / 4.))
        .pt(space((layout.item_gap) / 4.))
        // 卡片风格的行间分隔线画在行上方的间距里，左右与正文对齐；第一行和挨着色块的不画。
        .when(divider, |row| {
            row.child(
                div()
                    .absolute()
                    .top(space((layout.item_gap / 2.) / 4.))
                    .left(space((divider_left) / 4.))
                    .right(space((layout.item_padding_x + layout.card_padding_x) / 4.))
                    .h(gpui::px(1.))
                    .bg(tokens.border.divider),
            )
        })
        .child(frame)
        .when(state.active, |row| row.aria_active_descendant())
}

/// 未加载行的骨架：照两行文本卡片画（1.x `renderPlaceholderItem`），标准档高 84 px。
pub fn placeholder(env: &CardEnv<'_>) -> AnyElement {
    let tokens = env.tokens;
    let layout = env.layout;
    let bar = |width: gpui::DefiniteLength| {
        div()
            .h(space((12.) / 4.))
            .w(width)
            .rounded(radius::SM)
            .bg(tokens.fill.default)
    };
    let line = |width: f32| {
        div()
            .flex()
            .h(space((20.) / 4.))
            .items_center()
            .child(bar(relative(width)))
    };
    let lines = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(line(0.75))
        .child(line(0.5));
    let icon = div()
        .size(space((16.) / 4.))
        .flex_none()
        .rounded(radius::SM)
        .bg(tokens.fill.default);

    let frame = div()
        .flex()
        .py(space((layout.card_padding_y) / 4.))
        .px(space((layout.card_padding_x) / 4.))
        .border_color(if layout.seamless {
            tokens.border.divider
        } else {
            kwikpaste_ui::theme::transparent()
        })
        .when(layout.seamless, |frame| frame.border_b_1())
        .when(!layout.seamless, |frame| {
            frame.border_1().rounded(radius::LG)
        });
    let frame = if layout.header_row {
        frame.flex_col().gap(space((layout.body_gap) / 4.)).child(
            div()
                .flex()
                .h(space((layout.header_height) / 4.))
                .items_center()
                .gap(space(1.))
                .child(icon)
                .child(bar(space((64.) / 4.).into())),
        )
    } else {
        frame
            .items_start()
            .gap(space((layout.body_gap) / 4.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h(space((20.) / 4.))
                    .items_center()
                    .child(icon),
            )
    };

    div()
        .px(space((layout.item_padding_x) / 4.))
        .pt(space((layout.item_gap) / 4.))
        .child(frame.child(lines))
        .into_any_element()
}

/// 卡片要显示的图片：缩略图（图片记录）或单图文件记录的原图；返回（路径，显示框）。
pub fn image_target(item: &ListItem, layout: &LayoutSpec) -> Option<ImageTarget> {
    match item.kind {
        ItemKind::Image => Some(ImageTarget {
            path: item.image_thumbnail_path.clone(),
            modified: None,
            file_name: Some(item.content.clone()),
            display: item.image_display.unwrap_or_else(|| {
                predict_image_box(item.width, item.height, layout.image_max_height)
            }),
        }),
        ItemKind::Files if item.files_preview_kind == Some(FilesPreview::ImagePreview) => {
            let row = item.file_rows().first()?;
            Some(ImageTarget {
                path: Some(row.path.clone()),
                modified: row.modified,
                file_name: None,
                display: predict_image_box(row.width, row.height, layout.image_max_height),
            })
        }
        _ => None,
    }
}

/// 卡片图片区的数据来源。
pub struct ImageTarget {
    /// 已知的图片文件路径；图片记录还没有缩略图时为空。
    pub path: Option<Arc<str>>,
    /// 可变原文件的修改时间；捕获图片的缩略图为空。
    pub modified: Option<i64>,
    /// 缺缩略图时向数据源要缩略图用的文件名（图片记录的 `content`）。
    pub file_name: Option<Arc<str>>,
    pub display: ImageBox,
}

/// 微信输入法扩展收到的记录：来源 id 带 `wetype:` 前缀。
fn is_wetype_origin(item: &ListItem) -> bool {
    item.origin_device_id
        .as_deref()
        .is_some_and(|id| id.starts_with("wetype:"))
}

fn meta(
    env: &CardEnv<'_>,
    item: &ListItem,
    hint: Option<SharedString>,
    app_icon_state: Option<Visual>,
    actions: Option<AnyElement>,
    checkbox: Option<AnyElement>,
    marks: Div,
) -> Div {
    let tokens = env.tokens;
    let key = if is_wetype_origin(item) {
        "clipboard:origin.fromWeType"
    } else {
        "clipboard:origin.fromDevice"
    };
    let origin = item
        .origin_device_name
        .as_deref()
        .map(|name| t_args(key, &[("name", name)]));

    div()
        .flex()
        .h(space((env.layout.header_height) / 4.))
        .items_center()
        .justify_between()
        .gap(space((6.) / 4.))
        .kp_text(TextSize::Xs)
        .text_color(tokens.text.muted)
        .child(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap(space((6.) / 4.))
                .overflow_hidden()
                .child(hinted_icon(env, item, hint, app_icon_state))
                .child(type_tag(env, item.type_key()))
                .children(origin.map(|origin| {
                    div()
                        .truncate()
                        .child(SharedString::from(format!("· {origin}")))
                }))
                .child(div().flex_none().child(SharedString::from(format!(
                    "· {}",
                    time_label(item.created_at, &env.now)
                )))),
        )
        // 右侧：置顶、敏感标记；悬停时换成快捷动作（置顶动作本身带状态，标记不再重复），多选时跟复选框。
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(space((6.) / 4.))
                .when(actions.is_none(), |side| side.child(marks))
                .children(actions)
                .children(checkbox),
        )
}

/// 来源图标；按住修饰键时叠一个数字角标，图标本身隐去（1.x `KeyHint` 包着来源图标）。
fn hinted_icon(
    env: &CardEnv<'_>,
    item: &ListItem,
    hint: Option<SharedString>,
    app_icon_state: Option<Visual>,
) -> AnyElement {
    let icon = app_icon(env, item, app_icon_state);
    let Some(key) = hint else {
        return icon;
    };

    div()
        .relative()
        .flex()
        .flex_none()
        .child(div().flex().opacity(0.).child(icon))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(KeyHint::new(key)),
        )
        .into_any_element()
}

/// 类型标签：识别出的子类型（链接、邮箱、颜色、路径）画成淡色底的彩色小胶囊，其余是次级文字。
fn type_tag(env: &CardEnv<'_>, key: TypeKey) -> Div {
    let tag = match key {
        TypeKey::Url => env.kind_tags.link,
        TypeKey::Email => env.kind_tags.email,
        TypeKey::Color => env.kind_tags.color,
        TypeKey::Path => env.kind_tags.path,
        _ => {
            return div()
                .truncate()
                .text_color(env.tokens.text.secondary)
                .child(type_label(key));
        }
    };

    div()
        .flex_none()
        .px(space(1.5))
        .rounded(radius::SM)
        .bg(tag.background)
        .text_color(tag.foreground)
        .child(type_label(key))
}

fn type_label(key: TypeKey) -> SharedString {
    match key {
        TypeKey::Text => t("clipboard:types.text"),
        TypeKey::Html => t("clipboard:types.html"),
        TypeKey::Rtf => t("clipboard:types.rtf"),
        TypeKey::Url => t("clipboard:types.url"),
        TypeKey::Email => t("clipboard:types.email"),
        TypeKey::Color => t("clipboard:types.color"),
        TypeKey::Path => t("clipboard:types.path"),
        TypeKey::Image => t("clipboard:types.image"),
        TypeKey::Files => t("clipboard:types.files"),
    }
}

/// 生成条目在 UIA/AccessKit 中使用的可读名称。
pub fn accessibility_name(item: &ListItem, now: &DateTime<Local>) -> SharedString {
    let summary = item
        .summary
        .as_deref()
        .or_else(|| match item.kind {
            ItemKind::Image => Some(&*item.content),
            ItemKind::Files => item.file_rows().first().map(|row| &*row.name),
            ItemKind::Text => None,
        })
        .and_then(|value| value.lines().next())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let kind = type_label(item.type_key());
    let time = time_label(item.created_at, now);
    match summary {
        Some(summary) => format!("{kind} · {summary} · {time}").into(),
        None => format!("{kind} · {time}").into(),
    }
}

/// 来源应用图标；同步来的记录用设备平台图标；都没有时是快贴的图标（1.x 的顺序）。
/// 头部行里配 12 px 的小字用 14 px，紧凑密度里和正文并排用 16 px，三种来源同一尺寸。
fn app_icon(env: &CardEnv<'_>, item: &ListItem, state: Option<Visual>) -> AnyElement {
    let size = if env.layout.header_row {
        space((14.) / 4.)
    } else {
        space((16.) / 4.)
    };
    if item.source_app_id.is_some() && item.source_app_icon_path.is_some() {
        return match state {
            Some(Visual::Ready(image)) => img(ImageSource::Render(image))
                .size(size)
                .flex_none()
                .into_any_element(),
            Some(Visual::Loading) => div().size(size).flex_none().into_any_element(),
            Some(Visual::Failed) | None => img(ImageSource::Image(logo()))
                .size(size)
                .flex_none()
                .into_any_element(),
        };
    }

    if item.origin_device_id.is_some() {
        let icon = match item.platform {
            _ if is_wetype_origin(item) => IconName::Smartphone,
            Platform::Macos => IconName::Laptop,
            Platform::Windows => IconName::Monitor,
        };
        return div()
            .flex_none()
            .child(Icon::new(icon).size(size).color(env.tokens.text.secondary))
            .into_any_element();
    }

    img(ImageSource::Image(logo()))
        .size(size)
        .flex_none()
        .into_any_element()
}

/// 快贴图标（1.x `public/logo.png`，macOS 用 `logo-mac.png`）。
pub fn logo() -> Arc<Image> {
    use std::sync::LazyLock;

    static LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
        let bytes: &[u8] = if cfg!(target_os = "macos") {
            include_bytes!("../../../assets/logo-mac.png")
        } else {
            include_bytes!("../../../assets/logo.png")
        };
        Arc::new(Image::from_bytes(gpui::ImageFormat::Png, bytes.to_vec()))
    });

    LOGO.clone()
}

fn content(
    env: &CardEnv<'_>,
    item: &ListItem,
    index: usize,
    image: Option<Visual>,
    file_icons: &[Option<Visual>],
    show_original: bool,
    on_link: Option<LinkHandler>,
) -> AnyElement {
    // 有备注时显示备注；悬停且开了“显示原文”时换回原内容（1.x `NoteContentSwitcher`）。
    if let Some(note) = &item.note
        && !show_original
    {
        return note_annotation(env, note);
    }

    match item.kind {
        ItemKind::Text => text_body(env, item, on_link),
        ItemKind::Image => {
            let body = image_body(env, item, index, image);
            match &item.image_text_snippet {
                Some(snippet) => div()
                    .flex()
                    .flex_col()
                    .gap(space(1.5))
                    .child(body)
                    .child(image_text_line(env, snippet))
                    .into_any_element(),
                None => body,
            }
        }
        ItemKind::Files if item.files_preview_kind == Some(FilesPreview::ImagePreview) => {
            image_body(env, item, index, image)
        }
        ItemKind::Files => files_body(env, item.file_rows(), file_icons),
    }
}

fn shared(text: &Arc<str>) -> SharedString {
    // 制表符在 GPUI 里宽度为 0，换成空格免得两边的字粘在一起。
    if text.contains('\t') {
        return SharedString::from(text.replace('\t', "    "));
    }

    SharedString::from(text)
}

fn text_body(env: &CardEnv<'_>, item: &ListItem, on_link: Option<LinkHandler>) -> AnyElement {
    let tokens = env.tokens;
    if item.sub_kind == Some(SubKind::Color)
        && let Some(value) = &item.color_preview
        && let Some(color) = css_color(value)
    {
        return div()
            .flex()
            .items_center()
            .gap(space((8.) / 4.))
            .kp_text(TextSize::Sm)
            .child(
                div()
                    .size(space((16.) / 4.))
                    .flex_none()
                    .rounded(radius::SM)
                    .border_1()
                    .border_color(tokens.border.subtle)
                    .bg(color),
            )
            .child(div().kp_mono().child(shared(value)))
            .into_any_element();
    }

    let summary = item.summary.as_ref().map(shared).unwrap_or_default();
    let link = on_link.filter(|_| item.link_sub_kind().is_some());
    let Some(on_link) = link else {
        return div()
            .w_full()
            .kp_text(TextSize::Sm)
            .line_clamp(env.layout.text_max_lines)
            .text_ellipsis()
            .child(summary)
            .into_any_element();
    };

    // 按住修饰键：主色、下划线，点了打开；按下不冒泡给卡片（不选中、不触发单击粘贴）。
    div()
        .id("card-link")
        .w_full()
        .kp_text(TextSize::Sm)
        .line_clamp(env.layout.text_max_lines)
        .text_ellipsis()
        .text_color(tokens.accent.solid)
        .underline()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_link(window, cx);
        })
        .child(summary)
        .into_any_element()
}

fn note_annotation(env: &CardEnv<'_>, note: &Arc<str>) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(space(1.))
        .kp_text(TextSize::Sm)
        .child(
            div().flex_none().pt(space((3.) / 4.)).child(
                Icon::new(IconName::NotebookPen)
                    .size(space((14.) / 4.))
                    .color(env.tokens.accent.solid),
            ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .line_clamp(env.layout.text_max_lines)
                .text_ellipsis()
                .child(shared(&Arc::from(note_line(note)))),
        )
        .into_any_element()
}

/// 多行备注在卡片上并成一行，用 ` / ` 隔开（如 `ty / tongyi`），跳过空行。
fn note_line(note: &str) -> String {
    note.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn image_body(
    env: &CardEnv<'_>,
    item: &ListItem,
    index: usize,
    image: Option<Visual>,
) -> AnyElement {
    let tokens = env.tokens;
    let Some(target) = image_target(item, env.layout) else {
        return div().into_any_element();
    };
    let (width, height) = (
        space((target.display.width) / 4.),
        space((target.display.height) / 4.),
    );

    match image.unwrap_or(Visual::Loading) {
        // 圆角缩略图，上面叠一圈不占位置的细描边：白底截图、浅色图片也有边界，行高不变。
        Visual::Ready(image) => div()
            .flex()
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(width)
                    .h(height)
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
                            .border_color(tokens.border.divider),
                    ),
            )
            .into_any_element(),
        Visual::Failed => div()
            .flex()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(width)
                    .h(height)
                    .items_center()
                    .justify_center()
                    .rounded(radius::SM)
                    .bg(tokens.fill.subtle)
                    .child(
                        Icon::new(IconName::ImageOff)
                            .size(space((target.display.height.min(16.)) / 4.))
                            .color(tokens.text.faint),
                    ),
            )
            .into_any_element(),
        Visual::Loading => {
            let skeleton = div()
                .flex_none()
                .w(width)
                .h(height)
                .rounded(radius::SM)
                .bg(tokens.fill.subtle);
            let skeleton = if env.reduce_motion {
                skeleton.into_any_element()
            } else {
                skeleton
                    .with_animation(
                        ElementId::NamedInteger("thumbnail-pulse".into(), index as u64),
                        Animation::new(PULSE_PERIOD)
                            .repeat()
                            .with_easing(pulsating_between(0.5, 1.))
                            .with_max_fps(PULSE_MAX_FPS),
                        |skeleton, delta| skeleton.opacity(delta),
                    )
                    .into_any_element()
            };

            div().flex().child(skeleton).into_any_element()
        }
    }
}

/// 搜索靠图片里的文字命中时，缩略图下面的一行命中片段，关键词用强调色标出。
fn image_text_line(env: &CardEnv<'_>, snippet: &TextSnippet) -> AnyElement {
    let tokens = env.tokens;
    let text = SharedString::from(snippet.text.clone());
    let matched = snippet.matched.clone();
    let highlight = (!matched.is_empty()
        && matched.end <= text.len()
        && text.is_char_boundary(matched.start)
        && text.is_char_boundary(matched.end))
    .then(|| {
        (
            matched,
            HighlightStyle {
                color: Some(tokens.accent.text),
                ..HighlightStyle::default()
            },
        )
    });

    div()
        .flex()
        .items_center()
        .gap(space(1.5))
        .min_w_0()
        .kp_text(TextSize::Xs)
        .text_color(tokens.text.secondary)
        .child(
            Icon::new(IconName::ScanText)
                .size(space(3.))
                .color(tokens.text.muted),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(StyledText::new(text).with_highlights(highlight)),
        )
        .into_any_element()
}

fn files_body(env: &CardEnv<'_>, rows: &[FileRow], file_icons: &[Option<Visual>]) -> AnyElement {
    let tokens = env.tokens;

    div()
        .flex()
        .flex_col()
        .gap(space(1.))
        .kp_text(TextSize::Sm)
        .children(rows.iter().enumerate().map(|(index, row)| {
            let icon_state = file_icons.get(index).and_then(Option::clone);
            div()
                .flex()
                .items_center()
                .gap(space(1.))
                .min_w_0()
                .children(row.icon_path.as_ref().map(|_| {
                    match icon_state {
                        Some(Visual::Ready(image)) => img(ImageSource::Render(image))
                            .size(space((20.) / 4.))
                            .flex_none()
                            .into_any_element(),
                        Some(Visual::Loading) | Some(Visual::Failed) | None => {
                            div().size(space((20.) / 4.)).flex_none().into_any_element()
                        }
                    }
                }))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .when(!row.exists, |name| {
                            name.line_through()
                                .text_decoration_color(tokens.text.primary)
                        })
                        .child(shared(&row.name)),
                )
        }))
        .into_any_element()
}

/// 卡片下方的快捷信息：一行放得下的片段，放不下的整个隐藏（1.x `QuickSnippets`）。
/// `marks` 是右下角水印的个数，给水印留出位置；`links` 时链接、邮箱显示成链接，点了打开。
fn snippets(
    env: &CardEnv<'_>,
    item: &ListItem,
    marks: usize,
    links: bool,
    on_pick: Option<SnippetHandler>,
) -> Option<AnyElement> {
    if item.quick_snippets.is_empty() {
        return None;
    }
    let tokens = env.tokens;
    let reserve = match marks {
        0 => 0.,
        1 => 24.,
        _ => 44.,
    };

    Some(
        div()
            .flex()
            .flex_wrap()
            .h(space((24.) / 4.))
            .gap(space(1.))
            .overflow_hidden()
            .pr(space((reserve) / 4.))
            .children(
                item.quick_snippets
                    .iter()
                    .enumerate()
                    .map(|(index, snippet)| {
                        let long = snippet.text.chars().count() >= SNIPPET_TOOLTIP_MIN_CHARS;
                        let openable = snippet_open_url(snippet.kind, &snippet.text).is_some();
                        let open = links && openable;
                        let chip = div()
                            .id(ElementId::NamedInteger("snippet".into(), index as u64))
                            .flex()
                            .h(space((24.) / 4.))
                            .max_w_full()
                            .items_center()
                            .px(space((8.) / 4.))
                            .rounded(radius::MD)
                            .bg(tokens.fill.subtle)
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .when_some(env.snippet_colors, |chip, colors| {
                                chip.gap(space(1.5))
                                    .child(snippet_dot(snippet_color(&colors, snippet.kind)))
                            })
                            // 提示挂在内容上：它自己要 `on_hover`，和小块的悬停（右键菜单认它）分开。
                            .child(
                                div()
                                    .id(ElementId::NamedInteger(
                                        "snippet-text".into(),
                                        index as u64,
                                    ))
                                    .truncate()
                                    .when(open, |text| {
                                        text.text_color(tokens.accent.solid).underline()
                                    })
                                    .child(SharedString::from(snippet.text.clone()))
                                    .when(long && on_pick.is_some(), |text| {
                                        text.kp_tooltip(SharedString::from(snippet.text.clone()))
                                    }),
                            );
                        let Some(on_pick) = on_pick.clone() else {
                            return chip.into_any_element();
                        };
                        let text: Arc<str> = Arc::from(snippet.text.as_str());
                        let kind = snippet.kind;
                        let on_hover = on_pick.clone();
                        let hover_text = text.clone();
                        let on_open = on_pick.clone();
                        let open_text = text.clone();

                        // 按下时拦住事件，不触发卡片的选中、单击粘贴和双击粘贴（1.x `SnippetChip`）。
                        chip.cursor_pointer()
                            .hover(|style| {
                                style
                                    .bg(tokens.fill.default)
                                    .text_color(tokens.text.primary)
                            })
                            .on_hover(move |hovered, window, cx| {
                                let event = SnippetEvent::Hover {
                                    text: hover_text.clone(),
                                    kind,
                                    hovered: *hovered,
                                };
                                on_hover(event, window, cx);
                            })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .when(openable, |chip| {
                                chip.on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                                    cx.stop_propagation();
                                    on_open(
                                        SnippetEvent::Open(open_text.clone(), kind),
                                        window,
                                        cx,
                                    );
                                })
                            })
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let event = if open {
                                    SnippetEvent::Open(text.clone(), kind)
                                } else {
                                    SnippetEvent::Pick(text.clone())
                                };
                                on_pick(event, window, cx);
                            })
                            .into_any_element()
                    }),
            )
            .into_any_element(),
    )
}

/// 快捷信息「打开」的去处：链接（`www.` 开头补 `https://`）用浏览器、邮箱用 `mailto:`；
/// 其它类型（以及 `ftp://` 这类不交给浏览器的链接）没有。
pub fn snippet_open_url(kind: SnippetKind, text: &str) -> Option<String> {
    let starts_with = |prefix: &str| {
        text.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    };
    match kind {
        SnippetKind::Link if starts_with("www.") => Some(format!("https://{text}")),
        SnippetKind::Link if starts_with("http://") || starts_with("https://") => {
            Some(text.to_owned())
        }
        SnippetKind::Email => Some(format!("mailto:{text}")),
        _ => None,
    }
}

/// 快捷信息类型对应的颜色。
pub fn snippet_color(colors: &SnippetTokens, kind: SnippetKind) -> KindTag {
    match kind {
        SnippetKind::Custom => colors.custom,
        SnippetKind::Number => colors.number,
        SnippetKind::Phone => colors.phone,
        SnippetKind::Email => colors.email,
        SnippetKind::Time => colors.time,
        SnippetKind::Link => colors.link,
    }
}

/// 快捷信息前的类型圆点。
pub fn snippet_dot(color: KindTag) -> Div {
    div()
        .flex_none()
        .size(space(1.5))
        .rounded_full()
        .bg(color.foreground)
}

/// 置顶、敏感标记：有头部行时是右下角 20 px 的水印，没有时是正文右侧 16 px 的小图标。
fn status_marks(tokens: &SemanticTokens, pinned: bool, sensitive: bool, inline: bool) -> Div {
    let size = space((14.) / 4.);
    let marks = div()
        .flex()
        .gap(space(1.))
        .when(pinned, |marks| {
            marks.child(
                Icon::new(IconName::PushPin)
                    .size(size)
                    .color(tokens.status.warning.solid),
            )
        })
        .when(sensitive, |marks| {
            marks.child(
                Icon::new(IconName::KeyRound)
                    .size(size)
                    .color(tokens.text.faint),
            )
        });

    if inline {
        marks.flex_none().h(space((20.) / 4.)).items_center()
    } else {
        marks
            .absolute()
            .right(space((8.) / 4.))
            .bottom(space((8.) / 4.))
            .items_end()
    }
}

#[cfg(test)]
mod tests {
    use kwikpaste_core::clipboard::SnippetKind;

    use super::{note_line, snippet_open_url};

    #[test]
    fn multi_line_notes_join_into_one_line() {
        assert_eq!(note_line("ty\n tongyi \n\n"), "ty / tongyi");
        assert_eq!(note_line("备注"), "备注");
    }

    #[test]
    fn only_links_and_emails_open() {
        let open = |kind, text| snippet_open_url(kind, text);
        assert_eq!(
            open(SnippetKind::Link, "https://a.com/x").as_deref(),
            Some("https://a.com/x")
        );
        assert_eq!(
            open(SnippetKind::Link, "WWW.a.com").as_deref(),
            Some("https://WWW.a.com")
        );
        assert_eq!(open(SnippetKind::Link, "ftp://a.com"), None);
        assert_eq!(
            open(SnippetKind::Email, "a@b.com").as_deref(),
            Some("mailto:a@b.com")
        );
        assert_eq!(open(SnippetKind::Number, "123456"), None);
    }
}
