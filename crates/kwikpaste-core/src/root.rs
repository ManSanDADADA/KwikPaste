//! core 的组合根：持有设置、数据库、剪贴板各存储与清理调度，给宿主一个统一入口。
//!
//! 公开 async 方法都先 [`hop`] 到 core runtime 上执行，宿主在任何执行器里都可以直接 await，
//! 不需要处在 tokio 上下文里。同步方法只做内存操作或很小的文件读写，可以在任意线程调用；
//! 带 [`ClipboardBackend`] 参数的方法要在创建该后端的线程上调用（系统剪贴板句柄是 `!Send`）。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use clipboard_rs::WatcherShutdown;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::clipboard::{
    self, AppIconStore, AppsRegistry, CleanupPreview, CleanupReport, CleanupStatus,
    ClipboardBackend, ClipboardPayload, ClipboardProvider, ClipboardReader, FileIconStore,
    ImageStore, WatcherPause, WritebackGuard,
};
use crate::db::items::UpsertResult;
use crate::db::models::{ClipboardApp, ClipboardItem, ClipboardItemQuery, ClipboardKind};
use crate::db::{self, DatabaseState};
use crate::env::{AppInfo, CoreOptions};
use crate::error::Result;
use crate::events::{CoreEvent, EventSink};
use crate::paths::CorePaths;
use crate::platform::{NoPlatformServices, PlatformServices};
use crate::presenter::{
    self, ClipboardItemPage, ClipboardItemView, ClipboardPreviewPayload, FileIconResult,
    PreviewContentMetrics,
};
use crate::runtime::hop;
use crate::settings::{
    History, Language, Settings, SettingsDelta, SettingsLoadReport, SettingsStore,
};
use crate::window_state::WindowStateStore;

/// core 的句柄，克隆很便宜，各处共用同一份状态。
#[derive(Clone)]
pub struct Core(pub(crate) Arc<CoreInner>);

pub(crate) struct CoreInner {
    pub(crate) info: AppInfo,
    pub(crate) paths: CorePaths,
    /// 切换存储位置、覆盖导入后重开数据库时沿用启动时的连接池上限。
    pub(crate) db_max_connections: u32,
    /// 见 [`CoreOptions::fixture_apps`]。
    pub(crate) fixture_apps: bool,
    pub(crate) rt: Handle,
    pub(crate) events: Arc<dyn EventSink>,
    pub(crate) settings: SettingsStore,
    pub(crate) db: DatabaseState,
    pub(crate) guard: WritebackGuard,
    pub(crate) images: ImageStore,
    pub(crate) app_icons: AppIconStore,
    pub(crate) file_icons: FileIconStore,
    pub(crate) window_state: WindowStateStore,
    pub(crate) cleanup: clipboard::cleanup::CleanupScheduler,
    pub(crate) ocr: crate::ocr::Scheduler,
    pub(crate) extensions: crate::extensions::ExtensionStore,
    /// 来源应用缓存，监听与偏好页共用。
    pub(crate) apps: AppsRegistry,
    /// 局域网同步：已配对设备随 core 读入，网络部分由宿主启用。
    pub(crate) sync: crate::sync::LanSyncService,
    pub(crate) watcher_pause: WatcherPause,
    /// 去重入库串行执行，见 [`clipboard::persist::store_and_emit`]。
    pub(crate) upsert_lock: tokio::sync::Mutex<()>,
    /// 快速粘贴进行中：上一次还没粘完时新的触发直接忽略，避免连按叠出多次粘贴。
    pub(crate) quick_paste_running: AtomicBool,
    platform: RwLock<Arc<dyn PlatformServices>>,
    clipboard_provider: RwLock<Arc<dyn ClipboardProvider>>,
    cleanup_task: Mutex<Option<JoinHandle<()>>>,
    watcher: Mutex<Option<WatcherShutdown>>,
}

impl Core {
    /// 启动 core：读设置 → 打开数据库并迁移 → 建各存储 → 启动自动清理（首轮立即执行）。
    ///
    /// `rt` 通常来自 [`crate::CoreRuntime::handle`]，必须启用了定时器（`enable_all`）。
    pub async fn start(
        info: AppInfo,
        paths: CorePaths,
        options: CoreOptions,
        events: Arc<dyn EventSink>,
        rt: Handle,
    ) -> Result<Core> {
        let handle = rt.clone();
        hop(&rt, async move {
            let settings = SettingsStore::new(&paths, options.locale.clone())?;
            let pool = db::init(&paths, options.db_max_connections).await?;
            let images = ImageStore::new(&paths)?;
            let app_icons = AppIconStore::new(&paths)?;
            let file_icons = FileIconStore::new(&paths)?;
            let window_state = WindowStateStore::new(&paths)?;
            let extensions = crate::extensions::ExtensionStore::load(paths.extensions_dir());
            let peers = crate::sync::PeerStore::load(&paths.sync_dir());

            let inner = Arc::new(CoreInner {
                info,
                paths,
                db_max_connections: options.db_max_connections,
                fixture_apps: options.fixture_apps,
                rt: handle,
                events,
                settings,
                db: DatabaseState::new(pool),
                guard: WritebackGuard::new(),
                images,
                app_icons,
                file_icons,
                window_state,
                cleanup: Default::default(),
                ocr: Default::default(),
                extensions,
                apps: AppsRegistry::default(),
                sync: crate::sync::LanSyncService::new(Arc::new(peers)),
                watcher_pause: WatcherPause::default(),
                upsert_lock: tokio::sync::Mutex::new(()),
                quick_paste_running: AtomicBool::new(false),
                platform: RwLock::new(Arc::new(NoPlatformServices)),
                clipboard_provider: RwLock::new(default_clipboard_provider()),
                cleanup_task: Mutex::new(None),
                watcher: Mutex::new(None),
            });

            inner.sync.bind(Arc::downgrade(&inner));
            inner.ocr.bind(Arc::downgrade(&inner));
            inner.ocr.startup().await;
            if let Err(err) = inner.apps.load_from_db(&inner).await {
                log::warn!("apps registry: initial DB load failed: {err}");
            }

            if inner.settings.cleanup_paused() {
                log::warn!(
                    "history settings fell back on load; automatic cleanup is paused until they are saved"
                );
            }
            let task = clipboard::cleanup::spawn(&inner);
            *inner.cleanup_task() = Some(task);

            Ok(Core(inner))
        })
        .await
    }

    /// 停止剪贴板监听、局域网同步与后台清理，WAL checkpoint 后关闭连接池。
    /// 之后不要再调用其它 async 方法。
    pub async fn shutdown(&self) -> Result<()> {
        drop(lock(&self.0.watcher).take());
        let core = self.clone();
        self.hop(async move {
            core.0.ocr.shutdown().await;
            crate::sync::shutdown(&core.0).await;
            if let Some(task) = core.0.cleanup_task().take() {
                task.abort();
            }
            let pool = core.0.db.pool().await;
            // 关库前把 WAL 写回主库并截断：安装更新、换数据目录之前尽量让数据都落在主文件里。
            if let Err(err) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&pool)
                .await
            {
                log::warn!("wal checkpoint before shutdown failed: {err}");
            }
            pool.close().await;
            Ok(())
        })
        .await
    }

    /// 接上平台层能力（前台应用识别、应用扫描、提示音）。启动后、开始监听前调用一次；
    /// 接上后在后台把默认忽略的应用补成完整记录（偏好页的忽略列表能显示名称和图标）。
    pub fn set_platform_services(&self, services: Arc<dyn PlatformServices>) {
        *self
            .0
            .platform
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = services;

        let core = self.clone();
        let excluded = self
            .0
            .settings
            .snapshot()
            .clipboard
            .filters
            .excluded_app_ids;
        self.0.rt.spawn(async move {
            if let Err(err) = clipboard::apps_registry::add_apps_from_ids(&core.0, excluded).await {
                log::warn!("apps registry: excluded app materialization failed: {err}");
            }
        });
    }

    /// 换掉打开剪贴板的方式。默认是本机系统剪贴板；自测和测试换成 [`clipboard::MemoryClipboard`]。
    pub fn set_clipboard_provider(&self, provider: Arc<dyn ClipboardProvider>) {
        *self
            .0
            .clipboard_provider
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = provider;
    }

    /// 在独立线程上启动 OS 级剪贴板监听：每次复制按设置入库并发 [`CoreEvent::ClipboardUpserted`]。
    /// 已在监听时什么都不做；[`Core::shutdown`] 时停止。
    pub fn start_watcher(&self) -> Result<()> {
        let mut watcher = lock(&self.0.watcher);
        if watcher.is_some() {
            return Ok(());
        }

        *watcher = Some(clipboard::watcher::spawn(&self.0)?);
        Ok(())
    }

    /// 2.0 正常启动后调用：在后台删掉 1.x 留下的 WebView2 数据目录（只删一次，见 [`crate::legacy`]）。
    /// 不阻塞调用方，失败只记日志、下次启动再试。
    pub fn remove_legacy_webview_data(&self) {
        let paths = self.0.paths.clone();
        self.0.rt.spawn_blocking(
            move || match crate::legacy::remove_webview_data_once(&paths) {
                Ok(crate::legacy::WebviewCleanup::Removed) => {
                    log::info!("removed the WebView2 data left by 1.x");
                }
                Ok(_) => {}
                Err(err) => log::warn!("the WebView2 data left by 1.x was not removed: {err}"),
            },
        );
    }

    /// 暂停或恢复采集。暂停期间监听收到的变化直接丢弃（切换存储位置、覆盖导入备份时用）。
    pub fn set_capture_paused(&self, paused: bool) {
        self.0.watcher_pause.set_paused(paused);
    }

    /// 播放一次复制提示音（偏好页试听）。
    pub fn play_copy_sound(&self, volume_percent: u8) {
        self.0.platform().play_copy_sound(volume_percent.min(100));
    }

    pub fn info(&self) -> &AppInfo {
        &self.0.info
    }

    pub fn paths(&self) -> &CorePaths {
        &self.0.paths
    }

    /// core runtime 的句柄，宿主需要在上面跑自己的 tokio 任务时使用。
    pub fn runtime(&self) -> &Handle {
        &self.0.rt
    }

    // ---- 设置 ----

    pub fn settings(&self) -> Settings {
        self.0.settings.snapshot()
    }

    /// 当前界面语言，Rust 侧文案（[`crate::i18n`]）据此取词。
    pub fn language(&self) -> Language {
        crate::i18n::current_language(&self.0.settings)
    }

    /// 启动时读设置文件哪些字段没有采用原值。
    pub fn settings_load_report(&self) -> SettingsLoadReport {
        self.0.settings.load_report()
    }

    /// 用 JSON patch（camelCase 键，与 `settings.json` 相同）深度合并到当前设置并落盘。
    /// 成功后发 [`CoreEvent::SettingsUpdated`]；改到 `clipboard.history` 时同时请求一轮清理。
    pub async fn update_settings(&self, patch: serde_json::Value) -> Result<Settings> {
        let core = self.clone();
        self.hop(async move {
            let delta = SettingsDelta::from_patch(&patch);
            let next = core.0.settings.update(patch)?;
            if delta.touches("clipboard.history") {
                clipboard::cleanup::request(&core.0);
            }
            if delta.touches("sync") {
                crate::sync::settings_changed(&core.0);
            }
            core.emit_settings(&next, delta);
            Ok(next)
        })
        .await
    }

    /// 恢复默认设置（保留历史记录与资源文件）。
    pub async fn reset_settings(&self) -> Result<Settings> {
        let core = self.clone();
        self.hop(async move {
            let next = core.0.settings.reset()?;
            core.0.ocr.settings_changed();
            clipboard::cleanup::request(&core.0);
            crate::sync::settings_changed(&core.0);
            core.emit_settings(&next, SettingsDelta::replaced());
            Ok(next)
        })
        .await
    }

    // ---- 剪贴板 ----

    /// 回环抑制：监听读到内容后用它判断是不是自己刚写回的。
    pub fn writeback_guard(&self) -> &WritebackGuard {
        &self.0.guard
    }

    pub fn image_store(&self) -> &ImageStore {
        &self.0.images
    }

    pub fn app_icon_store(&self) -> &AppIconStore {
        &self.0.app_icons
    }

    pub fn file_icon_store(&self) -> &FileIconStore {
        &self.0.file_icons
    }

    /// 窗口位置与尺寸存档（`state/window-state.gpui.json`），随存储位置切换。
    pub fn window_state(&self) -> &WindowStateStore {
        &self.0.window_state
    }

    /// 按当前的采集设置读取剪贴板，返回第一个启用且有内容的表示。
    pub fn read_payload(&self, backend: &dyn ClipboardBackend) -> Result<Option<ClipboardPayload>> {
        let capture = self.0.settings.snapshot().clipboard.capture;
        ClipboardReader::with_backend(backend).read_with_capture(&capture)
    }

    /// 按当前设置把载荷转成待入库记录：类型过滤、大小上限、敏感内容、子类型识别，图片原图落盘。
    /// 返回 `None` 表示按设置不收录。
    pub fn build_item(&self, payload: &ClipboardPayload) -> Result<Option<ClipboardItem>> {
        let settings = self.0.settings.snapshot();
        clipboard::build_item_with_settings(
            &self.0.images,
            payload,
            &settings.clipboard.capture,
            &settings.clipboard.sensitive,
            settings.clipboard.content.copy_plain,
        )
    }

    /// 去重入库：`source_app` 先登记到应用表；成功后发 [`CoreEvent::ClipboardUpserted`]，
    /// 新记录还会触发一次条数与存储上限检查。
    pub async fn store_item(
        &self,
        item: ClipboardItem,
        source_app: Option<ClipboardApp>,
    ) -> Result<UpsertResult> {
        let core = self.clone();
        self.hop(async move {
            clipboard::persist::persist_and_notify(&core.0, &item, source_app.as_ref()).await
        })
        .await
    }

    /// 把记录写回剪贴板并登记回环抑制；`plain = true` 只写纯文本（文件记录写成路径文本）。
    pub fn write_to_clipboard(
        &self,
        backend: &dyn ClipboardBackend,
        item: &ClipboardItem,
        plain: bool,
    ) -> Result<()> {
        clipboard::write_to_clipboard(backend, &self.0.images, &self.0.guard, item, plain)
    }

    /// 把一段纯文本（快捷信息、拆词选区）写回剪贴板并登记回环抑制。
    pub fn write_text_fragment(&self, backend: &dyn ClipboardBackend, text: &str) -> Result<()> {
        clipboard::write_text_fragment(backend, &self.0.guard, text)
    }

    /// 图片记录（`content` 即文件名）的原图路径；文件名不合法时报错。
    pub fn image_origin_path(&self, file_name: &str) -> Result<PathBuf> {
        clipboard::validate_image_file_name(file_name)?;
        Ok(self.0.images.origin_path(file_name))
    }

    /// 确保缩略图存在并返回路径；首次生成在阻塞线程池里解码，并发张数有上限。
    pub async fn ensure_thumbnail(&self, file_name: &str) -> Result<PathBuf> {
        clipboard::validate_image_file_name(file_name)?;
        let core = self.clone();
        let file_name = file_name.to_owned();
        self.hop(async move { core.0.images.ensure_thumbnail_async(&file_name).await })
            .await
    }

    /// 单图文件记录（`filesPreviewKind = imagePreview`）的缩略图：确保存在并返回路径，规则与图片记录相同。
    /// `path` 是记录里那个文件的路径；只接受存在的图片文件。
    pub async fn ensure_file_thumbnail(&self, path: &str) -> Result<PathBuf> {
        let source = PathBuf::from(path);
        if !source.is_absolute() || !presenter::is_image_path(path) || !source.is_file() {
            return Err(crate::error::AppError::Clipboard(format!(
                "not an image file: {path:?}"
            )));
        }
        let core = self.clone();
        self.hop(async move { core.0.images.ensure_file_thumbnail_async(&source).await })
            .await
    }

    // ---- 记录查询 ----

    /// 一页列表的数据库原始行与总数，没有经过展示层（敏感内容未脱敏）。只给测试用。
    #[cfg(test)]
    pub(crate) async fn query_items_raw(
        &self,
        query: ClipboardItemQuery,
    ) -> Result<(Vec<ClipboardItem>, i64)> {
        let core = self.clone();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            db::items::query_items_page(&pool, &query).await
        })
        .await
    }

    /// 列表的一页：查询后经展示层加工（缩略图与图标路径、文件条目、颜色预览、显示时间、
    /// 快捷信息、可用动作），敏感内容按设置脱敏。与 1.4.0 的 `list_clipboard_items` 输出一致。
    pub async fn list_items(&self, query: ClipboardItemQuery) -> Result<ClipboardItemPage> {
        let core = self.clone();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let mut query = query;
            let clipboard = core.0.settings.snapshot().clipboard;
            query.ocr_enabled = core.ocr_enabled();
            let (rows, total) = db::items::query_items_page(&pool, &query).await?;
            let ctx = core.list_context(&pool, &clipboard);
            let mut list = Vec::with_capacity(rows.len());
            for row in rows {
                let mut view = presenter::present_list_item(&ctx, row).await?;
                crate::ocr::attach_view(&pool, &mut view, &query).await?;
                list.push(view);
            }
            let has_more = query.offset + (list.len() as i64) < total;

            Ok(ClipboardItemPage {
                list,
                total,
                has_more,
            })
        })
        .await
    }

    /// 单条记录的列表视图，收到 [`CoreEvent::ClipboardUpserted`] 后按 id 刷新一张卡片用；
    /// 与列表同样裁剪和加工。记录不存在时返回 `None`。
    pub async fn list_item(&self, id: &str) -> Result<Option<ClipboardItemView>> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let Some(item) = db::items::find_item_for_list_by_id(&pool, &id).await? else {
                return Ok(None);
            };
            let clipboard = core.0.settings.snapshot().clipboard;
            let mut view =
                presenter::present_list_item(&core.list_context(&pool, &clipboard), item).await?;
            let query = ClipboardItemQuery {
                ocr_enabled: core.ocr_enabled(),
                ..Default::default()
            };
            crate::ocr::attach_view(&pool, &mut view, &query).await?;
            Ok(Some(view))
        })
        .await
    }

    /// 预览面板的数据：文本（按设置脱敏）与可点选的词、图片原图路径、文件条目。
    pub async fn preview_payload(&self, id: &str) -> Result<Option<ClipboardPreviewPayload>> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let Some(item) = db::items::find_item_by_id(&pool, &id).await? else {
                return Ok(None);
            };
            let redact = core
                .0
                .settings
                .snapshot()
                .clipboard
                .sensitive
                .redact_secrets;
            let payload = presenter::build_preview_payload(
                &pool,
                &core.0.images,
                &core.0.file_icons,
                item,
                redact,
            )
            .await?;
            Ok(Some(payload))
        })
        .await
    }

    /// 图片识别文本的预览，形状与文本记录一致，可直接使用原文和选词视图。
    /// OCR 关闭、记录不是图片或没有已完成的非空识别文本时返回 `None`。
    pub async fn image_text_preview(
        &self,
        id: &str,
    ) -> Result<Option<(ClipboardPreviewPayload, PreviewContentMetrics)>> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let clipboard = core.0.settings.snapshot().clipboard;
            if !core.ocr_enabled() {
                return Ok(None);
            }
            let pool = core.0.db.pool().await;
            let Some(item) = db::items::find_item_by_id(&pool, &id).await? else {
                return Ok(None);
            };
            if item.kind != ClipboardKind::Image {
                return Ok(None);
            }
            let Some(text) = core.image_text(&id).await?.filter(|text| !text.is_empty()) else {
                return Ok(None);
            };

            Ok(Some(presenter::build_image_text_preview(
                &item,
                text,
                clipboard.preview.text_view,
            )))
        })
        .await
    }

    /// 预览面板定尺寸用的内容度量（文本行数或词块、图片宽高、文件条数）。
    pub async fn preview_metrics(&self, id: &str) -> Result<Option<PreviewContentMetrics>> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let Some(item) = db::items::find_item_by_id(&pool, &id).await? else {
                return Ok(None);
            };
            let clipboard = core.0.settings.snapshot().clipboard;
            Ok(Some(presenter::preview_content_metrics(&item, &clipboard)))
        })
        .await
    }

    /// 文件记录里第 `index` 个路径的类型图标（命中缓存或现抽）与是否仍存在。
    /// `file_types` 是记录的 `file_types` 字段。
    pub async fn file_icon(
        &self,
        path: &str,
        file_types: Option<&str>,
        index: usize,
    ) -> Result<FileIconResult> {
        let core = self.clone();
        let path = path.to_owned();
        let file_types = file_types.map(str::to_owned);
        self.hop(async move {
            let pool = core.0.db.pool().await;
            let (icon_path, exists) = presenter::resolve_file_icon_path(
                &pool,
                &core.0.file_icons,
                &path,
                file_types.as_deref(),
                index,
            )
            .await?;
            Ok(FileIconResult { icon_path, exists })
        })
        .await
    }

    /// 来源应用图标（`clipboard_apps.icon_file`）的绝对路径；文件名不合法时报错。
    pub fn app_icon_path(&self, file_name: &str) -> Result<PathBuf> {
        clipboard::validate_image_file_name(file_name)?;
        Ok(self.0.app_icons.icon_path(file_name))
    }

    /// 单条记录的完整字段，预览与写回用。
    pub async fn find_item(&self, id: &str) -> Result<Option<ClipboardItem>> {
        let core = self.clone();
        let id = id.to_owned();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            db::items::find_item_by_id(&pool, &id).await
        })
        .await
    }

    // ---- 历史清理 ----

    /// 清理状态快照（最近一次清理、存储占用、自动清理是否暂停）。
    pub fn cleanup_status(&self) -> CleanupStatus {
        clipboard::cleanup::status(&self.0)
    }

    /// 历史设置读盘时有回落且用户还没保存过，后台自动清理处于暂停。
    pub fn cleanup_paused(&self) -> bool {
        self.0.settings.cleanup_paused()
    }

    /// 立即按当前设置完整清理一次（用户的显式操作，自动清理暂停时也执行）。
    pub async fn run_cleanup_now(&self) -> Result<CleanupReport> {
        let core = self.clone();
        self.hop(async move { clipboard::cleanup::run_now(&core.0).await })
            .await
    }

    /// 按候选历史设置预演一轮清理，事务回滚，不删任何东西。
    pub async fn preview_cleanup(&self, history: History) -> Result<CleanupPreview> {
        let core = self.clone();
        self.hop(async move { clipboard::cleanup::preview(&core.0, &history).await })
            .await
    }

    /// 数据实际占用的字节数（数据目录减去 SQLite 可复用空间和 WAL 旁路文件）。
    pub async fn storage_bytes_in_use(&self) -> Result<u64> {
        let core = self.clone();
        self.hop(async move {
            let pool = core.0.db.pool().await;
            clipboard::cleanup::storage_bytes_in_use(&core.0, &pool).await
        })
        .await
    }

    pub(crate) async fn hop<T, F>(&self, fut: F) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        hop(&self.0.rt, fut).await
    }

    /// 列表加工上下文：显示时间按本机时区算。
    fn list_context<'a>(
        &'a self,
        pool: &'a sqlx::SqlitePool,
        clipboard: &crate::settings::Clipboard,
    ) -> presenter::ListContext<'a, chrono::Local> {
        presenter::ListContext::new(
            pool,
            &self.0.images,
            &self.0.app_icons,
            &self.0.file_icons,
            clipboard,
            chrono::Local::now(),
        )
        .with_devices(self.0.sync.peers())
    }

    fn emit_settings(&self, settings: &Settings, delta: SettingsDelta) {
        self.0.events.emit(CoreEvent::SettingsUpdated {
            settings: Arc::new(settings.clone()),
            delta,
        });
    }
}

impl CoreInner {
    fn cleanup_task(&self) -> std::sync::MutexGuard<'_, Option<JoinHandle<()>>> {
        lock(&self.cleanup_task)
    }

    /// 当前接上的平台层能力。
    pub(crate) fn platform(&self) -> Arc<dyn PlatformServices> {
        self.platform
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 打开剪贴板。返回的后端只在当前线程上同步用完，不能跨 await 持有。
    pub(crate) fn clipboard(&self) -> Result<Box<dyn ClipboardBackend>> {
        self.clipboard_provider
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .open()
    }

    /// 当前界面语言。
    pub(crate) fn language(&self) -> Language {
        crate::i18n::current_language(&self.settings)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 默认打开本机系统剪贴板；core 自己的单元测试默认用内存剪贴板，绝不碰本机剪贴板。
fn default_clipboard_provider() -> Arc<dyn ClipboardProvider> {
    if cfg!(test) {
        Arc::new(clipboard::MemoryClipboard::new())
    } else {
        Arc::new(clipboard::SystemClipboardProvider)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;

    use super::*;
    use crate::clipboard::{MemoryClipboard, MemoryState};
    use crate::db::models::ClipboardKind;
    use crate::testing::{block_on, sample_png, Fixture};

    /// 测试线程上没有 tokio 上下文，每个公开方法都直接在自制执行器里 await。
    #[test]
    fn facade_works_without_a_tokio_context() {
        assert!(Handle::try_current().is_err());
        let fixture = Fixture::new();
        let core = fixture.start();

        let settings =
            block_on(core.update_settings(json!({"appearance": {"theme": "dark"}}))).unwrap();
        assert_eq!(settings.appearance.theme, crate::settings::Theme::Dark);
        assert_eq!(
            core.settings().appearance.theme,
            crate::settings::Theme::Dark
        );

        let clipboard = MemoryClipboard::with_state(MemoryState {
            text: Some("https://example.com".to_owned()),
            ..MemoryState::default()
        });
        let payload = core.read_payload(&clipboard).unwrap().unwrap();
        let item = core.build_item(&payload).unwrap().unwrap();
        let first = block_on(core.store_item(item.clone(), None)).unwrap();
        let again = block_on(core.store_item(item.clone(), None)).unwrap();
        assert!(!first.deduplicated);
        assert!(again.deduplicated);
        assert_eq!(first.id, again.id);

        let image = MemoryClipboard::with_state(MemoryState {
            png: Some(sample_png(30, 20)),
            ..MemoryState::default()
        });
        let image_item = core
            .build_item(&core.read_payload(&image).unwrap().unwrap())
            .unwrap()
            .unwrap();
        block_on(core.store_item(image_item.clone(), None)).unwrap();
        let thumbnail = block_on(core.ensure_thumbnail(&image_item.content)).unwrap();
        assert!(thumbnail.is_file());
        assert!(core
            .image_origin_path(&image_item.content)
            .unwrap()
            .is_file());
        assert!(block_on(core.ensure_thumbnail("../escape.png")).is_err());

        let (rows, total) = block_on(core.query_items_raw(ClipboardItemQuery::default())).unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
        let listed = block_on(core.list_items(ClipboardItemQuery::default())).unwrap();
        assert_eq!(listed.total, 2);
        assert!(listed
            .list
            .iter()
            .all(|item| !item.available_actions.is_empty() && !item.display_created_at.is_empty()));
        let image_view = listed
            .list
            .iter()
            .find(|view| view.item.kind == ClipboardKind::Image)
            .unwrap();
        assert_eq!(
            image_view.image_display_size,
            Some(presenter::ImageDisplaySize {
                width: 30,
                height: 20
            })
        );
        assert!(block_on(core.list_item(&first.id)).unwrap().is_some());
        let preview = block_on(core.preview_payload(&first.id)).unwrap().unwrap();
        assert_eq!(preview.text.as_deref(), Some("https://example.com"));
        assert!(matches!(
            block_on(core.preview_metrics(&image_item.id)).unwrap(),
            Some(PreviewContentMetrics::Image { .. })
        ));
        let missing = block_on(core.file_icon("C:/kwikpaste/missing.txt", Some("f"), 0)).unwrap();
        assert!(!missing.exists);
        let full = block_on(core.find_item(&first.id)).unwrap().unwrap();
        assert_eq!(full.content, "https://example.com");
        assert!(block_on(core.list_groups()).unwrap().is_empty());

        let target = MemoryClipboard::new();
        core.write_to_clipboard(&target, &full, false).unwrap();
        assert_eq!(
            target.snapshot().text.as_deref(),
            Some("https://example.com")
        );
        assert!(core.writeback_guard().should_skip(&full.content_hash));
        core.write_text_fragment(&target, "example").unwrap();
        assert_eq!(target.snapshot().text.as_deref(), Some("example"));

        // WAL / SHM 不计入占用，见 `ops::storage_tests`。
        assert!(block_on(core.storage_bytes_in_use()).unwrap() > 0);
        let preview = block_on(core.preview_cleanup(History::default())).unwrap();
        assert_eq!(preview.removed, 0);
        assert_eq!(block_on(core.run_cleanup_now()).unwrap().removed, 0);
        assert!(!core.cleanup_status().auto_cleanup_paused);

        let events = fixture.take_events();
        assert!(events.iter().any(|event| matches!(
            event,
            CoreEvent::SettingsUpdated { delta, .. } if delta.touches("appearance.theme")
        )));
        let upserts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                CoreEvent::ClipboardUpserted {
                    kind, deduplicated, ..
                } => Some((*kind, *deduplicated)),
                _ => None,
            })
            .collect();
        assert_eq!(
            upserts,
            [
                (ClipboardKind::Text, false),
                (ClipboardKind::Text, true),
                (ClipboardKind::Image, false)
            ]
        );

        block_on(core.shutdown()).unwrap();
    }

    #[test]
    fn automatic_cleanup_waits_until_history_settings_are_saved() {
        let fixture = Fixture::new();
        fixture.write_settings(
            r#"{"clipboard": {"history": {"retention": {"value": 1, "unit": "minutes"}, "maxCount": "many"}}}"#,
        );
        let core = fixture.start();
        assert!(core.cleanup_paused());

        let clipboard = MemoryClipboard::with_state(MemoryState {
            text: Some("old record".to_owned()),
            ..MemoryState::default()
        });
        let mut item = core
            .build_item(&core.read_payload(&clipboard).unwrap().unwrap())
            .unwrap()
            .unwrap();
        item.created_at = Utc::now() - Duration::days(2);
        item.updated_at = item.created_at;
        let stored = block_on(core.store_item(item, None)).unwrap();

        block_on(core.0.rt.spawn({
            let core = core.clone();
            async move { clipboard::cleanup::run_due(&core.0).await }
        }))
        .unwrap();
        assert!(block_on(core.find_item(&stored.id)).unwrap().is_some());
        assert!(core.cleanup_status().auto_cleanup_paused);

        // 用户在偏好页保存一次历史设置：暂停解除，过期记录被清理。
        block_on(core.update_settings(json!({"clipboard": {"history": {"maxCount": 0}}}))).unwrap();
        assert!(!core.cleanup_paused());
        block_on(core.0.rt.spawn({
            let core = core.clone();
            async move { clipboard::cleanup::run_due(&core.0).await }
        }))
        .unwrap();

        assert!(block_on(core.find_item(&stored.id)).unwrap().is_none());
        assert!(fixture
            .take_events()
            .iter()
            .any(|event| matches!(event, CoreEvent::ClipboardCleaned { removed: 1 })));
        block_on(core.shutdown()).unwrap();
    }
}
