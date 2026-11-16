//! 按钮与图标按钮。颜色来自主题语义与组件 token，高度走 rem。

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, prelude::FluentBuilder as _, rems,
};
use gpui_component::{
    Disableable as _, Selectable as _, Sizable as _,
    button::{Button as KitButton, ButtonVariants as _},
};

use crate::{
    icon::IconName,
    styled::KpStyled as _,
    theme::{TextSize, control_height, radius},
    tooltip::TooltipExt as _,
};

/// 按钮类型，对应 antd 的 `type` / `danger`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    /// antd 默认按钮：容器底色加 `colorBorder` 描边。
    #[default]
    Default,
    /// antd `type="primary"`。
    Primary,
    /// antd `type="primary" danger`：危险操作的确认按钮。
    Danger,
    /// antd 默认按钮加 `danger`：容器底色，错误色描边和文字（1.x 多选栏的“删除”）。
    DangerOutline,
    /// antd `type="text"`：无底无边，悬停出现填充色；图标按钮多用它。
    Ghost,
    /// antd `type="link"`。
    Link,
}

/// 按钮高度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    /// 20 px：卡片上的快捷动作。
    XSmall,
    /// 24 px：antd `size="small"`。
    Small,
    /// 32 px：antd 默认（`controlHeight`）。
    #[default]
    Medium,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    role: Option<gpui::Role>,
    icon: Option<IconName>,
    icon_only: bool,
    kind: ButtonKind,
    size: ButtonSize,
    loading: bool,
    disabled: bool,
    selected: Option<bool>,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// 文字按钮。
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: Some(label.into()),
            accessibility_label: None,
            role: None,
            icon: None,
            icon_only: false,
            kind: ButtonKind::Default,
            size: ButtonSize::Medium,
            loading: false,
            disabled: false,
            selected: None,
            tooltip: None,
            on_click: None,
        }
    }

    /// 图标按钮。`label` 必填：它既是悬停提示，也是读屏名称。
    pub fn icon(id: impl Into<ElementId>, icon: IconName, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            icon: Some(icon),
            icon_only: true,
            tooltip: Some(label.clone()),
            kind: ButtonKind::Ghost,
            ..Self::new(id, label)
        }
    }

    /// 文字前加图标。
    pub fn with_icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn primary(self) -> Self {
        self.kind(ButtonKind::Primary)
    }

    pub fn danger(self) -> Self {
        self.kind(ButtonKind::Danger)
    }

    pub fn danger_outline(self) -> Self {
        self.kind(ButtonKind::DangerOutline)
    }

    pub fn ghost(self) -> Self {
        self.kind(ButtonKind::Ghost)
    }

    pub fn link(self) -> Self {
        self.kind(ButtonKind::Link)
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn small(self) -> Self {
        self.size(ButtonSize::Small)
    }

    pub fn xsmall(self) -> Self {
        self.size(ButtonSize::XSmall)
    }

    /// 加载中：显示转圈并忽略点击，外观保持原样。
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 选中态（切换类按钮），同时以 toggled 状态暴露给读屏。
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    /// 覆盖控件向辅助技术暴露的名称；可见文字仍保持不变。
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// 覆盖默认的 Button 角色（例如偏好设置侧栏的 Tab）。
    pub fn accessibility_role(mut self, role: gpui::Role) -> Self {
        self.role = Some(role);
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl Button {
    /// antd 默认危险按钮。gpui-component 的描边按钮底色带一层错误色，悬停又回到默认按钮的描边，
    /// 对不上 antd，所以自己画：底色 `colorBgContainer`，描边和文字 `colorError`，悬停换
    /// `colorErrorHover`；禁用时与 antd 默认按钮的禁用态相同。
    fn render_danger_outline(self, cx: &App) -> AnyElement {
        let tokens = crate::theme::semantic(cx);
        let label = self.label.unwrap_or_default();
        let accessibility_label = self
            .accessibility_label
            .clone()
            .unwrap_or_else(|| label.clone());
        let disabled = self.disabled || self.loading;
        let (height, padding) = match self.size {
            ButtonSize::XSmall => (control_height::XS, rems(0.25)),
            ButtonSize::Small => (control_height::SM, rems(0.4375)),
            ButtonSize::Medium => (control_height::MD, rems(0.9375)),
        };
        let text_size = match self.size {
            ButtonSize::XSmall => TextSize::Xs,
            ButtonSize::Small | ButtonSize::Medium => TextSize::Sm,
        };
        let on_click = self.on_click;
        let button = div()
            .id(self.id.clone())
            .role(self.role.unwrap_or(gpui::Role::Button))
            .aria_label(accessibility_label)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(rems(0.5))
            .h(height)
            .px(padding)
            .rounded(match self.size {
                ButtonSize::Medium => radius::MD,
                _ => radius::SM,
            })
            .border_1()
            .kp_text(text_size)
            .whitespace_nowrap()
            .map(|button| {
                if disabled {
                    button
                        .border_color(tokens.border.default)
                        .bg(tokens.fill.subtle)
                        .text_color(tokens.text.faint)
                } else {
                    button
                        .cursor_pointer()
                        .border_color(tokens.status.danger.solid)
                        .text_color(tokens.status.danger.solid)
                        .hover(|style| {
                            style
                                .border_color(tokens.status.danger.hover)
                                .text_color(tokens.status.danger.hover)
                        })
                }
            })
            .when_some(self.icon, |button, icon| {
                button.child(crate::Icon::new(icon).size(rems(0.875)))
            })
            .child(label)
            .when_some(on_click.filter(|_| !disabled), |button, on_click| {
                button.on_click(move |event, window, cx| on_click(event, window, cx))
            })
            .into_any_element();

        match self.tooltip {
            Some(tooltip) => tooltip_host(self.id, button, tooltip),
            None => button,
        }
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.kind == ButtonKind::DangerOutline {
            return self.render_danger_outline(cx);
        }

        let label = self.label.unwrap_or_default();
        let accessibility_label = self
            .accessibility_label
            .clone()
            .unwrap_or_else(|| label.clone());
        let button = KitButton::new(self.id.clone())
            .map(|button| match self.kind {
                ButtonKind::Default | ButtonKind::DangerOutline => button,
                ButtonKind::Primary => button.primary(),
                ButtonKind::Danger => button.danger(),
                ButtonKind::Ghost => button.ghost(),
                ButtonKind::Link => button.link(),
            })
            .map(|button| match self.size {
                ButtonSize::XSmall => button.xsmall(),
                ButtonSize::Small => button.small(),
                // gpui-component 的默认尺寸是 16 px 字；antd 默认按钮是 14 px 字、32 px 高、左右 15 px，
                // 所以借小号的字号和图标，再把高度和内边距撑回 antd 的值。
                ButtonSize::Medium => button.small().h(control_height::MD).map(|button| {
                    if self.icon_only {
                        button.w(control_height::MD)
                    } else {
                        button.px(rems(0.9375))
                    }
                }),
            })
            .when_some(self.icon, |button, icon| button.icon(icon.kit_icon()))
            // 转圈图标替换的是按钮自己的图标，纯文字按钮加载时要先给一个占位图标。
            .when(self.loading && self.icon.is_none(), |button| {
                button.icon(gpui_component::Icon::new(
                    gpui_component::IconName::LoaderCircle,
                ))
            })
            .map(|button| {
                let button = if self.icon_only {
                    button.accessibility_label(accessibility_label.clone())
                } else {
                    button.label(label.clone())
                };
                button.when_some(self.accessibility_label.clone(), |button, label| {
                    button.accessibility_label(label)
                })
            })
            .loading(self.loading)
            .disabled(self.disabled)
            .when_some(self.selected, |button, selected| {
                button.selected(selected).toggled(selected)
            })
            .when_some(self.role, |button, role| button.role(role))
            .when_some(self.on_click, |button, on_click| {
                button.on_click(move |event, window, cx| on_click(event, window, cx))
            });

        let Some(tooltip) = self.tooltip else {
            return button.into_any_element();
        };

        tooltip_host(self.id, button.into_any_element(), tooltip)
    }
}

/// 给不带 Tooltip 接口的子元素包一层只负责悬停提示的容器。
fn tooltip_host(id: ElementId, child: AnyElement, tooltip: SharedString) -> AnyElement {
    div()
        .id((id, "tooltip"))
        .flex_none()
        .child(child)
        .kp_tooltip(tooltip)
        .into_any_element()
}
