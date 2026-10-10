//! 最小化使用统计（上报协议 v2），用来统计每日 / 每周 / 每月活跃安装、新增安装、留存、升级路径和版本分布；
//! 更新本身不依赖它。
//!
//! 开着「自动检查更新」时每个 UTC 日上报一次（启动后和运行中每小时看一次当天是否已送达），版本变了也补报一次；
//! 关掉自动检查后只在手动检查更新时上报。请求带每日随机 ID、上报类型、软件版本、系统、架构、界面语言，
//! 以及距上次送达的天数、新安装所在的 ISO 周和升级前的版本：服务端靠这些在不持有长期 ID 的情况下精确去重。
//! 每日 ID 按 UTC 日轮换，不从机器指纹、账号或局域网同步身份派生；发送失败静默跳过，不影响检查更新。
//! 状态文件 `<bootstrap>/update-usage.json` 与 1.x 共用：同一台电脑从 1.x 换到 2.0 不算新增安装。
//! 只有发布流水线打出的包上报：本地打包、开发和测试构建都不发。

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use kwikpaste_core::settings::{Language, Settings};
use kwikpaste_core::{AppEnv, Core};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const ENDPOINT: &str = "https://paste.fastthree.com/api/v2/usage";
const STATE_FILENAME: &str = "update-usage.json";
const STATE_VERSION: u16 = 1;
const SYSTEM: &str = if cfg!(target_os = "macos") {
    "macos"
} else {
    "windows"
};
/// `native-release.yml` 构建时设置；本机打包、自测和其它 CI 构建都没有，不会混进线上统计。
const OFFICIAL_BUILD: bool = option_env!("KWIKPASTE_OFFICIAL_BUILD").is_some();

#[derive(Default)]
pub(crate) struct UsageState {
    gate: tokio::sync::Mutex<()>,
}

#[derive(Clone, Copy)]
pub(crate) enum Trigger {
    /// 启动后和运行中每小时一次：开着自动检查更新时，当天还没送达或版本变了就上报。
    Daily,
    /// 实际执行了一次检查更新（手动或到期的自动检查）。
    Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    First,
    Active,
}

/// 新增字段都可缺省：1.x 读写同一个文件时会忽略、丢掉它们，2.0 读到缺失时按“不知道”处理。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DailyState {
    version: u16,
    day: String,
    daily_id: String,
    /// 新安装的首次上报还没送达；送达前每次上报都标成首次。
    first_pending: bool,
    /// 上次送达的 UTC 日。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_sent_day: Option<String>,
    /// 上次送达时的软件版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_sent_version: Option<String>,
    /// 新安装所在的 ISO 周（`2026-W41`）；统计上线前就装好的为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    install_week: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    daily_id: String,
    kind: Kind,
    version: String,
    system: &'static str,
    arch: &'static str,
    language: Language,
    days_since_last: Option<i64>,
    install_week: Option<String>,
    previous_version: Option<String>,
    extensions: Vec<ExtensionUsage>,
}

#[derive(Serialize)]
struct ExtensionUsage {
    id: String,
    version: String,
    enabled: bool,
}

/// 只上报安装版本与启用状态，不携带路径、扩展自有状态或协议内部信息。
fn installed_extensions(core: &Core) -> Vec<ExtensionUsage> {
    core.installed_extensions()
        .into_iter()
        .map(|(id, installed)| ExtensionUsage {
            id,
            version: installed.version,
            enabled: installed.enabled,
        })
        .collect()
}

/// 在 core runtime 后台发送，检查更新不等网络和磁盘；多次触发排队执行，状态读写不会交错。
pub(crate) fn schedule(core: &Core, state: &Arc<UsageState>, trigger: Trigger) {
    let Some(endpoint) = endpoint(core.info().env) else {
        return;
    };

    let core = core.clone();
    let state = state.clone();
    core.runtime().clone().spawn(async move {
        let _guard = state.gate.lock().await;
        if let Err(err) = report(&core, trigger, &endpoint).await {
            log::debug!("update usage report skipped: {err:#}");
        }
    });
}

/// 只有正式发布的包发往线上；`e2e-overrides` 下可以指定地址（开发构建也发）。
fn endpoint(env: AppEnv) -> Option<String> {
    if let Some(endpoint) = crate::overrides::var(crate::overrides::USAGE_ENDPOINT) {
        return Some(endpoint);
    }
    (env == AppEnv::Prod && OFFICIAL_BUILD).then(|| ENDPOINT.to_owned())
}

/// 每日 ID 在发送前落盘，重启或发送失败都不会让同一天换出第二个 ID；送达后才记下当天和版本。
async fn report(core: &Core, trigger: Trigger, endpoint: &str) -> Result<()> {
    let settings = core.settings();
    let now = Utc::now();
    let path = core.paths().bootstrap_dir().join(STATE_FILENAME);
    let mut state = load_state(&path, now, || is_fresh_install(&settings))?;
    let version = core.info().version.to_string();
    let Some(kind) = kind_for(
        trigger,
        &state,
        &version,
        now.date_naive(),
        settings.update.auto_check,
    ) else {
        return Ok(());
    };
    if state.first_pending && state.install_week.is_none() {
        state.install_week = Some(iso_week(now.date_naive()));
        write_state(&path, &state)?;
    }

    let payload = Payload {
        extensions: installed_extensions(core),
        daily_id: state.daily_id.clone(),
        kind,
        version: version.clone(),
        system: SYSTEM,
        arch: std::env::consts::ARCH,
        language: settings.appearance.language,
        days_since_last: days_since_last(&state, now.date_naive()),
        install_week: state.install_week.clone(),
        previous_version: state
            .last_sent_version
            .clone()
            .filter(|previous| previous != &version),
    };
    send_payload(&payload, endpoint).await?;

    state.first_pending = false;
    state.last_sent_day = Some(now.date_naive().to_string());
    state.last_sent_version = Some(version);
    write_state(&path, &state)
}

/// 这次要不要发、发哪种：首次上报没送达前总是发；之后当天没送达或版本变了才发，
/// 每日触发还要求开着自动检查更新。
fn kind_for(
    trigger: Trigger,
    state: &DailyState,
    version: &str,
    today: NaiveDate,
    auto_check: bool,
) -> Option<Kind> {
    if state.first_pending {
        return Some(Kind::First);
    }
    let due = state.last_sent_day.as_deref() != Some(today.to_string().as_str())
        || state.last_sent_version.as_deref() != Some(version);
    let allowed = match trigger {
        Trigger::Daily => auto_check,
        Trigger::Check => true,
    };
    (due && allowed).then_some(Kind::Active)
}

/// 距上次送达的 UTC 天数；没有记录或时钟倒退时为空，服务端按“之前没见过”处理。
fn days_since_last(state: &DailyState, today: NaiveDate) -> Option<i64> {
    let last = NaiveDate::parse_from_str(state.last_sent_day.as_deref()?, "%Y-%m-%d").ok()?;
    let days = (today - last).num_days();
    (days >= 0).then_some(days)
}

fn iso_week(day: NaiveDate) -> String {
    let week = day.iso_week();
    format!("{}-W{:02}", week.year(), week.week())
}

/// 状态文件第一次创建时判断是否新安装：引导没走完、也从没检查过更新。
/// 从没有统计功能的旧版本升级上来的安装已有这些痕迹，不算新增。
fn is_fresh_install(settings: &Settings) -> bool {
    !settings.onboarding.completed && settings.update.last_checked_at.is_none()
}

/// 独立的请求客户端，禁止跟随重定向，统计字段不会被转发到别的主机。
async fn send_payload(payload: &Payload, endpoint: &str) -> Result<()> {
    let body = serde_json::to_vec(payload)?;
    let response = crate::http::no_redirect_client()?
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await?;
    let status = response.status();
    if status != reqwest::StatusCode::NO_CONTENT {
        anyhow::bail!("usage endpoint answered {status}");
    }

    Ok(())
}

fn load_state(
    path: &Path,
    now: DateTime<Utc>,
    fresh_install: impl FnOnce() -> bool,
) -> Result<DailyState> {
    let previous = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<DailyState>(&bytes).ok(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };
    let (state, changed) = state_for_day(previous, now, fresh_install);
    if changed {
        write_state(path, &state)?;
    }

    Ok(state)
}

/// 只保留当天的随机 ID：跨 UTC 日直接换新，不留旧 ID 或任何可推算的种子。
/// 文件缺失、损坏或格式版本不对时重建，并按当前设置重新判断是否新安装。
fn state_for_day(
    previous: Option<DailyState>,
    now: DateTime<Utc>,
    fresh_install: impl FnOnce() -> bool,
) -> (DailyState, bool) {
    let day = now.date_naive().to_string();
    let Some(previous) = previous.filter(|state| state.version == STATE_VERSION) else {
        let state = DailyState {
            version: STATE_VERSION,
            day,
            daily_id: Uuid::new_v4().to_string(),
            first_pending: fresh_install(),
            last_sent_day: None,
            last_sent_version: None,
            install_week: None,
        };
        return (state, true);
    };

    if previous.day == day && is_random_id(&previous.daily_id) {
        return (previous, false);
    }

    let state = DailyState {
        day,
        daily_id: Uuid::new_v4().to_string(),
        ..previous
    };
    (state, true)
}

fn is_random_id(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value)
}

/// 小型运行状态放在启动锚点目录：不改已发布的设置结构，也不进历史备份。
fn write_state(path: &Path, state: &DailyState) -> Result<()> {
    let parent = path
        .parent()
        .context("usage state has no parent directory")?;
    fs::create_dir_all(parent).context("create usage state directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec(state)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).context("save usage state")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testing::{Reply, Server};

    fn at(time: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(time)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    fn sent(day: &str, version: &str) -> DailyState {
        DailyState {
            version: STATE_VERSION,
            day: day.to_owned(),
            daily_id: Uuid::new_v4().to_string(),
            first_pending: false,
            last_sent_day: Some(day.to_owned()),
            last_sent_version: Some(version.to_owned()),
            install_week: None,
        }
    }

    fn payload(kind: Kind) -> Payload {
        Payload {
            extensions: Vec::new(),
            daily_id: Uuid::new_v4().to_string(),
            kind,
            version: "2.0.0".to_owned(),
            system: "windows",
            arch: "x86_64",
            language: Language::EnUS,
            days_since_last: Some(3),
            install_week: None,
            previous_version: Some("1.4.0".to_owned()),
        }
    }

    #[test]
    fn same_day_restart_keeps_id_and_pending_first_report() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(STATE_FILENAME);
        let time = at("2026-09-30T12:00:00Z");

        let state = load_state(&path, time, || true).unwrap();
        let restored = load_state(&path, time, || panic!("state file exists")).unwrap();

        assert_eq!(restored.daily_id, state.daily_id);
        assert!(restored.first_pending);
    }

    #[test]
    fn next_utc_day_replaces_id_but_keeps_report_history() {
        let mut state = sent("2026-09-30", "2.0.0");
        state.install_week = Some("2026-W40".to_owned());
        let old = state.daily_id.clone();

        let (state, changed) = state_for_day(Some(state), at("2026-10-01T00:00:00Z"), || false);

        assert!(changed);
        assert_ne!(state.daily_id, old);
        assert_eq!(state.day, "2026-10-01");
        assert_eq!(state.last_sent_day.as_deref(), Some("2026-09-30"));
        assert_eq!(state.install_week.as_deref(), Some("2026-W40"));
    }

    #[test]
    fn corrupt_state_and_invalid_id_are_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(STATE_FILENAME);
        fs::write(&path, b"not json").unwrap();
        let time = at("2026-09-30T12:00:00Z");

        let mut state = load_state(&path, time, || false).unwrap();
        assert!(is_random_id(&state.daily_id));
        assert!(!state.first_pending);

        state.daily_id = "not-a-random-id".to_owned();
        let (state, changed) = state_for_day(Some(state), time, || true);
        assert!(changed);
        assert!(is_random_id(&state.daily_id));
        assert!(!state.first_pending);
    }

    /// 1.x 写下的状态文件 2.0 原样读回（新字段为空）；2.0 加的字段 1.x 读时会忽略。
    #[test]
    fn state_is_shared_with_1x() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(STATE_FILENAME);
        let id = Uuid::new_v4().to_string();
        fs::write(
            &path,
            format!(r#"{{"version":1,"day":"2026-10-02","dailyId":"{id}","firstPending":false}}"#),
        )
        .unwrap();

        let state = load_state(&path, at("2026-10-02T09:00:00Z"), || true).unwrap();

        assert_eq!(state.daily_id, id);
        assert!(!state.first_pending);
        assert_eq!(state.last_sent_day, None);
        assert_eq!(days_since_last(&state, date("2026-10-02")), None);

        let written = serde_json::to_value(sent("2026-10-02", "2.0.0")).unwrap();
        for key in ["version", "day", "dailyId", "firstPending"] {
            assert!(written.get(key).is_some(), "{key}");
        }
    }

    #[test]
    fn only_untouched_settings_count_as_fresh_install() {
        let mut settings = Settings::default();
        assert!(is_fresh_install(&settings));

        settings.update.last_checked_at = Some("2026-09-29T00:00:00Z".to_owned());
        assert!(!is_fresh_install(&settings));

        settings.update.last_checked_at = None;
        settings.onboarding.completed = true;
        assert!(!is_fresh_install(&settings));
    }

    #[test]
    fn reports_once_a_day_and_again_after_an_upgrade() {
        let today = date("2026-10-07");
        let done = sent("2026-10-07", "2.0.0");
        assert_eq!(kind_for(Trigger::Daily, &done, "2.0.0", today, true), None);
        assert_eq!(kind_for(Trigger::Check, &done, "2.0.0", today, true), None);
        assert_eq!(
            kind_for(Trigger::Daily, &done, "2.0.1", today, true),
            Some(Kind::Active)
        );

        let yesterday = sent("2026-10-06", "2.0.0");
        assert_eq!(
            kind_for(Trigger::Daily, &yesterday, "2.0.0", today, true),
            Some(Kind::Active)
        );
        // 关掉自动检查后只在检查更新时上报。
        assert_eq!(
            kind_for(Trigger::Daily, &yesterday, "2.0.0", today, false),
            None
        );
        assert_eq!(
            kind_for(Trigger::Check, &yesterday, "2.0.0", today, false),
            Some(Kind::Active)
        );

        let mut pending = sent("2026-10-06", "2.0.0");
        pending.first_pending = true;
        assert_eq!(
            kind_for(Trigger::Daily, &pending, "2.0.0", today, false),
            Some(Kind::First)
        );
    }

    #[test]
    fn gap_and_install_week() {
        let state = sent("2026-10-01", "2.0.0");
        assert_eq!(days_since_last(&state, date("2026-10-07")), Some(6));
        assert_eq!(days_since_last(&state, date("2026-09-30")), None);
        assert_eq!(iso_week(date("2026-10-07")), "2026-W41");
        assert_eq!(iso_week(date("2027-01-01")), "2026-W53");
    }

    #[test]
    fn payload_contains_exactly_the_ten_fields() {
        let json = serde_json::to_value(payload(Kind::First)).unwrap();
        let object = json.as_object().unwrap();

        assert_eq!(object.len(), 10);
        for key in [
            "dailyId",
            "kind",
            "version",
            "system",
            "arch",
            "language",
            "daysSinceLast",
            "installWeek",
            "previousVersion",
            "extensions",
        ] {
            assert!(object.contains_key(key), "{key}");
        }
        assert_eq!(json["kind"], "first");
        assert_eq!(json["extensions"], serde_json::json!([]));
        assert_eq!(json["language"], "en-US");
        assert_eq!(json["installWeek"], serde_json::Value::Null);
        assert_eq!(serde_json::to_value(Kind::Active).unwrap(), "active");
    }

    #[test]
    fn payload_reports_installed_extensions_without_protocol_or_paths() {
        let core = crate::testing::TestCore::start("2.0.0");
        let file = core.root().join("extension.exe");
        fs::write(&file, b"MZ fake extension").unwrap();
        core.block_on(core.core.install_extension(
            "ocr",
            "1.0.0",
            kwikpaste_ext_protocol::OCR_PROTOCOL,
            &file,
        ))
        .unwrap();
        core.block_on(core.core.set_extension_enabled("ocr", false))
            .unwrap();
        let mut payload = payload(Kind::Active);
        payload.extensions = installed_extensions(&core.core);
        let json = serde_json::to_value(payload).unwrap();
        assert_eq!(
            json["extensions"],
            serde_json::json!([{"id":"ocr","version":"1.0.0","enabled":false}])
        );
    }

    /// 开发构建和本机打的包默认不上报；只有发布流水线的正式构建发往线上（这里只比较地址，不发请求）。
    #[test]
    fn only_official_builds_report_by_default() {
        if cfg!(feature = "e2e-overrides") {
            return;
        }
        assert_eq!(endpoint(AppEnv::Dev), None);
        assert_eq!(
            endpoint(AppEnv::Prod).as_deref(),
            OFFICIAL_BUILD.then_some(ENDPOINT)
        );
    }

    #[test]
    fn real_http_transport_sends_only_contract_and_rejects_failure_or_redirect() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (status, ok) in [
            ("204 No Content", true),
            ("503 Service Unavailable", false),
            ("302 Found", false),
        ] {
            let server = Server::start(vec![
                Reply::status(status).header("Location", "http://127.0.0.1:1/must-not-follow"),
            ]);
            let endpoint = server.url("/api/v2/usage");

            let result = runtime.block_on(send_payload(&payload(Kind::Active), &endpoint));

            assert_eq!(result.is_ok(), ok, "{status}");
            let requests = server.requests();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].line.starts_with("POST /api/v2/usage HTTP/1.1"));
            let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(body.as_object().unwrap().len(), 10);
            assert_eq!(body["kind"], "active");
            assert_eq!(body["version"], "2.0.0");
            assert_eq!(body["daysSinceLast"], 3);
            assert_eq!(body["previousVersion"], "1.4.0");
        }
    }
}
