//! antd `message` 式的全局提示：窗口顶部居中的胶囊，不带关闭按钮，宽度随内容，默认 3 s 消失。
//!
//! 不用 gpui-component 的 Notification：它是右上角 382 px 的卡片，悬停还会露出关闭按钮，
//! 360 px 宽的剪贴板面板放不下。

use std::{fmt, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems, svg,
};
use gpui_base::{animation::cubic_bezier, h_flex, v_flex};

use crate::{
    overlay,
    styled::KpStyled as _,
    theme::{self, TextSize, motion, radius, space},
};

/// 提示类型，决定图标与图标颜色。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastKind {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl ToastKind {
    pub const ALL: [Self; 4] = [Self::Info, Self::Success, Self::Warning, Self::Error];
}

type ActionHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// 提示末尾的文字按钮。
#[derive(Clone)]
pub struct ToastAction {
    label: SharedString,
    handler: ActionHandler,
}

impl fmt::Debug for ToastAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToastAction")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// 一条提示。
#[derive(Clone, Debug)]
pub struct Toast {
    kind: ToastKind,
    message: SharedString,
    key: Option<SharedString>,
    duration: Duration,
    action: Option<ToastAction>,
}

impl Toast {
    pub fn new(kind: ToastKind, message: impl Into<SharedString>) -> Self {
        Self {
            kind,
            message: message.into(),
            key: None,
            duration: motion::TOAST,
            action: None,
        }
    }

    pub fn info(message: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Info, message)
    }

    pub fn success(message: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Success, message)
    }

    pub fn warning(message: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Warning, message)
    }

    pub fn error(message: impl Into<SharedString>) -> Self {
        Self::new(ToastKind::Error, message)
    }

    /// 同 key 的提示原地替换内容并重新计时，不会叠出第二条。
    pub fn key(mut self, key: impl Into<SharedString>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    /// 在消息后面加一个文字按钮。
    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.action = Some(ToastAction {
            label: label.into(),
            handler: Rc::new(handler),
        });
        self
    }
}

/// 在窗口顶部显示一条提示。窗口不是经 [`crate::open_window`] 打开的就静默忽略。
pub fn show(toast: Toast, window: &mut Window, cx: &mut App) {
    let Some(overlays) = overlay::overlays(window, cx) else {
        return;
    };

    overlays.update(cx, |overlays, cx| overlays.push_toast(toast, window, cx));
}

struct Entry {
    generation: u64,
    kind: ToastKind,
    message: SharedString,
    key: Option<SharedString>,
    action: Option<ToastAction>,
    leaving: bool,
}

/// 正在显示的提示，按出现顺序自上而下排列。
#[derive(Default)]
pub(crate) struct ToastStack {
    entries: Vec<Entry>,
    next_generation: u64,
}

impl ToastStack {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 加入一条提示，返回它的代次和停留时长，供调用方安排消失。
    ///
    /// 同 key 的提示原地替换内容并换成新代次，旧代次的计时到期后什么也不做。
    pub(crate) fn push(&mut self, toast: Toast) -> (u64, Duration) {
        self.next_generation += 1;
        let generation = self.next_generation;

        if let Some(key) = &toast.key
            && let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| entry.key.as_ref() == Some(key))
        {
            entry.generation = generation;
            entry.kind = toast.kind;
            entry.message = toast.message;
            entry.action = toast.action;
            entry.leaving = false;
            return (generation, toast.duration);
        }

        self.entries.push(Entry {
            generation,
            kind: toast.kind,
            message: toast.message,
            key: toast.key,
            action: toast.action,
            leaving: false,
        });

        (generation, toast.duration)
    }

    /// 开始退场；该代次已被替换或移除时返回 false。
    pub(crate) fn begin_leave(&mut self, generation: u64) -> bool {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.generation == generation)
        else {
            return false;
        };

        entry.leaving = true;
        true
    }

    pub(crate) fn remove(&mut self, generation: u64) {
        self.entries.retain(|entry| entry.generation != generation);
    }

    pub(crate) fn render(&self, _: &mut Window, cx: &mut App) -> AnyElement {
        let reduce_motion = cx.reduce_motion();

        // antd：容器距顶 8 px，每条提示外包 8 px 内边距。
        v_flex()
            .absolute()
            .top(space(2.))
            .left_0()
            .right_0()
            .items_center()
            .children(self.entries.iter().map(|entry| {
                let capsule = div().max_w_full().p(space(2.)).child(ToastCapsule {
                    kind: entry.kind,
                    message: entry.message.clone(),
                    action: entry.action.clone(),
                });

                animate(capsule, entry.generation, entry.leaving, reduce_motion)
            }))
            .into_any_element()
    }
}

/// 进场：自上方 8 px 淡入；退场：淡出。系统关闭动画时直接显示与移除。
fn animate(capsule: gpui::Div, generation: u64, leaving: bool, reduce_motion: bool) -> AnyElement {
    if reduce_motion {
        return capsule.into_any_element();
    }

    let [x1, y1, x2, y2] = motion::EASE_OUT_CIRC;
    let id = ElementId::NamedInteger(
        if leaving {
            "kp-toast-leave"
        } else {
            "kp-toast-enter"
        }
        .into(),
        generation,
    );

    capsule
        .relative()
        .with_animation(
            id,
            Animation::new(motion::MID).with_easing(cubic_bezier(x1, y1, x2, y2)),
            move |capsule, delta| {
                if leaving {
                    capsule.opacity(1. - delta)
                } else {
                    capsule.opacity(delta).top(px(-8. * (1. - delta)))
                }
            },
        )
        .into_any_element()
}

/// 提示胶囊本体（toast 层与组件展示共用）。
#[derive(IntoElement)]
pub struct ToastCapsule {
    kind: ToastKind,
    message: SharedString,
    action: Option<ToastAction>,
}

impl ToastCapsule {
    pub fn new(kind: ToastKind, message: impl Into<SharedString>) -> Self {
        Self {
            kind,
            message: message.into(),
            action: None,
        }
    }
}

impl RenderOnce for ToastCapsule {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let icon_color = match self.kind {
            ToastKind::Info => tokens.status.info.solid,
            ToastKind::Success => tokens.status.success.solid,
            ToastKind::Warning => tokens.status.warning.solid,
            ToastKind::Error => tokens.status.danger.solid,
        };

        // antd message：内边距 9 px 12 px（(controlHeightLG − 22) / 2、paddingSM），圆角 borderRadiusLG，
        // 底色 colorBgElevated，阴影 boxShadow，图标 16 px、与文字间隔 8 px，文字 14 / 22 px。
        // 文字过长时在窗口宽度内换行（antd 的胶囊是 inline-block）：图标对齐第一行。
        h_flex()
            .max_w_full()
            .items_start()
            .gap(space(2.))
            .px(space(3.))
            .py(rems(0.5625))
            .rounded(radius::LG)
            .bg(tokens.surface.overlay)
            .shadow(tokens.shadow.float())
            .text_color(tokens.text.primary)
            .kp_text(TextSize::Sm)
            .line_height(rems(1.375))
            .child(
                svg()
                    .flex_none()
                    .mt(rems(0.1875))
                    .size(rems(1.))
                    .text_color(icon_color)
                    .data(status_icon(self.kind)),
            )
            .child(div().min_w_0().child(self.message))
            .children(self.action.map(|action| {
                let handler = action.handler;
                div()
                    .id("kp-toast-action")
                    .flex_none()
                    .text_color(tokens.text.link)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(move |_, window, cx| handler(window, cx))
                    .child(action.label)
            }))
    }
}

/// antd message 的实心圆形状态图标（@ant-design/icons-svg 4.5.0，MIT 许可）。
fn status_icon(kind: ToastKind) -> &'static [u8] {
    match kind {
        ToastKind::Info => INFO_CIRCLE_FILLED,
        ToastKind::Success => CHECK_CIRCLE_FILLED,
        ToastKind::Warning => EXCLAMATION_CIRCLE_FILLED,
        ToastKind::Error => CLOSE_CIRCLE_FILLED,
    }
}

pub(crate) const CHECK_CIRCLE_FILLED: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="64 64 896 896"><path d="M512 64C264.6 64 64 264.6 64 512s200.6 448 448 448 448-200.6 448-448S759.4 64 512 64zm193.5 301.7l-210.6 292a31.8 31.8 0 01-51.7 0L318.5 484.9c-3.8-5.3 0-12.7 6.5-12.7h46.9c10.2 0 19.9 4.9 25.9 13.3l71.2 98.8 157.2-218c6-8.3 15.6-13.3 25.9-13.3H699c6.5 0 10.3 7.4 6.5 12.7z"/></svg>"#;

pub(crate) const CLOSE_CIRCLE_FILLED: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" fill-rule="evenodd" viewBox="64 64 896 896"><path d="M512 64c247.4 0 448 200.6 448 448S759.4 960 512 960 64 759.4 64 512 264.6 64 512 64zm127.98 274.82h-.04l-.08.06L512 466.75 384.14 338.88c-.04-.05-.06-.06-.08-.06a.12.12 0 00-.07 0c-.03 0-.05.01-.09.05l-45.02 45.02a.2.2 0 00-.05.09.12.12 0 000 .07v.02a.27.27 0 00.06.06L466.75 512 338.88 639.86c-.05.04-.06.06-.06.08a.12.12 0 000 .07c0 .03.01.05.05.09l45.02 45.02a.2.2 0 00.09.05.12.12 0 00.07 0c.02 0 .04-.01.08-.05L512 557.25l127.86 127.87c.04.04.06.05.08.05a.12.12 0 00.07 0c.03 0 .05-.01.09-.05l45.02-45.02a.2.2 0 00.05-.09.12.12 0 000-.07v-.02a.27.27 0 00-.05-.06L557.25 512l127.87-127.86c.04-.04.05-.06.05-.08a.12.12 0 000-.07c0-.03-.01-.05-.05-.09l-45.02-45.02a.2.2 0 00-.09-.05.12.12 0 00-.07 0z"/></svg>"#;

pub(crate) const EXCLAMATION_CIRCLE_FILLED: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="64 64 896 896"><path d="M512 64C264.6 64 64 264.6 64 512s200.6 448 448 448 448-200.6 448-448S759.4 64 512 64zm-32 232c0-4.4 3.6-8 8-8h48c4.4 0 8 3.6 8 8v272c0 4.4-3.6 8-8 8h-48c-4.4 0-8-3.6-8-8V296zm32 440a48.01 48.01 0 010-96 48.01 48.01 0 010 96z"/></svg>"#;

pub(crate) const INFO_CIRCLE_FILLED: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="64 64 896 896"><path d="M512 64C264.6 64 64 264.6 64 512s200.6 448 448 448 448-200.6 448-448S759.4 64 512 64zm32 664c0 4.4-3.6 8-8 8h-48c-4.4 0-8-3.6-8-8V456c0-4.4 3.6-8 8-8h48c4.4 0 8 3.6 8 8v272zm-32-344a48.01 48.01 0 010-96 48.01 48.01 0 010 96z"/></svg>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_key_replaces_in_place() {
        let mut stack = ToastStack::default();
        let (first, _) = stack.push(Toast::success("one").key("copy"));
        let (second, _) = stack.push(Toast::error("two").key("copy"));

        assert_eq!(stack.entries.len(), 1);
        assert_eq!(stack.entries[0].message, "two");
        assert_eq!(stack.entries[0].kind, ToastKind::Error);
        assert!(
            !stack.begin_leave(first),
            "the replaced generation's timer must do nothing"
        );
        assert!(stack.begin_leave(second));
    }

    #[test]
    fn toasts_without_key_stack_in_order() {
        let mut stack = ToastStack::default();
        let (first, duration) = stack.push(Toast::info("a"));
        stack.push(Toast::info("b").duration(Duration::from_secs(1)));

        assert_eq!(duration, motion::TOAST);
        assert_eq!(
            stack
                .entries
                .iter()
                .map(|entry| entry.message.to_string())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );

        stack.remove(first);
        assert_eq!(stack.entries.len(), 1);
        assert!(!stack.is_empty());
    }

    #[test]
    fn status_icons_are_svg() {
        for kind in ToastKind::ALL {
            let svg = std::str::from_utf8(status_icon(kind)).unwrap();
            assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
            assert!(svg.ends_with("</svg>"));
        }
    }
}
