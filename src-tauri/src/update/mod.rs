mod portable;

use std::{sync::Mutex, time::Duration};

use anyhow::Context;
use chrono::{DateTime, Utc};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update as TauriUpdate, UpdaterExt};
use url::Url;

use crate::core::{AppError, Result};
use crate::settings::{SettingsStore, Update as UpdateSettings, UpdateFrequency};

const UPDATE_PROGRESS_EVENT: &str = "update://progress";
const STABLE_ENDPOINT_ENV: &str = "KWIKPASTE_UPDATE_ENDPOINT";
const BETA_ENDPOINT_ENV: &str = "KWIKPASTE_UPDATE_BETA_ENDPOINT";
const NIGHTLY_ENDPOINT_ENV: &str = "KWIKPASTE_UPDATE_NIGHTLY_ENDPOINT";
// 每个渠道按优先级排列的更新源：七牛 CDN 在前，保证国内可达；GitHub 在后兜底。
// updater 逐个尝试，第一个成功解析出 latest.json 的即被采用；404、5xx、网络错误都会落到下一个。
const DEFAULT_STABLE_ENDPOINTS: &[&str] = &[
    "https://dl.fastthree.com/kwikpaste/stable/latest.json",
    "https://github.com/ManSanDADADA/KwikPaste/releases/latest/download/latest.json",
];
const DEFAULT_BETA_ENDPOINTS: &[&str] = &[
    "https://dl.fastthree.com/kwikpaste/beta/latest.json",
    "https://github.com/ManSanDADADA/KwikPaste/releases/download/channel-beta/latest.json",
];
const DEFAULT_NIGHTLY_ENDPOINTS: &[&str] = &[
    "https://dl.fastthree.com/kwikpaste/nightly/latest.json",
    "https://github.com/ManSanDADADA/KwikPaste/releases/download/channel-nightly/latest.json",
];
// 不设超时时，被墙的 GitHub 端点要等系统 TCP 超时（Windows 约 21 秒）才会放弃。
// configure_client 也会作用于安装包下载，所以那里只放建连超时；整体超时只作用于检查请求。
const CHECK_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const AUTO_CHECK_INITIAL_DELAY_SECONDS: u64 = 8;
const AUTO_CHECK_SETTINGS_REFRESH_SECONDS: u64 = 60 * 60;
const AUTO_CHECK_FAILURE_RETRY_SECONDS: u64 = 60 * 60;

pub struct UpdateState {
    current: Mutex<Option<PendingUpdate>>,
}

struct PendingUpdate {
    update: TauriUpdate,
    metadata: UpdateMetadata,
    bytes: Option<Vec<u8>>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateStatus {
    pub current_version: String,
    pub update: Option<UpdateMetadata>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMetadata {
    pub current_version: String,
    pub version: String,
    pub date: Option<String>,
    pub body: Option<String>,
    pub target: String,
    pub download_url: String,
    pub downloaded: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub progress: Option<f64>,
}

#[derive(Clone, Copy)]
pub enum CheckMode {
    Manual,
    Auto,
}

impl UpdateState {
    pub fn new() -> Self {
        Self {
            current: Mutex::new(None),
        }
    }

    fn snapshot(&self, current_version: String) -> AppUpdateStatus {
        let update = self.with_current(|current| {
            current.as_ref().map(|pending| {
                let mut metadata = pending.metadata.clone();
                metadata.downloaded = pending.bytes.is_some();
                metadata
            })
        });

        AppUpdateStatus {
            current_version,
            update,
        }
    }

    fn set_update(&self, update: Option<PendingUpdate>) {
        self.with_current(|current| {
            *current = update;
        });
    }

    fn mark_downloaded(&self, version: &str, bytes: Vec<u8>) -> Result<UpdateMetadata> {
        self.with_current(|current| {
            let pending = current
                .as_mut()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("no update is ready")))?;

            if pending.metadata.version != version {
                return Err(AppError::Other(anyhow::anyhow!(
                    "the selected update is no longer current"
                )));
            }

            pending.bytes = Some(bytes);
            pending.metadata.downloaded = true;

            Ok(pending.metadata.clone())
        })
    }

    fn install_downloaded(&self, version: &str) -> Result<()> {
        self.with_current(|current| {
            let pending = current
                .as_mut()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("no update is ready")))?;

            if pending.metadata.version != version {
                return Err(AppError::Other(anyhow::anyhow!(
                    "the selected update is no longer current"
                )));
            }

            let bytes = pending
                .bytes
                .as_ref()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("update is not downloaded")))?;

            if crate::core::portable::is_portable() {
                return portable::install(bytes);
            }

            pending
                .update
                .install(bytes)
                .context("failed to install update")?;

            Ok(())
        })
    }

    fn with_current<R>(&self, f: impl FnOnce(&mut Option<PendingUpdate>) -> R) -> R {
        let mut guard = self.current.lock().unwrap_or_else(|poisoned| {
            log::error!("update state mutex poisoned, recovering");
            poisoned.into_inner()
        });
        f(&mut guard)
    }
}

pub fn init(app: &AppHandle) {
    app.manage(UpdateState::new());

    if crate::core::portable::is_portable() {
        portable::cleanup_leftovers();
    }
}

pub async fn status(app: &AppHandle) -> AppUpdateStatus {
    app.state::<UpdateState>()
        .snapshot(app.package_info().version.to_string())
}

pub async fn check(app: &AppHandle, mode: CheckMode) -> Result<AppUpdateStatus> {
    if matches!(mode, CheckMode::Auto) && !should_auto_check(app) {
        return Ok(status(app).await);
    }

    let settings = app.state::<SettingsStore>().snapshot();
    let channels = update_channels(
        settings.update.include_beta,
        settings.update.include_nightly,
    )?;
    let found = check_channels(app, channels).await?;
    let state = app.state::<UpdateState>();

    if let Some(update) = found {
        let metadata = metadata_from_update(&update);
        let skipped = settings
            .update
            .skipped_version
            .as_deref()
            .is_some_and(|version| version == metadata.version);

        if skipped {
            state.set_update(None);
        } else {
            state.set_update(Some(PendingUpdate {
                update,
                metadata,
                bytes: None,
            }));
        }
    } else {
        state.set_update(None);
    }

    persist_last_checked_at(app);

    Ok(status(app).await)
}

pub async fn download(app: &AppHandle, version: String) -> Result<UpdateMetadata> {
    let update = {
        let state = app.state::<UpdateState>();
        state.with_current(|current| {
            let pending = current
                .as_ref()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("no update is ready")))?;

            if pending.metadata.version != version {
                return Err(AppError::Other(anyhow::anyhow!(
                    "the selected update is no longer current"
                )));
            }

            Ok(pending.update.clone())
        })?
    };

    let mut downloaded = 0_u64;
    let app_handle = app.clone();
    let bytes = update
        .download(
            move |chunk_len, total| {
                downloaded = downloaded.saturating_add(chunk_len as u64);
                emit_progress(&app_handle, downloaded, total);
            },
            || {},
        )
        .await
        .context("failed to download update")?;

    app.state::<UpdateState>().mark_downloaded(&version, bytes)
}

pub fn install(app: &AppHandle, version: String) -> Result<()> {
    log::info!("installing downloaded update {version}");
    app.state::<UpdateState>().install_downloaded(&version)?;
    log::info!("update {version} installed, requesting app restart");
    app.request_restart();

    Ok(())
}

pub fn skip(app: &AppHandle, version: String) -> Result<AppUpdateStatus> {
    let patch = serde_json::json!({
        "update": {
            "skippedVersion": version,
        },
    });
    let next = app.state::<SettingsStore>().update(patch)?;
    crate::commands::emit_settings_updated(app, &next);
    app.state::<UpdateState>().set_update(None);

    Ok(app
        .state::<UpdateState>()
        .snapshot(app.package_info().version.to_string()))
}

pub fn schedule_auto_check(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(AUTO_CHECK_INITIAL_DELAY_SECONDS)).await;

        loop {
            let delay = next_auto_check_delay(&handle);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
                continue;
            }

            run_auto_check(&handle).await;
            tokio::time::sleep(Duration::from_secs(AUTO_CHECK_FAILURE_RETRY_SECONDS)).await;
        }
    });
}

async fn run_auto_check(app: &AppHandle) {
    match check(app, CheckMode::Auto).await {
        Ok(status) if status.update.is_some() => {
            if let Err(err) = crate::window::show_window(app, crate::window::UPDATE_WINDOW_LABEL) {
                log::warn!("show update window after automatic check failed: {err}");
            }
        }
        Ok(_) => {}
        Err(err) => {
            log::warn!("automatic update check failed: {err}");
        }
    }
}

fn metadata_from_update(update: &TauriUpdate) -> UpdateMetadata {
    UpdateMetadata {
        body: update.body.clone(),
        current_version: update.current_version.clone(),
        date: update.date.map(|date| {
            date.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| date.to_string())
        }),
        download_url: update.download_url.to_string(),
        downloaded: false,
        target: update.target.clone(),
        version: update.version.clone(),
    }
}

fn update_channels(include_beta: bool, include_nightly: bool) -> Result<Vec<Vec<Url>>> {
    let stable = channel_endpoints(STABLE_ENDPOINT_ENV, DEFAULT_STABLE_ENDPOINTS);
    let beta = channel_endpoints(BETA_ENDPOINT_ENV, DEFAULT_BETA_ENDPOINTS);
    let nightly = channel_endpoints(NIGHTLY_ENDPOINT_ENV, DEFAULT_NIGHTLY_ENDPOINTS);

    update_channels_from_values(include_beta, include_nightly, &stable, &beta, &nightly)
}

/// 环境变量供本地调试用：设置后该渠道只走这一个地址，不再使用默认镜像列表。
fn channel_endpoints(env: &str, defaults: &[&str]) -> Vec<String> {
    match std::env::var(env) {
        Ok(endpoint) if !endpoint.trim().is_empty() => vec![endpoint],
        _ => defaults
            .iter()
            .map(|endpoint| (*endpoint).to_owned())
            .collect(),
    }
}

/// 每个启用的渠道各自一组镜像地址，组内按优先级排列。
///
/// 渠道之间不能拼成一个列表交给 updater：它采用第一个成功响应的地址而不比较版本，
/// 旧的 nightly 指针会挡住更新的正式版。渠道由 [`check_channels`] 分别检查后取最新。
fn update_channels_from_values(
    include_beta: bool,
    include_nightly: bool,
    stable: &[String],
    beta: &[String],
    nightly: &[String],
) -> Result<Vec<Vec<Url>>> {
    let mut channels = vec![stable];
    if include_beta {
        channels.push(beta);
    }
    if include_nightly {
        channels.push(nightly);
    }

    channels
        .into_iter()
        .map(|mirrors| {
            mirrors
                .iter()
                .map(|endpoint| parse_endpoint(endpoint))
                .collect()
        })
        .collect()
}

/// 并发检查每个渠道，返回所有渠道里版本最新的更新。
///
/// 只要有一个渠道正常响应（无论有没有更新）就不算失败：比如开着 nightly 但还没发过 nightly，
/// 不应该因此让整个检查报错。所有渠道都失败时才返回最后一个错误。
/// 结果按渠道顺序汇总，版本相同时仍以靠前的渠道为准。
async fn check_channels(app: &AppHandle, channels: Vec<Vec<Url>>) -> Result<Option<TauriUpdate>> {
    let portable_target = portable::updater_target();
    let mut checks = Vec::with_capacity(channels.len());
    for mirrors in channels {
        let mut builder = app
            .updater_builder()
            .endpoints(mirrors)
            .context("failed to configure update endpoints")?
            .timeout(CHECK_REQUEST_TIMEOUT)
            .configure_client(|client| client.connect_timeout(CONNECT_TIMEOUT));
        // 便携版不能落到默认的 `windows-<arch>` 条目：那是 NSIS 安装包，装出来的是另一份安装版。
        if let Some(target) = &portable_target {
            builder = builder.target(target.clone());
        }
        let updater = builder.build().context("failed to build updater")?;

        checks.push(tauri::async_runtime::spawn(
            async move { updater.check().await },
        ));
    }

    let mut newest: Option<TauriUpdate> = None;
    let mut last_error = None;
    let mut any_responded = false;

    for check in checks {
        let result = match check.await {
            // 只有指定了便携条目才会出现：新版本没带便携包时当作没有更新，不让整个检查报错。
            Ok(Err(tauri_plugin_updater::Error::TargetNotFound(target))) => {
                log::warn!("update channel has no package for {target}, skipping");
                Ok(None)
            }
            joined => joined
                .map_err(anyhow::Error::new)
                .and_then(|found| found.map_err(anyhow::Error::new)),
        };

        match result {
            Ok(found) => {
                any_responded = true;

                if let Some(update) = found {
                    let is_newest = newest
                        .as_ref()
                        .is_none_or(|current| is_newer_version(&update.version, &current.version));
                    if is_newest {
                        newest = Some(update);
                    }
                }
            }
            Err(err) => {
                log::warn!("update channel check failed: {err}");
                last_error = Some(err);
            }
        }
    }

    match (any_responded, last_error) {
        (false, Some(err)) => Err(err.context("failed to check for updates").into()),
        _ => Ok(newest),
    }
}

/// 按语义化版本比较；解析不了的版本一律视为不更新，避免拿到畸形 latest.json 时误装。
fn is_newer_version(candidate: &str, current: &str) -> bool {
    let parse = |version: &str| semver::Version::parse(version.trim_start_matches('v'));

    match (parse(candidate), parse(current)) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

fn parse_endpoint(endpoint: &str) -> Result<Url> {
    endpoint
        .parse::<Url>()
        .map_err(|err| AppError::Other(anyhow::anyhow!("update endpoint is invalid: {err}")))
}

fn should_auto_check(app: &AppHandle) -> bool {
    next_auto_check_delay(app).is_zero()
}

fn next_auto_check_delay(app: &AppHandle) -> Duration {
    let settings = app.state::<SettingsStore>().snapshot();

    next_auto_check_delay_for_settings(&settings.update, Utc::now())
}

fn next_auto_check_delay_for_settings(settings: &UpdateSettings, now: DateTime<Utc>) -> Duration {
    let settings_refresh = Duration::from_secs(AUTO_CHECK_SETTINGS_REFRESH_SECONDS);

    if !settings.auto_check {
        return settings_refresh;
    }

    let Some(last_checked_at) = settings.last_checked_at.as_deref() else {
        return Duration::ZERO;
    };
    let Ok(last_checked_at) = chrono::DateTime::parse_from_rfc3339(last_checked_at) else {
        return Duration::ZERO;
    };

    let elapsed = now
        .signed_duration_since(last_checked_at.with_timezone(&Utc))
        .num_seconds();
    let remaining = frequency_seconds(settings.frequency).saturating_sub(elapsed);
    if remaining <= 0 {
        return Duration::ZERO;
    }

    Duration::from_secs(remaining as u64).min(settings_refresh)
}

fn frequency_seconds(frequency: UpdateFrequency) -> i64 {
    match frequency {
        UpdateFrequency::Daily => 24 * 60 * 60,
        UpdateFrequency::Weekly => 7 * 24 * 60 * 60,
        UpdateFrequency::Monthly => 30 * 24 * 60 * 60,
    }
}

fn persist_last_checked_at(app: &AppHandle) {
    let patch = serde_json::json!({
        "update": {
            "lastCheckedAt": Utc::now().to_rfc3339(),
        },
    });

    match app.state::<SettingsStore>().update(patch) {
        Ok(next) => crate::commands::emit_settings_updated(app, &next),
        Err(err) => log::warn!("persist update check timestamp failed: {err}"),
    }
}

fn emit_progress(app: &AppHandle, downloaded: u64, total: Option<u64>) {
    let progress = total.filter(|value| *value > 0).map(|value| {
        let ratio = downloaded as f64 / value as f64;
        ratio.clamp(0.0, 1.0)
    });

    if let Err(err) = app.emit(
        UPDATE_PROGRESS_EVENT,
        UpdateDownloadProgress {
            downloaded,
            total,
            progress,
        },
    ) {
        log::warn!("emit update progress failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_settings(
        auto_check: bool,
        frequency: UpdateFrequency,
        last_checked_at: Option<String>,
    ) -> UpdateSettings {
        UpdateSettings {
            auto_check,
            frequency,
            include_beta: false,
            include_nightly: false,
            last_checked_at,
            skipped_version: None,
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn auto_check_delay_uses_settings_refresh_when_disabled() {
        let settings = update_settings(false, UpdateFrequency::Daily, None);

        assert_eq!(
            next_auto_check_delay_for_settings(&settings, fixed_now()),
            Duration::from_secs(AUTO_CHECK_SETTINGS_REFRESH_SECONDS)
        );
    }

    #[test]
    fn auto_check_delay_is_due_without_previous_check() {
        let settings = update_settings(true, UpdateFrequency::Daily, None);

        assert_eq!(
            next_auto_check_delay_for_settings(&settings, fixed_now()),
            Duration::ZERO
        );
    }

    #[test]
    fn auto_check_delay_uses_configured_frequency() {
        let now = fixed_now();
        let last_checked_at = (now - chrono::Duration::hours(24)).to_rfc3339();
        let daily = update_settings(true, UpdateFrequency::Daily, Some(last_checked_at.clone()));
        let weekly = update_settings(true, UpdateFrequency::Weekly, Some(last_checked_at));

        assert_eq!(
            next_auto_check_delay_for_settings(&daily, now),
            Duration::ZERO
        );
        assert_eq!(
            next_auto_check_delay_for_settings(&weekly, now),
            Duration::from_secs(AUTO_CHECK_SETTINGS_REFRESH_SECONDS)
        );
    }

    #[test]
    fn auto_check_delay_sleeps_until_due_when_within_refresh_window() {
        let now = fixed_now();
        let last_checked_at = (now - chrono::Duration::minutes(23 * 60 + 30)).to_rfc3339();
        let settings = update_settings(true, UpdateFrequency::Daily, Some(last_checked_at));

        assert_eq!(
            next_auto_check_delay_for_settings(&settings, now),
            Duration::from_secs(30 * 60)
        );
    }

    #[test]
    fn update_channels_keep_mirrors_within_each_channel() {
        let channels = update_channels_from_values(
            true,
            true,
            &owned(&[
                "https://cdn.example.com/stable",
                "https://gh.example.com/stable",
            ]),
            &owned(&[
                "https://cdn.example.com/beta",
                "https://gh.example.com/beta",
            ]),
            &owned(&[
                "https://cdn.example.com/nightly",
                "https://gh.example.com/nightly",
            ]),
        )
        .unwrap();

        assert_eq!(
            as_strs(&channels),
            [
                vec![
                    "https://cdn.example.com/stable",
                    "https://gh.example.com/stable"
                ],
                vec![
                    "https://cdn.example.com/beta",
                    "https://gh.example.com/beta"
                ],
                vec![
                    "https://cdn.example.com/nightly",
                    "https://gh.example.com/nightly"
                ],
            ]
        );
    }

    #[test]
    fn update_channels_skip_disabled_channels() {
        let channels = update_channels_from_values(
            false,
            false,
            &owned(&[
                "https://cdn.example.com/stable",
                "https://gh.example.com/stable",
            ]),
            &owned(&["https://cdn.example.com/beta"]),
            &owned(&["https://cdn.example.com/nightly"]),
        )
        .unwrap();

        assert_eq!(
            as_strs(&channels),
            [vec![
                "https://cdn.example.com/stable",
                "https://gh.example.com/stable"
            ]]
        );
    }

    #[test]
    fn newest_version_wins_across_channels() {
        // 正式版发布后 nightly 指针可能还停在旧版本，比较必须按语义化版本而不是渠道先后
        assert!(is_newer_version("1.2.0", "1.1.1-nightly.20260923.1"));
        assert!(!is_newer_version("1.1.1-nightly.20260923.1", "1.2.0"));
        assert!(is_newer_version("1.2.1-nightly.20261001.1", "1.2.0"));
        assert!(is_newer_version("1.2.0", "1.2.0-beta.3"));
        assert!(is_newer_version("1.2.0-beta.2", "1.2.0-beta.1"));
        assert!(is_newer_version("v1.2.0", "1.1.0"));
        assert!(!is_newer_version("1.2.0", "1.2.0"));
        assert!(!is_newer_version("not-a-version", "1.1.0"));
    }

    #[test]
    fn update_defaults_enable_every_channel() {
        let defaults = UpdateSettings::default();

        assert!(defaults.auto_check);
        assert!(defaults.include_beta);
        assert!(defaults.include_nightly);
    }

    #[test]
    fn default_endpoints_are_all_https() {
        // updater 在 release 构建里对整批端点做协议校验，混进一个非 https 地址会让全部端点一起失效，
        // 而开发构建只打印警告，本地调试时发现不了。
        let defaults = DEFAULT_STABLE_ENDPOINTS
            .iter()
            .chain(DEFAULT_BETA_ENDPOINTS)
            .chain(DEFAULT_NIGHTLY_ENDPOINTS);

        for endpoint in defaults {
            assert_eq!(
                parse_endpoint(endpoint).unwrap().scheme(),
                "https",
                "{endpoint}"
            );
        }
    }

    fn owned(endpoints: &[&str]) -> Vec<String> {
        endpoints
            .iter()
            .map(|endpoint| (*endpoint).to_owned())
            .collect()
    }

    fn as_strs(channels: &[Vec<Url>]) -> Vec<Vec<&str>> {
        channels
            .iter()
            .map(|mirrors| mirrors.iter().map(Url::as_str).collect())
            .collect()
    }
}
