//! 下拉菜单与右键菜单（antd `Dropdown`）。弹层画在窗口内（gpui-component 的 PopupMenu），
//! 不开原生菜单，不抢前台。打开时菜单拿焦点，钩子转来的 ↑/↓/←/→/Enter/Esc 由它处理。

use std::{rc::Rc, sync::Arc};

use gpui::{
    Anchor, AnyElement, App, Context, DismissEvent, Div, ElementId, Entity, Focusable as _,
    ImageSource, InteractiveElement, Interactivity, IntoElement, MouseButton, ParentElement,
    Pixels, Rems, RenderImage, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement,
    Styled, Window, div, img, prelude::FluentBuilder as _,
};
use gpui_base::Selectable;
use gpui_component::{
    menu::{ContextMenuExt as _, DropdownMenu, PopupMenu, PopupMenuItem},
    popover::Popover,
};

use crate::{
    icon::IconName,
    styled::KpStyled as _,
    theme::{self, TextSize, space},
};

type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// gpui-component 菜单的 key context。
const POPUP_MENU_CONTEXT: &str = "PopupMenu";

/// 菜单项前面的图标。
#[derive(Clone, Debug)]
pub enum MenuIcon {
    Name(IconName),
    /// 资源路径（见 [`crate::Assets`]），例如分组图标。
    Path(SharedString),
}

impl MenuIcon {
    fn kit_icon(self) -> gpui_component::Icon {
        match self {
            Self::Name(name) => name.kit_icon(),
            Self::Path(path) => gpui_component::Icon::empty().path(path),
        }
    }
}

/// 一个菜单项。
#[derive(Clone)]
pub struct MenuItem {
    label: SharedString,
    icon: Option<MenuIcon>,
    /// 名称前面的位图（应用图标）。与 `icon` 不同，它画在名称一侧，左侧的勾选标记照常显示。
    image: Option<Arc<RenderImage>>,
    /// 位图还没有（解码中或应用没有图标）时占住同一格的图标，让一列名称左边对齐。
    image_placeholder: Option<IconName>,
    shortcut: Option<SharedString>,
    danger: bool,
    checked: bool,
    disabled: bool,
    on_click: ClickHandler,
}

impl MenuItem {
    pub fn new(
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            icon: None,
            image: None,
            image_placeholder: None,
            shortcut: None,
            danger: false,
            checked: false,
            disabled: false,
            on_click: Rc::new(on_click),
        }
    }

    /// 右侧的快捷键提示（1.x 右键菜单的 accelerator，`text-ant-description text-xs`）。
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(MenuIcon::Name(icon));
        self
    }

    pub fn icon_path(mut self, path: impl Into<SharedString>) -> Self {
        self.icon = Some(MenuIcon::Path(path.into()));
        self
    }

    pub fn image(mut self, image: Option<Arc<RenderImage>>) -> Self {
        self.image = image;
        self
    }

    pub fn image_placeholder(mut self, icon: IconName) -> Self {
        self.image_placeholder = Some(icon);
        self
    }

    /// 危险操作：文字和图标为错误色（antd `danger: true`）。
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// 选中项（antd `selectedKeys`）。
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
}

/// 子菜单：悬停或 → 展开（1.x 右键菜单的“移动到分组”）。
#[derive(Clone)]
pub struct Submenu {
    label: SharedString,
    icon: Option<MenuIcon>,
    entries: Vec<MenuEntry>,
}

impl Submenu {
    pub fn new(label: impl Into<SharedString>, entries: Vec<MenuEntry>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            entries,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(MenuIcon::Name(icon));
        self
    }
}

/// 菜单里的一行。
#[derive(Clone)]
pub enum MenuEntry {
    Item(MenuItem),
    Submenu(Submenu),
    Separator,
}

impl From<MenuItem> for MenuEntry {
    fn from(item: MenuItem) -> Self {
        Self::Item(item)
    }
}

impl From<Submenu> for MenuEntry {
    fn from(submenu: Submenu) -> Self {
        Self::Submenu(submenu)
    }
}

/// 子菜单的最宽宽度：最多窗口的一半。筛选菜单靠右，子菜单向左展开，只有这么多地方。
const SUBMENU_MAX_WIDTH: Rems = space(60.);
/// 子菜单一行里名字以外的宽度：菜单内边距、行内边距、勾选标记、应用图标和间距。
const SUBMENU_ROW_CHROME: Rems = space(16.);

/// `label_max_width`：名字的最宽宽度，超出截断。gpui-component 包在外面的行没有 `min_w_0`，
/// 只能从名字这一层限宽。
fn build_menu(
    menu: PopupMenu,
    entries: Vec<MenuEntry>,
    label_max_width: Option<Pixels>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let menu = menu.scrollable(true);
    entries.into_iter().fold(menu, |menu, entry| match entry {
        MenuEntry::Separator => menu.separator(),
        MenuEntry::Submenu(submenu) => {
            let entries = submenu.entries;
            menu.submenu_with_icon(
                submenu.icon.map(MenuIcon::kit_icon),
                submenu.label,
                window,
                cx,
                // 来源应用这类子菜单可能很长、名字很宽：限高滚动、名字截断，否则会超出面板被裁掉。
                move |menu, window, cx| {
                    let rem = window.rem_size();
                    let max_width = SUBMENU_MAX_WIDTH
                        .to_pixels(rem)
                        .min(window.bounds().size.width / 2.);
                    let label_max_width = max_width - SUBMENU_ROW_CHROME.to_pixels(rem);
                    build_menu(
                        menu.scrollable(true),
                        entries.clone(),
                        Some(label_max_width),
                        window,
                        cx,
                    )
                },
            )
        }
        MenuEntry::Item(item) => menu.item(menu_item(item, label_max_width, cx)),
    })
}

fn menu_item(item: MenuItem, label_max_width: Option<Pixels>, cx: &App) -> PopupMenuItem {
    let tokens = theme::semantic(cx);
    let label = item.label;
    let image = item.image;
    let image_placeholder = item.image_placeholder;
    let shortcut = item.shortcut;
    let danger = item.danger;
    let color = if danger {
        tokens.status.danger.text
    } else {
        tokens.text.primary
    };
    let handler = item.on_click;

    PopupMenuItem::element(move |_, cx| {
        let tokens = theme::semantic(cx);

        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap(space(3.))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(space(2.))
                    .map(|row| match (image.clone(), image_placeholder) {
                        (Some(image), _) => {
                            row.child(img(ImageSource::Render(image)).size(space(4.)).flex_none())
                        }
                        (None, Some(icon)) => row.child(
                            crate::Icon::new(icon)
                                .size(space(4.))
                                .color(tokens.text.muted),
                        ),
                        (None, None) => row,
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when_some(label_max_width, |label, width| label.max_w(width))
                            .kp_text(TextSize::Sm)
                            .text_color(color)
                            .child(label.clone()),
                    ),
            )
            .when_some(shortcut.clone(), |row, shortcut| {
                row.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.muted)
                        .child(shortcut),
                )
            })
    })
    .checked(item.checked)
    .disabled(item.disabled)
    .on_click(move |_, window, cx| handler(window, cx))
    .when_some(item.icon, |entry, icon| {
        let icon = icon.kit_icon();
        entry.icon(if danger {
            icon.text_color(tokens.status.danger.text)
        } else {
            icon
        })
    })
}

/// 给元素挂右键菜单。`build` 在每次打开时调用；返回空列表时不弹出。
///
/// 菜单在帧外构建时就拿走焦点：gpui-component 要到画菜单那一帧的 prepaint 里才聚焦菜单，
/// 那时先画的元素（例如列表）已按旧焦点向无障碍树报过焦点，同一帧两个节点报焦点在 debug 下会 panic。
pub fn context_menu<E>(
    element: E,
    build: impl Fn(&mut Window, &mut App) -> Vec<MenuEntry> + 'static,
) -> AnyElement
where
    E: InteractiveElement + ParentElement + Styled + IntoElement + 'static,
{
    element
        .context_menu(move |menu, window, cx| {
            let entries = build(window, cx);
            let menu = build_menu(menu, entries, None, window, cx);
            if !menu.is_empty() {
                window.focus(&menu.focus_handle(cx), cx);
            }
            menu
        })
        .into_any_element()
}

/// 焦点在打开的菜单里时关掉它（连同父菜单），例如窗口隐藏时。没有打开的菜单时什么也不做。
pub fn dismiss_menu(window: &mut Window, cx: &mut App) {
    if menu_open(window) {
        window.dispatch_action(Box::new(gpui_base::actions::Cancel), cx);
    }
}

/// 焦点是否在打开的菜单里。
pub fn menu_open(window: &Window) -> bool {
    window
        .context_stack()
        .iter()
        .any(|context| context.contains(POPUP_MENU_CONTEXT))
}

/// 菜单的触发元素：一个可自由排版的容器，点击（或右键）时打开菜单。
pub struct MenuTrigger {
    base: Stateful<Div>,
    id: ElementId,
    selected: bool,
}

impl MenuTrigger {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: div().id(id.clone()),
            id,
            selected: false,
        }
    }

    /// 点击打开 `build` 给出的菜单（antd `Dropdown trigger={["click"]}`），菜单左上角对齐触发元素左下角。
    pub fn dropdown(
        self,
        build: impl Fn(&mut Window, &mut App) -> Vec<MenuEntry> + 'static,
    ) -> AnyElement {
        self.dropdown_menu(move |menu, window, cx| {
            let entries = build(window, cx);
            build_menu(menu, entries, None, window, cx)
        })
        .into_any_element()
    }

    /// 右键打开 `build` 给出的菜单，菜单左下角对齐触发元素左上角：开在它上方，不压住被右键的元素，
    /// 放不下时贴着窗口边缘。触发元素在父元素里最多占满一行。
    ///
    /// 与 [`context_menu`] 不同，菜单按触发元素定位，不按指针位置；`build` 在每次打开时调用。
    pub fn right_click_menu(
        self,
        build: impl Fn(&mut Window, &mut App) -> Vec<MenuEntry> + 'static,
    ) -> AnyElement {
        let id = Arc::new(self.id.clone());
        let menu_id = ElementId::NamedChild(id.clone(), "menu".into());

        Popover::new(ElementId::NamedChild(id, "popover".into()))
            .appearance(false)
            .overlay_closable(false)
            .mouse_button(MouseButton::Right)
            .anchor(Anchor::BottomLeft)
            .trigger_style(StyleRefinement::default().max_w_full().min_w_0())
            .trigger(self)
            .content(move |_, window, cx| {
                // 打开期间内容每帧都会调用，菜单只建一次；关上后这一格随之回收，下次打开重建。
                let slot =
                    window.use_keyed_state(menu_id.clone(), cx, |_, _| None::<Entity<PopupMenu>>);
                if let Some(menu) = slot.read(cx).clone() {
                    return menu;
                }

                let entries = build(window, cx);
                let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
                    build_menu(menu, entries, None, window, cx)
                });
                window.focus(&menu.focus_handle(cx), cx);
                let popover = cx.entity().downgrade();
                window
                    .subscribe(&menu, cx, move |_, _: &DismissEvent, window, cx| {
                        popover
                            .update(cx, |state, cx| state.dismiss(window, cx))
                            .ok();
                    })
                    .detach();
                slot.update(cx, |slot, _| *slot = Some(menu.clone()));
                menu
            })
            .into_any_element()
    }
}

impl Styled for MenuTrigger {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for MenuTrigger {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for MenuTrigger {}

impl ParentElement for MenuTrigger {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl Selectable for MenuTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }

    /// 打开菜单不改触发元素的外观：选中态由调用方按业务含义设置。
    fn open(self, _: bool) -> Self {
        self
    }
}

impl IntoElement for MenuTrigger {
    type Element = Stateful<Div>;

    fn into_element(self) -> Self::Element {
        self.base.cursor_pointer()
    }
}

impl DropdownMenu for MenuTrigger {}
