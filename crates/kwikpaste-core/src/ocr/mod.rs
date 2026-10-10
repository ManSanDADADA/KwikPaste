//! 事件驱动的 OCR 调度：闲置时不保留线程、连接、管道、图片或文本缓冲。
mod client;
mod db;
pub use kwikpaste_ext_protocol as protocol;
pub use protocol::OcrSupport;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use sqlx::{sqlite::SqliteConnectOptions, ConnectOptions, Connection, SqliteConnection};

use crate::{root::CoreInner, Core, CoreEvent, Result};
use protocol::{Outcome, Request, Response};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OcrStatus {
    pub enabled: bool,
    pub total_images: u64,
    pub recognized: u64,
    pub with_text: u64,
    pub failed: u64,
    pub pending: u64,
    pub running: bool,
}

/// 识别文字里命中关键词的一小段：`matched` 是关键词在 `text` 里的字节范围（可能为空）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSnippet {
    pub text: String,
    pub matched: std::ops::Range<usize>,
}

/// 片段最多的字符数，以及命中处前面保留的字符数。
const SNIPPET_CHARS: usize = 48;
const SNIPPET_LEAD: usize = 10;

/// 在识别文字里找关键词（先整串、再逐个词，不分大小写），截一段带省略号的单行片段。
/// 换行和连续空白压成一个空格；都找不到时从开头截。
pub(crate) fn snippet(text: &str, keyword: &str) -> Option<TextSnippet> {
    let flat: Vec<char> = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    if flat.is_empty() {
        return None;
    }
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let lower: Vec<char> = flat.iter().copied().map(fold).collect();
    let keyword = keyword.trim();
    let found = std::iter::once(keyword)
        .chain(keyword.split_whitespace())
        .map(|needle| needle.chars().map(fold).collect::<Vec<_>>())
        .filter(|needle| !needle.is_empty() && needle.len() <= lower.len())
        .find_map(|needle| {
            lower
                .windows(needle.len())
                .position(|window| window == needle.as_slice())
                .map(|at| (at, needle.len()))
        });
    let (at, len) = found.unwrap_or((0, 0));
    let mut start = at.saturating_sub(SNIPPET_LEAD);
    let end = (start + SNIPPET_CHARS).max(at + len).min(flat.len());
    start = start.min(end.saturating_sub(SNIPPET_CHARS));

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let mut matched = 0..0;
    for (index, ch) in flat.iter().enumerate().take(end).skip(start) {
        if index == at && len > 0 {
            matched.start = out.len();
        }
        out.push(*ch);
        if index + 1 == at + len && len > 0 {
            matched.end = out.len();
        }
    }
    if end < flat.len() {
        out.push('…');
    }
    Some(TextSnippet { text: out, matched })
}

#[derive(Default)]
struct State {
    generation: u64,
    running: bool,
    dirty: bool,
    suspended: usize,
    stopped: bool,
    child: Option<client::ChildHandle>,
}

#[derive(Default)]
pub(crate) struct Scheduler {
    core: OnceLock<Weak<CoreInner>>,
    state: Mutex<State>,
    finished: tokio::sync::Notify,
    support: Mutex<Option<OcrSupport>>,
    pub(crate) probe_lock: tokio::sync::Mutex<()>,
    startup: Mutex<Option<tokio::task::JoinHandle<()>>>,
    #[cfg(target_os = "windows")]
    helper_peak: std::sync::atomic::AtomicU64,
}

impl Scheduler {
    #[cfg(test)]
    pub(crate) fn track_child_for_test(&self, child: client::ChildHandle) {
        self.state().child = Some(child);
    }

    pub(crate) fn clear_support(&self) {
        *self.support.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    pub(crate) fn bind(&self, core: Weak<CoreInner>) {
        let _ = self.core.set(core);
    }

    fn core(&self) -> Option<Arc<CoreInner>> {
        self.core.get()?.upgrade()
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn valid(&self, generation: u64) -> bool {
        let state = self.state();
        state.generation == generation && state.suspended == 0 && !state.stopped
    }

    /// 仅已启用且确有派生队列时设置一次启动延迟；关闭会取消这个延迟。
    pub(crate) async fn startup(&self) {
        let Some(core) = self.core() else {
            return;
        };
        if core.extensions.resolve("ocr").is_none() {
            return;
        }
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new("SELECT EXISTS(");
        query.push(db::NEXT_JOB).push(")");
        let pending = query
            .build_query_scalar::<bool>()
            .fetch_one(&core.db.pool().await)
            .await;
        if !matches!(pending, Ok(true)) {
            return;
        }
        let weak = Arc::downgrade(&core);
        let task = core.rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            if let Some(core) = weak.upgrade() {
                core.ocr
                    .startup
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take();
                core.ocr.nudge();
            }
        });
        *self.startup.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
    }

    /// 只在启用后响应事件；锁内合并唤醒，避免收尾与新插入之间丢工作。
    pub(crate) fn nudge(&self) {
        let Some(core) = self.core() else {
            return;
        };
        if core.extensions.resolve("ocr").is_none() {
            return;
        }
        let mut state = self.state();
        if state.stopped {
            return;
        }
        state.dirty = true;
        if state.running || state.suspended != 0 {
            return;
        }
        state.running = true;
        state.dirty = false;
        let generation = state.generation;
        core.events.emit(CoreEvent::OcrChanged);
        let worker = core.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("ocr-session".into())
            .spawn(move || {
                if let Err(err) = session(&worker, generation) {
                    log::warn!("OCR session stopped: {err}");
                }
                // session 已关闭专用 SQLite 连接、helper 与读取线程后才发布 idle。
                let mut state = worker.ocr.state();
                state.running = false;
                state.child = None;
                let again = state.dirty;
                drop(state);
                worker.events.emit(CoreEvent::OcrChanged);
                worker.ocr.finished.notify_waiters();
                if again {
                    worker.ocr.nudge();
                }
            })
        {
            state.running = false;
            log::warn!("OCR session thread unavailable: {err}");
            core.events.emit(CoreEvent::OcrChanged);
        }
    }

    /// 设置变化立即废弃在途结果；关闭后不会重新启动。
    pub(crate) fn settings_changed(&self) {
        self.invalidate();
        if let Some(core) = self.core() {
            core.events.emit(CoreEvent::OcrChanged);
        }
        self.nudge();
    }

    fn invalidate(&self) {
        if let Some(task) = self
            .startup
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            task.abort();
        }
        let child = {
            let mut state = self.state();
            state.generation = state.generation.wrapping_add(1);
            state.child.clone()
        };
        if let Some(child) = child {
            client::kill(&child);
        }
    }

    /// 换库/删行前暂停并等待专用连接真正关闭，RAII 在成功和失败路径都恢复调度。
    pub(crate) async fn suspend(&self) -> Suspension {
        let inactive = {
            let state = self.state();
            !state.running && state.child.is_none()
        };
        if inactive
            && self
                .core()
                .is_none_or(|core| core.extensions.resolve("ocr").is_none())
        {
            return Suspension(Weak::new());
        }
        self.state().suspended += 1;
        self.invalidate();
        loop {
            let done = self.finished.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if !self.state().running {
                break;
            }
            done.await;
        }
        Suspension(self.core.get().cloned().unwrap_or_default())
    }

    pub(crate) async fn shutdown(&self) {
        self.state().stopped = true;
        let _guard = self.suspend().await;
    }
}

pub(crate) struct Suspension(Weak<CoreInner>);
impl Drop for Suspension {
    fn drop(&mut self) {
        if let Some(core) = self.0.upgrade() {
            let mut state = core.ocr.state();
            state.suspended = state.suspended.saturating_sub(1);
            state.dirty = true;
            drop(state);
            core.events.emit(CoreEvent::OcrChanged);
            core.ocr.nudge();
        }
    }
}

/// 开启受限页缓存的独立连接；显式 close 会终止 sqlx 的连接工作线程。
async fn connect(core: &CoreInner) -> anyhow::Result<SqliteConnection> {
    let options = SqliteConnectOptions::new()
        .filename(crate::db::db_path(&core.paths)?)
        .foreign_keys(true)
        .pragma("cache_size", "-256")
        .pragma("mmap_size", "0")
        .statement_cache_capacity(0)
        .disable_statement_logging();
    Ok(SqliteConnection::connect_with(&options).await?)
}

fn session(core: &CoreInner, generation: u64) -> anyhow::Result<()> {
    let mut connection = core.rt.block_on(connect(core))?;
    let result = run_jobs(core, generation, &mut connection);
    let closed = core.rt.block_on(connection.close());
    result?;
    closed?;
    Ok(())
}

/// 一次仅持有一条轻量队列记录；进度事件不超过每秒两次。
fn run_jobs(
    core: &CoreInner,
    generation: u64,
    connection: &mut SqliteConnection,
) -> anyhow::Result<()> {
    let mut helper: Option<client::Client> = None;
    let mut failures = 0u32;
    let mut last_event = Instant::now();
    loop {
        if !core.ocr.valid(generation) || core.extensions.resolve("ocr").is_none() {
            break;
        }
        let job: Option<(String, String, String)> = core
            .rt
            .block_on(sqlx::query_as(db::NEXT_JOB).fetch_optional(&mut *connection))?;
        let Some((id, name, hash)) = job else {
            let mut state = core.ocr.state();
            if state.dirty && state.generation == generation {
                state.dirty = false;
                continue;
            }
            break;
        };
        if let Err(err) = crate::clipboard::validate_image_file_name(&name) {
            core.rt.block_on(write_if_current(
                core,
                generation,
                connection,
                &id,
                &hash,
                &Outcome::Skipped {
                    reason: err.to_string(),
                },
            ))?;
            continue;
        }
        if helper.is_none() && failures != 0 {
            let until = Instant::now() + Duration::from_secs((1u64 << failures.min(5)).min(30));
            while Instant::now() < until && core.ocr.valid(generation) {
                std::thread::sleep(Duration::from_millis(50));
            }
            if !core.ocr.valid(generation) {
                break;
            }
        }
        if helper.is_none() {
            let Some(exe) = core.extensions.resolve("ocr") else {
                break;
            };
            match client::Client::start(&exe) {
                Ok(client) => {
                    let mut state = core.ocr.state();
                    if state.generation != generation || state.suspended != 0 || state.stopped {
                        drop(client);
                        break;
                    }
                    state.child = Some(client.child.clone());
                    helper = Some(client);
                }
                Err(err) => {
                    log::warn!("OCR helper start failed: {err}");
                }
            }
        }
        let request = Request::Recognize {
            job_id: id.clone(),
            path: core
                .images
                .origin_path(&name)
                .to_string_lossy()
                .into_owned(),
        };
        let response = helper.as_mut().map(|helper| helper.request(&request));
        #[cfg(target_os = "windows")]
        if let Some(helper) = &helper {
            core.ocr.helper_peak.fetch_max(
                client::peak_private_usage(&helper.child),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        let outcome = match response {
            Some(Ok(Response::Recognize { job_id, outcome })) if job_id == id => {
                failures = 0;
                match outcome {
                    Outcome::Done { text, language } => Outcome::Done {
                        text: text.chars().take(protocol::MAX_TEXT_CHARS).collect(),
                        language,
                    },
                    other => other,
                }
            }
            other => {
                if let Some(client) = helper.take() {
                    client::kill(&client.child);
                    drop(client);
                }
                core.ocr.state().child = None;
                failures = failures.saturating_add(1);
                Outcome::Failed {
                    reason: format!("helper failed: {other:?}"),
                }
            }
        };
        core.rt.block_on(write_if_current(
            core, generation, connection, &id, &hash, &outcome,
        ))?;
        if last_event.elapsed() >= Duration::from_millis(500) {
            core.events.emit(CoreEvent::OcrChanged);
            last_event = Instant::now();
        }
    }
    drop(helper);
    core.ocr.state().child = None;
    Ok(())
}

/// 与换库/清空共用 generation 栅栏和入库串行锁，旧结果不能写入新对象。
async fn write_if_current(
    core: &CoreInner,
    generation: u64,
    connection: &mut SqliteConnection,
    id: &str,
    hash: &str,
    outcome: &Outcome,
) -> anyhow::Result<bool> {
    let _serial = core.upsert_lock.lock().await;
    if !core.ocr.valid(generation) || core.extensions.resolve("ocr").is_none() {
        return Ok(false);
    }
    db::save(connection, id, hash, outcome).await?;
    Ok(true)
}

impl Core {
    /// Windows 测量入口：所有已完成请求所在 helper 的私有提交峰值。
    #[cfg(target_os = "windows")]
    pub fn ocr_helper_peak_private_usage(&self) -> u64 {
        self.0
            .ocr
            .helper_peak
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 单次聚合返回历史图像、成功、终态失败和派生队列计数。
    pub async fn ocr_status(&self) -> Result<OcrStatus> {
        let core = self.clone();
        self.hop(async move {
            let row: (i64, i64, i64, i64, i64) = sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(t.status = 'done'),0), COALESCE(SUM(t.status = 'done' AND t.text <> ''),0), COALESCE(SUM(t.status = 'skipped' OR (t.status = 'failed' AND t.attempts >= 3)),0), COALESCE(SUM(t.item_id IS NULL OR (t.status = 'failed' AND t.attempts < 3)),0) FROM clipboard_items i LEFT JOIN image_texts t ON t.item_id = i.id WHERE i.kind = 'image'")
                .fetch_one(&core.0.db.pool().await).await.map_err(anyhow::Error::from)?;
            Ok(OcrStatus { enabled: core.ocr_enabled(), total_images: row.0 as u64, recognized: row.1 as u64, with_text: row.2 as u64, failed: row.3 as u64, pending: row.4 as u64, running: core.0.ocr.state().running })
        }).await
    }

    /// 显式探测走一次性 helper，缓存实际选中的语言，不在主进程加载 OCR 框架。
    pub async fn ocr_support(&self) -> Result<OcrSupport> {
        let core = self.clone();
        self.hop(async move {
            let _serial = core.0.ocr.probe_lock.lock().await;
            let Some(exe) = core.resolve_extension("ocr") else {
                return Ok(match core.installed_extensions().get("ocr") {
                    None => OcrSupport::NotInstalled,
                    Some(entry) if !entry.enabled => OcrSupport::Disabled,
                    Some(_) => OcrSupport::Unsupported,
                });
            };
            if let Some(support) = core
                .0
                .ocr
                .support
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
            {
                return Ok(support.clone());
            }
            let (sender, receiver) = tokio::sync::oneshot::channel();
            std::thread::Builder::new()
                .name("ocr-probe".into())
                .spawn(move || {
                    let result = (|| {
                        let mut helper = client::Client::start(&exe)?;
                        match helper.request(&Request::Probe)? {
                            Response::Probe { support } => Ok(support),
                            _ => Err(std::io::Error::other("unexpected OCR probe response")),
                        }
                    })();
                    let _ = sender.send(result);
                })
                .map_err(anyhow::Error::from)?;
            let support = receiver
                .await
                .map_err(|err| anyhow::anyhow!(err))?
                .map_err(anyhow::Error::from)?;
            *core.0.ocr.support.lock().unwrap_or_else(|p| p.into_inner()) = Some(support.clone());
            Ok(support)
        })
        .await
    }

    /// 清空派生数据，并使旧 generation 的结果不可落库。
    pub async fn clear_ocr_data(&self) -> Result<()> {
        let core = self.clone();
        self.hop(async move {
            let _pause = core.0.ocr.suspend().await;
            let mut connection = connect(&core.0).await.map_err(crate::AppError::from)?;
            let result = sqlx::query("DELETE FROM image_texts")
                .execute(&mut connection)
                .await;
            connection.close().await.map_err(anyhow::Error::from)?;
            result.map_err(anyhow::Error::from)?;
            core.0.events.emit(CoreEvent::OcrChanged);
            Ok(())
        })
        .await
    }

    /// 返回已完成的识别文本（包括空文本），不改变历史记录。
    pub async fn image_text(&self, id: &str) -> Result<Option<String>> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            Ok(sqlx::query_scalar(
                "SELECT text FROM image_texts WHERE item_id = ? AND status = 'done'",
            )
            .bind(id)
            .fetch_optional(&core.0.db.pool().await)
            .await
            .map_err(anyhow::Error::from)?)
        })
        .await
    }
}

/// 以同一设置快照补齐 UI 契约，不在 OCR 关闭时访问派生表。
pub(crate) async fn attach_view(
    pool: &sqlx::SqlitePool,
    view: &mut crate::presenter::ClipboardItemView,
    query: &crate::db::models::ClipboardItemQuery,
) -> Result<()> {
    use crate::presenter::ClipboardAction;
    if !query.ocr_enabled || view.item.kind != crate::db::models::ClipboardKind::Image {
        return Ok(());
    }
    let (has_text, matched) =
        crate::db::items::image_text_flags(pool, &view.item.id, query.keyword.as_deref()).await?;
    view.has_image_text = has_text;
    view.image_text_matched = matched;
    if matched {
        if let Some(keyword) = query.keyword.as_deref() {
            let text: Option<String> = sqlx::query_scalar(
                "SELECT text FROM image_texts WHERE item_id = ? AND status = 'done'",
            )
            .bind(&view.item.id)
            .fetch_optional(pool)
            .await
            .map_err(anyhow::Error::from)?;
            view.image_text_snippet = text.and_then(|text| snippet(&text, keyword));
        }
    }
    if has_text {
        if let Some(index) = view
            .available_actions
            .iter()
            .position(|action| *action == ClipboardAction::SaveImage)
        {
            view.available_actions
                .insert(index + 1, ClipboardAction::CopyImageText);
        }
    }
    Ok(())
}
