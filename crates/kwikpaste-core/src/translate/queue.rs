//! 翻译队列：有任务时起一条会话线程和一个扩展进程，队列空了再留一会儿就全部退出。
//! 闲置时不留线程、定时器和进程。

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use kwikpaste_ext_protocol::{
    TranslateEngine, TranslateFailure, TranslateOutcome, TranslateRequest, TranslateResponse,
};

use super::{config, languages, save, translatable, TranslationState};
use crate::db::models::ClipboardItem;
use crate::events::CoreEvent;
use crate::extensions::process::{self, ChildHandle, ProcessHost};
use crate::root::CoreInner;
use crate::settings::TranslateEngineKind;

const FREE_TIMEOUT: Duration = Duration::from_secs(30);
const AI_TIMEOUT: Duration = Duration::from_secs(120);
/// 队列空了以后进程再留这么久：连着翻几条时不用每次重起进程、重新连接。
const LINGER: Duration = Duration::from_secs(60);
/// 自动任务失败后自动队列暂停这么久，免得断网或被限流时一直重复请求。
const AUTO_PAUSE: Duration = Duration::from_secs(60);

type Helper = ProcessHost<TranslateRequest, TranslateResponse>;

struct Job {
    id: String,
    auto: bool,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    active: Option<String>,
    /// 最近一次失败的原因，重新排队时清掉。
    failed: HashMap<String, (TranslateFailure, String)>,
    /// 有一条会话线程在接任务。
    running: bool,
    /// `stop` 每调一次加一；在跑的会话发现变了就收尾退出。
    epoch: u64,
    /// 还没放开的 [`Paused`] 个数；不为零时新任务直接丢掉。
    suspended: u32,
    child: Option<ChildHandle>,
    auto_paused_until: Option<Instant>,
    /// 「复制译文」「粘贴译文」刚写出的文本的指纹：监听把它记成新记录时，自动模式不翻。
    written: Option<String>,
}

#[derive(Default)]
pub(crate) struct TranslateService {
    core: OnceLock<Weak<CoreInner>>,
    state: Mutex<State>,
    wake: Condvar,
    finished: tokio::sync::Notify,
}

impl TranslateService {
    pub(crate) fn bind(&self, core: Weak<CoreInner>) {
        let _ = self.core.set(core);
    }

    fn core(&self) -> Option<Arc<CoreInner>> {
        self.core.get()?.upgrade()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn status(&self, id: &str) -> TranslationState {
        let state = self.state();
        if state.active.as_deref() == Some(id) || state.queue.iter().any(|job| job.id == id) {
            return TranslationState::Pending;
        }
        match state.failed.get(id) {
            Some((kind, message)) => TranslationState::Failed {
                kind: *kind,
                message: message.clone(),
            },
            None => TranslationState::Idle,
        }
    }

    pub(crate) fn pending(&self, id: &str) -> bool {
        self.status(id) == TranslationState::Pending
    }

    pub(crate) fn mark_written(&self, content_hash: String) {
        self.state().written = Some(content_hash);
    }

    /// 排队，没有会话在跑就起一条。手动请求排到队首；已经在队里的自动请求不重复排。
    pub(crate) fn enqueue(&self, id: String, auto: bool) {
        let Some(core) = self.core() else {
            return;
        };
        let (start, epoch) = {
            let mut state = self.state();
            if state.suspended > 0 {
                return;
            }
            if state.active.as_deref() == Some(id.as_str()) {
                return;
            }
            if let Some(position) = state.queue.iter().position(|job| job.id == id) {
                if auto {
                    return;
                }
                state.queue.remove(position);
            }
            state.failed.remove(&id);
            let job = Job {
                id: id.clone(),
                auto,
            };
            if auto {
                state.queue.push_back(job);
            } else {
                state.queue.push_front(job);
            }
            let start = !state.running;
            state.running = true;
            (start, state.epoch)
        };
        if start {
            let worker = core.clone();
            let spawned = std::thread::Builder::new()
                .name("translate-session".into())
                .spawn(move || session(&worker, epoch));
            if let Err(err) = spawned {
                log::warn!("translate session thread unavailable: {err}");
                let mut state = self.state();
                state.running = false;
                state.queue.clear();
                drop(state);
                self.finished.notify_waiters();
            }
        } else {
            self.wake.notify_one();
        }
        core.events
            .emit(CoreEvent::TranslationChanged { id: Some(id) });
    }

    /// 新文本记录入库：自动模式开着、可翻译、不是刚写出的译文、自动队列没在暂停时排到队尾。
    pub(crate) fn on_inserted(&self, item: &ClipboardItem, id: &str) {
        let Some(core) = self.core() else {
            return;
        };
        if core.extensions.resolve("translate").is_none() {
            return;
        }
        let settings = core.settings.snapshot().extensions.translate;
        if !settings.auto || !translatable(item) {
            return;
        }
        if settings.engine == TranslateEngineKind::Ai && !config::load(&core.paths).complete() {
            return;
        }
        {
            let state = self.state();
            if state.written.as_deref() == Some(item.content_hash.as_str()) {
                return;
            }
            if state
                .auto_paused_until
                .is_some_and(|until| Instant::now() < until)
            {
                return;
            }
        }
        self.enqueue(id.to_owned(), true);
    }

    /// 清队列、结束扩展进程、等会话线程收尾。扩展安装、卸载、启停和 core 关闭前调用：
    /// Windows 上运行中的 exe 换不掉。返回的 [`Paused`] 放开之前不接新任务，要拿到改完扩展目录。
    pub(crate) async fn stop(&self) -> Paused<'_> {
        let child = {
            let mut state = self.state();
            state.epoch = state.epoch.wrapping_add(1);
            state.suspended += 1;
            state.queue.clear();
            state.child.clone()
        };
        if let Some(child) = child {
            process::kill(&child);
        }
        self.wake.notify_all();
        loop {
            let done = self.finished.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if !self.state().running {
                break;
            }
            done.await;
        }
        Paused(self)
    }
}

/// [`TranslateService::stop`] 的暂停期；放开后排队的任务才会再起会话。
pub(crate) struct Paused<'a>(&'a TranslateService);

impl Drop for Paused<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.suspended = state.suspended.saturating_sub(1);
    }
}

fn session(core: &Arc<CoreInner>, epoch: u64) {
    let mut helper: Option<Helper> = None;
    while let Some(job) = next_job(core, epoch, &mut helper) {
        let outcome = run(core, epoch, &job, &mut helper);
        let mut state = core.translate.state();
        state.active = None;
        let mut auto_failed = false;
        if let Some(TranslateOutcome::Failed { kind, message }) = outcome {
            // 只记类型：服务返回的说明里可能带着原文。
            log::warn!("translation of {} failed: {kind:?}", job.id);
            if state.epoch == epoch {
                state.failed.insert(job.id.clone(), (kind, message));
                if job.auto {
                    auto_failed = true;
                    state.auto_paused_until = Some(Instant::now() + AUTO_PAUSE);
                    state.queue.retain(|job| !job.auto);
                }
            }
        }
        drop(state);
        // 自动任务失败时队列里别的自动任务也被清掉了，列表整体刷新一次。
        core.events.emit(CoreEvent::TranslationChanged {
            id: (!auto_failed).then_some(job.id),
        });
    }
}

/// 取下一条任务；没有就等，等满 [`LINGER`] 还没有、或会话被 `stop` 叫停时收尾并返回 `None`。
fn next_job(core: &CoreInner, epoch: u64, helper: &mut Option<Helper>) -> Option<Job> {
    let service = &core.translate;
    let mut state = service.state();
    let mut idle = false;
    loop {
        let stopped = state.epoch != epoch;
        if !stopped {
            if let Some(job) = state.queue.pop_front() {
                state.active = Some(job.id.clone());
                return Some(job);
            }
        }
        if stopped || idle {
            // 在锁内收尾：之后排队的任务看到 running 为假会另起会话，`stop` 等到这里时进程已经退出。
            drop(helper.take());
            if stopped {
                state.queue.clear();
            }
            state.child = None;
            state.running = false;
            drop(state);
            service.finished.notify_waiters();
            return None;
        }
        let (next, timeout) = service
            .wake
            .wait_timeout(state, LINGER)
            .unwrap_or_else(|p| p.into_inner());
        state = next;
        idle = timeout.timed_out();
    }
}

/// 翻一条：取全文、组请求、等结果，成功就落库。`None` 表示这条不用再管：记录没了、
/// 不再可翻译，或扩展没了。
fn run(
    core: &Arc<CoreInner>,
    epoch: u64,
    job: &Job,
    helper: &mut Option<Helper>,
) -> Option<TranslateOutcome> {
    let failed = |message: String| TranslateOutcome::Failed {
        kind: TranslateFailure::Other,
        message,
    };
    let exe = core.extensions.resolve("translate")?;
    let pool = core.rt.block_on(core.db.pool());
    let item = core
        .rt
        .block_on(crate::db::items::find_item_by_id(&pool, &job.id))
        .ok()
        .flatten()
        .filter(translatable)?;
    let settings = core.settings.snapshot().extensions.translate;
    let (first, second) = languages(&settings, core.language());
    let (engine, timeout) = match settings.engine {
        TranslateEngineKind::Free => (TranslateEngine::Free, FREE_TIMEOUT),
        TranslateEngineKind::Ai => {
            let ai = config::load(&core.paths);
            let engine = TranslateEngine::OpenAi {
                base_url: ai.base_url.trim().to_owned(),
                api_key: ai.api_key.trim().to_owned(),
                model: ai.model.trim().to_owned(),
                prompt: Some(ai.prompt).filter(|prompt| !prompt.trim().is_empty()),
            };
            (engine, AI_TIMEOUT)
        }
    };
    let request = TranslateRequest::Translate {
        job_id: job.id.clone(),
        text: crate::clipboard::fragment_source(&item).to_owned(),
        first,
        second,
        auto: job.auto,
        engine,
    };
    // 进程起不来（被杀毒软件删了、系统不让运行）、超时、管道断开都走这里，按「其它」显示。
    let outcome = match send(core, epoch, &exe, helper, request, timeout) {
        Ok(TranslateResponse::Translate { outcome, .. }) => outcome,
        Err(err) => failed(err.to_string()),
    };
    if let TranslateOutcome::Done {
        text,
        source,
        target,
        provider,
    } = &outcome
    {
        // 请求可能等了很久，其间换存储位置或覆盖导入会关掉开头取的连接池。
        let pool = core.rt.block_on(core.db.pool());
        let saved = core.rt.block_on(save(
            &pool,
            &job.id,
            &item.content_hash,
            text,
            source.as_deref(),
            target,
            provider,
        ));
        if let Err(err) = saved {
            return Some(failed(err.to_string()));
        }
    }
    Some(outcome)
}

/// 没有进程就先起一个，再发一帧等一帧。出错后 `ProcessHost::request` 已经结束了进程，这里丢掉它，
/// 下一条任务重新起。
fn send(
    core: &CoreInner,
    epoch: u64,
    exe: &Path,
    helper: &mut Option<Helper>,
    request: TranslateRequest,
    timeout: Duration,
) -> io::Result<TranslateResponse> {
    if helper.is_none() {
        let host = Helper::start("translate", exe)?;
        let mut state = core.translate.state();
        if state.epoch != epoch {
            return Err(io::Error::other("translate session stopped"));
        }
        state.child = Some(host.child.clone());
        drop(state);
        *helper = Some(host);
    }
    let Some(host) = helper.as_mut() else {
        return Err(io::Error::other("translate helper unavailable"));
    };
    let result = host.request(request, timeout);
    if result.is_err() {
        *helper = None;
        core.translate.state().child = None;
    }
    result
}
