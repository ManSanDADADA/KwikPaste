//! 单行输入框、搜索框与多行文本框。
//!
//! 右键菜单一律关闭：gpui-component 的输入框菜单在 Windows 上是 Win32 原生菜单
//! （`SetForegroundWindow` + `TrackPopupMenuEx`），会抢前台、破坏不激活面板。

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityInputHandler as _, FocusHandle,
    Focusable as _, IntoElement, ParentElement as _, Rems, RenderOnce, SharedString, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    FocusableExt as _, Sizable as _,
    input::{Input as KitInput, InputEvent, InputState, Textarea as KitTextarea, TextareaState},
};

use crate::{
    icon::{Icon, IconName},
    tag::KeyHint,
    theme::{self, space},
};

/// 输入框的 key context。应用要在输入框聚焦时改写某个键（例如搜索框把 ↑/↓ 交给列表），就用
/// `"<自己的 context> > Input"` 绑定：与输入框自己的绑定同深度，后注册的优先。
pub const INPUT_KEY_CONTEXT: &str = "Input";

/// 输入框的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputEvent {
    /// 内容变了（输入、粘贴、清空）。输入法组字期间不发，组字结束时补发一次。
    Change(SharedString),
    Focus,
    Blur,
}

/// 输入框的内容状态，在视图里长期持有，渲染时交给 [`Input`]。
#[derive(Clone)]
pub struct TextInput {
    state: Entity<InputState>,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, window: &mut Window, cx: &mut App) -> Self {
        let placeholder = placeholder.into();
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .context_menu(false)
        });

        Self { state }
    }

    pub fn value(&self, cx: &App) -> SharedString {
        self.state.read(cx).value()
    }

    /// 改写内容，不触发变化回调。
    pub fn set_value(&self, value: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
        let value: SharedString = value.into();
        self.state.update(cx, |state, cx| {
            state.set_value(value.to_string(), window, cx)
        });
    }

    /// 切换语言时更新占位文字。
    pub fn set_placeholder(
        &self,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let placeholder = placeholder.into();
        self.state.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx)
        });
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle(cx).is_focused(window)
    }

    /// 全选已有内容（再次聚焦搜索框时便于直接覆盖输入）。
    pub fn select_all(&self, window: &mut Window, cx: &mut App) {
        self.state
            .update(cx, |state, cx| state.select_all(window, cx));
    }

    /// 内容变化时回调（输入、粘贴、清空，含输入法组字的中间态）。返回的订阅要由视图保存。
    pub fn on_change<V: 'static>(
        &self,
        cx: &mut Context<V>,
        handler: impl Fn(&mut V, SharedString, &mut Context<V>) + 'static,
    ) -> Subscription {
        cx.subscribe(&self.state, move |view, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = state.read(cx).value();
                handler(view, value, cx);
            }
        })
    }

    /// 失焦或回车时回调当前内容，用于不该每个键都落盘的输入。返回的订阅要由视图保存。
    pub fn on_commit<V: 'static>(
        &self,
        window: &mut Window,
        cx: &mut Context<V>,
        handler: impl Fn(&mut V, SharedString, &mut Window, &mut Context<V>) + 'static,
    ) -> Subscription {
        cx.subscribe_in(
            &self.state,
            window,
            move |view, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    handler(view, state.read(cx).value(), window, cx);
                }
            },
        )
    }

    /// 内容变化（输入法组字期间不发，组字结束时补发）、获得焦点、失去焦点。返回的订阅要由视图保存。
    pub fn on_event_in<V: 'static>(
        &self,
        window: &mut Window,
        cx: &mut Context<V>,
        handler: impl Fn(&mut V, TextInputEvent, &mut Window, &mut Context<V>) + 'static,
    ) -> Subscription {
        cx.subscribe_in(
            &self.state,
            window,
            move |view, state, event: &InputEvent, window, cx| {
                let event = match event {
                    InputEvent::Change => {
                        // 1.x 搜索框在 compositionstart..compositionend 之间不回调：拼音的中间串
                        // 不该触发查询。组字确认时输入框再发一次 Change，那时已没有标记文本。
                        let composing = state.update(cx, |state, cx| {
                            state.marked_text_range(window, cx).is_some()
                        });
                        if composing {
                            return;
                        }
                        TextInputEvent::Change(state.read(cx).value())
                    }
                    InputEvent::Focus => TextInputEvent::Focus,
                    InputEvent::Blur => TextInputEvent::Blur,
                    InputEvent::PressEnter { .. } => return,
                };
                handler(view, event, window, cx);
            },
        )
    }
}

/// 输入框尺寸。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputSize {
    /// 24 px：antd `size="small"`，主窗口搜索框。
    Small,
    /// 32 px：antd 默认。
    #[default]
    Medium,
}

#[derive(IntoElement)]
pub struct Input {
    input: TextInput,
    size: InputSize,
    width: Option<Rems>,
    cleanable: bool,
    disabled: bool,
    search: bool,
    hint_key: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    chips: Vec<AnyElement>,
    suffix: Option<AnyElement>,
}

impl Input {
    pub fn new(input: &TextInput) -> Self {
        Self {
            input: input.clone(),
            size: InputSize::Medium,
            width: None,
            cleanable: false,
            disabled: false,
            search: false,
            hint_key: None,
            accessibility_label: None,
            chips: Vec::new(),
            suffix: None,
        }
    }

    /// 搜索框：前缀放搜索图标、可清空。不给宽度时占满父元素（主窗口头部）。
    pub fn search(input: &TextInput) -> Self {
        Self {
            search: true,
            cleanable: true,
            ..Self::new(input)
        }
    }

    pub fn small(mut self) -> Self {
        self.size = InputSize::Small;
        self
    }

    pub fn width(mut self, width: Rems) -> Self {
        self.width = Some(width);
        self
    }

    pub fn cleanable(mut self, cleanable: bool) -> Self {
        self.cleanable = cleanable;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 按住修饰键时搜索图标换成快捷键角标（1.x 搜索框的 `KeyHint hintKey="F"`）；`None` 时显示图标。
    pub fn hint_key(mut self, key: Option<SharedString>) -> Self {
        self.hint_key = key;
        self
    }

    /// 搜索框：排在搜索图标后、输入文字前的标签（已选的筛选条件）。
    pub fn chips(mut self, chips: Vec<AnyElement>) -> Self {
        self.chips = chips;
        self
    }

    /// 排在清空按钮后面的元素。
    pub fn suffix(mut self, suffix: impl IntoElement) -> Self {
        self.suffix = Some(suffix.into_any_element());
        self
    }

    /// 读屏名称；不给时用占位文字。
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }
}

impl RenderOnce for Input {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let focused = self.input.is_focused(window, cx);
        let icon_size = match self.size {
            InputSize::Small => theme::TextSize::Sm.font_size(),
            InputSize::Medium => theme::TextSize::Base.font_size(),
        };
        let hint_key = self.hint_key;
        let chips = self.chips;

        KitInput::new(&self.input.state)
            .when(self.size == InputSize::Small, |input| input.small())
            .when_some(self.width, |input, width| input.w(width).max_w_full())
            .cleanable(self.cleanable)
            .disabled(self.disabled)
            .when(self.search, |input| {
                // 搜索框是填充底的胶囊，不画描边；打开即聚焦时它几乎常驻聚焦态，所以聚焦只把
                // 搜索图标换成主色，不要描边和光晕。
                // 角标叠在图标的位置上，图标只是隐去，宽度不变，输入文字不会跳动。
                let icon_color = if focused {
                    tokens.accent.solid
                } else {
                    tokens.text.faint
                };
                input
                    .bg(tokens.fill.subtle)
                    .border_color(theme::transparent())
                    .focus_ring(false)
                    .rounded_full()
                    .prefix(
                        div()
                            .flex()
                            .items_center()
                            .gap(space(1.))
                            .child(
                                div()
                                    .relative()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .when(hint_key.is_some(), |icon| icon.opacity(0.))
                                            .child(
                                                Icon::new(IconName::Search)
                                                    .size(icon_size)
                                                    .color(icon_color),
                                            ),
                                    )
                                    .when_some(hint_key, |prefix, key| {
                                        prefix.child(
                                            div()
                                                .absolute()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .size_full()
                                                .child(KeyHint::new(key)),
                                        )
                                    }),
                            )
                            .children(chips),
                    )
            })
            .when_some(self.suffix, |input, suffix| input.suffix(suffix))
            .when_some(self.accessibility_label, |input, label| {
                input.aria_label(label)
            })
    }
}

/// 多行文本框的内容状态，在视图里长期持有，渲染时交给 [`TextArea`]。
#[derive(Clone)]
pub struct TextAreaInput {
    state: Entity<TextareaState>,
}

impl TextAreaInput {
    /// 随内容在 `min_rows`..=`max_rows` 行之间自动增高（antd `autoSize`）。
    pub fn new(
        placeholder: impl Into<SharedString>,
        min_rows: usize,
        max_rows: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let placeholder = placeholder.into();
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder)
                .context_menu(false)
                .auto_grow(min_rows, max_rows)
        });

        Self { state }
    }

    /// 不随内容增高、超出时滚动的文本框，光标在开头；渲染时配 [`TextArea::fill`] 撑满父元素。
    pub fn filling(value: impl Into<SharedString>, window: &mut Window, cx: &mut App) -> Self {
        let value = value.into();
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .context_menu(false)
                .default_value(value)
        });

        Self { state }
    }

    /// 多行文本变化时通知拥有者；返回的订阅需要长期保存。
    pub fn on_change<V: 'static>(
        &self,
        cx: &mut Context<V>,
        handler: impl Fn(&mut V, SharedString, &mut Context<V>) + 'static,
    ) -> Subscription {
        cx.observe(&self.state, move |view, state, cx| {
            handler(view, state.read(cx).value(), cx);
        })
    }

    pub fn value(&self, cx: &App) -> SharedString {
        self.state.read(cx).value()
    }

    /// 改写内容，不触发变化回调；光标放到末尾（多行框默认会落在开头）。
    pub fn set_value(&self, value: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
        let value: SharedString = value.into();
        self.state.update(cx, |state, cx| {
            state.set_value(value.to_string(), window, cx);
            let end = state.value().len();
            state.set_selected_range(end..end, cx);
        });
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }
}

/// 多行文本框（antd `Input.TextArea`）。
#[derive(IntoElement)]
pub struct TextArea {
    input: TextAreaInput,
    fill: bool,
}

impl TextArea {
    pub fn new(input: &TextAreaInput) -> Self {
        Self {
            input: input.clone(),
            fill: false,
        }
    }

    /// 高度撑满父元素（[`TextAreaInput::filling`] 的文本框）。
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }
}

impl RenderOnce for TextArea {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        KitTextarea::new(&self.input.state)
            .w_full()
            .when(self.fill, |area| area.h(gpui::relative(1.)))
    }
}
