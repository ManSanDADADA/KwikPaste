//! 数字输入框（antd `InputNumber` 加单位后缀）：只能输入非负整数，失焦或回车时按范围夹取后提交，
//! 避免连续输入时频繁落盘（1.x `NumberControl`）。

use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Rems,
    RenderOnce, SharedString, Styled as _, Subscription, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::input::{Input as KitInput, InputEvent, InputState};

use crate::{styled::KpStyled as _, theme};

/// 输入框的内容状态与取值范围，在视图里长期持有，渲染时交给 [`NumberInput`]。
#[derive(Clone)]
pub struct NumberInputState {
    state: Entity<InputState>,
    min: u64,
    max: u64,
}

impl NumberInputState {
    pub fn new(value: u64, min: u64, max: u64, window: &mut Window, cx: &mut App) -> Self {
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .context_menu(false)
                .validate(|text, _| text.bytes().all(|byte| byte.is_ascii_digit()))
                .default_value(value.to_string())
        });

        Self { state, min, max }
    }

    /// 外部取值变了（例如设置从别处改动）时写回，不触发提交。
    pub fn set_value(&self, value: u64, window: &mut Window, cx: &mut App) {
        let text = value.to_string();
        if self.state.read(cx).value().as_ref() == text {
            return;
        }
        self.state
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    /// 当前输入夹到范围内的值；输入为空时取下限。
    pub fn clamped(&self, cx: &App) -> u64 {
        clamp(self.state.read(cx).value().as_ref(), self.min, self.max)
    }

    /// 正在编辑时按失焦提交一次。关窗前用：移除窗口不会再发失焦，没提交的输入会丢。
    pub fn commit_if_focused(&self, window: &Window, cx: &mut App) {
        if self.state.read(cx).focus_handle(cx).is_focused(window) {
            self.state.update(cx, |_, cx| cx.emit(InputEvent::Blur));
        }
    }

    /// 失焦或回车时回调，给出夹取后的值（同时把输入框改成这个值）。返回的订阅要由视图保存。
    pub fn on_commit<V: 'static>(
        &self,
        window: &mut Window,
        cx: &mut Context<V>,
        handler: impl Fn(&mut V, u64, &mut Window, &mut Context<V>) + 'static,
    ) -> Subscription {
        let (min, max) = (self.min, self.max);
        cx.subscribe_in(
            &self.state,
            window,
            move |view, state, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    return;
                }
                let value = clamp(state.read(cx).value().as_ref(), min, max);
                let text = value.to_string();
                if state.read(cx).value().as_ref() != text {
                    state.update(cx, |state, cx| state.set_value(text, window, cx));
                }
                handler(view, value, window, cx);
            },
        )
    }
}

/// 空输入取下限；数字太长、超出 `u64` 时取上限。
fn clamp(text: &str, min: u64, max: u64) -> u64 {
    let max = max.max(min);
    if text.is_empty() {
        return min;
    }

    text.parse::<u64>()
        .map_or(max, |value| value.clamp(min, max))
}

/// 数字输入框。单位文字（秒、MB……）显示在框内右侧。
#[derive(IntoElement)]
pub struct NumberInput {
    state: NumberInputState,
    suffix: Option<SharedString>,
    width: Option<Rems>,
    disabled: bool,
    accessibility_label: Option<SharedString>,
}

impl NumberInput {
    pub fn new(state: &NumberInputState) -> Self {
        Self {
            state: state.clone(),
            suffix: None,
            width: None,
            disabled: false,
            accessibility_label: None,
        }
    }

    pub fn suffix(mut self, suffix: impl Into<SharedString>) -> Self {
        self.suffix = Some(suffix.into());
        self
    }

    pub fn width(mut self, width: Rems) -> Self {
        self.width = Some(width);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 读屏名称（偏好窗里用设置行标题）。
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }
}

impl RenderOnce for NumberInput {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme::semantic(cx);

        KitInput::new(&self.state.state)
            .when_some(self.width, |input, width| input.w(width).max_w_full())
            .disabled(self.disabled)
            .when_some(self.suffix, |input, suffix| {
                input.suffix(
                    div()
                        .flex_none()
                        .kp_text(theme::TextSize::Sm)
                        .text_color(if self.disabled {
                            tokens.border_disabled
                        } else {
                            tokens.text.secondary
                        })
                        .child(suffix),
                )
            })
            .when_some(self.accessibility_label, |input, label| {
                input.aria_label(label)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::clamp;

    #[test]
    fn drafts_are_clamped_into_the_range() {
        assert_eq!(clamp("", 5, 86_400), 5);
        assert_eq!(clamp("3", 5, 86_400), 5);
        assert_eq!(clamp("120", 5, 86_400), 120);
        assert_eq!(clamp("999999", 5, 86_400), 86_400);
        assert_eq!(clamp("99999999999999999999999", 0, 500), 500);
        assert_eq!(clamp("7", 10, 1), 10);
    }
}
