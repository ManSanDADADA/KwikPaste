//! 列表跑分（`KWIKPASTE_SELFTEST=1 KwikPaste --selftest-list-bench`）：附录 D §3.7 前四项门槛与 r2-02 锚定场景。
//!
//! 用合成的 1 万行夹具，在真正的面板里按时间表执行：
//! - 以 6000 px/s 下滚、3000 px/s 上滚、跳到冷区后上滚、冷区下滚，每帧记 render→paint 耗时；
//! - 静止 3 s 数帧（目标 0 帧/s）；隐藏后重新显示 5 次，量触发到首帧画完的时间；
//! - 锚定：删视口上方的行、删可见行、删滚动顶部行、上方行变高、顶部行变高、跳到未加载区、
//!   不在顶部时来新记录（挂起，滚回顶部那一帧消费）、在顶部时来新记录、改列表宽度；
//! - 拖滚动条到 50% / 100% 的落点；方向键平滑露出的耗时。
//!
//! 每帧一行 CSV、事件日志和汇总写到 `--selftest-out <目录>`（默认 `<临时目录>/kwikpaste-list-bench`），
//! 汇总里每个门槛一行 `GATE`；任一门槛不过时以退出码 3 结束。

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{AnyWindowHandle, App, Context, Entity, ListOffset, Window, point, px};

use super::{
    frame::{FrameTiming, Snapshot},
    list::ClipboardList,
};
use crate::{
    clipboard::{
        model::{
            controller::{ListUpdate, Nav},
            item::{ItemKind, ListItem},
        },
        source::{
            FixtureStore,
            synthetic::{self, AssetSet, GenerateOptions},
        },
    },
    platform::{self, Panel, PanelCommand, Trigger, TriggerSource},
};

/// 下滚速度（附录 D §3.7）。
const SPEED: f32 = 6000.;

/// 时间表上的一步。
#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Show,
    Hide,
    Drive {
        phase: &'static str,
        speed: f32,
        seconds: f32,
    },
    Idle,
    IdleMeasure {
        seconds: f32,
    },
    JumpFar,
    Goto {
        row: usize,
        offset: f32,
    },
    InsertDeferred,
    RemoveAbove,
    RemoveVisible,
    RemoveScrollTop,
    GrowAbove,
    GrowScrollTop,
    JumpCold,
    JumpColdAfter,
    ReuseScrollTop,
    ReuseScrollTopAfter,
    ScrollbarReport,
    Widen,
    WidenAfter,
    Drag {
        fraction: f32,
    },
    DragRelease,
    Arrow,
    WheelToTop,
    InsertAtTop,
    TopReport,
    Finish,
}

/// 一次锚定检查：记下操作前的位置，在操作后的第一帧比较。
struct Check {
    label: String,
    after_seq: u64,
    before: Vec<(Arc<str>, f32)>,
    /// 必须保持不动的行（空表示前后都可见的全部行）。
    stable: Option<Vec<Arc<str>>>,
    gated: bool,
}

/// 一个滚动阶段的帧耗时（微秒）与跳动统计。
#[derive(Default)]
struct PhaseStats {
    spans: Vec<f64>,
    jumps: u32,
    max_jump: f32,
    placeholder_frames: u32,
}

/// 一段持续滚动。
struct Drive {
    started: Instant,
    speed: f32,
    seconds: f32,
    done: f32,
}

pub struct Bench {
    out_dir: PathBuf,
    t0: Instant,
    store: Arc<Mutex<FixtureStore>>,
    assets: AssetSet,
    next_index: usize,
    snapshot: Rc<RefCell<Snapshot>>,
    phase: &'static str,
    frame: u64,
    seen_seq: u64,
    discontinuity: bool,
    previous: Vec<(Arc<str>, f32)>,
    last_timing: FrameTiming,
    last_render: Option<Instant>,
    drive: Option<Drive>,
    checks: Vec<Check>,
    csv: Vec<String>,
    log: Vec<String>,
    phases: HashMap<&'static str, PhaseStats>,
    /// 每个锚定检查的结果：（名称，最大位移，是否计入门槛）。
    anchors: Vec<(String, f32, bool)>,
    /// 显示请求发出的时刻；首帧画完时算延迟。
    show_requested: Option<Instant>,
    show_latencies: Vec<f64>,
    idle_fps: Option<f64>,
    drags: Vec<(f32, f64)>,
    reveal_started: Option<(Instant, usize)>,
    reveal_ms: Vec<f64>,
    widen_before: Option<f32>,
    width_result: Option<(f32, f32)>,
    jump_target: Option<usize>,
    reuse_top: Option<usize>,
    deferred_ids: Vec<Arc<str>>,
    top_checks: Vec<(String, bool)>,
}

impl Bench {
    fn elapsed_ms(&self) -> f64 {
        self.t0.elapsed().as_secs_f64() * 1000.
    }

    fn note(&mut self, line: impl AsRef<str>) {
        let line = format!("[{:9.1} ms] {}", self.elapsed_ms(), line.as_ref());
        log::info!("list bench: {}", line);
        self.log.push(line);
    }

    /// 跑分驱动的滚动这一帧要走多少 px。
    pub(super) fn drive_step(&mut self, now: Instant) -> Option<f32> {
        let drive = self.drive.as_mut()?;
        let elapsed = (now - drive.started).as_secs_f32().min(drive.seconds);
        let want = drive.speed * elapsed;
        let step = want - drive.done;
        drive.done = want;
        if elapsed >= drive.seconds {
            self.drive = None;
            self.phase = "after-drive";
        }
        Some(step)
    }

    /// 每帧 render 开头：结算上一帧（CSV 一行、跳动、检查）。
    pub(super) fn before_frame(
        bench: &Rc<RefCell<Self>>,
        list: &mut ClipboardList,
        now: Instant,
        window: &mut Window,
        _cx: &mut Context<ClipboardList>,
    ) {
        let mut bench = bench.borrow_mut();
        // 有检查或方向键计时在等结果时持续出帧，免得静止时一直等到下一个步骤。
        if !bench.checks.is_empty() || bench.reveal_started.is_some() {
            window.request_animation_frame();
        }
        let snapshot = bench.snapshot.borrow().clone();
        let fresh = snapshot.seq != bench.seen_seq;
        let dt = bench
            .last_render
            .map(|last| (now - last).as_secs_f64() * 1000.)
            .unwrap_or_default();
        bench.last_render = Some(now);
        bench.frame += 1;
        if !fresh {
            return;
        }
        bench.seen_seq = snapshot.seq;

        let positions: Vec<(Arc<str>, f32)> = snapshot
            .rows
            .iter()
            .filter_map(|row| {
                list.model
                    .get(row.ix)
                    .map(|item| (item.id.clone(), row.top))
            })
            .collect();
        let placeholders = snapshot.rows.len() - positions.len();

        // 跳动：同一行这一帧的位置 − 上一帧的位置 + 本视图主动滚过的距离，应为 0。
        let applied = list.motion.applied;
        let (mut top_jump, mut max_jump) = (f32::NAN, 0f32);
        if !bench.discontinuity && !bench.previous.is_empty() {
            for (id, before) in &bench.previous {
                if let Some((_, now_y)) = positions.iter().find(|(other, _)| other == id) {
                    let jump = now_y - before + applied;
                    if top_jump.is_nan() && *before >= 0. {
                        top_jump = jump;
                    }
                    max_jump = max_jump.max(jump.abs());
                }
            }
        }
        bench.discontinuity = false;

        let phase = bench.phase;
        let timing = bench.last_timing;
        {
            let stats = bench.phases.entry(phase).or_default();
            stats.spans.push(timing.span_us);
            if top_jump.abs() > 0.5 {
                stats.jumps += 1;
            }
            stats.max_jump = stats.max_jump.max(if top_jump.is_nan() {
                0.
            } else {
                top_jump.abs()
            });
            if placeholders > 0 {
                stats.placeholder_frames += 1;
            }
        }
        let row = format!(
            "{},{:.1},{:.2},{},{},{:.1},{:.1},{:.1},{},{},{},{:.1},{:.1},{:.1},{:.2},{:.2},{:.2},{}",
            bench.frame,
            bench.elapsed_ms(),
            dt,
            phase,
            snapshot.top.0,
            snapshot.top.1,
            snapshot.scroll_px,
            snapshot.content_height,
            snapshot.rows.len(),
            placeholders,
            timing.items_rendered,
            timing.list_prepaint_us,
            timing.list_paint_us,
            timing.span_us,
            applied,
            top_jump,
            max_jump,
            list.model.cached_rows(),
        );
        bench.csv.push(row);

        // 锚定检查：操作后的第一帧。
        let due: Vec<Check> = {
            let seq = snapshot.seq;
            let (due, waiting): (Vec<Check>, Vec<Check>) = std::mem::take(&mut bench.checks)
                .into_iter()
                .partition(|check| check.after_seq <= seq);
            bench.checks = waiting;
            due
        };
        for check in due {
            let mut worst = 0f32;
            let mut common = 0;
            for (id, before) in &check.before {
                let stable = check
                    .stable
                    .as_ref()
                    .is_none_or(|stable| stable.iter().any(|other| other == id));
                if !stable {
                    continue;
                }
                if let Some((_, after)) = positions.iter().find(|(other, _)| other == id) {
                    common += 1;
                    worst = worst.max((after - before).abs());
                }
            }
            let line = format!(
                "CHECK {}: {common} rows compared, max displacement {worst:.2} px; logical top now ({}, {:.1})",
                check.label, snapshot.top.0, snapshot.top.1
            );
            bench.note(line);
            bench
                .anchors
                .push((check.label, worst, check.gated && common > 0));
        }

        // 方向键：选中行完整露出的时刻。
        if let Some((started, ix)) = bench.reveal_started
            && snapshot.fully_visible(ix)
            && list.motion_idle()
        {
            let ms = (now - started).as_secs_f64() * 1000.;
            bench.reveal_ms.push(ms);
            bench.reveal_started = None;
        }

        bench.previous = positions;
    }

    /// 整帧画完（render→paint 计时已出）。
    pub(super) fn after_paint(&mut self, timing: &FrameTiming) {
        self.last_timing = *timing;
        if let Some(requested) = self.show_requested.take() {
            let ms = requested.elapsed().as_secs_f64() * 1000.;
            self.show_latencies.push(ms);
            self.note(format!("first frame after show request: {ms:.2} ms"));
        }
    }

    fn check(
        &mut self,
        label: &str,
        list: &ClipboardList,
        stable: Option<Vec<Arc<str>>>,
        gated: bool,
    ) {
        self.checks.push(Check {
            label: label.to_owned(),
            after_seq: list.snapshot.borrow().seq + 1,
            before: self.previous.clone(),
            stable,
            gated,
        });
        self.discontinuity = true;
    }
}

/// 跑分的输出目录：`--selftest-out <目录>`，默认 `<临时目录>/kwikpaste-list-bench`。
fn out_dir() -> PathBuf {
    let mut args = kwikpaste_os::args();
    while let Some(arg) = args.next() {
        if arg == "--selftest-out"
            && let Some(dir) = args.next()
        {
            return PathBuf::from(dir);
        }
    }
    std::env::temp_dir().join("kwikpaste-list-bench")
}

fn schedule() -> Vec<(f32, Step)> {
    let mut steps = vec![
        (1.0, Step::Show),
        (
            2.0,
            Step::Drive {
                phase: "down",
                speed: SPEED,
                seconds: 5.,
            },
        ),
        (7.5, Step::Idle),
        (
            8.0,
            Step::Drive {
                phase: "up",
                speed: -SPEED / 2.,
                seconds: 5.,
            },
        ),
        (13.5, Step::Idle),
        (14.0, Step::JumpFar),
        (
            15.0,
            Step::Drive {
                phase: "up-cold",
                speed: -SPEED / 2.,
                seconds: 3.,
            },
        ),
        (18.5, Step::Idle),
        (
            19.0,
            Step::Drive {
                phase: "down-cold",
                speed: SPEED,
                seconds: 3.,
            },
        ),
        (22.5, Step::Idle),
        // 滚动条停 2 s 后用 0.5 s 淡出；从 26 s 开始数 3 s。
        (26.0, Step::IdleMeasure { seconds: 3. }),
    ];
    let mut at = 30.0;
    for _ in 0..5 {
        steps.push((at, Step::Hide));
        steps.push((at + 0.4, Step::Show));
        at += 0.8;
    }
    steps.extend([
        (
            35.0,
            Step::Goto {
                row: 300,
                offset: 17.,
            },
        ),
        (36.0, Step::RemoveAbove),
        (36.5, Step::RemoveVisible),
        (37.0, Step::RemoveScrollTop),
        (
            37.5,
            Step::Goto {
                row: 320,
                offset: 0.,
            },
        ),
        (38.0, Step::GrowAbove),
        (
            38.4,
            Step::Goto {
                row: 330,
                offset: 12.,
            },
        ),
        (38.8, Step::GrowScrollTop),
        (39.3, Step::JumpCold),
        (39.32, Step::ScrollbarReport),
        (39.9, Step::JumpColdAfter),
        (40.2, Step::ScrollbarReport),
        (40.5, Step::Widen),
        (40.8, Step::WidenAfter),
        (41.0, Step::Drag { fraction: 0.5 }),
        (41.3, Step::DragRelease),
        (41.6, Step::Drag { fraction: 1. }),
        (41.9, Step::DragRelease),
        (42.2, Step::Goto { row: 0, offset: 0. }),
    ]);
    let mut at = 42.8;
    for _ in 0..8 {
        steps.push((at, Step::Arrow));
        at += 0.25;
    }
    steps.extend([
        // 离顶部不到一个预取半径：刷新范围含第 0 行，复用的卡片落地后仍在缓存里。
        (
            44.8,
            Step::Goto {
                row: 12,
                offset: 10.,
            },
        ),
        (45.0, Step::ReuseScrollTop),
        (45.3, Step::ReuseScrollTopAfter),
        (
            45.5,
            Step::Goto {
                row: 40,
                offset: 0.,
            },
        ),
        (46.0, Step::InsertDeferred),
        (46.3, Step::WheelToTop),
        (49.0, Step::TopReport),
        (49.5, Step::InsertAtTop),
        (50.0, Step::TopReport),
        (50.5, Step::Finish),
    ]);
    steps
}

/// 开始跑分：`list` 已经接上面板，`store` 是它的夹具后端。
pub fn start(
    list: &Entity<ClipboardList>,
    store: Arc<Mutex<FixtureStore>>,
    assets: AssetSet,
    rows: usize,
    window: AnyWindowHandle,
    cx: &mut App,
) {
    let snapshot = list.read(cx).snapshot.clone();
    let bench = Rc::new(RefCell::new(Bench {
        out_dir: out_dir(),
        t0: Instant::now(),
        store,
        assets,
        next_index: rows,
        snapshot,
        phase: "init",
        frame: 0,
        seen_seq: 0,
        discontinuity: true,
        previous: Vec::new(),
        last_timing: FrameTiming::default(),
        last_render: None,
        drive: None,
        checks: Vec::new(),
        csv: vec![
            "frame,t_ms,dt_ms,phase,top_ix,top_off,scroll_px,content_h,rows,placeholders,items_rendered,prepaint_us,paint_us,span_us,applied,top_jump,max_jump,cache_rows".to_owned(),
        ],
        log: Vec::new(),
        phases: HashMap::new(),
        anchors: Vec::new(),
        show_requested: None,
        show_latencies: Vec::new(),
        idle_fps: None,
        drags: Vec::new(),
        reveal_started: None,
        reveal_ms: Vec::new(),
        widen_before: None,
        width_result: None,
        jump_target: None,
        reuse_top: None,
        deferred_ids: Vec::new(),
        top_checks: Vec::new(),
    }));
    list.update(cx, |list, _| list.enable_bench(bench.clone()));

    let list = list.clone();
    cx.spawn(async move |cx| {
        let started = Instant::now();
        for (at, step) in schedule() {
            let wait = Duration::from_secs_f32(at).saturating_sub(started.elapsed());
            cx.background_executor().timer(wait).await;
            let result = window.update(cx, |_, _, cx| {
                list.update(cx, |list, cx| run_step(&bench, list, step, cx))
            });
            if let Err(err) = result {
                log::error!("list bench step {step:?} failed: {err:#}");
            }
            if let Step::IdleMeasure { seconds } = step {
                let frames_before = platform::rendered_frames();
                cx.background_executor()
                    .timer(Duration::from_secs_f32(seconds))
                    .await;
                let frames = platform::rendered_frames().saturating_sub(frames_before);
                let mut bench = bench.borrow_mut();
                let fps = frames as f64 / f64::from(seconds);
                bench.idle_fps = Some(fps);
                bench.note(format!(
                    "idle: {frames} frames in {seconds} s ({fps:.2} fps)"
                ));
            }
        }
    })
    .detach();
}

fn run_step(
    bench: &Rc<RefCell<Bench>>,
    list: &mut ClipboardList,
    step: Step,
    cx: &mut Context<ClipboardList>,
) {
    bench.borrow_mut().note(format!("STEP {step:?}"));
    match step {
        Step::Show => {
            bench.borrow_mut().show_requested = Some(Instant::now());
            if let Some(panel) = cx.try_global::<Panel>() {
                panel.request(PanelCommand::Show(Trigger::now(TriggerSource::Selftest)));
            }
        }
        Step::Hide => {
            if let Some(panel) = cx.try_global::<Panel>() {
                panel.request(PanelCommand::Hide(Trigger::now(TriggerSource::Selftest)));
            }
        }
        Step::Drive {
            phase,
            speed,
            seconds,
        } => {
            let mut bench = bench.borrow_mut();
            bench.phase = phase;
            bench.drive = Some(Drive {
                started: Instant::now(),
                speed,
                seconds,
                done: 0.,
            });
        }
        Step::Idle | Step::IdleMeasure { .. } => {
            bench.borrow_mut().phase = "idle";
        }
        Step::JumpFar => {
            let row = list.total() / 2;
            list.state.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.),
            });
            let mut bench = bench.borrow_mut();
            bench.phase = "far";
            bench.discontinuity = true;
        }
        Step::Goto { row, offset } => {
            list.state.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(offset),
            });
            let mut bench = bench.borrow_mut();
            bench.phase = "anchors";
            bench.discontinuity = true;
        }
        Step::InsertDeferred => {
            let item = new_item(bench);
            let id = item.id.clone();
            if let Ok(mut store) = bench.borrow().store.lock() {
                store.insert_newest(item);
            }
            bench.borrow_mut().deferred_ids.push(id.clone());
            bench
                .borrow_mut()
                .check("A1 new item while scrolled (deferred)", list, None, true);
            list.on_update(
                ListUpdate::Upserted {
                    id,
                    kind: ItemKind::Text,
                    deduplicated: false,
                    group_id: None,
                },
                cx,
            );
        }
        Step::RemoveAbove => {
            let top = list.state.logical_scroll_top().item_ix;
            let Some(id) = list
                .model
                .get(top.saturating_sub(5))
                .map(|item| item.id.clone())
            else {
                bench.borrow_mut().note("A2 skipped: row not loaded");
                return;
            };
            remove_from_store(bench, &id);
            bench
                .borrow_mut()
                .check("A2 remove a row above the viewport", list, None, true);
            list.remove_item(&id, cx);
        }
        Step::RemoveVisible => {
            let top = list.state.logical_scroll_top().item_ix;
            let target = top + 3;
            let Some(id) = list.model.get(target).map(|item| item.id.clone()) else {
                bench.borrow_mut().note("A3 skipped: row not loaded");
                return;
            };
            let above: Vec<Arc<str>> = (top..target)
                .filter_map(|ix| list.model.get(ix).map(|item| item.id.clone()))
                .collect();
            remove_from_store(bench, &id);
            bench.borrow_mut().check(
                "A3 remove a visible row (rows above it)",
                list,
                Some(above),
                true,
            );
            list.remove_item(&id, cx);
        }
        Step::RemoveScrollTop => {
            let top = list.state.logical_scroll_top();
            let Some(id) = list.model.get(top.item_ix).map(|item| item.id.clone()) else {
                bench.borrow_mut().note("A4 skipped: row not loaded");
                return;
            };
            remove_from_store(bench, &id);
            let label = format!(
                "A4 remove the scroll-top row (offset {:.1}; rows below move up by its visible part)",
                top.offset_in_item.as_f32()
            );
            bench.borrow_mut().check(&label, list, None, false);
            list.remove_item(&id, cx);
        }
        Step::GrowAbove => {
            let top = list.state.logical_scroll_top().item_ix;
            let ids: Vec<Arc<str>> = (top.saturating_sub(12)..top.saturating_sub(2))
                .filter_map(|ix| list.model.get(ix).map(|item| item.id.clone()))
                .collect();
            bench
                .borrow_mut()
                .check("A5 ten rows above the viewport grow", list, None, true);
            for id in ids {
                grow(bench, list, &id, cx);
            }
        }
        Step::GrowScrollTop => {
            let top = list.state.logical_scroll_top();
            let Some(id) = list.model.get(top.item_ix).map(|item| item.id.clone()) else {
                bench.borrow_mut().note("A6 skipped: row not loaded");
                return;
            };
            let label = format!(
                "A6 the scroll-top row grows (offset {:.1}; the row itself stays)",
                top.offset_in_item.as_f32()
            );
            bench
                .borrow_mut()
                .check(&label, list, Some(vec![id.clone()]), true);
            grow(bench, list, &id, cx);
        }
        Step::JumpCold => {
            let row = list.total() * 6 / 10;
            list.state.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.),
            });
            let mut bench = bench.borrow_mut();
            bench.jump_target = Some(row);
            bench.discontinuity = true;
        }
        Step::JumpColdAfter => {
            let top = list.state.logical_scroll_top();
            let snapshot = list.snapshot.borrow().clone();
            let target = bench.borrow().jump_target;
            let first = snapshot.rows.first().map(|row| (row.ix, row.top));
            let ok = target == Some(top.item_ix)
                && top.offset_in_item <= px(0.5)
                && first.is_some_and(|(_, y)| y.abs() <= 0.5);
            let mut bench = bench.borrow_mut();
            bench.note(format!(
                "A9 jump to an unloaded row {target:?}: logical top now ({}, {:.1}), first row {first:?}",
                top.item_ix,
                top.offset_in_item.as_f32()
            ));
            bench.anchors.push((
                "A9 jump into unloaded rows keeps the top row at y=0".to_owned(),
                if ok { 0. } else { 1. },
                true,
            ));
        }
        Step::ReuseScrollTop => {
            // 复用顶行：core 把它挪到最前，列表收到去重的 `ClipboardUpserted`（FB-9Z00CH）。
            let top = list.state.logical_scroll_top().item_ix;
            let Some(item) = list.model.get(top).cloned() else {
                bench.borrow_mut().note("A11 skipped: row not loaded");
                return;
            };
            if let Ok(mut store) = bench.borrow().store.lock() {
                store.remove(&item.id);
                store.insert_newest((*item).clone());
            }
            bench.borrow_mut().reuse_top = Some(top);
            list.on_update(
                ListUpdate::Upserted {
                    id: item.id.clone(),
                    kind: item.kind,
                    deduplicated: true,
                    group_id: None,
                },
                cx,
            );
        }
        Step::ReuseScrollTopAfter => {
            let top = list.state.logical_scroll_top();
            let target = bench.borrow().reuse_top;
            let ok = target == Some(top.item_ix);
            let mut bench = bench.borrow_mut();
            bench.note(format!(
                "A11 reuse the scroll-top row {target:?}: logical top now ({}, {:.1})",
                top.item_ix,
                top.offset_in_item.as_f32()
            ));
            bench.anchors.push((
                "A11 reusing the scroll-top row keeps its row index".to_owned(),
                if ok { 0. } else { 1. },
                true,
            ));
        }
        Step::ScrollbarReport => {
            let snapshot = list.snapshot.borrow().clone();
            let max = snapshot.content_height - snapshot.viewport.size.height.as_f32();
            let fraction = if max > 0. {
                snapshot.scroll_px / max
            } else {
                0.
            };
            let true_fraction = snapshot.top.0 as f32 / list.total().max(1) as f32;
            bench.borrow_mut().note(format!(
                "A10 scrollbar: content {:.0} px, thumb at {fraction:.4}, true row fraction {true_fraction:.4} (row {} of {})",
                snapshot.content_height,
                snapshot.top.0,
                list.total()
            ));
        }
        Step::Widen => {
            let content = list.snapshot.borrow().content_height;
            bench.borrow_mut().widen_before = Some(content);
            // 不能调 Window::resize（会激活窗口）：给列表容器加 24 px 右边距，制造一次宽度变化。
            list.extra_right = 24.;
            bench.borrow_mut().discontinuity = true;
        }
        Step::WidenAfter => {
            let content = list.snapshot.borrow().content_height;
            let mut bench = bench.borrow_mut();
            let before = bench.widen_before.unwrap_or_default();
            bench.width_result = Some((before, content));
            bench.note(format!(
                "A15 width change: content height {before:.0} -> {content:.0} px after the hint was re-applied"
            ));
            list.extra_right = 0.;
            bench.discontinuity = true;
        }
        Step::Drag { fraction } => {
            let max = list.state.max_offset_for_scrollbar().y.as_f32();
            list.state.scrollbar_drag_started();
            list.state
                .set_offset_from_scrollbar(point(px(0.), px(-max * fraction)));
            let top = list.state.logical_scroll_top().item_ix;
            let total = list.total().max(1);
            let ideal = (total as f32 * fraction) as usize;
            let error = (top as f64 - ideal as f64).abs() / total as f64 * 100.;
            let mut bench = bench.borrow_mut();
            bench.drags.push((fraction, error));
            bench.discontinuity = true;
            bench.note(format!(
                "A16 drag the thumb to {:.0}%: row {top} (ideal ~{ideal}), error {error:.3}% of {total} rows",
                fraction * 100.
            ));
        }
        Step::DragRelease => {
            list.state.scrollbar_drag_ended();
            bench.borrow_mut().discontinuity = true;
        }
        Step::Arrow => {
            list.navigate(Nav::Down, cx);
            let index = list.controller.active_index(&list.model);
            bench.borrow_mut().reveal_started = Some((Instant::now(), index));
            bench.borrow_mut().phase = "keys";
        }
        Step::WheelToTop => {
            bench.borrow_mut().phase = "wheel";
            // 90 格滚轮，每格 3 行，在 2 s 内送完；平滑滚动把它们摊到多帧。
            let entity = cx.entity();
            cx.spawn(async move |_, cx| {
                for _ in 0..90 {
                    entity.update(cx, |list, cx| list.on_wheel_lines(3., cx));
                    cx.background_executor()
                        .timer(Duration::from_millis(20))
                        .await;
                }
            })
            .detach();
        }
        Step::InsertAtTop => {
            let item = new_item(bench);
            let id = item.id.clone();
            if let Ok(mut store) = bench.borrow().store.lock() {
                store.insert_newest(item);
            }
            bench.borrow_mut().deferred_ids.push(id.clone());
            list.on_update(
                ListUpdate::Upserted {
                    id,
                    kind: ItemKind::Text,
                    deduplicated: false,
                    group_id: None,
                },
                cx,
            );
        }
        Step::TopReport => {
            let top = list.state.logical_scroll_top();
            let first = (0..list.total())
                .filter_map(|index| list.model.get(index))
                .find(|item| !item.is_pinned)
                .map(|item| item.id.clone());
            let newest = bench.borrow().deferred_ids.last().cloned();
            let ok = top.item_ix == 0
                && top.offset_in_item <= px(0.5)
                && first.is_some()
                && first == newest;
            let label = format!(
                "top after new items: logical top ({}, {:.1}), first row {first:?}, newest {newest:?}",
                top.item_ix,
                top.offset_in_item.as_f32()
            );
            let mut bench = bench.borrow_mut();
            bench.note(&label);
            bench.top_checks.push((label, ok));
        }
        Step::Finish => {
            let code = bench.borrow_mut().finish(list, cx);
            if code == 0 {
                cx.quit();
            } else {
                std::process::exit(code);
            }
        }
    }
    // 静止测量期间不能自己触发重绘。
    if !matches!(step, Step::Idle | Step::IdleMeasure { .. }) {
        cx.notify();
    }
}

fn remove_from_store(bench: &Rc<RefCell<Bench>>, id: &str) {
    if let Ok(mut store) = bench.borrow().store.lock() {
        store.remove(id);
    }
}

/// 让一行变高：加便签并多两行摘要（后端和本地缓存一起改，模拟 core 确认后的本地合并）。
fn grow(
    bench: &Rc<RefCell<Bench>>,
    list: &mut ClipboardList,
    id: &str,
    cx: &mut Context<ClipboardList>,
) {
    let patch = |item: &mut ListItem| {
        item.note = Some(Arc::from("新增的便签\n第二行便签"));
    };
    if let Ok(mut store) = bench.borrow().store.lock() {
        store.patch(id, patch);
    }
    list.patch_item(id, patch, cx);
}

fn new_item(bench: &Rc<RefCell<Bench>>) -> ListItem {
    let mut bench = bench.borrow_mut();
    let index = bench.next_index;
    bench.next_index += 1;
    let mut item = synthetic::item(
        &bench.assets,
        index,
        GenerateOptions {
            pinned: 0,
            ..GenerateOptions::default()
        },
    );
    item.id = Arc::from(format!("bench-new-{index}"));
    item
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((p / 100.) * (sorted.len() - 1) as f64).round() as usize;
    sorted
        .get(rank.min(sorted.len() - 1))
        .copied()
        .unwrap_or(f64::NAN)
}

impl Bench {
    /// 写结果，返回退出码（0 全部通过，3 有门槛未过）。
    fn finish(&mut self, list: &ClipboardList, cx: &App) -> i32 {
        let mut summary = Vec::new();
        let mut failed = false;
        let mut gate = |summary: &mut Vec<String>, name: &str, pass: bool, detail: String| {
            if !pass {
                failed = true;
            }
            summary.push(format!(
                "GATE {} {name}: {detail}",
                if pass { "PASS" } else { "FAIL" }
            ));
        };

        let build = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        summary.push(format!(
            "build={build} rows={} cached_rows={} images={:?}",
            list.total(),
            list.model.cached_rows(),
            list.images.read(cx).stats()
        ));

        let scroll_phases = ["down", "up", "up-cold", "down-cold"];
        let mut all = Vec::new();
        for phase in scroll_phases {
            let Some(stats) = self.phases.get(phase) else {
                continue;
            };
            // 每段的第一帧包含启动滚动前的空闲间隔，不计入。
            let spans: Vec<f64> = stats.spans.iter().skip(1).map(|us| us / 1000.).collect();
            summary.push(format!(
                "scroll {phase}: frames={} mean={:.3} ms p95={:.3} ms max={:.3} ms jumps={} max_jump={:.2} px placeholder_frames={}",
                spans.len(),
                mean(&spans),
                percentile(&spans, 95.),
                percentile(&spans, 100.),
                stats.jumps,
                stats.max_jump,
                stats.placeholder_frames
            ));
            all.extend(spans);
        }
        let (all_mean, all_p95) = (mean(&all), percentile(&all, 95.));
        gate(
            &mut summary,
            "scroll frame time at 6000 px/s (mean <= 0.8 ms, p95 <= 1.2 ms)",
            all_mean <= 0.8 && all_p95 <= 1.2,
            format!(
                "mean {all_mean:.3} ms, p95 {all_p95:.3} ms over {} frames ({build})",
                all.len()
            ),
        );

        let fps = self.idle_fps.unwrap_or(f64::NAN);
        gate(
            &mut summary,
            "visible and idle (0 fps)",
            fps <= 0.34,
            format!("{fps:.2} fps"),
        );

        // 第一次显示包含首次布局和字形栅格化，门槛只看隐藏后的重新显示。
        let reshows: Vec<f64> = self.show_latencies.iter().skip(1).copied().collect();
        let worst_show = percentile(&reshows, 100.);
        gate(
            &mut summary,
            "first frame after re-show (<= 16 ms)",
            !reshows.is_empty() && worst_show <= 16.,
            format!(
                "{:?} ms",
                self.show_latencies
                    .iter()
                    .map(|ms| (ms * 100.).round() / 100.)
                    .collect::<Vec<_>>()
            ),
        );

        let half = self
            .drags
            .iter()
            .find(|(fraction, _)| (*fraction - 0.5).abs() < f32::EPSILON)
            .map(|(_, error)| *error)
            .unwrap_or(f64::NAN);
        gate(
            &mut summary,
            "drag the scrollbar to 50% with 10k rows (error <= 1%)",
            half <= 1.,
            format!("{half:.3}% (all drags {:?})", self.drags),
        );

        for (label, worst, gated) in &self.anchors {
            summary.push(format!(
                "anchor {label}: {worst:.2} px{}",
                if *gated { "" } else { " (informational)" }
            ));
        }
        let worst_anchor = self
            .anchors
            .iter()
            .filter(|(_, _, gated)| *gated)
            .map(|(_, worst, _)| *worst)
            .fold(0f32, f32::max);
        gate(
            &mut summary,
            "anchoring: rows that should stay put move 0 px",
            worst_anchor <= 0.5,
            format!(
                "worst {worst_anchor:.2} px over {} checks",
                self.anchors.len()
            ),
        );
        for (label, ok) in &self.top_checks {
            summary.push(format!("top {}: {label}", if *ok { "ok" } else { "WRONG" }));
        }
        if let Some((before, after)) = self.width_result {
            summary.push(format!(
                "width change: content height {before:.0} -> {after:.0} px"
            ));
        }
        summary.push(format!(
            "arrow reveal: {:?} ms",
            self.reveal_ms
                .iter()
                .map(|ms| ms.round())
                .collect::<Vec<_>>()
        ));

        for line in &summary {
            log::info!("list bench: {line}");
        }
        if let Err(err) = self.write(&summary) {
            log::error!("list bench results could not be written: {err:#}");
        }

        if failed { 3 } else { 0 }
    }

    fn write(&self, summary: &[String]) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.out_dir)?;
        std::fs::write(self.out_dir.join("frames.csv"), self.csv.join("\n"))?;
        std::fs::write(self.out_dir.join("events.log"), self.log.join("\n"))?;
        std::fs::write(self.out_dir.join("summary.txt"), summary.join("\n"))?;
        log::info!("list bench results written to {}", self.out_dir.display());
        Ok(())
    }
}

/// 跑分用的合成数据：1 万行（`--selftest-rows <n>` 可改），开头 2 条置顶。
pub fn fixture(assets: &AssetSet) -> (FixtureStore, usize) {
    let mut rows = 10_000;
    let mut args = kwikpaste_os::args();
    while let Some(arg) = args.next() {
        if arg == "--selftest-rows"
            && let Some(value) = args.next().and_then(|value| value.parse().ok())
        {
            rows = value;
        }
    }

    let items = synthetic::generate(
        assets,
        GenerateOptions {
            rows,
            ..GenerateOptions::default()
        },
    );
    (FixtureStore::new(items), rows)
}
