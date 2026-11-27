//! 自动检查始终开启：启动 8 秒后首次检查，之后每小时醒一次，距上次成功检查满 6 小时（按墙钟）才再查；
//! 失败的检查不算，下一小时重试。墙钟比较让系统睡眠的时间也计入间隔（单调时钟在 macOS 睡眠时不走）。
//! 可静默更新时先后台下载，等待期间每分钟检查一次宿主空闲状态，不弹窗、不显示徽标；版本发布满 3 天
//! 还没静默装上（一直没等到空闲）就改走提示。提示每个版本先弹一次，之后每隔 3 天再弹。

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

pub(crate) const INITIAL_DELAY: Duration = Duration::from_secs(8);
pub(crate) const TICK: Duration = Duration::from_secs(60 * 60);
pub(crate) const IDLE_POLL: Duration = Duration::from_secs(60);
pub(crate) const INSTALL_IDLE: Duration = Duration::from_secs(10 * 60);
const CHECK_INTERVAL: TimeDelta = TimeDelta::hours(6);
const SILENT_GRACE: TimeDelta = TimeDelta::days(3);
const REPROMPT_AFTER: TimeDelta = TimeDelta::days(3);

/// 本次运行还没成功检查过，或距上次成功检查已满间隔（时钟倒退也算到期）。
pub(crate) fn check_due(last_success: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_success.is_none_or(|at| elapsed_or_rewound(at, now, CHECK_INTERVAL))
}

/// 版本发布（清单的 `pub_date`）已满静默等待期；没有发布时间时不算超期。
pub(crate) fn silent_overdue(published: Option<&str>, now: DateTime<Utc>) -> bool {
    published
        .and_then(parse)
        .is_some_and(|at| now.signed_duration_since(at) >= SILENT_GRACE)
}

/// 该弹更新窗了：这个版本还没弹过，或上次弹出已满间隔（没记时间、时钟倒退都算到期）。
pub(crate) fn prompt_due(
    prompted_version: Option<&str>,
    prompted_at: Option<&str>,
    version: &str,
    now: DateTime<Utc>,
) -> bool {
    prompted_version != Some(version)
        || prompted_at
            .and_then(parse)
            .is_none_or(|at| elapsed_or_rewound(at, now, REPROMPT_AFTER))
}

fn elapsed_or_rewound(at: DateTime<Utc>, now: DateTime<Utc>, interval: TimeDelta) -> bool {
    let elapsed = now.signed_duration_since(at);
    elapsed >= interval || elapsed < TimeDelta::zero()
}

fn parse(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_are_due_six_wall_clock_hours_after_the_last_success() {
        let at = DateTime::parse_from_rfc3339("2026-10-14T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(check_due(None, at));
        assert!(!check_due(Some(at), at + TimeDelta::hours(5)));
        assert!(check_due(Some(at), at + TimeDelta::hours(6)));
        assert!(check_due(Some(at), at - TimeDelta::hours(1)));
    }

    #[test]
    fn silent_updates_fall_back_three_days_after_release_and_prompts_repeat_every_three_days() {
        let published = "2026-10-14T00:00:00Z";
        let at = parse(published).unwrap();
        assert!(!silent_overdue(Some(published), at + TimeDelta::hours(71)));
        assert!(silent_overdue(Some(published), at + TimeDelta::days(3)));
        assert!(!silent_overdue(None, at + TimeDelta::days(30)));

        let now = at + TimeDelta::days(1);
        assert!(prompt_due(None, None, "2.0.1", now));
        assert!(prompt_due(Some("2.0.0"), Some(published), "2.0.1", now));
        assert!(!prompt_due(Some("2.0.1"), Some(published), "2.0.1", now));
        assert!(prompt_due(
            Some("2.0.1"),
            Some(published),
            "2.0.1",
            at + TimeDelta::days(3)
        ));
        assert!(prompt_due(Some("2.0.1"), None, "2.0.1", now));
        assert!(prompt_due(
            Some("2.0.1"),
            Some(published),
            "2.0.1",
            at - TimeDelta::hours(1)
        ));
    }
}
