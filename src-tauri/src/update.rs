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
    let endpoints = update_endpoints(
        settings.update.include_beta,
        settings.update.include_nightly,
    )?;
    let updater = app
        .updater_builder()
        .endpoints(endpoints)
        .context("failed to configure update endpoints")?
        .build()
        .context("failed to build updater")?;

    let found = updater
        .check()
        .await
        .context("failed to check for updates")?;
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

fn update_endpoints(include_beta: bool, include_nightly: bool) -> Result<Vec<Url>> {
    let stable = channel_endpoints(STABLE_ENDPOINT_ENV, DEFAULT_STABLE_ENDPOINTS);
    let beta = channel_endpoints(BETA_ENDPOINT_ENV, DEFAULT_BETA_ENDPOINTS);
    let nightly = channel_endpoints(NIGHTLY_ENDPOINT_ENV, DEFAULT_NIGHTLY_ENDPOINTS);

    update_endpoints_from_values(include_beta, include_nightly, &stable, &beta, &nightly)
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

/// 按「渠道优先、镜像其次」展开成 updater 的端点列表。
///
/// updater 采用第一个成功的端点，所以同一渠道的镜像必须排在一起：若按镜像分组，
/// nightly 在主镜像缺失时会直接落到 beta 的主镜像，用户被悄悄降级，而没去试 nightly 的兜底源。
fn update_endpoints_from_values(
    include_beta: bool,
    include_nightly: bool,
    stable: &[String],
    beta: &[String],
    nightly: &[String],
) -> Result<Vec<Url>> {
    let mut channels = Vec::new();
    if include_nightly {
        channels.push(nightly);
    }
    if include_beta {
        channels.push(beta);
    }
    channels.push(stable);

    channels
        .into_iter()
        .flatten()
        .map(|endpoint| parse_endpoint(endpoint))
        .collect()
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
    fn update_endpoints_group_mirrors_by_channel() {
        let endpoints = update_endpoints_from_values(
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
            endpoints.iter().map(Url::as_str).collect::<Vec<_>>(),
            [
                "https://cdn.example.com/nightly",
                "https://gh.example.com/nightly",
                "https://cdn.example.com/beta",
                "https://gh.example.com/beta",
                "https://cdn.example.com/stable",
                "https://gh.example.com/stable",
            ]
        );
    }

    #[test]
    fn update_endpoints_skip_disabled_channels() {
        let endpoints = update_endpoints_from_values(
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
            endpoints.iter().map(Url::as_str).collect::<Vec<_>>(),
            [
                "https://cdn.example.com/stable",
                "https://gh.example.com/stable"
            ]
        );
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
}
