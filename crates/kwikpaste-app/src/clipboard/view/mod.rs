//! 主窗口的视图层（附录 D §3）：头部与搜索框、分组栏、列表（含卡片快捷动作、多选、页脚）。
// render、prepaint、paint 和事件回调里 panic 会让进程以 0xC0000409 中止（附录 D §2.4 第 4 条）。
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)
)]

pub mod bench;
mod card;
mod editing;
mod editor;
mod frame;
mod group_bar;
pub mod group_dialogs;
mod header;
pub mod host;
pub(crate) mod image_cache;
mod list;
mod panel;
pub mod pin;
mod preview;
mod screen_pin;
pub mod selftest;
mod shortcuts;
mod viewer;

use gpui::{Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, actions};
use kwikpaste_core::settings::{PanelAction, Shortcuts, effective_panel_keys};
use kwikpaste_ui::INPUT_KEY_CONTEXT;

pub(crate) use card::logo as app_logo;
pub use editing::request_panel;
pub(crate) use group_bar::entry_glyph as group_bar_glyph;
pub(crate) use list::quick_action_glyph;
pub use panel::{ClipboardPanel, PanelIntent};

/// 主窗口的 key context。Windows 上面板收不到键盘消息，平台层的钩子用 `Window::dispatch_keystroke`
/// 把按键注入面板窗口，按焦点所在的路径匹配这里的绑定；列表和搜索框都在这个 context 之下。
pub const KEY_CONTEXT: &str = "ClipboardPanel";
/// 搜索框外层的 key context：输入框聚焦时 ↑/↓/Enter/Esc/Tab 交给列表（1.x `data-allow-global-keyboard`）。
pub const SEARCH_CONTEXT: &str = "ClipboardSearch";

actions!(
    clipboard_panel,
    [
        /// ↑：当前项上移。
        SelectPrevious,
        /// ↓：当前项下移。
        SelectNext,
        /// Enter：粘贴当前项；多选时勾选 / 取消当前项。
        PasteSelected,
        /// Mod+Enter：粘贴为纯文本 / 路径。
        PasteSelectedPlain,
        /// Esc：按多选、分组、分类、窗口的顺序逐层退出。
        Dismiss,
        /// Mod+F：聚焦搜索框（进入编辑态）。
        FocusSearch,
        /// Mod+Q：切换全部 / 收藏。
        ToggleRange,
        /// ←：上一个分类。
        PreviousCategory,
        /// →：下一个分类。
        NextCategory,
        /// Tab：下一个自定义分组。
        NextGroup,
        /// Shift+Tab：上一个自定义分组。
        PreviousGroup,
        /// Mod+C：复制当前项。
        CopySelected,
        /// Mod+D：收藏 / 取消收藏当前项。
        ToggleFavorite,
        /// Mod+T：置顶 / 取消置顶当前项。
        TogglePinned,
        /// Mod+M：编辑当前项的备注。
        EditNote,
        /// Mod+E：编辑当前项的内容。
        EditContent,
        /// 用户改过键的「查看」：图片看大图、文本编辑内容（默认 Shift+空格由列表监听处理）。
        ViewSelected,
        /// Delete / Mod+Delete / Mod+Backspace：删除当前项；多选时删除已勾选的记录。
        DeleteSelected,
        /// Mod+O：打开链接 / 邮箱 / 文件位置。
        OpenSelected,
        /// Mod+S：拆词。
        SplitSelected,
        /// Mod+L：翻译当前项。
        TranslateSelected,
        /// Mod+A：进入多选并全选 / 取消全选。
        SelectAll,
        /// Mod+P：固定 / 取消固定窗口。
        PinWindow,
        /// Mod+,：打开偏好设置。
        OpenPreferences,
        /// Mod+K：快捷键列表。
        ShowShortcuts,
        /// Mod+N：新增分组。
        NewGroup,
    ]
);

/// Mod+数字：粘贴第 N 个可见项（包含置顶行）（`slot` 0–9 对应数字键 1–9、0）。
#[derive(Clone, Debug, PartialEq, Eq, Action)]
#[action(namespace = clipboard_panel, no_json)]
pub struct QuickPaste {
    pub key: char,
}

/// 从有效设置生成绑定；搜索镜像必须排在组件输入绑定之后。
fn bindings(shortcuts: &Shortcuts) -> anyhow::Result<Vec<KeyBinding>> {
    let panel: std::rc::Rc<KeyBindingContextPredicate> =
        KeyBindingContextPredicate::parse(KEY_CONTEXT)?.into();
    let search: std::rc::Rc<KeyBindingContextPredicate> =
        KeyBindingContextPredicate::parse(&format!("{SEARCH_CONTEXT} > {INPUT_KEY_CONTEXT}"))?
            .into();
    let mut bindings = Vec::new();
    for action in PanelAction::ALL {
        for (index, key) in effective_panel_keys(shortcuts, action)
            .into_iter()
            .enumerate()
        {
            let command: Box<dyn Action> = match action {
                PanelAction::PasteSelected => Box::new(PasteSelected),
                PanelAction::PasteSelectedPlain => Box::new(PasteSelectedPlain),
                PanelAction::PasteNth => {
                    let Some(key) = key.chars().last() else {
                        continue;
                    };
                    Box::new(QuickPaste { key })
                }
                PanelAction::CopySelected => Box::new(CopySelected),
                PanelAction::FavoriteSelected => Box::new(ToggleFavorite),
                PanelAction::PinSelected => Box::new(TogglePinned),
                PanelAction::NoteSelected => Box::new(EditNote),
                PanelAction::EditContent => Box::new(EditContent),
                PanelAction::SplitSelected => Box::new(SplitSelected),
                PanelAction::TranslateSelected => Box::new(TranslateSelected),
                PanelAction::OpenSelected => Box::new(OpenSelected),
                PanelAction::DeleteSelected => Box::new(DeleteSelected),
                PanelAction::SelectAll => Box::new(SelectAll),
                PanelAction::Navigate if index == 0 => Box::new(SelectPrevious),
                PanelAction::Navigate => Box::new(SelectNext),
                PanelAction::SwitchCategory if index == 0 => Box::new(PreviousCategory),
                PanelAction::SwitchCategory => Box::new(NextCategory),
                PanelAction::SwitchCustomGroup if index == 0 => Box::new(NextGroup),
                PanelAction::SwitchCustomGroup => Box::new(PreviousGroup),
                PanelAction::FocusSearch => Box::new(FocusSearch),
                PanelAction::ToggleRange => Box::new(ToggleRange),
                PanelAction::CreateGroup => Box::new(NewGroup),
                // 按住预览、默认 Shift+空格查看由列表的按下、松开监听处理：绑成动作会吞掉搜索框里的输入。
                PanelAction::PreviewSelected => continue,
                PanelAction::ViewImage if key == "shift-space" => continue,
                PanelAction::ViewImage => Box::new(ViewSelected),
                PanelAction::PinWindow => Box::new(PinWindow),
                PanelAction::ShowShortcuts => Box::new(ShowShortcuts),
                PanelAction::OpenPreference => Box::new(OpenPreferences),
                PanelAction::ClosePreviewFilterWindow => Box::new(Dismiss),
            };
            bindings.push(KeyBinding::load(
                &key,
                command.boxed_clone(),
                Some(panel.clone()),
                false,
                None,
                &DummyKeyboardMapper,
            )?);
            if matches!(
                action,
                PanelAction::Navigate
                    | PanelAction::PasteSelected
                    | PanelAction::PasteSelectedPlain
                    | PanelAction::ClosePreviewFilterWindow
                    | PanelAction::SwitchCustomGroup
                    | PanelAction::FocusSearch
            ) {
                bindings.push(KeyBinding::load(
                    &key,
                    command,
                    Some(search.clone()),
                    false,
                    None,
                    &DummyKeyboardMapper,
                )?);
            }
        }
    }
    Ok(bindings)
}

/// 去掉面板自己的旧绑定，保留其它组件的原始注册顺序（尤其是输入框）。
fn without_panel_bindings(bindings: impl Iterator<Item = KeyBinding>) -> Vec<KeyBinding> {
    let search = format!("{SEARCH_CONTEXT} > {INPUT_KEY_CONTEXT}");
    bindings
        .filter(|binding| {
            !binding.predicate().is_some_and(|predicate| {
                let context = predicate.to_string();
                context == KEY_CONTEXT || context == search
            })
        })
        .collect()
}

/// 构造成功后再重绑；不重复初始化组件、主题或它们的订阅。
pub fn rebind(shortcuts: &Shortcuts, cx: &mut App) {
    let bindings = match bindings(shortcuts) {
        Ok(bindings) => bindings,
        Err(error) => {
            log::error!("could not build panel key bindings: {error:#}");
            return;
        }
    };
    let retained = without_panel_bindings(cx.key_bindings().borrow().bindings().cloned());
    cx.clear_key_bindings();
    cx.bind_keys(retained);
    cx.bind_keys(bindings);
    #[cfg(target_os = "windows")]
    kwikpaste_os::hook_keys::apply(shortcuts);
}

/// 组件层初始化之后注册默认面板键；core 加载后再应用用户设置。
pub fn init(cx: &mut App) {
    rebind(&Shortcuts::default(), cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebind_preserves_input_order_and_replaces_search_mirrors() {
        let mut previous = vec![KeyBinding::new(
            "enter",
            FocusSearch,
            Some(INPUT_KEY_CONTEXT),
        )];
        previous.extend(bindings(&Shortcuts::default()).unwrap());
        let mut shortcuts = Shortcuts::default();
        shortcuts.panel.insert("pasteSelected".into(), "f6".into());
        shortcuts
            .panel
            .insert("focusSearch".into(), "secondary-shift-b".into());
        shortcuts.panel.insert("copySelected".into(), String::new());
        let mut rebound = without_panel_bindings(previous.into_iter());
        assert_eq!(rebound.len(), 1);
        assert_eq!(
            rebound[0].predicate().unwrap().to_string(),
            INPUT_KEY_CONTEXT
        );
        rebound.extend(bindings(&shortcuts).unwrap());
        let pastes: Vec<_> = rebound
            .iter()
            .filter(|binding| binding.action().as_any().is::<PasteSelected>())
            .collect();
        assert_eq!(pastes.len(), 2);
        assert!(
            pastes
                .iter()
                .all(|binding| binding.keystrokes()[0].unparse() == "f6")
        );
        assert!(
            !rebound
                .iter()
                .any(|binding| binding.action().as_any().is::<CopySelected>())
        );
        let wanted = gpui::Keystroke::parse("secondary-shift-b")
            .unwrap()
            .unparse();
        assert_eq!(
            rebound
                .iter()
                .filter(|binding| binding.action().as_any().is::<FocusSearch>()
                    && binding.keystrokes()[0].unparse() == wanted)
                .count(),
            2
        );
    }

    /// 钩子吞下的每个精确组合都有面板绑定（不然目标应用收不到，面板也不处理）。无修饰空格是按住预览，
    /// 由列表的按下、松开监听处理，不走绑定。
    #[test]
    fn every_hooked_combo_has_an_exact_panel_binding() {
        assert_hook_bindings(&Shortcuts::default());
        let mut shortcuts = Shortcuts::default();
        shortcuts
            .panel
            .insert("favoriteSelected".into(), "secondary-shift-b".into());
        shortcuts
            .panel
            .insert("pasteNth".into(), "secondary-shift".into());
        shortcuts.panel.insert("copySelected".into(), String::new());
        shortcuts
            .panel
            .insert("viewItem".into(), "secondary-shift-v".into());
        assert_hook_bindings(&shortcuts);
    }

    fn assert_hook_bindings(shortcuts: &Shortcuts) {
        let bound: Vec<String> = bindings(shortcuts)
            .unwrap()
            .iter()
            .filter(|binding| {
                binding
                    .predicate()
                    .is_some_and(|predicate| predicate.to_string() == KEY_CONTEXT)
            })
            .map(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .map(|keystroke| keystroke.unparse())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();

        for combo in kwikpaste_os::hook_keys::HookKeys::from_shortcuts(shortcuts).keystrokes() {
            if combo == "space" || combo == "shift-space" {
                continue;
            }
            // Windows 的 Ctrl 在 macOS 绑定里对应 secondary（⌘），其余修饰键仍须精确一致。
            let portable = combo
                .strip_prefix("ctrl-")
                .map(|rest| format!("secondary-{rest}"))
                .unwrap_or_else(|| combo.clone());
            let wanted = gpui::Keystroke::parse(&portable)
                .expect("hook combo parses")
                .unparse();
            assert!(
                bound.contains(&wanted),
                "{combo} is hooked but not exactly bound"
            );
        }
    }

    #[test]
    fn delete_shortcuts_share_the_panel_action_without_overriding_inputs() {
        let bindings = bindings(&Shortcuts::default()).unwrap();
        let delete_bindings: Vec<_> = bindings
            .iter()
            .filter(|binding| binding.action().as_any().is::<DeleteSelected>())
            .collect();
        assert_eq!(delete_bindings.len(), 3);
        for binding in &delete_bindings {
            assert_eq!(
                binding.predicate().expect("panel context").to_string(),
                KEY_CONTEXT
            );
        }
        for combo in ["delete", "secondary-delete", "secondary-backspace"] {
            let wanted = gpui::Keystroke::parse(combo)
                .expect("delete combo parses")
                .unparse();
            assert!(delete_bindings.iter().any(|binding| {
                binding.keystrokes().len() == 1 && binding.keystrokes()[0].unparse() == wanted
            }));
        }
    }
}
