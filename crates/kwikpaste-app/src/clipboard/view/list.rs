//! 主窗口的剪贴板列表（附录 D §3.2 的 ListView）：`list(ListState)`、稀疏分页、置顶优先排序、页脚，
//! 以及卡片上的悬停快捷动作、数字角标、多选（见 [`ops`]、[`selecting`]、[`parts`]）。
//!
//! 落实 r2-02 的硬约束：
//! - L1 面板已设 `inactive_frame_interval: None`（平台层），这里不放常驻动画；
//! - L2 平均行高 hint 在首帧布局后、第一页测量后、结构变化后重新施加；宽度变化在布局的当帧由
//!   `ListFrame` 补上，滚动条一帧也不塌；平均行高每行只采一次样；
//! - L3 不用 scroll handler：每帧读 `logical_scroll_top()` 判断到顶，加载范围按上一帧的布局快照算；
//! - L4 在顶部刷新时整体重建后补 `scroll_to(0)`；
//! - L5 文本 `line_clamp(n).text_ellipsis()`（见 `card.rs`）；
//! - L6 图片行按载荷宽高预测显式尺寸；
//! - L7 剪贴板图片一律经 [`KpImageCache`]。

mod menu;
mod ops;
mod parts;
pub(crate) use parts::quick_action_glyph;
mod previewing;
pub use previewing::PreviewTrigger;
mod selecting;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Local};
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, ClickEvent, Context, DispatchPhase,
    Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    KeyUpEvent, ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Point, Render, Role, ScrollDelta, ScrollWheelEvent,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, canvas, div, list,
    point, prelude::FluentBuilder as _, px,
};
use kwikpaste_core::{
    CoreEvent,
    ops::{ReorderAnchor, ReorderSection},
    settings::{AutoPaste, MiddleClickAction, Settings},
};
use kwikpaste_ui::{
    ListScrollbar, TextAreaInput, close_dialog, context_menu, dismiss_menu,
    theme::{self, px_rems},
};

use super::{
    bench::Bench,
    card::{self, CardEnv, CardState, LinkHandler, SnippetHandler, Visual},
    editing::{self, EditTarget},
    frame::{FrameTimer, FrameTiming, ListFrame, PaintedCallback, Snapshot, WidthHint},
    host::ItemHost,
    image_cache::{ImageKey, ImageState, KpImageCache, ResizeMode, path_of},
};

use crate::{
    clipboard::{
        model::{
            actions::{DeletePolicy, QuickAction, visible_actions},
            controller::{ListController, ListUpdate, Nav, NavOutcome, UpdateAction},
            freshness::{Activation, Freshness},
            item::{FilesPreview, ItemKind, ListItem},
            layout::LayoutSpec,
            list_model::{Applied, FetchRequest, ListModel},
            selection::Selection,
        },
        source::{ClipboardSource, Group, ListQuery, core_source::item_kind},
    },
    platform::{
        CoreEvents, Panel, PanelEvent, drag_out::DragTracker, window_drag::WindowDragArea as _,
    },
};

/// 列表上下各多排版的距离（px）。
const OVERDRAW: f32 = 200.;
/// 平滑滚动与平滑 reveal 的指数缓动时间常数（r2-02 §3.5：τ = 30 ms）。
const TAU_S: f32 = 0.030;
/// 滚轮一行的距离（设计 px）：与 Chromium 在 Windows 上相同（100/3 px，默认每格 3 行 = 100 px）。
const WHEEL_LINE: f32 = 100. / 3.;
/// 平均行高的采样数到这个值，就用实测均值重新施加一次 hint（附录 D L2）。
const MIN_HEIGHT_SAMPLES: u64 = 8;
const MAX_HEIGHT_SAMPLES: u64 = 5000;
/// 缩略图路径缓存的上限（1.x `useImageThumbnail` 的 512）。
const THUMBNAIL_PATHS_MAX: usize = 512;
/// 列表视图里最多保留的已缩放应用/文件图标数。
const ICON_CACHE_CAPACITY: usize = 128;

/// 列表发出的事件：粘贴类是已交给宿主的通知；拆词面板与快捷键列表还没做，先只发事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListIntent {
    /// 已交给宿主粘贴（通知，自测和探针据此核对粘贴的是哪一条）。
    Paste {
        id: Arc<str>,
        plain: bool,
    },
    /// 已交给宿主粘贴一条记录里的快捷信息。
    PasteSnippet {
        id: Arc<str>,
        text: Arc<str>,
    },
    SplitWords {
        id: Arc<str>,
    },
    ShowShortcuts,
    /// 已交给宿主拖出（通知，自测据此核对拖的是哪一条）。
    DragOut {
        id: Arc<str>,
    },
}

/// 指针相对面板显示时的状态。面板出现在静止的光标下时，光标下的卡片不算被悬停选中：
/// 不然面板一显示，当前项就被光标下那张卡片（往往是刷新前的旧第一行）抢走。
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pointer {
    /// 显示之后还没收到指针移动。
    Waiting,
    /// 显示之后第一次收到的指针位置（系统在窗口出现时会补发一次移动）。
    Resting(Point<Pixels>),
    /// 指针动过了：悬停照常选中。
    Moved,
}

/// 指针离开显示时的位置超过这个距离才算动过。
const POINTER_SLOP: f32 = 2.;

/// 正在编辑的完整文本；原文用于判定未改动的保存。
struct ContentEdit {
    id: Arc<str>,
    input: TextAreaInput,
    original: String,
}

/// 正在编辑的备注。
struct NoteEdit {
    id: Arc<str>,
    input: TextAreaInput,
}

/// 没有缩略图的图片记录：向数据源要缩略图的进度。
#[derive(Clone, Debug)]
enum Thumbnail {
    Pending,
    Ready(Arc<str>),
    Failed,
}

/// 平均行高估计（只用已加载行的实测高度）。
///
/// 每行只采一次：按帧采样的话，匀速滚动时高的行在视口里停留的帧数多，均值会偏高（实测 97.6 对真实 84.6）。
#[derive(Debug, Default)]
struct HeightSamples {
    sum: f64,
    count: u64,
    /// 已经采过的列表行；行表整体重建后清空。
    sampled: HashSet<usize>,
}

impl HeightSamples {
    fn add(&mut self, ix: usize, height: f32) {
        if self.count < MAX_HEIGHT_SAMPLES && height > 0. && self.sampled.insert(ix) {
            self.sum += f64::from(height);
            self.count += 1;
        }
    }

    fn mean(&self) -> Option<f32> {
        (self.count > 0).then(|| (self.sum / self.count as f64) as f32)
    }
}

/// 施加 hint 时的状态。
#[derive(Debug, Default)]
struct Hints {
    /// 最近一次施加 hint 时的列表宽度；与 [`ListFrame`] 共用：宽度在布局时变了，它当场重新施加。
    width: Rc<Cell<Option<Pixels>>>,
    measured: bool,
    dirty: bool,
    /// 最近一次施加的耗时（微秒），跑分记录。
    last_apply_us: f64,
}

/// 平滑滚动、平滑 reveal 与跑分驱动的滚动。
#[derive(Debug, Default)]
pub(super) struct Motion {
    last_frame: Option<Instant>,
    /// 本帧用的时间步长（秒）；空闲后的第一帧按一个 60 Hz 帧算，免得一步跳太远（r2-02 §3.5）。
    dt: f32,
    /// 正在平滑露出的列表行。
    reveal: Option<usize>,
    /// 平滑滚轮剩下的距离（px，正数向下）。
    wheel: f32,
    /// 上一次快照之后本视图主动滚过的距离（px），跑分用来区分“自己滚的”和“跳动”。
    pub(super) applied: f32,
}

/// 已经同步给 `ListState` 的行数。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Rows {
    count: usize,
}

/// 面板内排序的临时状态；拖动期间只记录源、分区、指针和候选锚点，落地仍由 core 校验。
#[derive(Clone, Debug)]
struct ReorderDrag {
    id: Arc<str>,
    section: ReorderSection,
    /// 拖动源滚出缓存后仍能绘制 ghost；落地顺序由 core 校验。
    item: Arc<ListItem>,
    pointer: Point<Pixels>,
    anchor: Option<Arc<str>>,
    after: bool,
    active: bool,
    source_bounds: Bounds<Pixels>,
    grab_offset: Pixels,
}

/// Bounds painted during an in-panel reorder. This is deliberately separate from
/// preview anchors: preview visibility and hover timing must not affect drop geometry.
#[derive(Clone, Default)]
struct ReorderGeometry {
    root: Option<Bounds<Pixels>>,
    area: Option<Bounds<Pixels>>,
    cards: Vec<(Arc<str>, ReorderSection, Bounds<Pixels>)>,
    ghost: Option<Bounds<Pixels>>,
    indicator: Option<Bounds<Pixels>>,
    repaint_requested: bool,
}

type ReorderMeasurements = (
    bool,
    Option<Bounds<Pixels>>,
    Option<Bounds<Pixels>>,
    Option<Bounds<Pixels>>,
);

pub struct ClipboardList {
    pub(super) source: Arc<dyn ClipboardSource>,
    pub(super) model: ListModel,
    pub(super) controller: ListController,
    layout: LayoutSpec,
    pub(super) state: ListState,
    focus: FocusHandle,
    window: AnyWindowHandle,
    pub(super) images: Entity<KpImageCache>,
    icons: Entity<KpImageCache>,
    thumbnails: HashMap<Arc<str>, Thumbnail>,
    rows: Rows,
    visible: bool,
    pub(super) snapshot: Rc<RefCell<Snapshot>>,
    seen_seq: u64,
    /// 上一帧画成骨架的列表行。
    placeholders: Rc<RefCell<HashSet<usize>>>,
    heights: HeightSamples,
    hints: Hints,
    pub(super) motion: Motion,
    /// 跑分时额外留出的右边距（px），用来制造“宽度变化”（不能调 `Window::resize`）。
    pub(super) extra_right: f32,
    pub(super) bench: Option<Rc<RefCell<Bench>>>,
    timing: Option<Rc<RefCell<FrameTiming>>>,
    rem: Pixels,
    /// 本帧的本地时间。
    now: DateTime<Local>,
    /// 自测时忽略系统的“减少动画”（`KP_FORCE_MOTION=1`）。
    force_motion: bool,
    /// 快捷动作、删除保护、点击行为等用到的设置（core 设置变化时更新）。
    settings: Settings,
    /// 按设置算好的删除保护（每行都要看）。
    delete_policy: DeletePolicy,
    /// 自定义分组（空态文案取分组名）。
    groups: Vec<Group>,
    selection: Selection,
    /// 指针所在的卡片：显示快捷动作、备注原文。
    pub(super) hovered: Option<Arc<str>>,
    /// 按住修饰键：显示数字角标。
    pub(super) key_hints: bool,
    /// 刚复制成功的快捷动作按钮，1 秒后复原。
    copied: Option<(Arc<str>, QuickAction)>,
    copied_reset: Option<Task<()>>,
    /// 批量删除后第一页落地时再补拉视图范围。
    refetch_view_after_first_page: bool,
    note: Option<NoteEdit>,
    content_edit: Option<ContentEdit>,
    content_loading: Option<Task<()>>,
    /// 粘贴、复制交给谁做（平台层的粘贴链路，或夹具的替身）。
    host: Rc<dyn ItemHost>,
    freshness: Freshness,
    /// 数据还没追上 core 时按下的 Enter / Mod+数字，第一页落地后执行。
    pending_activation: Option<Activation>,
    activation_timeout: Option<Task<()>>,
    pointer: Pointer,
    /// 预览窗（见 `previewing`）。
    previewing: previewing::Previewing,
    /// 卡片上按下左键后拖动多远算拖出（平台层的拖出接口）。
    drag: DragTracker,
    reorder: Option<ReorderDrag>,
    reorder_geometry: Rc<RefCell<ReorderGeometry>>,
    reorder_scroll_task: Option<Task<()>>,
    pending_reorder_release: Option<Point<Pixels>>,
    pending_drag_out: Option<Arc<str>>,
    /// 左键按下的卡片；拖出开始时清掉，点击时据此执行单击动作。
    armed_click: Option<Arc<str>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ListIntent> for ClipboardList {}

impl gpui::Focusable for ClipboardList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ClipboardList {
    pub fn new(
        source: Arc<dyn ClipboardSource>,
        host: Rc<dyn ItemHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let images = cx.new(|_| KpImageCache::new());
        let icons = cx.new(|_| KpImageCache::with_capacity(ICON_CACHE_CAPACITY));
        let mut subscriptions = vec![cx.observe(&images, |_, _, cx| cx.notify())];
        subscriptions.push(cx.observe(&icons, |_, _, cx| cx.notify()));
        // 面板的事件实体在面板窗口打开之前就有了，构造时直接订阅。
        if let Some(panel) = cx.try_global::<Panel>() {
            let events = panel.events().clone();
            subscriptions.push(cx.subscribe_in(
                &events,
                window,
                |list, _, event: &PanelEvent, window, cx| list.on_panel_event(*event, window, cx),
            ));
        }

        let settings = source.settings();
        let mut list = Self {
            source,
            model: ListModel::new(),
            controller: ListController::new(),
            layout: LayoutSpec::from_settings(&settings.clipboard.display),
            state: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            focus: cx.focus_handle(),
            window: window.window_handle(),
            images,
            icons,
            thumbnails: HashMap::new(),
            rows: Rows::default(),
            visible: false,
            snapshot: Rc::default(),
            seen_seq: 0,
            placeholders: Rc::default(),
            heights: HeightSamples::default(),
            hints: Hints::default(),
            motion: Motion::default(),
            extra_right: 0.,
            bench: None,
            timing: None,
            rem: window.rem_size(),
            now: Local::now(),
            force_motion: crate::selftest::active()
                && std::env::var_os("KP_FORCE_MOTION").is_some_and(|value| value == "1"),
            delete_policy: DeletePolicy::from_settings(&settings.clipboard.content),
            settings,
            groups: Vec::new(),
            selection: Selection::default(),
            hovered: None,
            key_hints: false,
            copied: None,
            copied_reset: None,
            refetch_view_after_first_page: false,
            note: None,
            content_edit: None,
            content_loading: None,
            host,
            freshness: Freshness::default(),
            pending_activation: None,
            activation_timeout: None,
            pointer: Pointer::Moved,
            previewing: previewing::Previewing::default(),
            drag: DragTracker::default(),
            reorder: None,
            reorder_geometry: Rc::default(),
            reorder_scroll_task: None,
            pending_reorder_release: None,
            pending_drag_out: None,
            armed_click: None,
            _subscriptions: subscriptions,
        };
        let request = list.model.reset_and_reload();
        list.fetch(request, cx);

        list
    }

    /// 跟随 core 的记录事件刷新（新记录、清理、导入备份或切换存储位置）和设置变化。
    pub fn follow_core_events(&mut self, events: &Entity<CoreEvents>, cx: &mut Context<Self>) {
        let subscription = cx.subscribe(events, |list, _, event: &CoreEvent, cx| {
            let update = match event {
                CoreEvent::SettingsUpdated { settings, .. } => {
                    list.apply_settings((**settings).clone(), cx);
                    return;
                }
                CoreEvent::ClipboardUpserted {
                    kind, deduplicated, ..
                } => ListUpdate::Upserted {
                    kind: item_kind(*kind),
                    deduplicated: *deduplicated,
                },
                CoreEvent::ClipboardCleaned { removed } => {
                    ListUpdate::Cleaned { removed: *removed }
                }
                CoreEvent::ClipboardReloaded => ListUpdate::Reloaded,
                CoreEvent::OcrChanged => ListUpdate::ImageTextChanged,
                _ => return,
            };
            list.on_update(update, cx);
        });
        self._subscriptions.push(subscription);
    }

    /// 打开跑分计时（自测）。
    pub(super) fn enable_bench(&mut self, bench: Rc<RefCell<Bench>>) {
        self.timing = Some(Rc::default());
        self.bench = Some(bench);
    }

    /// 列表所在的窗口（面板）。
    pub fn window_handle(&self) -> AnyWindowHandle {
        self.window
    }

    pub fn total(&self) -> usize {
        self.model.total()
    }

    /// 处理 core 的列表变化（新记录、清理、整体重载）。
    pub fn on_update(&mut self, update: ListUpdate, cx: &mut Context<Self>) {
        let action = self
            .controller
            .on_update(update, self.visible, self.at_top());
        log::debug!("list update {update:?} -> {action:?}");
        let reset_selection = match action {
            UpdateAction::ReloadNow { reset_selection }
            | UpdateAction::Defer { reset_selection } => reset_selection,
            UpdateAction::Ignore => false,
        };
        if reset_selection {
            // 清理、导入之后不再勾着可能已经不存在的记录（1.x `resetChecked`）。
            self.selection.reset();
        }
        if action != UpdateAction::Ignore {
            self.freshness.changed();
        }
        if let UpdateAction::ReloadNow { .. } = action {
            self.reload(cx);
        }
        cx.notify();
    }

    /// 面板事件。显示时和退出编辑态时把焦点放回列表：钩子转来的按键（Windows）和 key 窗口收到的
    /// 按键（macOS）都按焦点所在的 key context 匹配，列表的绑定才会生效。
    fn on_panel_event(&mut self, event: PanelEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PanelEvent::Shown => {
                self.visible = true;
                // `scrollToTopOnOpen` 默认开启：清掉选中、回到顶部，当前项就是第一个可见行；
                // 光标下的卡片要等指针动过才能抢走当前项。挂起的刷新当场发出。
                self.controller.on_shown();
                self.controller.set_first_visible(0);
                self.pointer = Pointer::Waiting;
                self.motion.reveal = None;
                self.motion.wheel = 0.;
                self.motion.last_frame = None;
                self.state.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.),
                });
                let deferred = self.controller.take_reload_at_top(true);
                if self.model.total() > 0 && self.model.get(0).is_none() {
                    // 深滚动可淘汰包括置顶行在内的第一页，Enter 必须等它重新加载。
                    self.freshness.changed();
                    self.reload(cx);
                } else if deferred {
                    self.reload(cx);
                }
                window.focus(&self.focus, cx);
                cx.notify();
            }
            PanelEvent::EditingEnded => {
                if self.note.is_none() && self.content_edit.is_none() {
                    window.focus(&self.focus, cx);
                }
            }
            // 备注框要的编辑态：拿到前台后聚焦输入框；没拿到也聚焦，Esc 仍能关掉备注框。
            PanelEvent::EditingStarted | PanelEvent::EditingRefused => {
                if editing::target(cx) == Some(EditTarget::Note)
                    && let Some(note) = &self.note
                {
                    note.input.focus(window, cx);
                }
                if editing::target(cx) == Some(EditTarget::Content)
                    && let Some(edit) = &self.content_edit
                {
                    edit.input.focus(window, cx);
                }
            }
            PanelEvent::PopupDismissed => {
                dismiss_menu(window, cx);
            }
            PanelEvent::Hidden => {
                self.visible = false;
                self.cancel_reorder(cx);
                self.motion.reveal = None;
                self.motion.wheel = 0.;
                self.hovered = None;
                // 面板已经隐藏，还挂着的 Enter 不再执行，开着的右键菜单、预览收起。
                self.pending_activation = None;
                self.activation_timeout = None;
                dismiss_menu(window, cx);
                self.close_preview(cx);
                // 隐藏时退出多选（1.x `exitClipboardSelection`），收起备注框（当作取消）。
                self.selection.exit();
                self.content_loading = None;
                if self.content_edit.is_some() {
                    close_dialog(window, cx);
                    self.finish_content(false, window, cx);
                }
                if self.note.is_some() {
                    close_dialog(window, cx);
                    self.finish_note(false, window, cx);
                }
                // 隐藏即释放（附录 B §5.3）：缩略图位图全部还给图集，行缓存只留第一页。
                self.images
                    .update(cx, |images, cx| images.clear(Some(window), cx));
                self.model.release_rows();
                // 缩略图路径只是字符串，留着；与 1.x 的 `useImageThumbnail` 一样封顶 512 条。
                if self.thumbnails.len() > THUMBNAIL_PATHS_MAX {
                    self.thumbnails.clear();
                }
            }
        }
    }

    // ---------------------------------------------------------------- 数据

    pub(super) fn fetch(&mut self, request: FetchRequest, cx: &mut Context<Self>) {
        if request.replace && request.range.start == 0 {
            self.freshness.first_page_sent(request.token);
        }
        let future = self.source.list(ListQuery {
            offset: request.range.start,
            limit: request.range.len(),
            filter: self.controller.filter().clone(),
            sort: self.settings.clipboard.content.sort,
        });

        cx.spawn(async move |this, cx| {
            let result = future.await;
            this.update(cx, |list, cx| list.on_fetched(request, result, cx))
                .ok();
        })
        .detach();
    }

    fn on_fetched(
        &mut self,
        request: FetchRequest,
        result: anyhow::Result<crate::clipboard::model::list_model::Page>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(page) => {
                // 排序刷新时保留视口锚点；置顶切换不能让正在浏览的卡片上下跳一行。
                let top = self.state.logical_scroll_top();
                let anchor = (request.replace && !self.at_top())
                    .then(|| self.model.get(top.item_ix).map(|item| item.id.clone()))
                    .flatten();
                if let Some(applied) = self.model.apply(&request, page) {
                    self.sync_rows(Some(&applied));
                    if let Some(index) = anchor.and_then(|id| self.model.index_of(&id)) {
                        self.state.scroll_to(ListOffset {
                            item_ix: index,
                            offset_in_item: top.offset_in_item,
                        });
                        self.controller.set_first_visible(index);
                    }
                    if request.replace
                        && request.range.start == 0
                        && std::mem::take(&mut self.refetch_view_after_first_page)
                        && let Some(request) = self.model.refetch_view()
                    {
                        self.fetch(request, cx);
                    }
                }
            }
            Err(err) => {
                log::warn!("list page {:?} could not be loaded: {err:#}", request.range);
                if self.model.fail(&request) {
                    self.sync_rows(None);
                }
            }
        }
        if request.replace && request.range.start == 0 {
            self.freshness.first_page_done(request.token);
            self.run_pending_activation(cx);
        }
        cx.notify();
    }

    /// 把模型的行数变化同步给 `ListState`。
    fn sync_rows(&mut self, applied: Option<&Applied>) {
        let target = Rows {
            count: self.model.total(),
        };
        let rebuild = applied.is_none_or(|applied| {
            applied.replaced
                && applied.range.start == 0
                && (self.at_top() || self.rows == Rows::default())
        });

        if rebuild {
            // 第一页整体替换且在顶部：重建行表、回到顶部（L4），下一帧按估计行高重新施加 hint。
            self.state.reset(target.count);
            self.state.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            self.motion.reveal = None;
            self.rows = target;
            self.hints.dirty = true;
            self.heights.sampled.clear();
            // 回到了顶部：当前项立刻是新的第一行，不等下一帧的布局快照。
            self.controller.set_first_visible(0);
            return;
        }

        if target.count != self.rows.count {
            if target.count > self.rows.count {
                self.state.splice(
                    self.rows.count..self.rows.count,
                    target.count - self.rows.count,
                );
            } else {
                self.state.splice(target.count..self.rows.count, 0);
            }
            self.rows.count = target.count;
            self.hints.dirty = true;
        }

        if let Some(applied) = applied {
            let start = applied.range.start;
            let end = applied.range.end.min(self.rows.count);
            if start < end {
                self.state.remeasure_items(start..end);
            }
        }
    }

    /// 在顶部重拉第一页（有新内容时，1.x `reload`）。
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        let request = self.model.reload();
        self.fetch(request, cx);
    }

    /// 查询条件变了：清空行表、回到顶部、显示加载态后拉第一页（1.x `resetAndReload`）。
    fn reload_from_scratch(&mut self, cx: &mut Context<Self>) {
        self.state.reset(0);
        self.state.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        self.rows = Rows::default();
        self.controller.set_first_visible(0);
        *self.snapshot.borrow_mut() = Snapshot::default();
        self.motion.reveal = None;
        self.motion.wheel = 0.;
        self.hints.dirty = true;
        self.heights.sampled.clear();
        let request = self.model.reset_and_reload();
        self.fetch(request, cx);
        cx.notify();
    }

    /// 删除一条后同步（单条删除、在收藏里取消收藏、移出当前分组）。
    pub(super) fn remove_item(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        self.controller.select_after_delete(&self.model, id);
        self.selection.forget(id);
        if self.hovered.as_deref() == Some(id) {
            self.hovered = None;
        }
        let Some(removed) = self.model.remove_by_id(id) else {
            return false;
        };

        if removed.index < self.rows.count {
            self.state.splice(removed.index..removed.index + 1, 0);
            self.rows.count -= 1;
        }
        self.controller.set_first_visible(
            self.state
                .logical_scroll_top()
                .item_ix
                .min(self.rows.count.saturating_sub(1)),
        );
        if let Some(request) = removed.refetch {
            self.fetch(request, cx);
        }
        cx.notify();
        true
    }

    /// 本地合并一条记录的改动，并让它重新测量（收藏、便签等）。
    pub(super) fn patch_item(
        &mut self,
        id: &str,
        patch: impl FnOnce(&mut ListItem),
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.model.patch_by_id(id, patch)?;
        self.state.remeasure_items(index..index + 1);
        cx.notify();
        Some(index)
    }

    // ---------------------------------------------------------------- 每帧

    /// 系统“减少动画”。自测可以用 `KP_FORCE_MOTION=1` 忽略它，在关了动画的机器上也能量平滑滚动。
    fn reduce_motion(&self, cx: &App) -> bool {
        !self.force_motion && cx.reduce_motion()
    }

    /// 没有正在进行的平滑滚动或露出。
    pub(super) fn motion_idle(&self) -> bool {
        self.motion.reveal.is_none() && self.motion.wheel == 0.
    }

    pub(super) fn at_top(&self) -> bool {
        let top = self.state.logical_scroll_top();
        top.item_ix == 0 && top.offset_in_item <= px(0.5)
    }

    /// 读上一帧的快照：记录平均行高、第一个可见行，按可见范围补数据。
    fn consume_snapshot(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.snapshot.borrow().clone();
        let fresh = snapshot.seq != self.seen_seq;
        self.seen_seq = snapshot.seq;

        if fresh {
            let placeholders = self.placeholders.borrow();
            for row in &snapshot.rows {
                if !placeholders.contains(&row.ix) {
                    self.heights.add(row.ix, row.height);
                }
            }
            // 跑分已经在本帧开头读过上一段主动滚动的距离。
            self.motion.applied = 0.;
        }

        let top = self.state.logical_scroll_top().item_ix;
        let (first, last) = match (snapshot.rows.first(), snapshot.rows.last()) {
            (Some(_), Some(last)) => (top, last.ix.max(top)),
            _ => (top, top + 10),
        };
        self.controller.set_first_visible(first);

        if self.model.loaded_initial() && self.rows.count > 0 {
            let visible = first..last + 1;
            if let Some(request) = self.model.load_range(visible) {
                self.fetch(request, cx);
            }
        }
    }

    /// L2：首帧布局后、第一页测量后、宽度或行表变化后，按平均行高给未测量的行施加 hint。
    fn apply_hints(&mut self) {
        let width = self.state.viewport_bounds().size.width;
        if width <= px(0.) || self.rows.count == 0 {
            return;
        }

        let measured = self.heights.count >= MIN_HEIGHT_SAMPLES;
        let due = self.hints.dirty
            || self.hints.width.get() != Some(width)
            || (measured && !self.hints.measured);
        if !due {
            return;
        }

        let estimate = self.height_estimate();
        let started = Instant::now();
        // ListState 是 Rc 句柄，对克隆调用 builder 改的是同一份状态。
        let _ = self.state.clone().with_uniform_item_height(px(estimate));
        self.hints.width.set(Some(width));
        self.hints.measured = measured;
        self.hints.dirty = false;
        self.hints.last_apply_us = started.elapsed().as_secs_f64() * 1e6;
        log::debug!(
            "list hints applied: {estimate:.1}px for {} rows (width {width:?}) in {:.0} us",
            self.rows.count,
            self.hints.last_apply_us
        );
    }

    /// 未测量行的估计高度：实测均值（样本够了之后），否则是骨架行高。
    fn height_estimate(&self) -> f32 {
        self.heights
            .mean()
            .filter(|_| self.heights.count >= MIN_HEIGHT_SAMPLES)
            .unwrap_or_else(|| {
                px_rems(self.layout.placeholder_height())
                    .to_pixels(self.rem)
                    .as_f32()
            })
    }

    /// 平滑滚轮、平滑 reveal、跑分驱动：按剩余距离 ×(1−e^{−dt/τ}) 逐帧滚动。
    fn run_motion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let dt = self
            .motion
            .last_frame
            .map(|last| (now - last).as_secs_f32())
            .unwrap_or(1. / 60.);
        self.motion.last_frame = Some(now);
        self.motion.dt = dt.clamp(0.001, 1. / 60.);
        let fraction = 1. - (-self.motion.dt / TAU_S).exp();

        if let Some(bench) = self.bench.clone()
            && let Some(step) = bench.borrow_mut().drive_step(now)
        {
            self.state.scroll_by(px(step));
            self.motion.applied += step;
            window.request_animation_frame();
        }

        if self.motion.wheel.abs() > 0.5 {
            let step = eased_step(self.motion.wheel, fraction);
            self.state.scroll_by(px(step));
            self.motion.applied += step;
            self.motion.wheel -= step;
            window.request_animation_frame();
        } else {
            self.motion.wheel = 0.;
        }

        if let Some(ix) = self.motion.reveal {
            let remaining = self.reveal_distance(ix);
            if remaining.abs() < 0.5 || self.reduce_motion(cx) {
                if remaining.abs() >= 0.5 {
                    self.state.scroll_by(px(remaining));
                    self.motion.applied += remaining;
                }
                self.motion.reveal = None;
            } else {
                let step = eased_step(remaining, fraction);
                self.state.scroll_by(px(step));
                self.motion.applied += step;
                window.request_animation_frame();
            }
        }
    }

    /// 让第 `ix` 行完整露出还要滚多少 px：在同一棵 sum tree 上试算后还原（r2-02 §3.5）。
    fn reveal_distance(&self, ix: usize) -> f32 {
        if self.state.viewport_bounds().size.height <= px(0.) {
            return 0.;
        }

        let saved = self.state.logical_scroll_top();
        let current = -self.state.scroll_px_offset_for_scrollbar().y.as_f32();
        self.state.scroll_to_reveal_item(ix);
        let target = -self.state.scroll_px_offset_for_scrollbar().y.as_f32();
        self.state.scroll_to(saved);

        target - current
    }

    /// 到顶的那一帧消费挂起的刷新（L3）。
    fn consume_reload_at_top(&mut self, cx: &mut Context<Self>) {
        if self.visible && self.controller.take_reload_at_top(self.at_top()) {
            log::debug!("deferred list reload consumed at the top");
            self.reload(cx);
        }
    }

    // ---------------------------------------------------------------- 键盘与指针

    pub(super) fn navigate(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let (outcome, request) = self.controller.navigate(nav, &mut self.model);
        if let Some(request) = request {
            self.fetch(request, cx);
        }
        if let NavOutcome::Moved { index } = outcome {
            self.motion.reveal = Some(index);
        }
        if matches!(outcome, NavOutcome::Moved { .. }) {
            self.preview_follow_active(cx);
        }
        cx.notify();
    }

    /// 把一条记录滚进视口（截图摆场景用）。
    pub(super) fn reveal_item(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(index) = self.model.index_of(id) {
            self.motion.reveal = Some(index);
            cx.notify();
        }
    }

    /// 停止自测中的平滑定位并恢复捕获前的滚动位置。
    pub(super) fn stop_reveal(&mut self, cx: &mut Context<Self>) {
        self.motion.reveal = None;
        cx.notify();
    }

    /// ↑ / ↓（主窗口的按键转来）。
    pub fn select_previous(&mut self, cx: &mut Context<Self>) {
        self.navigate(Nav::Up, cx);
    }

    pub fn select_next(&mut self, cx: &mut Context<Self>) {
        self.navigate(Nav::Down, cx);
    }

    /// 指针进出卡片：进入时它成为当前项（1.x hover 与键盘共用选中），并显示快捷动作。
    fn hover_card(&mut self, id: &Arc<str>, hovered: bool, cx: &mut Context<Self>) {
        if self.reorder.as_ref().is_some_and(|drag| drag.active) {
            return;
        }
        if hovered {
            if self.pointer == Pointer::Moved {
                self.controller.hover(id);
            }
            self.hovered = Some(id.clone());
        } else if self.hovered.as_ref() == Some(id) {
            self.hovered = None;
        }
        self.preview_hover(id, hovered, cx);
        cx.notify();
    }

    /// 交互自测使用的合成悬停，不读取系统指针位置；仍走正式的悬停预览计时器。
    pub(crate) fn selftest_hover(&mut self, id: &Arc<str>, cx: &mut Context<Self>) {
        // 合成事件没有经过 `pointer_moved`，因此不能依赖真实指针把状态推进到
        // `Moved`；否则预览用例会偶发地在 `preview_hover` 的门槛处直接返回。
        self.pointer = Pointer::Moved;
        self.hover_card(id, true, cx);
    }

    /// 交互自测读取已经绘制卡片的中心点，随后仍通过 GPUI 鼠标事件驱动真实拖动路径。
    pub(crate) fn selftest_card_center(&self, id: &str) -> Option<Point<Pixels>> {
        self.previewing
            .anchors
            .borrow()
            .iter()
            .find(|(anchor, _)| &**anchor == id)
            .map(|(_, bounds)| {
                point(
                    bounds.origin.x + bounds.size.width / 2.,
                    bounds.origin.y + bounds.size.height / 2.,
                )
            })
    }

    /// Read the actual painted reorder overlay for GPUI selftests.
    pub(crate) fn selftest_reorder_measurements(&self) -> Option<ReorderMeasurements> {
        let drag = self.reorder.as_ref()?;
        let geometry = self.reorder_geometry.borrow();
        let source = geometry
            .cards
            .iter()
            .find(|(id, _, _)| id.as_ref() == drag.id.as_ref())
            .map(|(_, _, bounds)| *bounds)
            .or(Some(drag.source_bounds));
        Some((drag.active, source, geometry.ghost, geometry.indicator))
    }

    pub(crate) fn selftest_reorder_active(&self) -> bool {
        self.reorder.as_ref().is_some_and(|drag| drag.active)
    }

    pub(crate) fn selftest_reorder_card_bounds(&self, id: &str) -> Option<Bounds<Pixels>> {
        self.reorder_geometry
            .borrow()
            .cards
            .iter()
            .find(|(candidate, _, _)| candidate.as_ref() == id)
            .map(|(_, _, bounds)| *bounds)
    }

    pub(crate) fn selftest_reorder_indicator_matches_gap(&self) -> Option<bool> {
        let drag = self.reorder.as_ref()?.clone();
        let anchor = drag.anchor?;
        let mut cards: Vec<_> = self
            .reorder_geometry
            .borrow()
            .cards
            .iter()
            .filter(|(_, section, _)| *section == drag.section)
            .cloned()
            .collect();
        cards.sort_by_key(|entry| entry.2.origin.y);
        let index = cards.iter().position(|(id, _, _)| id == &anchor)?;
        let anchor_bounds = cards.get(index)?.2;
        let expected = if drag.after {
            cards
                .get(index + 1)
                .map(|(_, _, next)| (anchor_bounds.bottom() + next.origin.y) / 2.)
                .unwrap_or(anchor_bounds.bottom())
        } else {
            cards
                .get(index.checked_sub(1).unwrap_or(usize::MAX))
                .map(|(_, _, previous)| (previous.bottom() + anchor_bounds.origin.y) / 2.)
                .unwrap_or(anchor_bounds.origin.y)
        };
        let indicator = self.reorder_geometry.borrow().indicator?;
        Some((indicator.origin.y - expected).abs() <= px(2.))
    }

    /// 指针移动：显示后第一次移动只记下位置（可能是系统补发的），离开它超过 [`POINTER_SLOP`]
    /// 才算动过，这时光标下的卡片成为当前项。
    fn pointer_moved(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.previewing.pointer = Some(position);
        match self.pointer {
            Pointer::Moved => {}
            Pointer::Waiting => self.pointer = Pointer::Resting(position),
            Pointer::Resting(rest) => {
                let dx = (position.x - rest.x).as_f32();
                let dy = (position.y - rest.y).as_f32();
                if dx.hypot(dy) <= POINTER_SLOP {
                    return;
                }
                self.pointer = Pointer::Moved;
                if let Some(id) = self.hovered.clone() {
                    self.controller.hover(&id);
                    self.preview_hover(&id, true, cx);
                    cx.notify();
                }
            }
        }
    }

    fn update_reorder_target(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((id, section)) = self
            .reorder
            .as_ref()
            .map(|drag| (drag.id.clone(), drag.section))
        else {
            return;
        };
        if let Some(drag) = self.reorder.as_mut() {
            drag.pointer = position;
        }
        if position.x < px(0.)
            || position.y < px(0.)
            || position.x > window.bounds().size.width
            || position.y > window.bounds().size.height
        {
            self.reorder = None;
            self.armed_click = None;
            self.drag.release();
            self.drag_out(id, window, cx);
            return;
        }
        let geometry = self.reorder_geometry.borrow();
        let mut cards: Vec<_> = geometry
            .cards
            .iter()
            .filter(|(_, card_section, bounds)| {
                *card_section == section
                    && geometry.area.is_some_and(|area| {
                        bounds.bottom() > area.top() && bounds.top() < area.bottom()
                    })
            })
            .filter(|(card_id, _, _)| card_id.as_ref() != id.as_ref())
            .cloned()
            .collect();
        cards.sort_by_key(|left| left.2.origin.y);
        if let Some(drag) = self.reorder.as_mut() {
            if let Some((anchor, _, _bounds)) = cards
                .iter()
                .find(|(_, _, bounds)| position.y < bounds.origin.y + bounds.size.height / 2.)
            {
                drag.anchor = Some(anchor.clone());
                drag.after = false;
            } else if let Some((anchor, _, _)) = cards.last() {
                drag.anchor = Some(anchor.clone());
                drag.after = true;
            } else {
                drag.anchor = None;
                drag.after = false;
            }
        }
        cx.notify();
    }

    fn start_reorder_autoscroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reorder_scroll_task.is_some() {
            return;
        }
        let entity = cx.entity().downgrade();
        self.reorder_scroll_task = Some(cx.spawn_in(window, async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let keep_running = entity
                    .update_in(cx, |list, window, cx| {
                        let Some(drag) = list.reorder.as_ref() else {
                            return false;
                        };
                        if !drag.active {
                            return drag.active;
                        }
                        let Some(area) = list.reorder_geometry.borrow().area else {
                            return true;
                        };
                        let edge = px(40.);
                        let depth = if drag.pointer.y < area.origin.y + edge {
                            (area.origin.y + edge - drag.pointer.y)
                                .as_f32()
                                .min(edge.as_f32())
                        } else if drag.pointer.y > area.origin.y + area.size.height - edge {
                            (drag.pointer.y - (area.origin.y + area.size.height - edge))
                                .as_f32()
                                .min(edge.as_f32())
                        } else {
                            0.
                        };
                        if depth > 0. {
                            let direction = if drag.pointer.y < area.origin.y + edge {
                                -1.
                            } else {
                                1.
                            };
                            let step = direction * 12. * (depth / edge.as_f32()).max(0.2);
                            list.reorder_geometry.borrow_mut().repaint_requested = false;
                            list.state.scroll_by(px(step));
                            // 这里不在绘制阶段，不能 request_animation_frame（会 panic）；
                            // update_reorder_target 里的 notify 会重绘。
                            list.update_reorder_target(drag.pointer, window, cx);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    fn reorder_area_contains(&self, position: Point<Pixels>) -> bool {
        self.reorder_geometry
            .borrow()
            .area
            .is_some_and(|area| area.contains(&position))
    }

    fn cancel_reorder(&mut self, cx: &mut Context<Self>) {
        if self.reorder.take().is_some() {
            self.reorder_scroll_task = None;
            self.drag.release();
            self.armed_click = None;
            self.reorder_geometry.borrow_mut().ghost = None;
            self.reorder_geometry.borrow_mut().indicator = None;
            cx.notify();
        }
    }

    pub(super) fn dismiss_reorder(&mut self, cx: &mut Context<Self>) -> bool {
        if self.reorder.is_some() {
            self.cancel_reorder(cx);
            true
        } else {
            false
        }
    }

    fn commit_reorder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.reorder.take() else {
            return;
        };
        self.reorder_scroll_task = None;
        self.drag.release();
        self.armed_click = None;
        let Some(anchor) = drag.anchor else {
            cx.notify();
            return;
        };
        let original = self.model.index_of(&drag.id);
        let changed = self.model.reorder_local(
            &drag.id,
            &anchor,
            drag.section == ReorderSection::Favorite,
            drag.after,
        );
        if !changed && original.is_some() {
            cx.notify();
            return;
        }
        if let (Some(before), Some(after)) = (original, self.model.index_of(&drag.id)) {
            self.state
                .remeasure_items(before.min(after)..before.max(after) + 1);
        }
        let anchor = if drag.after {
            ReorderAnchor::After(anchor.to_string())
        } else {
            ReorderAnchor::Before(anchor.to_string())
        };
        let selected_id = drag.id.clone();
        let future = self.source.reorder(drag.section, drag.id.clone(), anchor);
        cx.spawn_in(window, async move |list, cx| {
            let result = future.await;
            list.update_in(cx, |list, window, cx| {
                if let Err(err) = result {
                    Self::toast_error("commands:labels.reorder", &err, window, cx);
                    if let Some(request) = list.model.reload_current_range() {
                        list.fetch(request, cx);
                    }
                } else {
                    list.controller.select(&selected_id);
                    if let Some(request) = list.model.reload_current_range() {
                        list.fetch(request, cx);
                    }
                }
                if original.is_some() {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 按下卡片（1.x `handleCardMouseDown`）：多选时左键勾选、Shift 连选；否则左键选中、记下拖出的
    /// 起点（1.x 卡片 `draggable={!selecting}`）并预备单击动作（见 [`Self::click_card`]），中键按
    /// 中键设置执行。
    fn press_card(
        &mut self,
        item: Arc<ListItem>,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        if self.selection.active() {
            if event.button == MouseButton::Left {
                self.controller.select(&item.id);
                if event.modifiers.shift {
                    self.check_range(item, window, cx);
                } else {
                    self.toggle_checked(&item, window, cx);
                }
            }
            return;
        }

        let id = item.id.clone();
        match event.button {
            MouseButton::Left => {
                self.controller.select(&id);
                self.drag.press(id.to_string(), event.position);
                let section = if item.is_pinned {
                    Some(ReorderSection::Pinned)
                } else if item.is_favorite
                    && self.controller.filter().range
                        == crate::clipboard::model::filter::Range::Favorite
                {
                    Some(ReorderSection::Favorite)
                } else {
                    None
                };
                let bounds = self
                    .reorder_geometry
                    .borrow()
                    .cards
                    .iter()
                    .find(|(candidate, _, _)| candidate.as_ref() == id.as_ref())
                    .map(|(_, _, bounds)| *bounds);
                self.reorder = (!self.controller.filter().searching())
                    .then_some(section)
                    .flatten()
                    .map(|section| ReorderDrag {
                        id: id.clone(),
                        section,
                        item: item.clone(),
                        pointer: event.position,
                        anchor: None,
                        after: false,
                        active: false,
                        source_bounds: bounds.unwrap_or_else(|| Bounds {
                            origin: event.position,
                            size: gpui::size(px(0.), px(0.)),
                        }),
                        grab_offset: bounds
                            .map(|bounds| event.position.y - bounds.origin.y)
                            .unwrap_or(px(0.)),
                    });
                if self.reorder.is_some() {
                    self.reorder_geometry.borrow_mut().repaint_requested = false;
                }
                self.armed_click = Some(id);
            }
            MouseButton::Middle => {
                let action = self.settings.clipboard.content.middle_click;
                if action != MiddleClickAction::Disabled {
                    self.controller.select(&id);
                }
                match action {
                    MiddleClickAction::SingleClickPaste => self.paste_item(id, false, cx),
                    MiddleClickAction::SingleClickPastePlain => self.paste_item(id, true, cx),
                    MiddleClickAction::SingleClickCopy => self.copy(id, false, None, window, cx),
                    MiddleClickAction::SingleClickCopyPlain => {
                        self.copy(id, true, None, window, cx)
                    }
                    MiddleClickAction::Disabled => {}
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// 点击卡片（左键在同一张卡片上按下又松开）：“单击粘贴 / 复制”在没有拖出时执行，“双击粘贴 /
    /// 复制”在第二次点击时执行（1.x `handleCardDoubleClick`）；多选时不响应。单击动作不放在按下时：
    /// 那样拖出图片会先粘贴一次、投放时再贴一次。
    fn click_card(
        &mut self,
        item: Arc<ListItem>,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reorder.as_ref().is_some_and(|drag| drag.active) {
            return;
        }
        let armed = self.armed_click.take().is_some_and(|id| id == item.id);
        if self.selection.active() {
            return;
        }

        let id = item.id.clone();
        let double = click_count >= 2;
        match self.settings.clipboard.content.auto_paste {
            AutoPaste::SingleClickPaste if armed => self.paste_item(id, false, cx),
            AutoPaste::SingleClickCopy if armed => self.copy(id, false, None, window, cx),
            AutoPaste::DoubleClickPaste if double => self.paste_item(id, false, cx),
            AutoPaste::DoubleClickCopy if double => self.copy(id, false, None, window, cx),
            _ => {}
        }
    }

    pub(super) fn on_wheel_lines(&mut self, lines: f32, cx: &mut Context<Self>) {
        self.close_pointer_preview(cx);
        let distance = -lines * px_rems(WHEEL_LINE).to_pixels(self.rem).as_f32();
        if self.reduce_motion(cx) {
            self.state.scroll_by(px(distance));
        } else {
            self.motion.wheel += distance;
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- 渲染

    /// 一张卡片的图片区状态：已有路径就交给 KpImageCache；图片记录没有缩略图时先向数据源要。
    fn visual(
        &mut self,
        item: &ListItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Visual> {
        let target = card::image_target(item, &self.layout)?;
        let path = match (&target.path, &target.file_name) {
            (Some(path), _) => path.clone(),
            (None, Some(file_name)) => match self.thumbnails.get(file_name).cloned() {
                Some(Thumbnail::Ready(path)) => path,
                Some(Thumbnail::Failed) => return Some(Visual::Failed),
                Some(Thumbnail::Pending) => return Some(Visual::Loading),
                None => {
                    self.request_thumbnail(file_name.clone(), cx);
                    return Some(Visual::Loading);
                }
            },
            (None, None) => return Some(Visual::Failed),
        };

        let scale = window.scale_factor();
        let to_physical = |value: f32| {
            (px_rems(value).to_pixels(self.rem).as_f32() * scale)
                .round()
                .max(1.) as u32
        };
        let key = ImageKey {
            path: path_of(&path),
            width: to_physical(target.display.width),
            height: to_physical(target.display.height),
            resize: ResizeMode::Exact,
        };

        Some(
            match self
                .images
                .update(cx, |images, cx| images.request(key, window, cx))
            {
                ImageState::Ready(image) => Visual::Ready(image),
                ImageState::Loading => Visual::Loading,
                ImageState::Failed => Visual::Failed,
            },
        )
    }

    /// 请求一枚按窗口物理尺寸等比缩放的图标；图标缓存独立于隐藏时清空的缩略图缓存。
    fn icon(
        &mut self,
        path: &str,
        logical_size: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Visual {
        let physical = (px_rems(logical_size).to_pixels(self.rem).as_f32() * window.scale_factor())
            .ceil()
            .max(1.) as u32;
        let key = ImageKey {
            path: path_of(path),
            width: physical,
            height: physical,
            resize: ResizeMode::Contain,
        };
        match self
            .icons
            .update(cx, |icons, cx| icons.request(key, window, cx))
        {
            ImageState::Ready(image) => Visual::Ready(image),
            ImageState::Loading => Visual::Loading,
            ImageState::Failed => Visual::Failed,
        }
    }

    /// 卡片要的来源应用图标和文件行图标（顺序与 `ListItem::file_rows()` 相同）。
    fn card_icons(
        &mut self,
        item: &ListItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Option<Visual>, Vec<Option<Visual>>) {
        let app_size = if self.layout.header_row { 14. } else { 16. };
        let app_icon = item
            .source_app_id
            .as_ref()
            .and(item.source_app_icon_path.as_deref())
            .map(|path| self.icon(path, app_size, window, cx));

        let file_icons = if item.kind == ItemKind::Files
            && item.files_preview_kind != Some(FilesPreview::ImagePreview)
        {
            item.file_rows()
                .iter()
                .map(|row| {
                    row.icon_path
                        .as_deref()
                        .map(|path| self.icon(path, 20., window, cx))
                })
                .collect()
        } else {
            Vec::new()
        };

        (app_icon, file_icons)
    }

    fn request_thumbnail(&mut self, file_name: Arc<str>, cx: &mut Context<Self>) {
        self.thumbnails
            .insert(file_name.clone(), Thumbnail::Pending);
        let future = self.source.thumbnail(file_name.clone());

        cx.spawn(async move |this, cx| {
            let result = future.await;
            this.update(cx, |list, cx| {
                let thumbnail = match result {
                    Ok(path) => Thumbnail::Ready(Arc::from(path.to_string_lossy().as_ref())),
                    Err(err) => {
                        log::warn!("thumbnail for {file_name} is unavailable: {err:#}");
                        Thumbnail::Failed
                    }
                };
                list.thumbnails.insert(file_name, thumbnail);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_row(
        &mut self,
        index: usize,
        env: &CardEnv<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self.model.get(index).cloned() else {
            return card::placeholder(env);
        };
        let image = self.visual(&item, window, cx);
        let (app_icon, file_icons) = self.card_icons(&item, window, cx);
        let active = self.controller.is_active(index, &item.id);
        let selecting = self.selection.active();
        let hovered = self.hovered.as_ref() == Some(&item.id);
        let can_delete = self.can_delete(item.is_favorite, item.is_pinned);
        let hint = self
            .key_hints
            .then(|| self.controller.hint_key(index, selecting))
            .flatten();
        let actions = (hovered && !selecting)
            .then(|| {
                visible_actions(
                    &self.settings.clipboard.content.item_actions,
                    &item,
                    can_delete,
                )
            })
            .filter(|actions| !actions.is_empty())
            .map(|actions| self.quick_actions(&item, actions, cx));
        let checkbox = selecting.then(|| self.checkbox(&item, can_delete));
        let on_snippet = (!selecting && !item.quick_snippets.is_empty()).then(|| {
            let entity = cx.entity().downgrade();
            let target = item.clone();
            let handler: SnippetHandler = Rc::new(move |text, window, cx| {
                entity
                    .update(cx, |list, cx| {
                        list.pick_snippet(target.clone(), text, window, cx);
                    })
                    .ok();
            });
            handler
        });
        // 按住修饰键时链接、邮箱卡片的正文可点（1.x `isLinkActive = isModifierPressed && !selecting`）。
        let on_link = (self.key_hints && !selecting).then(|| {
            let entity = cx.entity().downgrade();
            let target = item.clone();
            let handler: LinkHandler = Rc::new(move |window, cx| {
                entity
                    .update(cx, |list, cx| list.open_link(&target, window, cx))
                    .ok();
            });
            handler
        });
        let after_highlight = index
            .checked_sub(1)
            .and_then(|previous| {
                let prior = self.model.get(previous)?;
                Some(card::is_highlighted(
                    self.controller.is_active(previous, &prior.id),
                    self.hovered.as_ref() == Some(&prior.id),
                    selecting && self.selection.is_checked(&prior.id),
                ))
            })
            .unwrap_or(false);
        let state = CardState {
            active,
            hovered,
            image,
            app_icon,
            file_icons,
            show_original: hovered && self.settings.clipboard.content.show_original_preview,
            hint,
            checked: selecting && self.selection.is_checked(&item.id),
            after_highlight,
            actions,
            checkbox,
            on_snippet,
            on_link,
            position: index + 1,
            set_size: self.model.total(),
            dragged: self
                .reorder
                .as_ref()
                .is_some_and(|drag| drag.active && drag.id == item.id),
            lifted: false,
        };
        let id = item.id.clone();
        let pressed = item.clone();
        let clicked = item.clone();
        let middle = self.settings.clipboard.content.middle_click != MiddleClickAction::Disabled
            || selecting;
        let anchor = self
            .wants_anchor(&item.id, active)
            .then(|| previewing::anchor_canvas(item.id.clone(), self.previewing.anchors.clone()));

        let reorder_geometry = self.reorder_geometry.clone();
        let reorder_id = item.id.clone();
        let reorder_section = item
            .is_pinned
            .then_some(ReorderSection::Pinned)
            .or_else(|| {
                (item.is_favorite
                    && !item.is_pinned
                    && self.controller.filter().range
                        == crate::clipboard::model::filter::Range::Favorite)
                    .then_some(ReorderSection::Favorite)
            });
        let record_reorder = reorder_section.is_some();
        let reorder_canvas = canvas(
            move |bounds, window, _| {
                let mut geometry = reorder_geometry.borrow_mut();
                if record_reorder {
                    geometry
                        .cards
                        .retain(|(id, _, _)| id.as_ref() != reorder_id.as_ref());
                    if let Some(section) = reorder_section {
                        geometry.cards.push((reorder_id.clone(), section, bounds));
                    }
                    if !geometry.repaint_requested {
                        geometry.repaint_requested = true;
                        window.request_animation_frame();
                    }
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        card::card(env, &item, index, state)
            .children(anchor)
            .child(reorder_canvas)
            .on_hover(cx.listener(move |list, hovered: &bool, _, cx| {
                list.hover_card(&id, *hovered, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let pressed = pressed.clone();
                    move |list, event: &MouseDownEvent, window, cx| {
                        list.press_card(pressed.clone(), event, window, cx);
                    }
                }),
            )
            .when(middle, |card| {
                card.on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |list, event: &MouseDownEvent, window, cx| {
                        list.press_card(pressed.clone(), event, window, cx);
                    }),
                )
            })
            .on_click(cx.listener(move |list, event: &ClickEvent, window, cx| {
                list.click_card(clicked.clone(), event.click_count(), window, cx);
            }))
            .into_any_element()
    }

    fn render_list_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = theme::semantic(cx);
        let layout = self.layout;
        let env = CardEnv {
            tokens,
            reorder_source_opacity: theme::components(cx).reorder.source_opacity,
            layout: &layout,
            now: self.now,
            reduce_motion: self.reduce_motion(cx),
        };
        let index = ix;
        let placeholder = self.model.get(index).is_none();
        if placeholder {
            self.placeholders.borrow_mut().insert(ix);
        }
        let element = self.render_row(index, &env, window, cx);

        if let Some(timing) = &self.timing {
            let mut timing = timing.borrow_mut();
            timing.items_rendered += 1;
            timing.placeholders_rendered += u32::from(placeholder);
        }
        element
    }
}

/// 指数缓动的一步；不足 1 px 时直接走完，免得尾巴拖很久。
fn eased_step(remaining: f32, fraction: f32) -> f32 {
    let step = remaining * fraction;
    if step.abs() < 1. {
        remaining.signum() * remaining.abs().min(1.)
    } else {
        step
    }
}

impl Render for ClipboardList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_start = Instant::now();
        self.rem = window.rem_size();
        // 时间标签每帧按当前本地时间算（附录 D §8 第 4 条）；一帧只取一次时钟。
        self.now = Local::now();
        if let Some(timing) = &self.timing {
            *timing.borrow_mut() = FrameTiming {
                render_start: Some(render_start),
                ..FrameTiming::default()
            };
        }
        if let Some(bench) = self.bench.clone() {
            Bench::before_frame(&bench, self, render_start, window, cx);
        }

        self.consume_snapshot(cx);
        if let Some(id) = self.pending_drag_out.take() {
            self.drag_out(id, window, cx);
        }
        if let Some(position) = self.pending_reorder_release.take()
            && self.reorder.as_ref().is_some_and(|drag| drag.active)
        {
            if self.reorder_area_contains(position) {
                self.commit_reorder(window, cx);
            } else {
                self.cancel_reorder(cx);
            }
        }
        self.placeholders.borrow_mut().clear();
        // 卡片位置每帧重记，滚出视口的卡片不留旧位置。
        self.previewing.anchors.borrow_mut().clear();
        let painted_reorder_geometry = self.reorder_geometry.borrow().clone();
        {
            let mut geometry = self.reorder_geometry.borrow_mut();
            geometry.cards.clear();
            geometry.ghost = None;
            geometry.indicator = None;
        }
        self.apply_hints();
        self.run_motion(window, cx);
        self.controller
            .set_first_visible(self.state.logical_scroll_top().item_ix);
        self.consume_reload_at_top(cx);

        let tokens = theme::semantic(cx);
        let layout = self.layout;
        let env = CardEnv {
            tokens,
            reorder_source_opacity: theme::components(cx).reorder.source_opacity,
            layout: &layout,
            now: self.now,
            reduce_motion: self.reduce_motion(cx),
        };

        let empty = self.model.loaded_initial() && self.model.total() == 0;
        let content: AnyElement = if empty {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .window_drag_area()
                .child(self.render_empty(cx))
                .into_any_element()
        } else {
            let list_element = list(
                self.state.clone(),
                cx.processor(|list, ix, window, cx| list.render_list_item(ix, window, cx)),
            )
            .size_full()
            .into_any_element();
            let entity = cx.entity().downgrade();
            let wheel = canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx| {
                        if phase != DispatchPhase::Capture || !bounds.contains(&event.position) {
                            return;
                        }
                        // 只接管鼠标滚轮的“行”增量；触控板的像素增量（自带惯性）交给列表自己处理。
                        let ScrollDelta::Lines(lines) = event.delta else {
                            return;
                        };
                        if let Some(entity) = entity.upgrade() {
                            entity.update(cx, |list, cx| list.on_wheel_lines(lines.y, cx));
                            cx.stop_propagation();
                        }
                    });
                },
            )
            // 不写 top_0 / left_0 时拿到的 bounds 不对，捕获会一直失败（r2-02 §3.6）。
            .absolute()
            .top_0()
            .left_0()
            .size_full();

            div()
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_hidden()
                .pr(px(self.extra_right))
                .child(ListFrame::new(
                    list_element,
                    self.state.clone(),
                    self.snapshot.clone(),
                    self.timing.clone(),
                    (self.rows.count > 0).then(|| WidthHint {
                        width: self.hints.width.clone(),
                        estimate: px(self.height_estimate()),
                    }),
                ))
                .child(wheel)
                .child(ListScrollbar::new("clipboard-list-scrollbar", &self.state))
                .into_any_element()
        };

        let reorder_overlay: Vec<AnyElement> = self
            .reorder
            .clone()
            .filter(|drag| drag.active)
            .and_then(|drag| {
                let item = drag.item.clone();
                let (root, area, (source, mut section_cards, mut all_section_cards)) = {
                    let geometry = &painted_reorder_geometry;
                    let root = geometry.root?;
                    let area = geometry.area?;
                    let source = geometry
                        .cards
                        .iter()
                        .find(|(id, _, _)| id.as_ref() == drag.id.as_ref())
                        .map(|(_, _, bounds)| *bounds)
                        .unwrap_or(drag.source_bounds);
                    let cards: Vec<_> = geometry
                        .cards
                        .iter()
                        .filter(|(_, section, _)| *section == drag.section)
                        .filter(|(id, _, _)| id.as_ref() != drag.id.as_ref())
                        .cloned()
                        .collect();
                    let all_cards: Vec<_> = geometry
                        .cards
                        .iter()
                        .filter(|(_, section, _)| *section == drag.section)
                        .cloned()
                        .collect();
                    (root, area, (source, cards, all_cards))
                };
                section_cards.sort_by_key(|left| left.2.origin.y);
                all_section_cards.sort_by_key(|left| left.2.origin.y);
                let ghost_top = (drag.pointer.y - drag.grab_offset)
                    .max(area.origin.y)
                    .min((area.bottom() - source.size.height).max(area.origin.y));
                let local_x = source.origin.x - root.origin.x;
                let local_y = ghost_top - root.origin.y;
                let image = self.visual(&item, window, cx);
                let (app_icon, file_icons) = self.card_icons(&item, window, cx);
                let ghost = card::card(
                    &env,
                    &item,
                    self.model.index_of(&item.id).unwrap_or_default(),
                    CardState {
                        active: false,
                        hovered: false,
                        image,
                        app_icon,
                        file_icons,
                        show_original: false,
                        hint: None,
                        checked: false,
                        after_highlight: false,
                        actions: None,
                        checkbox: None,
                        on_snippet: None,
                        on_link: None,
                        position: 1,
                        set_size: 1,
                        dragged: false,
                        lifted: true,
                    },
                )
                .absolute()
                .left(local_x)
                .top(local_y)
                .w(source.size.width)
                .h(source.size.height)
                .opacity(theme::components(cx).reorder.ghost_opacity)
                .into_any_element();
                /*
                 * The source is excluded above. Walking the painted bounds gives
                 * the same slot in card and seamless styles, including variable
                 * image/text heights.
                 */
                let anchor_index = drag
                    .anchor
                    .as_ref()
                    .and_then(|anchor| section_cards.iter().position(|(id, _, _)| id == anchor));
                let indicator_y = anchor_index
                    .and_then(|index| {
                        let anchor_id = section_cards.get(index)?.0.clone();
                        let all_index = all_section_cards
                            .iter()
                            .position(|(id, _, _)| id == &anchor_id)?;
                        let anchor_bounds = all_section_cards.get(all_index)?.2;
                        if drag.after {
                            Some(
                                all_section_cards
                                    .get(all_index + 1)
                                    .map(|(_, _, next)| {
                                        (anchor_bounds.bottom() + next.origin.y) / 2.
                                    })
                                    .unwrap_or(anchor_bounds.bottom()),
                            )
                        } else {
                            Some(
                                all_section_cards
                                    .get(all_index.checked_sub(1).unwrap_or(usize::MAX))
                                    .map(|(_, _, previous)| {
                                        (previous.bottom() + anchor_bounds.origin.y) / 2.
                                    })
                                    .unwrap_or(anchor_bounds.origin.y),
                            )
                        }
                    })
                    .unwrap_or(ghost_top);
                let indicator_card = anchor_index
                    .and_then(|index| {
                        section_cards.get(index).and_then(|(id, _, _)| {
                            all_section_cards
                                .iter()
                                .find(|(candidate, _, _)| candidate == id)
                                .map(|(_, _, bounds)| *bounds)
                        })
                    })
                    .or_else(|| section_cards.last().map(|(_, _, bounds)| *bounds))
                    .unwrap_or(source);
                let left_inset = px_rems(
                    self.layout.item_padding_x
                        + self.layout.card_padding_x
                        + if self.layout.header_row {
                            0.
                        } else {
                            16. + self.layout.body_gap
                        },
                )
                .to_pixels(self.rem);
                let right_inset = px_rems(self.layout.item_padding_x + self.layout.card_padding_x)
                    .to_pixels(self.rem);
                let indicator_left = indicator_card.origin.x + left_inset;
                let indicator_width =
                    (indicator_card.size.width - left_inset - right_inset).max(px(0.));
                let indicator_y = indicator_y.max(area.top()).min(area.bottom() - px(2.));
                let indicator_local_y = indicator_y - root.origin.y;
                let indicator = div()
                    .absolute()
                    .left(indicator_left - root.origin.x)
                    .top(indicator_local_y)
                    .w(indicator_width)
                    .h(px(2.))
                    .bg(tokens.accent.solid)
                    .into_any_element();
                let mut geometry = self.reorder_geometry.borrow_mut();
                geometry.ghost = Some(Bounds {
                    origin: point(source.origin.x, ghost_top),
                    size: source.size,
                });
                geometry.indicator = Some(Bounds {
                    origin: point(indicator_left, indicator_y),
                    size: gpui::size(indicator_width, px(2.)),
                });
                Some(vec![ghost, indicator])
            })
            .unwrap_or_default();

        let geometry = self.reorder_geometry.clone();
        let event_entity = cx.entity().downgrade();
        let root_bounds = canvas(
            move |bounds, _, _| {
                geometry.borrow_mut().root = Some(bounds);
            },
            move |_, _, window, _| {
                let move_entity = event_entity.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    if let Some(entity) = move_entity.upgrade() {
                        entity.update(cx, |list, cx| {
                            let Some(drag) = list.reorder.as_ref() else {
                                return;
                            };
                            let outside = event.position.x < px(0.)
                                || event.position.y < px(0.)
                                || event.position.x > window.bounds().size.width
                                || event.position.y > window.bounds().size.height;
                            let crossed = list.drag.crossed_threshold(event, window);
                            if event.pressed_button != Some(MouseButton::Left) {
                                list.reorder = None;
                                list.armed_click = None;
                                list.drag.release();
                                list.reorder_scroll_task = None;
                                cx.notify();
                            } else if (drag.active || crossed) && outside {
                                let id = drag.id.clone();
                                list.reorder = None;
                                list.armed_click = None;
                                list.drag.release();
                                list.reorder_scroll_task = None;
                                list.pending_drag_out = Some(id);
                                cx.notify();
                            }
                        });
                    }
                });
                let up_entity = event_entity.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                        return;
                    }
                    if let Some(entity) = up_entity.upgrade() {
                        entity.update(cx, |list, cx| {
                            if list.reorder.as_ref().is_some_and(|drag| drag.active) {
                                list.pending_reorder_release = Some(event.position);
                                cx.notify();
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let area_geometry = self.reorder_geometry.clone();
        let area_bounds = canvas(
            move |bounds, _, _| {
                area_geometry.borrow_mut().area = Some(bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let list_area = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(content)
            .child(area_bounds);

        let root = div()
            .id("clipboard-list")
            .role(Role::ListBox)
            .aria_label(crate::i18n::t("clipboard:accessibility.list"))
            .track_focus(&self.focus)
            .on_mouse_move(cx.listener(|list, event: &MouseMoveEvent, window, cx| {
                if let Some(reorder) = list.reorder.as_ref()
                    && (reorder.active || list.drag.crossed_threshold(event, window))
                {
                    if let Some(reorder) = list.reorder.as_mut() {
                        reorder.active = true;
                    }
                    list.armed_click = None;
                    list.close_preview(cx);
                    list.start_reorder_autoscroll(window, cx);
                    list.update_reorder_target(event.position, window, cx);
                    return;
                }
                list.pointer_moved(event.position, cx);
                // 在卡片上按住左键拖过系统阈值：拖出这条记录。
                if let Some(id) = list.drag.moved(event, window) {
                    list.armed_click = None;
                    list.drag_out(Arc::from(id), window, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|list, event: &MouseUpEvent, window, cx| {
                    if list.reorder.as_ref().is_some_and(|drag| drag.active) {
                        if list.reorder_area_contains(event.position) {
                            list.commit_reorder(window, cx);
                        } else {
                            list.cancel_reorder(cx);
                        }
                    } else {
                        list.reorder = None;
                        list.drag.release();
                    }
                }),
            )
            // 按住空格预览当前项（钩子转来的空格按下、松开）。
            .on_key_down(cx.listener(|list, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && list.reorder.is_some() {
                    list.cancel_reorder(cx);
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "space" && !event.keystroke.modifiers.modified() {
                    list.preview_space(true, cx);
                    cx.stop_propagation();
                }
            }))
            .on_key_up(cx.listener(|list, event: &KeyUpEvent, _, cx| {
                if event.keystroke.key == "space" {
                    list.preview_space(false, cx);
                    cx.stop_propagation();
                }
            }))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .text_color(tokens.text.primary)
            .child(root_bounds)
            .child(list_area)
            .children(reorder_overlay)
            .child(self.render_footer(cx));
        let entity = cx.entity().downgrade();
        let root = context_menu(root, move |_, cx| {
            entity
                .update(cx, |list, cx| list.context_menu_entries(cx))
                .unwrap_or_default()
        });

        match &self.timing {
            Some(timing) => {
                let bench = self.bench.clone();
                let on_painted: Option<PaintedCallback> = bench.map(|bench| {
                    let callback: PaintedCallback = Box::new(move |timing| {
                        bench.borrow_mut().after_paint(timing);
                    });
                    callback
                });
                FrameTimer::new(root, timing.clone(), on_painted).into_any_element()
            }
            None => root,
        }
    }
}
