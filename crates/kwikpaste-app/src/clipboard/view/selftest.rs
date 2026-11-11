//! 主窗口的交互自测（`--selftest-panel-ui`）与截图用的演示状态（`--selftest-list-demo` 加
//! `KP_PANEL_DEMO`）。
//!
//! 交互自测在示例夹具上跑一段脚本：按键经 `Window::dispatch_keystroke` 派发给面板窗口，与 Windows
//! 钩子转来的按键走同一条路；修饰键经 `ModifiersChanged`。界面发给面板的命令（进出编辑态、隐藏）
//! 只记进 [`RequestLog`] 不执行，脚本再自己发出相应的面板事件，所以不会取前台、不会隐藏面板，
//! 也不碰剪贴板（夹具数据源不写系统剪贴板）。每一步都检查状态，最后以退出码报告：0 通过，1 失败。

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Capslock, Entity, Focusable as _, Keystroke,
    Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PlatformInput, point, px,
};
use kwikpaste_ui::{MenuEntry, close_dialog, has_dialog, menu_open};

use super::{
    editing::{self, RequestLog},
    group_dialogs::{self, GroupEditor, GroupManager},
    list::{ClipboardList, ListIntent, PreviewTrigger},
    panel::ClipboardPanel,
    pin,
};
use crate::{
    clipboard::{
        model::{
            controller::ListUpdate,
            empty_state::empty_text,
            filter::{ListFilter, Range},
            item::{ItemKind, ListItem},
            menu::{MenuAction, menu_groups},
        },
        source::{ClipboardSource, FixtureStore, ListQuery, PreviewTextView},
    },
    platform::{EditTrigger, Panel, PanelCommand, PanelEvent},
};

/// 等列表数据、防抖等异步结果的上限。
const SETTLE: Duration = Duration::from_secs(5);

/// 跑交互脚本。
pub fn run(panel: Entity<ClipboardPanel>, store: Arc<Mutex<FixtureStore>>, cx: &mut App) {
    cx.set_global(RequestLog::default());
    let intents: Rc<RefCell<Vec<ListIntent>>> = Rc::default();
    let panel_intents: Rc<RefCell<Vec<super::panel::PanelIntent>>> = Rc::default();
    let list = panel.read(cx).list().clone();
    let sink = intents.clone();
    cx.subscribe(&list, move |_, intent: &ListIntent, _| {
        sink.borrow_mut().push(intent.clone());
    })
    .detach();
    let panel_sink = panel_intents.clone();
    cx.subscribe(&panel, move |_, intent: &super::panel::PanelIntent, _| {
        panel_sink.borrow_mut().push(intent.clone());
    })
    .detach();

    let window = list.read(cx).window_handle();
    cx.spawn(async move |cx| {
        let mut driver = Driver {
            panel,
            list,
            window,
            intents,
            panel_intents,
            store: Some(store),
            passed: 0,
            failed: Vec::new(),
        };
        driver.script(cx).await;

        let passed = driver.passed;
        let failed = driver.failed.len();
        for failure in &driver.failed {
            log::error!("panel ui selftest FAILED: {failure}");
        }
        log::info!("panel ui selftest: {passed} passed, {failed} failed");
        std::process::exit(i32::from(failed > 0));
    })
    .detach();
}

struct Driver {
    panel: Entity<ClipboardPanel>,
    list: Entity<ClipboardList>,
    window: AnyWindowHandle,
    intents: Rc<RefCell<Vec<ListIntent>>>,
    panel_intents: Rc<RefCell<Vec<super::panel::PanelIntent>>>,
    /// 夹具后端：模拟 core 在面板隐藏、显示前后存入新记录。
    store: Option<Arc<Mutex<FixtureStore>>>,
    passed: usize,
    failed: Vec<String>,
}

impl Driver {
    fn check(&mut self, name: &str, ok: bool, detail: impl FnOnce() -> String) {
        if ok {
            self.passed += 1;
            log::info!("panel ui selftest: ok   {name}");
        } else {
            let detail = detail();
            log::error!("panel ui selftest: FAIL {name}: {detail}");
            self.failed.push(format!("{name}: {detail}"));
        }
    }

    async fn pause(&self, cx: &mut AsyncApp, ms: u64) {
        cx.background_executor()
            .timer(Duration::from_millis(ms))
            .await;
    }

    /// 等到 `done` 成立（每 16 ms 看一次），超时返回 false。
    async fn settle(&self, cx: &mut AsyncApp, done: impl Fn(&ClipboardList, &App) -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < SETTLE {
            let list = self.list.clone();
            if cx.update(|cx| done(list.read(cx), cx)) {
                return true;
            }
            self.pause(cx, 16).await;
        }
        false
    }

    fn read<R>(&self, cx: &mut AsyncApp, read: impl FnOnce(&ClipboardList, &App) -> R) -> R {
        let list = self.list.clone();
        cx.update(|cx| read(list.read(cx), cx))
    }

    /// 派发一个按键（GPUI keystroke 字符串），与钩子转来的按键同路。
    fn key(&self, cx: &mut AsyncApp, keystroke: &str) {
        let Ok(keystroke) = Keystroke::parse(keystroke) else {
            log::error!("bad keystroke {keystroke}");
            return;
        };
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_keystroke(keystroke, cx);
            })
            .ok();
    }

    /// 按下 / 松开平台修饰键（Windows 的 Ctrl、macOS 的 ⌘）。
    fn modifier(&self, cx: &mut AsyncApp, down: bool) {
        let modifiers = Modifiers {
            control: down && !cfg!(target_os = "macos"),
            platform: down && cfg!(target_os = "macos"),
            ..Modifiers::default()
        };
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                        modifiers,
                        capslock: Capslock::default(),
                    }),
                    cx,
                );
            })
            .ok();
    }

    /// 代替平台层发出面板事件。
    fn emit(&self, cx: &mut AsyncApp, event: PanelEvent) {
        cx.update(|cx| {
            if let Some(panel) = cx.try_global::<Panel>() {
                panel.events().clone().update(cx, |_, cx| cx.emit(event));
            }
        });
    }

    fn last_request(&self, cx: &mut AsyncApp) -> Option<PanelCommand> {
        cx.update(|cx| {
            cx.try_global::<RequestLog>()
                .and_then(|log| log.commands.last().cloned())
        })
    }

    fn dialog_open(&self, cx: &mut AsyncApp) -> bool {
        self.window
            .update(cx, |_, window, cx| has_dialog(window, cx))
            .unwrap_or(false)
    }

    fn list_focused(&self, cx: &mut AsyncApp) -> bool {
        let list = self.list.clone();
        self.window
            .update(cx, |_, window, cx| {
                list.read(cx).focus_handle(cx).is_focused(window)
            })
            .unwrap_or(false)
    }

    fn search_focused(&self, cx: &mut AsyncApp) -> bool {
        let header = cx.update(|cx| self.panel.read(cx).header().clone());
        self.window
            .update(cx, |_, window, cx| {
                header.read(cx).input().is_focused(window, cx)
            })
            .unwrap_or(false)
    }

    fn source(&self, cx: &mut AsyncApp) -> Arc<dyn ClipboardSource> {
        self.read(cx, |list, _| list.source.clone())
    }

    /// 数据源里符合条件的条数（脚本的期望值，不写死在脚本里）。
    async fn expected(&self, cx: &mut AsyncApp, filter: ListFilter) -> usize {
        let source = self.source(cx);
        source
            .list(ListQuery {
                offset: 0,
                limit: 1,
                filter,
                sort: Default::default(),
                hide_favorites: false,
            })
            .await
            .map(|page| page.total)
            .unwrap_or(usize::MAX)
    }

    async fn filtered(&mut self, cx: &mut AsyncApp, name: &str, filter: ListFilter) {
        let want = self.expected(cx, filter.clone()).await;
        let ok = self
            .settle(cx, |list, _| {
                *list.filter() == filter && list.model.loaded_initial() && list.total() == want
            })
            .await;
        let (got, total) = self.read(cx, |list, _| (list.filter().clone(), list.total()));
        self.check(name, ok, || {
            format!("want {filter:?} with {want} rows, got {got:?} with {total}")
        });
    }

    fn active_index(&self, cx: &mut AsyncApp) -> usize {
        self.read(cx, |list, _| list.controller.active_index(&list.model))
    }

    /// 选中第一条满足条件的已加载记录，返回它的 id。
    fn select_where(
        &self,
        cx: &mut AsyncApp,
        wanted: impl Fn(&crate::clipboard::model::item::ListItem) -> bool,
    ) -> Option<Arc<str>> {
        let list = self.list.clone();
        cx.update(|cx| {
            list.update(cx, |list, cx| {
                let id = (0..list.total())
                    .filter_map(|index| list.model.get(index))
                    .find(|item| wanted(item))
                    .map(|item| item.id.clone())?;
                list.controller.select(&id);
                cx.notify();
                Some(id)
            })
        })
    }

    /// 头部齿轮应和托盘复用同一个偏好设置宿主意图。
    async fn header_actions(&mut self, cx: &mut AsyncApp) {
        let state = cx.update(|cx| {
            cx.try_global::<crate::platform::updater::UpdateStateHost>()
                .map(|host| host.0.clone())
        });
        if let Some(state) = state {
            let original = cx.update(|cx| state.read(cx).0.clone());
            let mut fake = original.clone();
            fake.supported = true;
            #[cfg(target_os = "windows")]
            {
                fake.install_kind = kwikpaste_updater::InstallKind::Portable;
            }
            #[cfg(target_os = "macos")]
            {
                fake.install_kind = kwikpaste_updater::InstallKind::MacApp {
                    bundle: PathBuf::from("selftest.app"),
                };
            }
            fake.update = Some(kwikpaste_updater::UpdateMetadata {
                current_version: fake.current_version.clone(),
                version: "9.9.9-selftest".to_owned(),
                date: None,
                body: None,
                target: "selftest".to_owned(),
                download_url: "https://example.invalid/update".to_owned(),
                downloaded: false,
                release_notes_url: "https://example.invalid/notes".to_owned(),
            });
            for (name, status, expected) in [
                (
                    "header shows a pending update badge",
                    fake.clone(),
                    Some("9.9.9-selftest"),
                ),
                (
                    "header hides updates that cannot self-install",
                    kwikpaste_updater::UpdateStatus {
                        supported: false,
                        ..fake.clone()
                    },
                    None,
                ),
                (
                    "header hides unmanaged updates",
                    kwikpaste_updater::UpdateStatus {
                        install_kind: kwikpaste_updater::InstallKind::Unmanaged,
                        ..fake.clone()
                    },
                    None,
                ),
                (
                    "header removes a withdrawn update badge",
                    kwikpaste_updater::UpdateStatus {
                        update: None,
                        ..fake
                    },
                    None,
                ),
            ] {
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        state.0 = status;
                        cx.notify();
                    })
                });
                self.pause(cx, 32).await;
                let version = cx.update(|cx| {
                    self.panel
                        .read(cx)
                        .header()
                        .read(cx)
                        .pending_update_version(cx)
                });
                self.check(name, version.as_deref() == expected, || {
                    format!("version {version:?}")
                });
            }
            cx.update(|cx| {
                state.update(cx, |state, cx| {
                    state.0 = original;
                    cx.notify();
                })
            });
        } else {
            self.check("updater state exists for the panel header", false, || {
                "missing UpdateStateHost".to_owned()
            });
        }

        self.panel_intents.borrow_mut().clear();
        cx.update(|cx| {
            let list = self.panel.read(cx).list().clone();
            list.update(cx, |_, cx| {
                cx.emit(super::list::ListIntent::OpenPreferences);
            });
        });
        let opened = self
            .panel_intents
            .borrow()
            .iter()
            .any(|intent| matches!(intent, super::panel::PanelIntent::OpenPreferences));
        let intents = self.panel_intents.borrow().clone();
        self.check(
            "footer settings emits the preferences intent",
            opened,
            || format!("intents {intents:?}"),
        );
    }

    /// Windows 原生子类把 GPUI 的 Drag 区作为标题命中，按钮仍留在客户区。
    #[cfg(target_os = "windows")]
    async fn drag_regions(&mut self, _cx: &mut AsyncApp) {
        let (caption, client) = kwikpaste_os::win::panel::selftest_drag_hit_regions();
        self.check("drag region uses the native caption hit", caption, || {
            "HTCAPTION was not accepted by the panel subclass".into()
        });
        self.check(
            "drag region leaves client buttons clickable",
            !client,
            || "HTCLIENT was treated as a drag".into(),
        );
    }

    async fn script(&mut self, cx: &mut AsyncApp) {
        let loaded = self
            .settle(cx, |list, _| {
                list.model.loaded_initial() && list.total() > 0
            })
            .await;
        self.check("loads the sample fixture", loaded, || "no rows".into());
        if !loaded {
            return;
        }
        let all = self.read(cx, |list, _| list.total());
        let reorder_fixtures = self.read(cx, |list, _| {
            [
                "sample-text-multiline",
                "sample-image-wide",
                "sample-image-strip",
            ]
            .into_iter()
            .filter_map(|id| list.model.find(id).map(|item| (**item).clone()))
            .collect::<Vec<_>>()
        });

        // AccessKit only retains a tree when a platform accessibility client is
        // connected (for example Narrator/UIA on Windows). Keep this probe
        // opt-in so the regular UI selftest remains deterministic in headless
        // development runs, while a connected client can export and verify the
        // same tree that it receives.
        self.pause(cx, 50).await;
        self.accessibility_tree(cx).await;
        #[cfg(target_os = "windows")]
        self.drag_regions(cx).await;

        self.pinned_scroll(cx).await;

        // 方向键：从第一个可见项往下走一行。
        let before = self.active_index(cx);
        self.key(cx, "down");
        let after = self.active_index(cx);
        self.check("down moves the active row", after == before + 1, || {
            format!("{before} -> {after}")
        });

        // 范围：Mod+Q 在全部与收藏之间切换。
        self.key(cx, "secondary-q");
        self.filtered(
            cx,
            "mod+q shows favorites",
            ListFilter {
                range: Range::Favorite,
                ..ListFilter::default()
            },
        )
        .await;
        self.key(cx, "secondary-q");
        self.filtered(cx, "mod+q back to all", ListFilter::default())
            .await;

        // 分类：←/→ 循环，Esc 先退分类。
        self.key(cx, "right");
        self.filtered(
            cx,
            "right selects text",
            ListFilter {
                category: Some(ItemKind::Text),
                ..ListFilter::default()
            },
        )
        .await;
        self.key(cx, "right");
        self.key(cx, "left");
        self.key(cx, "left");
        self.filtered(
            cx,
            "left wraps to files",
            ListFilter {
                category: Some(ItemKind::Files),
                ..ListFilter::default()
            },
        )
        .await;
        self.key(cx, "escape");
        self.filtered(cx, "escape clears the category", ListFilter::default())
            .await;

        // 自定义分组：Tab / Shift+Tab 在可见分组间循环（隐藏的跳过），Esc 先退分组。
        self.key(cx, "tab");
        let work = ListFilter {
            group_id: Some("demo-work".into()),
            ..ListFilter::default()
        };
        self.filtered(cx, "tab selects the first group", work.clone())
            .await;
        self.key(cx, "tab");
        self.filtered(
            cx,
            "tab skips the hidden group",
            ListFilter {
                group_id: Some("demo-code".into()),
                ..ListFilter::default()
            },
        )
        .await;
        self.key(cx, "shift-tab");
        self.filtered(cx, "shift-tab goes back", work).await;
        self.key(cx, "escape");
        self.filtered(cx, "escape clears the group", ListFilter::default())
            .await;

        self.search(cx, all).await;
        self.hints(cx).await;
        self.item_actions(cx).await;
        self.multi_select(cx).await;
        self.note(cx).await;
        self.snippet(cx).await;
        self.context_menu(cx).await;
        self.move_to_group(cx).await;
        self.pin(cx).await;
        self.groups(cx).await;
        self.preview(cx).await;
        self.split_words(cx).await;
        self.shortcuts(cx).await;
        self.drag_out(cx).await;
        self.reorder_drag(cx, reorder_fixtures).await;
        self.show_then_enter(cx).await;
        self.escape_layers(cx).await;
        self.header_actions(cx).await;
    }

    /// 置顶行属于同一虚拟列表：滚出视口后，当前项和数字提示都跟随模型下标。
    async fn pinned_scroll(&mut self, cx: &mut AsyncApp) {
        self.emit(cx, PanelEvent::Shown);
        self.pause(cx, 48).await;
        let pins: Vec<Arc<str>> = self.read(cx, |list, _| {
            (0..list.total())
                .filter_map(|index| list.model.get(index))
                .take_while(|item| item.is_pinned)
                .map(|item| item.id.clone())
                .collect()
        });
        let at_top = self.read(cx, |list, _| {
            list.state.item_count() == list.total()
                && list.controller.active_index(&list.model) == 0
                && list.controller.hint_key(0, false) == Some('1')
                && list.active_item().is_some_and(|item| item.is_pinned)
        });
        let target = pins.len() + 2;
        self.list.update(cx, |list, cx| {
            list.state.scroll_to(gpui::ListOffset {
                item_ix: target,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        let scrolled = self
            .settle(cx, |list, _| {
                let snapshot = list.snapshot.borrow();
                let Some(first) = snapshot.rows.first() else {
                    return false;
                };
                let index = first.ix;
                index >= pins.len()
                    && index == list.state.logical_scroll_top().item_ix
                    && list.controller.active_index(&list.model) == index
                    && list.controller.hint_index('1') == Some(index)
                    && list.controller.hint_key(index, false) == Some('1')
                    && list.controller.hint_key(index - 1, false).is_none()
                    && list
                        .active_item()
                        .zip(list.model.get(index))
                        .is_some_and(|(active, row)| active.id == row.id)
                    && pins.iter().all(|id| {
                        list.selftest_reorder_card_bounds(id).is_none_or(|bounds| {
                            bounds.bottom() <= snapshot.viewport.top()
                                || bounds.top() >= snapshot.viewport.bottom()
                        })
                    })
            })
            .await;
        self.check(
            "pinned rows scroll away and visible indices/hints follow the model",
            !pins.is_empty() && at_top && scrolled,
            || format!("pins {pins:?}, at_top {at_top}, scrolled {scrolled}"),
        );
        self.emit(cx, PanelEvent::Shown);
        self.pause(cx, 48).await;
    }

    /// Export the last AccessKit tree and verify the first-release list nodes.
    /// The tree is available only while a screen-reader/UIA client is attached;
    /// absence of a client is therefore reported as a skipped probe, not a
    /// product failure.
    async fn accessibility_tree(&mut self, cx: &mut AsyncApp) {
        let tree = self
            .window
            .update(cx, |_, window, _| window.debug_a11y_tree_json())
            .ok()
            .flatten();
        let Some(tree) = tree else {
            log::info!("panel ui selftest: a11y tree probe skipped (no AccessKit client)");
            return;
        };

        if let Some(path) = std::env::var_os("KWIKPASTE_A11Y_DUMP").map(PathBuf::from) {
            let write_result = std::fs::write(&path, &tree);
            let write_ok = write_result.is_ok();
            let write_error = write_result
                .as_ref()
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
            self.check("exports the accessibility tree", write_ok, || {
                format!("{}: {write_error}", path.display())
            });
            if write_ok {
                log::info!(
                    "panel ui selftest: a11y tree exported to {}",
                    path.display()
                );
            }
        } else {
            log::info!("panel ui selftest: a11y tree captured (set KWIKPASTE_A11Y_DUMP to export)");
        }

        let parsed = serde_json::from_str::<serde_json::Value>(&tree);
        let Ok(parsed) = parsed else {
            self.check("accessibility tree is valid JSON", false, || {
                "debug_a11y_tree_json returned invalid JSON".into()
            });
            return;
        };

        let list_named = tree_has_node(&parsed, "ListBox", |aria| {
            matches!(
                aria.get("label").and_then(serde_json::Value::as_str),
                Some("剪贴板历史" | "Clipboard history")
            )
        });
        self.check(
            "accessibility list has a localized name",
            list_named,
            || "missing ListBox label 剪贴板历史 / Clipboard history".into(),
        );

        let option_named = tree_has_node(&parsed, "ListBoxOption", |aria| {
            aria.get("label")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|label| !label.trim().is_empty())
        });
        self.check(
            "accessibility option has a readable name",
            option_named,
            || "missing non-empty ListBoxOption label".into(),
        );
    }

    /// 在窗口里的某个位置按下再松开右键。
    fn right_click(&self, cx: &mut AsyncApp, x: f32, y: f32) {
        let position = point(px(x), px(y));
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Right,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Right,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    }),
                    cx,
                );
            })
            .ok();
    }

    fn menu_open(&self, cx: &mut AsyncApp) -> bool {
        self.window
            .update(cx, |_, window, _| menu_open(window))
            .unwrap_or(false)
    }

    /// 等菜单打开或关上。
    async fn settle_menu(&self, cx: &mut AsyncApp, open: bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < SETTLE {
            if self.menu_open(cx) == open {
                return true;
            }
            self.pause(cx, 16).await;
        }
        false
    }

    /// 把指针停在可见卡片上（重复移动，越过显示后的指针门槛），返回悬停的记录。
    async fn hover_row(&self, cx: &mut AsyncApp) -> Option<Arc<ListItem>> {
        // WM_SHOWWINDOW 与首帧绘制是异步的；在 vsync 休眠/唤醒的窗口里，前两次注入可能
        // 发生在卡片还没有命中测试区域之前。重复同一对移动，但在第二次移动后马上
        // 检查，避免指针门槛已经越过后又用下一次移动重置悬停计时器。
        for _ in 0..100 {
            self.pointer_at(cx, 180., 356.);
            self.pause(cx, 16).await;
            self.pointer_at(cx, 180., 360.);
            self.pause(cx, 16).await;
            if let Some(item) = self.read(cx, |list, _| {
                list.hovered
                    .as_ref()
                    .and_then(|id| list.model.find(id))
                    .cloned()
            }) {
                return Some(item);
            }
        }
        None
    }

    /// 右键菜单：在卡片上按右键弹出（画在窗口里、拿焦点），↓ 选第一项、Enter 执行（粘贴这张卡片），
    /// 选完菜单关上；Esc 只关菜单、不隐藏面板；多选时不弹。
    async fn context_menu(&mut self, cx: &mut AsyncApp) {
        self.focus_list(cx);
        let Some(item) = self.hover_row(cx).await else {
            self.check("right click: a card under the pointer", false, || {
                "nothing hovered".into()
            });
            return;
        };

        let entries = self
            .list
            .update(cx, |list, cx| list.context_menu_entries(cx));
        let has_groups = entries
            .iter()
            .any(|entry| matches!(entry, MenuEntry::Submenu(_)));
        self.check(
            "the menu offers move to group when groups exist",
            has_groups,
            || "no submenu".into(),
        );

        self.right_click(cx, 180., 360.);
        let opened = self.settle_menu(cx, true).await;
        self.check("right click opens the card menu", opened, || {
            "no menu".into()
        });

        // 菜单在指针处展开，指针下的那一项被悬停选中；挪开指针再用键盘从第一项选起。
        self.pointer_at(cx, 20., 20.);
        self.pause(cx, 30).await;
        // 菜单不处理的键不能漏到列表：没有子菜单时 → 不换分类。
        self.key(cx, "right");
        let category = self.read(cx, |list, _| list.filter().category);
        self.check(
            "keys the menu ignores do not reach the list",
            category.is_none() && self.menu_open(cx),
            || format!("category {category:?}"),
        );
        self.intents.borrow_mut().clear();
        self.key(cx, "down");
        self.key(cx, "enter");
        let closed = self.settle_menu(cx, false).await;
        let pasted = self.intents.borrow().first().cloned();
        self.check(
            "enter runs the first item: paste the card under the pointer",
            closed
                && pasted
                    == Some(ListIntent::Paste {
                        id: item.id.clone(),
                        plain: false,
                    }),
            || format!("closed {closed}, intents {pasted:?}"),
        );

        let requests = cx.update(|cx| cx.global::<RequestLog>().commands.len());
        self.right_click(cx, 180., 360.);
        self.settle_menu(cx, true).await;
        self.key(cx, "escape");
        let closed = self.settle_menu(cx, false).await;
        let after = cx.update(|cx| cx.global::<RequestLog>().commands.len());
        self.check(
            "escape closes only the menu",
            closed && after == requests,
            || format!("closed {closed}, requests {requests} -> {after}"),
        );

        self.key(cx, "secondary-a");
        self.settle(cx, |list, _| list.selecting()).await;
        self.right_click(cx, 180., 360.);
        self.pause(cx, 80).await;
        let opened = self.menu_open(cx);
        self.check("no card menu while selecting", !opened, || {
            "menu opened".into()
        });
        self.key(cx, "escape");
        self.pointer_at(cx, 20., 20.);
    }

    /// 移动到分组、再点当前分组移出分组；在分组视图里移走的记录离开列表。
    async fn move_to_group(&mut self, cx: &mut AsyncApp) {
        let Some(id) = self.select_where(cx, |item| item.group_id.is_none() && !item.is_pinned)
        else {
            self.check("move to group: an ungrouped record", false, || {
                "none".into()
            });
            return;
        };
        let group: Arc<str> = "demo-work".into();
        let item = self.read(cx, |list, _| list.model.find(&id).cloned());
        let Some(item) = item else {
            return;
        };

        let list = self.list.clone();
        let (moved, target) = (item.clone(), group.clone());
        self.window
            .update(cx, |_, window, cx| {
                list.update(cx, |list, cx| {
                    list.move_to_group(moved, Some(target), window, cx)
                });
            })
            .ok();
        let wanted = id.clone();
        let target = group.clone();
        let moved = self
            .settle(cx, move |list, _| {
                list.model
                    .find(&wanted)
                    .is_some_and(|item| item.group_id.as_ref() == Some(&target))
            })
            .await;
        self.check("move to group sets the record's group", moved, || {
            "group not set".into()
        });

        let wanted = id.clone();
        let (removed, list) = (item.clone(), self.list.clone());
        self.window
            .update(cx, |_, window, cx| {
                list.update(cx, |list, cx| list.move_to_group(removed, None, window, cx));
            })
            .ok();
        let cleared = self
            .settle(cx, move |list, _| {
                list.model
                    .find(&wanted)
                    .is_some_and(|item| item.group_id.is_none())
            })
            .await;
        self.check(
            "picking the current group again removes it",
            cleared,
            || "still grouped".into(),
        );
    }

    /// 分组弹框：Mod+N 打开新增弹框并请求编辑态，编辑态开始后聚焦名称框，关掉弹框退出编辑态；
    /// 名称必填、去首尾空白、最多 32 个字；保存后分组栏多出新分组；管理框拖动排序、取消勾选后
    /// 保存，顺序与显隐写回数据源。
    async fn groups(&mut self, cx: &mut AsyncApp) {
        self.focus_list(cx);
        self.key(cx, "secondary-n");
        let opened = self.dialog_open(cx);
        let request = self.last_request(cx);
        self.check(
            "mod+n opens the new group dialog and asks for editing",
            opened
                && matches!(
                    request,
                    Some(PanelCommand::BeginEditing(EditTrigger::Keyboard))
                ),
            || format!("dialog {opened}, request {request:?}"),
        );
        self.emit(cx, PanelEvent::EditingStarted);
        self.pause(cx, 30).await;
        let focused = self
            .window
            .update(cx, |_, window, cx| {
                editing::dialog_input(cx).is_some_and(|input| input.is_focused(window, cx))
            })
            .unwrap_or(false);
        self.check("editing focuses the group name box", focused, || {
            "name box not focused".into()
        });
        self.window
            .update(cx, |_, window, cx| close_dialog(window, cx))
            .ok();
        self.pause(cx, 30).await;
        let (open, request) = (self.dialog_open(cx), self.last_request(cx));
        self.check(
            "closing the dialog ends editing",
            !open && matches!(request, Some(PanelCommand::EndEditing)),
            || format!("dialog {open}, request {request:?}"),
        );
        self.emit(cx, PanelEvent::EditingEnded);

        let source = self.source(cx);
        let editor = self
            .window
            .update(cx, |_, window, cx| {
                cx.new(|cx| GroupEditor::new(None, source.clone(), window, cx))
            })
            .ok();
        let Some(editor) = editor else {
            return;
        };
        let empty = editor.update(cx, |editor, cx| editor.submit(cx));
        self.check("an empty group name is refused", empty.is_none(), || {
            format!("{empty:?}")
        });
        let long = format!("  {}  ", "分组".repeat(20));
        self.window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.type_name(&long, window, cx);
                    editor.pick_icon("i-lets-icons:book", cx);
                });
            })
            .ok();
        let input = editor.update(cx, |editor, cx| editor.submit(cx));
        let ok = input.as_ref().is_some_and(|input| {
            input.name.chars().count() <= 32
                && !input.name.starts_with(' ')
                && input.icon == "i-lets-icons:book"
        });
        self.check(
            "group names are trimmed and cut to 32 characters",
            ok,
            || format!("{input:?}"),
        );

        let before = cx.update(|cx| self.panel.read(cx).group_list().len());
        let saved = match input {
            Some(input) => group_dialogs::save_group(source.as_ref(), None, input).await,
            None => Err(anyhow::anyhow!("no input")),
        };
        self.panel.update(cx, |panel, cx| panel.reload_groups(cx));
        let panel = self.panel.clone();
        let grew = self
            .settle(cx, move |_, cx| {
                panel.read(cx).group_list().len() == before + 1
            })
            .await;
        self.check(
            "saving adds the group to the group bar",
            saved.is_ok() && grew,
            || format!("{saved:?}"),
        );

        let groups = cx.update(|cx| self.panel.read(cx).group_list().to_vec());
        let manager = cx.new(|_| {
            let changed: group_dialogs::Changed = Rc::new(|_, _| {});
            GroupManager::new(groups.clone(), source.clone(), changed)
        });
        let first = groups.first().map(|group| group.id.clone());
        manager.update(cx, |manager, cx| {
            manager.move_group(0, 1, cx);
            if let Some(first) = first.clone() {
                manager.set_visible(first, false, cx);
            }
        });
        let (order, visible) = manager.read_with(cx, |manager, _| manager.layout());
        let layout = source
            .update_groups_layout(order.clone(), visible.clone())
            .await;
        let stored = source.groups().await.unwrap_or_default();
        let moved = stored.get(1).map(|group| group.id.clone()) == first
            && stored
                .iter()
                .find(|group| Some(&group.id) == first.as_ref())
                .is_some_and(|group| group.is_hidden);
        self.check(
            "managing groups saves their order and visibility",
            layout.is_ok() && moved,
            || format!("{layout:?}, order {order:?}, stored {stored:?}"),
        );
        self.panel.update(cx, |panel, cx| panel.reload_groups(cx));
    }

    /// 预览窗：指针停在卡片上 500 ms 后打开（窗口可见、不是前台窗口），Esc 先关预览而不隐藏面板；
    /// 键盘预览随 ↓ 换到新的当前项；滚轮关掉悬停预览。
    async fn preview(&mut self, cx: &mut AsyncApp) {
        self.focus_list(cx);
        // 预览用例不依赖真实鼠标：直接把合成指针悬停到夹具卡片，避免宿主光标位置
        // 或窗口首次显示时序让 on_hover 偶发收不到事件。
        let Some(item) = self.read(cx, |list, _| {
            (0..list.total())
                .filter_map(|index| list.model.get(index))
                .find(|item| !item.is_pinned)
                .cloned()
        }) else {
            self.check("preview: a card under the pointer", false, || {
                "nothing hovered".into()
            });
            return;
        };
        let wanted = item.id.clone();
        self.list
            .update(cx, |list, cx| list.selftest_hover(&wanted, cx));

        let started = Instant::now();
        // 前面的用例留下的异步刷新可能让指针下换成相邻的卡片：核对的是“预览的就是指针下的那张”。
        let opened = self
            .settle(cx, |list, _| {
                list.preview_visible()
                    && list.preview_session().is_some_and(|(id, trigger)| {
                        Some(&id) == list.hovered.as_ref() && trigger == PreviewTrigger::Hover
                    })
            })
            .await;
        let waited = started.elapsed();
        let (foreground, visible, session, hovered) = self.read(cx, |list, _| {
            (
                list.preview_foreground(),
                list.preview_visible(),
                list.preview_session(),
                list.hovered.clone(),
            )
        });
        self.check(
            "hovering a card opens its preview after the delay, without the foreground",
            opened && waited >= Duration::from_millis(450) && !foreground,
            || {
                format!(
                    "opened {opened} after {waited:?}, foreground {foreground}, visible {visible}, \
                     session {session:?}, hovered {hovered:?}, wanted {}",
                    item.id
                )
            },
        );

        let requests = cx.update(|cx| cx.global::<RequestLog>().commands.len());
        self.key(cx, "escape");
        let closed = self
            .settle(cx, |list, _| {
                list.preview_session().is_none() && !list.preview_visible()
            })
            .await;
        let after = cx.update(|cx| cx.global::<RequestLog>().commands.len());
        self.check(
            "escape closes the preview before anything else",
            closed && after == requests,
            || format!("closed {closed}, requests {requests} -> {after}"),
        );

        self.pointer_at(cx, 20., 20.);
        let active = self.read(cx, |list, _| list.active_item().map(|item| item.id.clone()));
        if let Some(active) = active {
            self.list.update(cx, |list, cx| {
                list.open_preview(active, PreviewTrigger::Keyboard, cx);
            });
        }
        self.key(cx, "down");
        let moved = self
            .settle(cx, |list, _| {
                let active = list.active_item().map(|item| item.id.clone());
                list.preview_visible()
                    && list
                        .preview_session()
                        .is_some_and(|(id, _)| Some(id) == active)
            })
            .await;
        self.check("a keyboard preview follows the active row", moved, || {
            "preview did not follow".into()
        });
        self.list.update(cx, |list, cx| list.close_preview(cx));

        // 打开“按住空格预览”：钩子转来的空格按下打开当前项的预览，松开关上。
        self.list.update(cx, |list, cx| {
            let mut settings = list.settings().clone();
            settings.clipboard.preview.space_enabled = true;
            list.apply_settings(settings, cx);
        });
        self.key(cx, "space");
        let held = self
            .settle(cx, |list, _| {
                list.preview_visible()
                    && list
                        .preview_session()
                        .is_some_and(|(_, trigger)| trigger == PreviewTrigger::Keyboard)
            })
            .await;
        self.window
            .update(cx, |_, window, cx| {
                if let Ok(keystroke) = Keystroke::parse("space") {
                    window.dispatch_event(PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
                }
            })
            .ok();
        let released = self
            .settle(cx, |list, _| list.preview_session().is_none())
            .await;
        self.check(
            "holding space previews the active row, releasing closes it",
            held && released,
            || format!("held {held}, released {released}"),
        );
        self.list.update(cx, |list, cx| {
            let mut settings = list.settings().clone();
            settings.clipboard.preview.space_enabled = false;
            list.apply_settings(settings, cx);
        });

        let Some(item) = self.read(cx, |list, _| {
            (0..list.total())
                .filter_map(|index| list.model.get(index))
                .find(|item| !item.is_pinned)
                .cloned()
        }) else {
            return;
        };
        let wanted = item.id.clone();
        self.list
            .update(cx, |list, cx| list.selftest_hover(&wanted, cx));
        self.settle(cx, move |list, _| {
            list.preview_session().is_some_and(|(id, _)| id == wanted)
        })
        .await;
        self.list
            .update(cx, |list, cx| list.on_wheel_lines(-1., cx));
        let closed = self
            .settle(cx, |list, _| list.preview_session().is_none())
            .await;
        self.check("scrolling closes a hover preview", closed, || {
            "still open".into()
        });
        self.pointer_at(cx, 20., 20.);
    }

    async fn split_words(&mut self, cx: &mut AsyncApp) {
        let Some(id) = self.select_where(cx, |item| item.kind == ItemKind::Text) else {
            self.check("split words: a text record", false, || {
                "no text record".into()
            });
            return;
        };
        self.focus_list(cx);
        self.intents.borrow_mut().clear();
        self.key(cx, "secondary-s");
        self.pause(cx, 80).await;
        let split = self.intents.borrow().iter().any(
            |intent| matches!(intent, ListIntent::SplitWords { id: intent_id } if intent_id == &id),
        );
        let intents = self.intents.borrow().clone();
        self.check("mod+s emits split words intent", split, || {
            format!("intents {intents:?}")
        });
    }

    async fn shortcuts(&mut self, cx: &mut AsyncApp) {
        self.focus_list(cx);
        self.key(cx, "secondary-k");
        let opened = self.dialog_open(cx);
        self.check("mod+k opens the shortcuts list", opened, || {
            "dialog is closed".into()
        });
        if opened {
            self.window
                .update(cx, |_, window, cx| close_dialog(window, cx))
                .ok();
            self.pause(cx, 40).await;
        }
    }

    /// 在卡片上按下左键再移动（`pressed_button` 为左键）。
    fn press_and_move(&self, cx: &mut AsyncApp, from: (f32, f32), to: (f32, f32)) {
        self.press_move_points(
            cx,
            point(px(from.0), px(from.1)),
            point(px(to.0), px(to.1)),
            true,
        );
    }

    fn press_move_points(
        &self,
        cx: &mut AsyncApp,
        from: gpui::Point<gpui::Pixels>,
        to: gpui::Point<gpui::Pixels>,
        release: bool,
    ) {
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position: from,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: to,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
                if release {
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button: MouseButton::Left,
                            position: to,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                        }),
                        cx,
                    );
                }
            })
            .ok();
    }

    fn mouse_press(&self, cx: &mut AsyncApp, position: gpui::Point<gpui::Pixels>) {
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
            })
            .ok();
    }

    fn mouse_move(&self, cx: &mut AsyncApp, position: gpui::Point<gpui::Pixels>) {
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
            })
            .ok();
    }

    fn mouse_move_without_button(&self, cx: &mut AsyncApp, position: gpui::Point<gpui::Pixels>) {
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: None,
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
            })
            .ok();
    }

    fn mouse_release(&self, cx: &mut AsyncApp, position: gpui::Point<gpui::Pixels>) {
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    }),
                    cx,
                );
            })
            .ok();
    }

    async fn card_center(&self, cx: &mut AsyncApp, id: &str) -> Option<gpui::Point<gpui::Pixels>> {
        for _ in 0..100 {
            let id = Arc::<str>::from(id);
            self.list
                .update(cx, |list, cx| list.selftest_hover(&id, cx));
            if let Some(center) = self.read(cx, |list, _| list.selftest_card_center(&id)) {
                return Some(center);
            }
            self.pause(cx, 16).await;
        }
        None
    }

    /// 面板内排序的 GPUI 事件回归：置顶、收藏、普通点击、Esc 取消以及重载持久化。
    async fn reorder_drag(&mut self, cx: &mut AsyncApp, fixtures: Vec<ListItem>) {
        self.emit(cx, PanelEvent::Shown);
        self.focus_list(cx);
        if let Some(store) = &self.store {
            if let Ok(mut store) = store.lock() {
                store.set_pinned("sample-text-pinned", false);
                store.patch("sample-text-pinned", |item| item.is_favorite = false);
                store.set_pinned("sample-url-pinned", true);
                for mut item in fixtures {
                    store.remove(&item.id);
                    if item.id.as_ref() == "sample-text-multiline" {
                        let mut favorite = item.clone();
                        favorite.id = "reorder-favorite-text".into();
                        favorite.is_pinned = false;
                        favorite.is_favorite = true;
                        store.insert_newest(favorite);
                    }
                    item.is_pinned = false;
                    item.is_favorite = item.id.as_ref() == "sample-image-strip";
                    store.insert_newest(item);
                }
                store.set_pinned("sample-text-multiline", true);
                store.set_pinned("sample-image-wide", true);
            }
            self.list.update(cx, |list, cx| list.reload(cx));
            self.settle(cx, |list, _| {
                list.model
                    .get(0)
                    .is_some_and(|item| item.id.as_ref() == "sample-image-wide")
                    && list
                        .model
                        .get(1)
                        .is_some_and(|item| item.id.as_ref() == "sample-text-multiline")
            })
            .await;
            self.pause(cx, 48).await;
        }
        self.filtered(cx, "reorder starts in all", ListFilter::default())
            .await;
        let pinned: Vec<Arc<str>> = self.read(cx, |list, _| {
            (0..list.total())
                .filter_map(|index| list.model.get(index))
                .filter(|item| item.is_pinned)
                .take(2)
                .map(|item| item.id.clone())
                .collect()
        });
        if let (Some(first), Some(second)) = (pinned.first().cloned(), pinned.get(1).cloned()) {
            let original_first = first.clone();
            let original_second = second.clone();
            let moved = self.card_center(cx, &first).await;
            let target = self.card_center(cx, &second).await;
            if let (Some(from), Some(to)) = (moved, target) {
                self.mouse_press(cx, from);
                let drop_point = point(to.x, to.y + px(12.));
                self.mouse_move(cx, drop_point);
                self.pause(cx, 32).await;
                let metrics = self.read(cx, |list, _| list.selftest_reorder_measurements());
                let source_bounds = self.read(cx, |list, _| {
                    list.selftest_reorder_card_bounds(&original_first)
                });
                let target_bounds = self.read(cx, |list, _| {
                    list.selftest_reorder_card_bounds(&original_second)
                });
                let geometry_ok =
                    metrics
                        .as_ref()
                        .is_some_and(|(active, source, ghost, indicator)| {
                            *active
                                && source.zip(*ghost).is_some_and(|(source, ghost)| {
                                    (ghost.origin.y - (drop_point.y - (from.y - source.origin.y)))
                                        .abs()
                                        <= px(2.)
                                })
                                && indicator.is_some()
                        });
                let gap_ok = self
                    .read(cx, |list, _| list.selftest_reorder_indicator_matches_gap())
                    .unwrap_or(false);
                self.check("reorder paints a real ghost and insertion indicator", geometry_ok && gap_ok, || {
                    format!("measurements {metrics:?}, source {source_bounds:?}, target {target_bounds:?}")
                });
                self.mouse_release(cx, drop_point);
                let expected_first = original_second.clone();
                let expected_second = original_first.clone();
                let swapped = self
                    .settle(cx, |list, _| {
                        list.model
                            .get(0)
                            .is_some_and(|item| item.id == expected_first)
                            && list
                                .model
                                .get(1)
                                .is_some_and(|item| item.id == expected_second)
                    })
                    .await;
                self.check("dragging a pinned card reorders in place", swapped, || {
                    "pinned order did not change".into()
                });
                // 夹具数据源带模拟延迟：先等它真正写入新顺序再重载，否则重载拿到旧顺序，
                // 还会在下一个用例中途落地。
                let store = self.store.clone();
                let (first, second) = (expected_first.clone(), expected_second.clone());
                self.settle(cx, move |_, _| {
                    store.as_ref().is_some_and(|store| {
                        store.lock().is_ok_and(|store| {
                            store
                                .index_of(&first)
                                .zip(store.index_of(&second))
                                .is_some_and(|(first, second)| first < second)
                        })
                    })
                })
                .await;
                self.list.update(cx, |list, cx| list.reload(cx));
                let expected_first = original_second.clone();
                let expected_second = original_first.clone();
                let persisted = self
                    .settle(cx, |list, _| {
                        list.model
                            .get(0)
                            .is_some_and(|item| item.id == expected_first)
                            && list
                                .model
                                .get(1)
                                .is_some_and(|item| item.id == expected_second)
                    })
                    .await;
                self.check("pinned reorder survives a reload", persisted, || {
                    "reload restored the old pinned order".into()
                });

                let from = self.card_center(cx, &original_second).await;
                let to = self.card_center(cx, &original_first).await;
                let from = from.unwrap_or(point(px(180.), px(220.)));
                let to = to
                    .map(|target_point| point(target_point.x, target_point.y + px(12.)))
                    .unwrap_or(point(px(180.), px(300.)));
                let before_cancel = self.read(cx, |list, _| {
                    (0..2)
                        .filter_map(|index| list.model.get(index))
                        .map(|item| item.id.clone())
                        .collect::<Vec<_>>()
                });
                self.focus_list(cx);
                self.mouse_press(cx, from);
                self.mouse_move(cx, to);
                self.pause(cx, 16).await;
                self.key(cx, "escape");
                self.pause(cx, 16).await;
                let esc_cancelled = self.read(cx, |list, _| {
                    !list.selftest_reorder_active()
                        && (0..2)
                            .filter_map(|index| list.model.get(index))
                            .map(|item| item.id.clone())
                            .collect::<Vec<_>>()
                            == before_cancel
                });
                self.mouse_release(cx, to);
                self.pause(cx, 16).await;
                let after_cancel = self.read(cx, |list, _| {
                    (0..2)
                        .filter_map(|index| list.model.get(index))
                        .map(|item| item.id.clone())
                        .collect::<Vec<_>>()
                });
                let cancelled = esc_cancelled && after_cancel == before_cancel;
                let cancelled_detail = self.read(cx, |list, _| {
                    (0..2)
                        .filter_map(|index| list.model.get(index))
                        .map(|item| item.id.to_string())
                        .collect::<Vec<_>>()
                });
                self.check(
                    "Esc cancels a reorder without changing order",
                    cancelled,
                    || format!("Esc changed the pinned order: before={before_cancel:?}, after={cancelled_detail:?}"),
                );
                let esc_cleared = self.read(cx, |list, _| !list.selftest_reorder_active());
                self.check("Esc leaves no active reorder state", esc_cleared, || {
                    "reorder remained active after Esc".into()
                });

                self.mouse_press(cx, from);
                self.mouse_move(cx, to);
                self.pause(cx, 16).await;
                self.mouse_release(cx, point(to.x, px(20.)));
                self.pause(cx, 32).await;
                let header_cancelled = self.read(cx, |list, _| !list.selftest_reorder_active());
                self.check(
                    "releasing over the panel header cancels reorder",
                    header_cancelled,
                    || "header release left reorder active".into(),
                );

                self.mouse_press(cx, from);
                self.mouse_move(cx, to);
                self.pause(cx, 16).await;
                self.mouse_move_without_button(cx, to);
                self.pause(cx, 16).await;
                let buttonless_cancelled = self.read(cx, |list, _| !list.selftest_reorder_active());
                self.check(
                    "a button-less move cancels an interrupted reorder",
                    buttonless_cancelled,
                    || "button-less move continued dragging".into(),
                );
            } else {
                self.check("dragging a pinned card reorders in place", false, || {
                    format!(
                        "pinned cards are outside the viewport: source {moved:?}, target {target:?}"
                    )
                });
            }
        } else {
            self.check("dragging a pinned card reorders in place", false, || {
                "fixture has fewer than two pinned cards".into()
            });
        }

        self.key(cx, "secondary-q");
        self.filtered(
            cx,
            "reorder favorite tab",
            ListFilter {
                range: Range::Favorite,
                ..ListFilter::default()
            },
        )
        .await;
        let favorites: Vec<Arc<str>> = self.read(cx, |list, _| {
            (0..list.total())
                .filter_map(|index| list.model.get(index))
                .filter(|item| item.is_favorite && !item.is_pinned)
                .take(2)
                .map(|item| item.id.clone())
                .collect()
        });
        if let (Some(first), Some(second)) = (favorites.first().cloned(), favorites.get(1).cloned())
        {
            let from = self.card_center(cx, &first).await;
            let to = self.card_center(cx, &second).await;
            if let (Some(from), Some(to)) = (from, to) {
                self.press_move_points(cx, from, point(to.x, to.y + px(40.)), true);
                let moved_id = first.clone();
                let changed = self
                    .settle(cx, |list, _| {
                        list.model
                            .find(&moved_id)
                            .and_then(|item| list.model.index_of(&item.id))
                            .is_some_and(|first_index| {
                                list.model
                                    .index_of(&second)
                                    .is_some_and(|second_index| first_index > second_index)
                            })
                    })
                    .await;
                self.check("dragging a favorite reorders in Favorite", changed, || {
                    "favorite order did not change".into()
                });
            }
        } else {
            self.check("dragging a favorite reorders in Favorite", false, || {
                "fixture has fewer than two non-pinned favorites".into()
            });
        }

        self.intents.borrow_mut().clear();
        if let Some(id) = self.select_where(cx, |item| !item.is_pinned)
            && let Some(center) = self.card_center(cx, &id).await
        {
            self.press_move_points(cx, center, center, true);
        }
        let drag_out = self
            .intents
            .borrow()
            .iter()
            .any(|intent| matches!(intent, ListIntent::DragOut { .. }));
        let intents = self.intents.borrow().clone();
        self.check(
            "a click without crossing the threshold is not drag-out",
            !drag_out,
            || format!("unexpected intents {intents:?}"),
        );
        self.key(cx, "secondary-q");
        self.filtered(cx, "reorder returns to all", ListFilter::default())
            .await;
    }

    /// 拖出：按住卡片拖过系统阈值交给宿主拖出那张卡片；只挪 1 px 不算拖；多选时按住已勾选的卡片
    /// 照样拖出（整组），勾选不变。
    async fn drag_out(&mut self, cx: &mut AsyncApp) {
        self.focus_list(cx);
        let Some(item) = self.hover_row(cx).await else {
            return;
        };
        let dragged = |intents: &[ListIntent]| {
            intents.iter().find_map(|intent| match intent {
                ListIntent::DragOut { id } => Some(id.clone()),
                _ => None,
            })
        };

        self.intents.borrow_mut().clear();
        self.press_and_move(cx, (180., 360.), (181., 360.));
        let nudged = dragged(&self.intents.borrow());
        self.press_and_move(cx, (180., 360.), (230., 380.));
        let pulled = dragged(&self.intents.borrow());
        self.check(
            "dragging a card past the threshold drags that card out, a nudge does not",
            nudged.is_none() && pulled.as_ref() == Some(&item.id),
            || format!("nudge {nudged:?}, drag {pulled:?}"),
        );

        self.key(cx, "secondary-a");
        self.settle(cx, |list, _| list.checked_count() > 0).await;
        let checked = self.read(cx, |list, _| list.checked_count());
        self.intents.borrow_mut().clear();
        self.press_and_move(cx, (180., 360.), (230., 380.));
        let selecting = dragged(&self.intents.borrow());
        let kept = self.read(cx, |list, _| list.checked_count());
        self.check(
            "dragging a checked card drags the selection and keeps it checked",
            selecting.as_ref() == Some(&item.id) && kept == checked,
            || format!("drag {selecting:?}, checked {checked} -> {kept}"),
        );
        self.key(cx, "escape");
        self.pointer_at(cx, 20., 20.);
    }

    /// Mod+P 固定窗口：点外部不再隐藏（平台开关关掉），再按一次恢复。
    async fn pin(&mut self, cx: &mut AsyncApp) {
        self.key(cx, "secondary-p");
        let request = self.last_request(cx);
        let pinned = cx.update(|cx| pin::pinned(cx));
        self.check(
            "mod+p pins the panel and keeps it on outside clicks",
            pinned && matches!(request, Some(PanelCommand::SetHideOnOutsideClick(false))),
            || format!("pinned {pinned}, request {request:?}"),
        );
        self.key(cx, "secondary-p");
        let request = self.last_request(cx);
        let pinned = cx.update(|cx| pin::pinned(cx));
        self.check(
            "mod+p again unpins it",
            !pinned && matches!(request, Some(PanelCommand::SetHideOnOutsideClick(true))),
            || format!("pinned {pinned}, request {request:?}"),
        );
    }

    /// 把指针挪到窗口里的某个位置（逻辑像素），像光标停在那里一样。
    fn pointer_at(&self, cx: &mut AsyncApp, x: f32, y: f32) {
        self.window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: point(px(x), px(y)),
                        pressed_button: None,
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
            })
            .ok();
    }

    /// 像 core 存入新记录那样：夹具里插到置顶行之后，列表收到 `ClipboardUpserted`。
    fn store_new_record(&self, cx: &mut AsyncApp, template: &ListItem, id: &str) {
        let Some(store) = &self.store else {
            return;
        };
        let item = ListItem {
            id: id.into(),
            summary: Some(format!("新记录 {id}").into()),
            note: None,
            is_pinned: false,
            is_favorite: false,
            group_id: None,
            ..template.clone()
        };
        if let Ok(mut store) = store.lock() {
            store.insert_newest(item);
        }
        self.list.update(cx, |list, cx| {
            list.on_update(
                ListUpdate::Upserted {
                    kind: ItemKind::Text,
                    deduplicated: false,
                    group_id: None,
                },
                cx,
            );
        });
    }

    /// 显示面板后立刻按 Enter（平台线的复现：粘的是上一条）。新记录在面板隐藏时或刚显示时到达；
    /// 一半的轮次面板出现在静止的光标下（首帧之后补一次指针移动，光标下是旧的第一行）；夹具查询慢
    /// 60 ms，Enter 一定赶在刷新落地之前。
    /// 置顶行存在时它仍是当前项；没有置顶行时才粘贴新记录。刷新必须先落地，不能粘贴旧的首行。
    async fn show_then_enter(&mut self, cx: &mut AsyncApp) {
        let template = self.read(cx, |list, _| {
            (0..list.total())
                .filter_map(|index| list.model.get(index))
                .find(|item| item.kind == ItemKind::Text)
                .map(|item| (**item).clone())
        });
        let Some(template) = template else {
            self.check("show then enter: a text record to copy", false, || {
                "no text record".into()
            });
            return;
        };

        let rounds = 8;
        let mut pasted_first = 0;
        let mut active_first = 0;
        let mut details = Vec::new();
        for round in 0..rounds {
            let id = format!("selftest-fresh-{round}");
            let after_show = round % 2 == 1;
            let over_card = round % 4 < 2;

            self.emit(cx, PanelEvent::Hidden);
            self.pause(cx, 30).await;
            self.pointer_at(cx, 20., 20.);
            if !after_show {
                self.store_new_record(cx, &template, &id);
            }
            self.intents.borrow_mut().clear();
            self.emit(cx, PanelEvent::Shown);
            if after_show {
                self.store_new_record(cx, &template, &id);
            }
            if over_card {
                // 面板出现在静止的光标下时系统会补发一次指针移动：等首帧画出（数据还是旧的），
                // 再在可见卡片的位置补一次移动，然后立刻按 Enter。
                self.pause(cx, 20).await;
                self.pointer_at(cx, 180., 360.);
            }
            let expected = self
                .store
                .as_ref()
                .and_then(|store| store.lock().ok())
                .and_then(|store| {
                    store
                        .page(&ListQuery {
                            offset: 0,
                            limit: 1,
                            filter: ListFilter::default(),
                            sort: Default::default(),
                            hide_favorites: false,
                        })
                        .items
                        .first()
                        .map(|item| item.id.clone())
                });
            self.key(cx, "enter");

            let intents = self.intents.clone();
            let started = Instant::now();
            while started.elapsed() < SETTLE && intents.borrow().is_empty() {
                self.pause(cx, 10).await;
            }
            let pasted = intents.borrow().first().cloned();
            if expected.is_some()
                && pasted
                    == expected
                        .clone()
                        .map(|id| ListIntent::Paste { id, plain: false })
            {
                pasted_first += 1;
            } else {
                details.push(format!("round {round}: pasted {pasted:?}"));
            }

            let wanted = id.clone();
            let caught_up = self
                .settle(cx, move |list, _| {
                    list.model.find(&wanted).is_some()
                        && list
                            .active_item()
                            .is_some_and(|item| Some(&item.id) == expected.as_ref())
                })
                .await;
            if caught_up {
                active_first += 1;
            } else {
                let active =
                    self.read(cx, |list, _| list.active_item().map(|item| item.id.clone()));
                details.push(format!("round {round}: active {active:?}"));
            }
        }

        self.check(
            "enter right after showing pastes the refreshed first row",
            pasted_first == rounds,
            || format!("{pasted_first}/{rounds}: {}", details.join("; ")),
        );
        self.check(
            "after showing the active row is the refreshed first row",
            active_first == rounds,
            || format!("{active_first}/{rounds}: {}", details.join("; ")),
        );
        self.pointer_at(cx, 20., 20.);
    }

    /// 点快捷信息：默认“双击粘贴”时粘贴这个片段（宿主是夹具替身，只核对意图）。
    async fn snippet(&mut self, cx: &mut AsyncApp) {
        let target = self.read(cx, |list, _| {
            (0..list.total())
                .filter_map(|index| list.model.get(index))
                .find_map(|item| {
                    item.quick_snippets
                        .first()
                        .map(|snippet| (item.clone(), Arc::<str>::from(snippet.text.as_str())))
                })
        });
        let Some((item, text)) = target else {
            self.check("a record with quick snippets", false, || "none".into());
            return;
        };

        self.intents.borrow_mut().clear();
        let list = self.list.clone();
        let (pick_item, pick_text) = (item.clone(), text.clone());
        self.window
            .update(cx, |_, window, cx| {
                list.update(cx, |list, cx| {
                    list.pick_snippet(pick_item, pick_text, window, cx)
                });
            })
            .ok();
        let pasted = self.intents.borrow().first().cloned();
        self.check(
            "clicking a quick snippet pastes it",
            pasted
                == Some(ListIntent::PasteSnippet {
                    id: item.id.clone(),
                    text: text.clone(),
                }),
            || format!("{pasted:?}"),
        );
    }

    async fn search(&mut self, cx: &mut AsyncApp, all: usize) {
        // Mod+F 请求编辑态（键盘触发）；编辑态开始后输入框拿到焦点。
        self.key(cx, "secondary-f");
        let request = self.last_request(cx);
        self.check(
            "mod+f requests keyboard editing",
            matches!(
                request,
                Some(PanelCommand::BeginEditing(EditTrigger::Keyboard))
            ),
            || format!("{request:?}"),
        );
        self.emit(cx, PanelEvent::EditingStarted);
        self.pause(cx, 30).await;
        let focused = self.search_focused(cx);
        self.check("editing focuses the search box", focused, || {
            "search box not focused".into()
        });

        // 输入经 200 ms 防抖、去首尾空白后成为搜索词。
        let header = cx.update(|cx| self.panel.read(cx).header().clone());
        self.window
            .update(cx, |_, window, cx| {
                header.update(cx, |header, cx| {
                    header.type_text("  kwikpaste ", window, cx)
                });
            })
            .ok();
        let keyword = ListFilter {
            keyword: "kwikpaste".into(),
            ..ListFilter::default()
        };
        self.filtered(cx, "typing searches after the debounce", keyword)
            .await;

        // 输入框聚焦时 ↓ 交给列表，焦点不动。
        let before = self.active_index(cx);
        self.key(cx, "down");
        let after = self.active_index(cx);
        let still = self.search_focused(cx);
        self.check(
            "down in the search box moves the list",
            after != before && still,
            || format!("{before} -> {after}, focused {still}"),
        );

        // 搜索框里的 Esc 和列表里一样逐层退出，没有可退的层就直接隐藏窗口。
        self.key(cx, "escape");
        let request = self.last_request(cx);
        self.check(
            "escape in the search box hides the panel",
            matches!(request, Some(PanelCommand::Hide(_))),
            || format!("{request:?}"),
        );
        self.emit(cx, PanelEvent::EditingEnded);
        self.pause(cx, 30).await;
        let focused = self.list_focused(cx);
        self.check("ending editing focuses the list", focused, || {
            "list not focused".into()
        });

        // 没有结果时显示搜索空态。
        self.window
            .update(cx, |_, window, cx| {
                header.update(cx, |header, cx| {
                    header.type_text("zz-no-such-record", window, cx)
                });
            })
            .ok();
        let ok = self
            .settle(cx, |list, _| {
                &*list.filter().keyword == "zz-no-such-record" && list.model.loaded_initial()
            })
            .await;
        let (total, key) = self.read(cx, |list, _| (list.total(), empty_text(list.filter()).key));
        self.check(
            "an unmatched search shows the search empty state",
            ok && total == 0 && key == "clipboard:empty.searchHistory",
            || format!("total {total}, key {key}"),
        );

        self.window
            .update(cx, |_, window, cx| {
                header.update(cx, |header, cx| header.type_text("", window, cx));
            })
            .ok();
        let back = self
            .settle(cx, |list, _| {
                list.filter().keyword.is_empty()
                    && list.model.loaded_initial()
                    && list.total() == all
            })
            .await;
        self.check("clearing the search restores the list", back, || {
            "list did not come back".into()
        });
    }

    async fn hints(&mut self, cx: &mut AsyncApp) {
        self.modifier(cx, true);
        let shown = self.read(cx, |list, _| list.key_hints);
        self.check("holding the modifier shows the number hints", shown, || {
            "hints off".into()
        });

        // Mod+2 粘贴第二个可见项（宿主接上之前是意图）。
        let target = self.read(cx, |list, _| {
            list.controller
                .hint_index('2')
                .and_then(|index| list.model.get(index))
                .map(|item| item.id.clone())
        });
        self.intents.borrow_mut().clear();
        self.key(cx, "secondary-2");
        let pasted = self.intents.borrow().first().cloned();
        self.check(
            "mod+2 pastes the second visible row",
            target.is_some()
                && pasted
                    == target
                        .clone()
                        .map(|id| ListIntent::Paste { id, plain: false }),
            || format!("want {target:?}, got {pasted:?}"),
        );

        self.modifier(cx, false);
        let hidden = self.read(cx, |list, _| !list.key_hints);
        self.check("releasing the modifier hides the hints", hidden, || {
            "hints still on".into()
        });
    }

    async fn item_actions(&mut self, cx: &mut AsyncApp) {
        // 收藏：Mod+D 翻转当前项。
        let id = self.select_where(cx, |item| !item.is_pinned && !item.is_favorite);
        self.key(cx, "secondary-d");
        let favorite = match &id {
            Some(id) => {
                let id = id.clone();
                self.settle(cx, move |list, _| {
                    list.model.find(&id).is_some_and(|item| item.is_favorite)
                })
                .await
            }
            None => false,
        };
        self.check("mod+d favorites the active row", favorite, || {
            format!("{id:?}")
        });
        self.key(cx, "secondary-d");
        if let Some(id) = id.clone() {
            let back = self
                .settle(cx, move |list, _| {
                    list.model.find(&id).is_some_and(|item| !item.is_favorite)
                })
                .await;
            self.check("mod+d again unfavorites it", back, || {
                "still favorite".into()
            });
        }

        // 置顶：Mod+T 后它进入列表首行，再按一次回去。
        self.key(cx, "secondary-t");
        let pinned = self
            .settle(cx, |list, _| {
                list.model
                    .get(0)
                    .is_some_and(|item| Some(&item.id) == id.as_ref() && item.is_pinned)
                    && list.state.item_count() == list.total()
            })
            .await;
        self.check("mod+t pins the active row", pinned, || {
            format!("{id:?} did not move to the first list row")
        });
        if let Some(id) = id.clone() {
            self.list.update(cx, |list, cx| {
                list.controller.select(&id);
                cx.notify();
            });
        }
        self.key(cx, "secondary-t");
        let unpinned = self
            .settle(cx, |list, _| {
                id.as_ref()
                    .and_then(|id| list.model.find(id))
                    .is_some_and(|item| !item.is_pinned)
                    && list.state.item_count() == list.total()
            })
            .await;
        self.check("mod+t again unpins it", unpinned, || {
            "row is still pinned or row count changed".into()
        });

        // 删除保护：收藏、置顶的记录按默认设置删不掉，不弹确认框。
        let total = self.read(cx, |list, _| list.total());
        let protected = self.select_where(cx, |item| item.is_pinned);
        self.key(cx, "secondary-backspace");
        let dialog = self.dialog_open(cx);
        self.check(
            "pinned rows are protected from deletion",
            protected.is_some() && !dialog,
            || format!("{protected:?}, dialog {dialog}"),
        );

        // 删除：Mod+Backspace 弹确认框，Enter 确认。
        let victim = self.select_where(cx, |item| !item.is_pinned && !item.is_favorite);
        self.key(cx, "secondary-backspace");
        self.pause(cx, 50).await;
        let dialog = self.dialog_open(cx);
        self.check("mod+backspace asks before deleting", dialog, || {
            "no confirm dialog".into()
        });
        self.key(cx, "enter");
        let deleted = match victim.clone() {
            Some(victim) => {
                self.settle(cx, move |list, _| {
                    list.total() + 1 == total && list.model.find(&victim).is_none()
                })
                .await
            }
            None => false,
        };
        self.check("enter in the dialog deletes the row", deleted, || {
            format!("{victim:?}")
        });
        let closed = !self.dialog_open(cx);
        self.check("the confirm dialog closes", closed, || "still open".into());
        // 确认框关掉后焦点交还给列表（钩子转来的按键要按列表的绑定匹配）。
        let list = self.list.clone();
        self.window
            .update(cx, |_, window, cx| {
                let focus = list.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            })
            .ok();
    }

    async fn multi_select(&mut self, cx: &mut AsyncApp) {
        let source = self.source(cx);
        let refs = source
            .item_refs(ListQuery {
                offset: 0,
                limit: 0,
                filter: ListFilter::default(),
                sort: Default::default(),
                hide_favorites: false,
            })
            .await
            .unwrap_or_default();
        let every = refs.len();

        // Mod+A：进入多选并全选（收藏、置顶也勾上）；再按一次取消全选。
        self.key(cx, "secondary-a");
        let all = self
            .settle(cx, |list, _| {
                list.selecting() && list.checked_count() == every
            })
            .await;
        let count = self.read(cx, |list, _| list.checked_count());
        self.check("mod+a selects every row", all, || {
            format!("want {every}, got {count}")
        });
        self.key(cx, "secondary-a");
        let none = self.read(cx, |list, _| list.selecting() && list.checked_count() == 0);
        self.check("mod+a again clears the selection", none, || {
            "selection not cleared".into()
        });

        // Enter 勾选当前项。
        self.select_where(cx, |item| !item.is_pinned && !item.is_favorite);
        self.key(cx, "enter");
        self.key(cx, "down");
        self.key(cx, "enter");
        let two = self.read(cx, |list, _| list.checked_count());
        self.check("enter toggles rows while selecting", two == 2, || {
            format!("{two} checked")
        });
        let total = self.read(cx, |list, _| list.total());

        // 批量删除：取消时保留勾选，确认后删除并退出多选。
        self.key(cx, "secondary-delete");
        self.pause(cx, 50).await;
        let dialog = self.dialog_open(cx);
        self.check("batch delete asks first", dialog, || "no dialog".into());
        self.key(cx, "escape");
        self.pause(cx, 50).await;
        let kept = self.read(cx, |list, _| list.selecting() && list.checked_count() == 2);
        let closed = !self.dialog_open(cx);
        self.check("cancelling keeps the selection", kept && closed, || {
            format!("kept {kept}, closed {closed}")
        });
        self.focus_list(cx);
        self.key(cx, "secondary-delete");
        self.pause(cx, 50).await;
        self.key(cx, "enter");
        let deleted = self
            .settle(cx, |list, _| {
                !list.selecting() && list.model.loaded_initial() && list.total() + 2 == total
            })
            .await;
        self.check("confirming deletes the checked rows", deleted, || {
            "rows not deleted".into()
        });
        self.focus_list(cx);
    }

    fn focus_list(&self, cx: &mut AsyncApp) {
        let list = self.list.clone();
        self.window
            .update(cx, |_, window, cx| {
                let focus = list.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            })
            .ok();
    }

    async fn note(&mut self, cx: &mut AsyncApp) {
        let id = self.select_where(cx, |item| item.note.is_none() && !item.is_pinned);
        self.key(cx, "secondary-m");
        self.pause(cx, 50).await;
        let request = self.last_request(cx);
        let dialog = self.dialog_open(cx);
        self.check(
            "mod+m opens the note dialog and asks for editing",
            dialog
                && matches!(
                    request,
                    Some(PanelCommand::BeginEditing(EditTrigger::Keyboard))
                ),
            || format!("dialog {dialog}, {request:?}"),
        );
        self.emit(cx, PanelEvent::EditingStarted);
        self.pause(cx, 30).await;

        let list = self.list.clone();
        let focused = self
            .window
            .update(cx, |_, window, cx| {
                let Some(input) = list.read(cx).note_input() else {
                    return false;
                };
                input.set_value("  自测备注 ", window, cx);
                input.focus_handle(cx).is_focused(window)
            })
            .unwrap_or(false);
        self.check("editing focuses the note box", focused, || {
            "note box not focused".into()
        });

        // 保存（等同点“保存”按钮）：关掉对话框、写入、退出编辑态。
        self.window
            .update(cx, |_, window, cx| {
                close_dialog(window, cx);
                list.update(cx, |list, cx| list.finish_note(true, window, cx));
            })
            .ok();
        let saved = match id.clone() {
            Some(id) => {
                self.settle(cx, move |list, _| {
                    list.model
                        .find(&id)
                        .is_some_and(|item| item.note.as_deref() == Some("自测备注"))
                })
                .await
            }
            None => false,
        };
        let request = self.last_request(cx);
        self.check(
            "saving trims the note and ends editing",
            saved && matches!(request, Some(PanelCommand::EndEditing)),
            || format!("saved {saved}, {request:?}"),
        );
        self.emit(cx, PanelEvent::EditingEnded);
        self.pause(cx, 30).await;
    }

    async fn escape_layers(&mut self, cx: &mut AsyncApp) {
        self.focus_list(cx);
        self.key(cx, "secondary-a");
        self.settle(cx, |list, _| list.selecting()).await;
        self.key(cx, "right");
        self.key(cx, "escape");
        let (selecting, category) =
            self.read(cx, |list, _| (list.selecting(), list.filter().category));
        self.check(
            "escape leaves multi-select before clearing the category",
            !selecting && category.is_some(),
            || format!("selecting {selecting}, category {category:?}"),
        );
        self.key(cx, "escape");
        self.key(cx, "escape");
        let request = self.last_request(cx);
        self.check(
            "the last escape hides the panel",
            matches!(request, Some(PanelCommand::Hide(_))),
            || format!("{request:?}"),
        );

        // 隐藏再显示：退出多选，按“打开窗口时选中”的默认设置回到全部。
        self.key(cx, "secondary-q");
        self.emit(cx, PanelEvent::Hidden);
        self.emit(cx, PanelEvent::Shown);
        self.filtered(cx, "showing resets the range to all", ListFilter::default())
            .await;
    }
}

fn tree_has_node<F>(value: &serde_json::Value, role: &str, predicate: F) -> bool
where
    F: Fn(&serde_json::Map<String, serde_json::Value>) -> bool + Copy,
{
    let serde_json::Value::Object(map) = value else {
        return false;
    };

    if let Some(serde_json::Value::Object(aria)) = map.get("aria") {
        let role_matches = aria
            .get("role")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == role);
        if role_matches && predicate(aria) {
            return true;
        }
    }

    map.values()
        .any(|child| tree_has_node(child, role, predicate))
}

/// 截图用的演示状态（`KP_PANEL_DEMO`）：`hover`、`hints`、`selection`、`search-empty`、
/// `search-focus`、`search-typed`、`group`、`note`、`delete`、`shortcuts` 等。数据加载完后摆好，
/// 供 PrintWindow 截图。
pub fn stage_demo(panel: Entity<ClipboardPanel>, cx: &mut App) {
    let Ok(stage) = std::env::var("KP_PANEL_DEMO") else {
        return;
    };
    cx.set_global(RequestLog::default());
    let list = panel.read(cx).list().clone();
    let window = list.read(cx).window_handle();

    cx.spawn(async move |cx| {
        let driver = Driver {
            panel,
            list,
            window,
            intents: Rc::default(),
            panel_intents: Rc::default(),
            store: None,
            passed: 0,
            failed: Vec::new(),
        };
        driver
            .settle(cx, |list, _| {
                list.model.loaded_initial() && list.total() > 0
            })
            .await;
        if !matches!(
            stage.as_str(),
            "preview-words" | "preview-text" | "preview-image" | "preview-files" | "preview-html"
        ) {
            // 普通演示只展示卡片悬停态，不让延迟预览窗随捕获时序弹出。
            driver.list.update(cx, |list, cx| {
                let mut settings = list.settings().clone();
                settings.clipboard.preview.hover_enabled = false;
                list.apply_settings(settings, cx);
            });
        }
        if matches!(
            stage.as_str(),
            "idle"
                | "hints"
                | "search-focus"
                | "preview-words"
                | "preview-text"
                | "preview-image"
                | "preview-files"
                | "preview-html"
        ) {
            // 截图状态固定在样例中的颜色条目，避免捕获时用户光标位置影响悬停底色。
            driver.list.update(cx, |list, cx| {
                // 颜色行属于固定示例夹具；直接绑定稳定 id，避免首屏先显示时它尚未解码。
                list.hovered = Some("sample-color".into());
                cx.notify();
            });
        }
        driver.stage(&stage, cx).await;
        log::info!("demo stage {stage} ready");
    })
    .detach();
}

impl Driver {
    async fn stage(&self, stage: &str, cx: &mut AsyncApp) {
        match stage {
            "hover" => {
                let id =
                    self.select_where(cx, |item| item.kind == ItemKind::Text && !item.is_pinned);
                self.list.update(cx, |list, cx| {
                    list.hovered = id;
                    cx.notify();
                });
            }
            "hints" => self.modifier(cx, true),
            "selection" => {
                self.key(cx, "secondary-a");
                self.settle(cx, |list, _| list.checked_count() > 0).await;
                self.key(cx, "secondary-a");
                self.select_where(cx, |item| !item.is_pinned && !item.is_favorite);
                self.key(cx, "enter");
                self.key(cx, "down");
                self.key(cx, "enter");
            }
            "search-empty" => {
                let header = cx.update(|cx| self.panel.read(cx).header().clone());
                self.window
                    .update(cx, |_, window, cx| {
                        header.update(cx, |header, cx| header.type_text("没有这条", window, cx));
                    })
                    .ok();
            }
            "search-focus" | "search-typed" => {
                let header = cx.update(|cx| self.panel.read(cx).header().clone());
                self.window
                    .update(cx, |_, window, cx| {
                        header.update(cx, |header, cx| {
                            header.set_editing(true, cx);
                            header.focus_input(window, cx);
                            if stage == "search-typed" {
                                header.type_text("hello", window, cx);
                            }
                        });
                    })
                    .ok();
            }
            "group" => {
                self.key(cx, "tab");
                self.key(cx, "right");
            }
            "group-empty" => {
                self.key(cx, "tab");
                self.key(cx, "secondary-q");
                self.key(cx, "left");
            }
            "note" => {
                self.select_where(cx, |item| item.kind == ItemKind::Text && !item.is_pinned);
                self.key(cx, "secondary-m");
                self.pause(cx, 50).await;
                self.emit(cx, PanelEvent::EditingStarted);
            }
            "delete" => {
                self.select_where(cx, |item| !item.is_pinned && !item.is_favorite);
                self.key(cx, "secondary-backspace");
            }
            "shortcuts" => self.key(cx, "secondary-k"),
            "preview-words" | "preview-text" | "preview-image" | "preview-files"
            | "preview-html" => {
                let source = self.list.read_with(cx, |list, _| list.source.clone());
                if stage == "preview-text" {
                    source
                        .set_preview_text_view(PreviewTextView::Plain)
                        .await
                        .ok();
                    let settings = source.settings();
                    self.list
                        .update(cx, |list, cx| list.apply_settings(settings, cx));
                }
                let wanted: fn(&ListItem) -> bool = match stage {
                    "preview-image" => |item: &ListItem| item.kind == ItemKind::Image,
                    "preview-files" => {
                        |item: &ListItem| item.kind == ItemKind::Files && item.file_rows().len() > 1
                    }
                    "preview-html" => |item: &ListItem| {
                        item.sub_kind == Some(crate::clipboard::model::item::SubKind::Html)
                    },
                    _ => |item: &ListItem| {
                        item.kind == ItemKind::Text
                            && item.sub_kind.is_none()
                            && !item.is_sensitive
                            && item.summary.as_deref().is_some_and(|text| text.len() > 40)
                    },
                };
                let Some(id) = self.select_where(cx, |item| !item.is_pinned && wanted(item)) else {
                    log::warn!("no record for demo stage {stage}");
                    return;
                };
                // 预览阶段同时固定列表悬停项，避免上一次初始化时留下的颜色条目影响首帧。
                self.list.update(cx, |list, cx| {
                    list.hovered = Some(id.clone());
                    cx.notify();
                });
                let saved_scroll = self
                    .list
                    .read_with(cx, |list, _| list.state.logical_scroll_top());
                self.list.update(cx, |list, cx| list.reveal_item(&id, cx));
                // 面板刚显示后 vsync 线程最多停 1 秒（见报告），等它醒来出帧，卡片位置才记得下。
                self.pause(cx, 1200).await;
                self.list.update(cx, |list, cx| {
                    list.open_preview(id, PreviewTrigger::Keyboard, cx);
                });
                if stage == "preview-files" {
                    // 文件图标由后台解码后才进入 GPUI 图集；自测等待一帧完整结果，避免首张截图落在 Loading。
                    self.pause(cx, 4000).await;
                    self.list.update(cx, |list, cx| {
                        list.state.scroll_to(saved_scroll);
                        list.stop_reveal(cx);
                        cx.notify();
                    });
                }
            }
            "group-menu" => {
                // 分组栏第一个自定义分组上按右键（1.x 的编辑、隐藏、删除菜单）。
                self.pointer_at(cx, 190., 56.);
                self.right_click(cx, 190., 56.);
                self.settle_menu(cx, true).await;
                self.list.update(cx, |list, cx| {
                    list.close_preview(cx);
                });
            }
            "group-new" => {
                self.key(cx, "secondary-n");
                self.emit(cx, PanelEvent::EditingStarted);
            }
            "group-edit" | "group-manage" => {
                let panel = self.panel.clone();
                let first = cx.update(|cx| panel.read(cx).group_list().first().cloned());
                self.window
                    .update(cx, |_, window, cx| {
                        panel.update(cx, |panel, cx| {
                            if stage == "group-edit" {
                                panel.edit_group(first, window, cx);
                            } else {
                                panel.manage_groups(window, cx);
                            }
                        });
                    })
                    .ok();
                if stage == "group-edit" {
                    self.emit(cx, PanelEvent::EditingStarted);
                }
            }
            "menu" | "menu-group" => {
                let Some(item) = self.hover_row(cx).await else {
                    return;
                };
                self.right_click(cx, 180., 360.);
                self.settle_menu(cx, true).await;
                self.pointer_at(cx, 20., 20.);
                self.list.update(cx, |list, cx| {
                    list.close_preview(cx);
                });
                if stage == "menu-group" {
                    self.pointer_at(cx, 20., 20.);
                    self.pause(cx, 30).await;
                    // ↓ 走到“移动到分组”（分隔线不占位置），→ 展开子菜单。
                    let before = menu_groups(&item, true, true)
                        .iter()
                        .flatten()
                        .take_while(|action| **action != MenuAction::MoveToGroup)
                        .count();
                    for _ in 0..=before {
                        self.key(cx, "down");
                    }
                    self.key(cx, "right");
                }
            }
            other => log::warn!("unknown demo stage {other}"),
        }
    }
}
