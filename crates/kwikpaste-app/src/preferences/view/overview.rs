//! 数据概览页：存储空间、四个数字卡、每日新增、内容构成与来源应用、分组与标记。
//! 卡片与其它偏好页的分组卡片同一套底色和描边；排行行悬停时才露出清理按钮。

use chrono::{Local, NaiveDate};
use gpui::{
    AnyElement, Context, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div, prelude::FluentBuilder as _, relative, rems,
};
use kwikpaste_core::{
    db::overview::{
        CategoryStat, ClearScope, ContentCategory, DailyCount, GroupStat, ItemTotals,
        OtherSourceApps, SourceAppStat,
    },
    ops::{PreferenceDirectory, StorageOverview},
    settings::StorageLimitAction,
};
use kwikpaste_ui::{
    Button, IconName, KpStyled as _, Tag, TagColor, TooltipExt as _, group_icon_path,
    theme::{self, SemanticTokens, TextSize, space},
};

use super::{Preferences, category_label};
use crate::{
    i18n,
    preferences::{icons::PrefIcon, text::format_bytes},
};

/// 排行行的悬停分组名：行内的清理按钮跟着整行显隐。
const ROW_GROUP: &str = "overview-row";
/// 柱状图每一天那一列的悬停分组名。
const TREND_GROUP: &str = "overview-trend-day";
/// 柱状图绘图区高度。
const TREND_HEIGHT: gpui::Rems = gpui::Rems(7.5);
/// 柱状图左侧刻度栏宽度。
const TREND_GUTTER: gpui::Rems = gpui::Rems(2.25);
/// 已用空间达到上限的这个比例时提示接近上限。
const SPACE_WARNING_RATIO: f64 = 0.8;

impl Preferences {
    pub(super) fn render_storage_overview(
        &self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = theme::semantic(cx);
        let Some(overview) = self.storage_overview.as_ref() else {
            return div()
                .flex()
                .flex_col()
                .items_center()
                .gap(space(3.))
                .pt(space(16.))
                .kp_text(TextSize::Sm)
                .text_color(tokens.text.muted)
                .child(PrefIcon::Loader.view(rems(1.5), tokens.text.faint))
                .child(i18n::t("preferences:storage.loading"))
                .into_any_element();
        };

        let history = &overview.history;
        let category_rows = self.category_rows(&history.categories, cx);
        let source_rows = self.source_rows(&history.source_apps, window, cx);
        div()
            .flex()
            .flex_col()
            .gap(space(4.))
            .child(self.storage_card(overview, cx))
            .child(stat_tiles(
                &history.totals,
                &history.daily,
                history.oldest_date,
                tokens,
            ))
            .child(trend_card(&history.daily, tokens))
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap(space(4.))
                    .child(ranking_card(
                        PrefIcon::Shapes,
                        i18n::t("preferences:overview.categories.title"),
                        category_rows,
                        None,
                        tokens,
                    ))
                    .child(ranking_card(
                        PrefIcon::AppWindow,
                        i18n::t("preferences:overview.sources.title"),
                        source_rows,
                        others_note(&history.other_source_apps),
                        tokens,
                    )),
            )
            .child(self.organize_card(&history.groups, &history.totals, cx))
            .into_any_element()
    }

    /// 存储空间：已用大小与上限、按类别分段的占用条、图例，底部是上限策略和可清理的缓存。
    fn storage_card(&self, overview: &StorageOverview, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let history_settings = &self.settings.clipboard.history;
        let limit = u64::from(history_settings.storage_limit_mb) * 1024 * 1024;
        let total = overview.usage.total_bytes;
        let ratio = if limit == 0 {
            0.
        } else {
            total as f64 / limit as f64
        };
        let percent = percent_label(ratio);
        let (status_key, status_color) = if ratio > 1. {
            ("over", TagColor::Error)
        } else if ratio >= SPACE_WARNING_RATIO {
            ("warning", TagColor::Warning)
        } else {
            ("ok", TagColor::Success)
        };
        let breakdown = &overview.breakdown;
        let segments = [
            ("database", breakdown.database_bytes, tokens.accent.solid),
            ("image", breakdown.image_bytes, tokens.hues.cyan_6),
            ("icon", breakdown.icon_bytes, tokens.hues.orange_6),
            ("other", breakdown.other_bytes, tokens.text.faint),
        ];
        // 满格代表上限；超出上限时满格改成实际占用，并在上限处画一条竖线。
        let scale = limit.max(total).max(1) as f32;
        let bar = div()
            .relative()
            .h(rems(0.625))
            .w_full()
            .child(
                div()
                    .flex()
                    .size_full()
                    .overflow_hidden()
                    .rounded_full()
                    .bg(tokens.fill.default)
                    .children(segments.iter().filter(|(_, bytes, _)| *bytes > 0).map(
                        |(_, bytes, color)| {
                            div().h_full().w(relative(*bytes as f32 / scale)).bg(*color)
                        },
                    )),
            )
            .when(limit > 0 && total > limit, |bar| {
                bar.child(
                    div()
                        .id("overview-limit-marker")
                        .absolute()
                        .top(rems(-0.25))
                        .left(relative(limit as f32 / scale))
                        .w(rems(0.125))
                        .h(rems(1.125))
                        .rounded_full()
                        .bg(tokens.text.primary)
                        .kp_tooltip(i18n::t("preferences:overview.space.limitMarker")),
                )
            });
        let legend = div()
            .flex()
            .flex_wrap()
            .gap_x(space(5.))
            .gap_y(space(1.5))
            .children(segments.iter().map(|(key, bytes, color)| {
                div()
                    .id(SharedString::from(format!("overview-segment-{key}")))
                    .flex()
                    .items_center()
                    .gap(space(1.5))
                    .kp_text(TextSize::Xs)
                    .kp_tooltip(i18n::t(&format!("preferences:overview.space.hints.{key}")))
                    .child(div().flex_none().size(rems(0.5)).rounded_full().bg(*color))
                    .child(
                        div()
                            .text_color(tokens.text.secondary)
                            .child(i18n::t(&format!(
                                "preferences:overview.space.segments.{key}"
                            ))),
                    )
                    .child(div().child(format_bytes(*bytes)))
            }));

        let limit_action = match history_settings.storage_limit_action {
            StorageLimitAction::Remind => "remind",
            StorageLimitAction::Cleanup => "cleanup",
        };
        // 放不下时缓存那一段换到下一行靠右，策略说明不截断。
        let policy = div()
            .flex()
            .flex_grow(1.)
            .min_w_0()
            .items_center()
            .gap(space(1.5))
            .text_color(tokens.text.secondary)
            .child(PrefIcon::Info.view(rems(0.875), tokens.text.muted))
            .child(div().min_w_0().child(i18n::t(&format!(
                "preferences:overview.space.limitAction.{limit_action}"
            ))))
            .child(
                Button::new(
                    "overview-adjust-limit",
                    i18n::t("preferences:overview.space.adjustLimit"),
                )
                .link()
                .xsmall()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.navigate_to(
                        super::TabId::Capture,
                        Some("localData.storageLimit"),
                        window,
                        cx,
                    );
                })),
            );
        let reclaimable = &overview.reclaimable;
        let cache = if reclaimable.bytes > 0 {
            div()
                .flex()
                .flex_none()
                .ml_auto()
                .items_center()
                .gap(space(2.))
                .text_color(tokens.text.secondary)
                .child(i18n::t_args(
                    "preferences:overview.space.reclaimable",
                    &[
                        ("count", &format_count(reclaimable.files)),
                        ("size", &format_bytes(reclaimable.bytes)),
                    ],
                ))
                .child(
                    Button::new(
                        "storage-clean",
                        i18n::t("preferences:overview.space.cleanCache"),
                    )
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.clean_resource_cache(cx))),
                )
        } else {
            div()
                .flex()
                .flex_none()
                .ml_auto()
                .items_center()
                .gap(space(1.5))
                .text_color(tokens.text.muted)
                .child(PrefIcon::Sparkles.view(rems(0.875), tokens.text.muted))
                .child(i18n::t("preferences:overview.space.cacheClean"))
        };

        overview_card(tokens)
            .child(card_header(
                PrefIcon::HardDrive,
                i18n::t("preferences:overview.space.title"),
                None,
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(space(1.))
                        .child(
                            Tag::new(i18n::t_args(
                                &format!("preferences:overview.space.status.{status_key}"),
                                &[("percent", &percent)],
                            ))
                            .color(status_color),
                        )
                        .child(
                            Button::icon(
                                "storage-open",
                                IconName::FolderOpen,
                                i18n::t("preferences:overview.openDirectory"),
                            )
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_directory(PreferenceDirectory::Data, cx);
                            })),
                        )
                        .into_any_element(),
                ),
                tokens,
            ))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(space(2.))
                    .mt(space(3.))
                    .child(
                        div()
                            .kp_text(TextSize::TwoXl)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format_bytes(total)),
                    )
                    .child(
                        div()
                            .kp_text(TextSize::Sm)
                            .text_color(tokens.text.muted)
                            .child(i18n::t_args(
                                "preferences:overview.space.limit",
                                &[("limit", &format_bytes(limit))],
                            )),
                    ),
            )
            .child(div().mt(space(3.)).child(bar))
            .child(div().mt(space(3.)).child(legend))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(space(4.))
                    .gap_y(space(2.))
                    .mt(space(4.))
                    .pt(space(3.))
                    .border_t_1()
                    .border_color(tokens.border.divider)
                    .kp_text(TextSize::Xs)
                    .child(policy)
                    .child(cache),
            )
            .into_any_element()
    }

    /// 内容构成：按条数从多到少，行尾可清理该类的普通记录。
    fn category_rows(
        &self,
        categories: &[CategoryStat],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = theme::semantic(cx);
        let total: u64 = categories.iter().map(|stat| stat.count).sum();
        let mut stats: Vec<&CategoryStat> =
            categories.iter().filter(|stat| stat.count > 0).collect();
        stats.sort_by_key(|stat| std::cmp::Reverse(stat.count));
        let max = stats.first().map_or(0, |stat| stat.count);

        let mut rows = Vec::with_capacity(stats.len());
        for stat in stats {
            let name = category_label(stat.category);
            let tooltip = i18n::t_count(
                "preferences:overview.categories.tooltip",
                count_arg(stat.count),
                &[
                    ("value", &format_count(stat.count)),
                    ("size", &format_bytes(stat.bytes)),
                    ("share", &share_label(stat.count, total)),
                ],
            );
            let clear = self.clear_button(
                format!("storage-category-{:?}", stat.category),
                ClearScope::Category {
                    category: stat.category,
                },
                i18n::t_args(
                    "preferences:overview.clear.categoryTitle",
                    &[("name", name.as_ref())],
                ),
                stat.count,
                stat.removable,
                cx,
            );
            rows.push(ranking_row(
                format!("overview-category-{:?}", stat.category),
                category_icon(stat.category)
                    .view(rems(1.), tokens.text.secondary)
                    .into_any_element(),
                name,
                stat.count,
                max,
                tooltip,
                clear,
                tokens,
            ));
        }
        rows
    }

    /// 来源应用排行：应用图标、名称和条数，同一应用的不同版本已在 core 里合并。
    fn source_rows(
        &self,
        apps: &[SourceAppStat],
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let tokens = theme::semantic(cx);
        let max = apps.iter().map(|stat| stat.count).max().unwrap_or(0);

        let mut rows = Vec::with_capacity(apps.len());
        for (index, stat) in apps.iter().enumerate() {
            let name: SharedString = stat
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .map(SharedString::from)
                .unwrap_or_else(|| i18n::t("preferences:overview.sources.unknown"));
            let icon = match stat.app_id {
                Some(_) => self.cached_app_icon(stat.icon_path.as_deref(), tokens, window, cx),
                None => div()
                    .flex()
                    .flex_none()
                    .size(rems(1.25))
                    .items_center()
                    .justify_center()
                    .child(PrefIcon::CircleHelp.view(rems(1.), tokens.text.muted))
                    .into_any_element(),
            };
            let tooltip = i18n::t_count(
                "preferences:overview.sources.tooltip",
                count_arg(stat.count),
                &[
                    ("name", name.as_ref()),
                    ("value", &format_count(stat.count)),
                    ("size", &format_bytes(stat.bytes)),
                ],
            );
            let scope = match stat.app_id {
                Some(_) => ClearScope::SourceApp {
                    app_ids: stat.app_ids.clone(),
                },
                None => ClearScope::UnknownSource,
            };
            let clear = self.clear_button(
                format!("storage-source-{index}"),
                scope,
                i18n::t_args(
                    "preferences:overview.clear.sourceAppTitle",
                    &[("name", name.as_ref())],
                ),
                stat.count,
                stat.removable,
                cx,
            );
            rows.push(ranking_row(
                format!("overview-source-{index}"),
                icon,
                name,
                stat.count,
                max,
                tooltip,
                clear,
                tokens,
            ));
        }
        rows
    }

    /// 排行行尾的清理按钮：全是收藏或置顶时禁用，提示里说明原因。
    fn clear_button(
        &self,
        id: String,
        scope: ClearScope,
        title: SharedString,
        count: u64,
        removable: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = i18n::t_args(
            if removable == count {
                "preferences:overview.clear.content"
            } else {
                "preferences:overview.clear.contentWithKept"
            },
            &[
                ("value", &format_count(removable)),
                ("kept", &format_count(count.saturating_sub(removable))),
            ],
        );
        let label = if removable == 0 {
            i18n::t("preferences:overview.clear.nothing")
        } else {
            i18n::t("preferences:overview.clear.tooltip")
        };
        Button::icon(SharedString::from(id), IconName::Trash2, label)
            .small()
            .disabled(removable == 0)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.clear_storage_scope(scope.clone(), title.clone(), content.clone(), window, cx);
            }))
            .into_any_element()
    }

    /// 分组与标记：左边是自定义分组的条数，右边是收藏、置顶等标记占全部记录的比例。
    fn organize_card(
        &self,
        groups: &[GroupStat],
        totals: &ItemTotals,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = theme::semantic(cx);
        let group_max = groups.iter().map(|group| group.count).max().unwrap_or(0);
        let group_rows = groups.iter().take(6).map(|group| {
            let icon = gpui::svg()
                .path(group_icon_path(&group.icon))
                .size(rems(1.))
                .flex_none()
                .text_color(tokens.text.secondary);
            let name = div()
                .flex()
                .min_w_0()
                .items_center()
                .gap(space(1.5))
                .child(div().min_w_0().truncate().child(group.name.clone()))
                .when(group.is_hidden, |name| {
                    name.child(PrefIcon::EyeOff.view(rems(0.75), tokens.text.muted))
                });
            meter_row(
                icon.into_any_element(),
                name.into_any_element(),
                group.count,
                group_max,
                tokens,
            )
        });
        let groups_column = div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .min_w_0()
            .child(column_title(
                i18n::t("preferences:overview.organize.groups"),
                tokens,
            ))
            .when(groups.is_empty(), |column| {
                column.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space(2.))
                        .h(rems(2.25))
                        .kp_text(TextSize::Sm)
                        .text_color(tokens.text.muted)
                        .child(i18n::t("preferences:overview.organize.noGroups"))
                        .child(
                            Button::new(
                                "overview-manage-groups",
                                i18n::t("preferences:overview.organize.manageGroups"),
                            )
                            .link()
                            .xsmall()
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.open_group_manager(window, cx);
                                },
                            )),
                        ),
                )
            })
            .children(group_rows);

        let marks = [
            (PrefIcon::Star, "favorites", totals.favorites),
            (PrefIcon::Pin, "pinned", totals.pinned),
            (PrefIcon::NotebookPen, "noted", totals.noted),
            (PrefIcon::ShieldAlert, "sensitive", totals.sensitive),
            (
                PrefIcon::Inbox,
                "ungrouped",
                totals.total.saturating_sub(totals.grouped),
            ),
        ];
        let marks_column = div()
            .flex()
            .flex_col()
            .gap(space(1.))
            .min_w_0()
            .child(column_title(
                i18n::t("preferences:overview.organize.marks"),
                tokens,
            ))
            .children(marks.into_iter().map(|(icon, key, count)| {
                meter_row(
                    icon.view(rems(1.), tokens.text.secondary)
                        .into_any_element(),
                    div()
                        .truncate()
                        .child(i18n::t(&format!("preferences:overview.organize.{key}")))
                        .into_any_element(),
                    count,
                    totals.total,
                    tokens,
                )
            }));

        overview_card(tokens)
            .child(card_header(
                PrefIcon::FolderTree,
                i18n::t("preferences:overview.organize.title"),
                None,
                None,
                tokens,
            ))
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap(space(8.))
                    .mt(space(3.))
                    .child(groups_column)
                    .child(marks_column),
            )
            .into_any_element()
    }
}

/// 概览卡片：与设置分组卡片同样的淡底、细描边和圆角。
fn overview_card(tokens: &SemanticTokens) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .p(space(4.))
        .rounded(theme::radius::LG)
        .bg(tokens.fill.faint)
        .border_1()
        .border_color(tokens.border.subtle)
}

/// 卡片标题行：主色图标、加粗标题、可选的灰色副标题，右侧放操作。
fn card_header(
    icon: PrefIcon,
    title: SharedString,
    subtitle: Option<SharedString>,
    extra: Option<AnyElement>,
    tokens: &SemanticTokens,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(space(3.))
        .min_h(rems(1.5))
        .child(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap(space(2.))
                .child(icon.view(rems(1.), tokens.accent.solid))
                .child(
                    div()
                        .flex_none()
                        .kp_text(TextSize::Sm)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(subtitle.map(|subtitle| {
                    div()
                        .min_w_0()
                        .truncate()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.muted)
                        .child(subtitle)
                })),
        )
        .children(extra.map(|extra| div().flex_none().child(extra)))
}

/// 四个数字卡：记录总数、近 30 天新增、复用次数、记录跨度。
fn stat_tiles(
    totals: &ItemTotals,
    daily: &[DailyCount],
    oldest: Option<NaiveDate>,
    tokens: &SemanticTokens,
) -> AnyElement {
    let today = daily
        .last()
        .map_or_else(|| Local::now().date_naive(), |day| day.date);
    let today_count = daily.last().map_or(0, |day| day.count);
    let recent: u64 = daily.iter().map(|day| day.count).sum();
    let average = recent as f64 / daily.len().max(1) as f64;
    let average = if average < 10. {
        format!("{average:.1}")
    } else {
        format!("{average:.0}")
    };
    let (span_value, span_hint) = match oldest {
        Some(oldest) => {
            let days = (today - oldest).num_days().max(0) + 1;
            (
                i18n::t_count(
                    "preferences:overview.tiles.span.value",
                    days,
                    &[("days", &format_count(days as u64))],
                )
                .to_string(),
                i18n::t_args(
                    "preferences:overview.tiles.span.since",
                    &[("date", &oldest.format("%Y-%m-%d").to_string())],
                ),
            )
        }
        None => (
            "--".to_owned(),
            i18n::t("preferences:overview.tiles.span.empty"),
        ),
    };
    let tiles = [
        (
            PrefIcon::Layers,
            i18n::t("preferences:overview.tiles.total.label"),
            format_count(totals.total),
            i18n::t_args(
                "preferences:overview.tiles.total.today",
                &[("value", &format_count(today_count))],
            ),
        ),
        (
            PrefIcon::CalendarPlus,
            i18n::t("preferences:overview.tiles.recent.label"),
            format_count(recent),
            i18n::t_args(
                "preferences:overview.tiles.recent.average",
                &[("value", &average)],
            ),
        ),
        (
            PrefIcon::Repeat,
            i18n::t("preferences:overview.tiles.reuses.label"),
            format_count(totals.reuses),
            i18n::t("preferences:overview.tiles.reuses.hint"),
        ),
        (
            PrefIcon::History,
            i18n::t("preferences:overview.tiles.span.label"),
            span_value,
            span_hint,
        ),
    ];
    div()
        .grid()
        .grid_cols(4)
        .gap(space(4.))
        .children(tiles.into_iter().map(|(icon, label, value, hint)| {
            overview_card(tokens)
                .px(space(4.))
                .py(space(3.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space(1.5))
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.secondary)
                        .child(icon.view(rems(0.875), tokens.text.muted))
                        .child(div().min_w_0().truncate().child(label)),
                )
                .child(
                    div()
                        .mt(space(1.5))
                        .truncate()
                        .kp_text(TextSize::TwoXl)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(value),
                )
                .child(
                    div()
                        .mt(space(0.5))
                        .truncate()
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.muted)
                        .child(hint),
                )
        }))
        .into_any_element()
}

/// 每日新增：近 30 天每天一根柱子，左侧刻度、底部每隔 7 天一个日期，悬停看当天条数。
fn trend_card(daily: &[DailyCount], tokens: &SemanticTokens) -> AnyElement {
    let max = daily.iter().map(|day| day.count).max().unwrap_or(0);
    let axis_max = nice_ceiling(max);
    let peak = daily
        .iter()
        .filter(|day| day.count > 0)
        .max_by_key(|day| (day.count, day.date));
    let days = daily.len();
    let last = days.saturating_sub(1);

    let gridline = |top: f32| {
        div()
            .absolute()
            .left_0()
            .right_0()
            .top(relative(top))
            .h(gpui::px(1.))
            .bg(tokens.border.divider)
    };
    let bars = daily.iter().enumerate().map(|(index, day)| {
        let tooltip = i18n::t_count(
            "preferences:overview.trend.tooltip",
            count_arg(day.count),
            &[
                ("date", &day.date.format("%Y-%m-%d").to_string()),
                ("value", &format_count(day.count)),
            ],
        );
        div()
            .id(("overview-trend", index))
            .group(TREND_GROUP)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_end()
            .rounded_t(theme::radius::XS)
            .hover(|style| style.bg(tokens.fill.faint))
            .kp_tooltip(tooltip)
            .when(day.count > 0, |column| {
                column.child(
                    div()
                        .w(relative(0.62))
                        .max_w(rems(1.5))
                        .min_h(gpui::px(2.))
                        .h(relative(day.count as f32 / axis_max as f32))
                        .rounded_t(theme::radius::XS)
                        .bg(tokens.accent.solid)
                        .group_hover(TREND_GROUP, |style| style.bg(tokens.accent.hover)),
                )
            })
    });
    let ticks = daily.iter().enumerate().filter_map(|(index, day)| {
        if index == 0 || !(last - index).is_multiple_of(7) {
            return None;
        }
        let label = if index == last {
            i18n::t("preferences:overview.trend.today")
        } else {
            day.date.format("%m/%d").to_string().into()
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left(relative((index as f32 + 0.5) / days.max(1) as f32))
                .ml(rems(-2.))
                .w(rems(4.))
                .flex()
                .justify_center()
                .child(label),
        )
    });
    let chart = div()
        .flex()
        .flex_col()
        .gap(space(1.5))
        .mt(space(4.))
        .kp_text(TextSize::Xs)
        .text_color(tokens.text.muted)
        .child(
            div()
                .flex()
                .h(TREND_HEIGHT)
                .child(div().relative().flex_none().w(TREND_GUTTER).h_full().when(
                    peak.is_some(),
                    |gutter| {
                        gutter
                            .child(
                                div()
                                    .absolute()
                                    .top(rems(-0.5))
                                    .right(space(2.))
                                    .child(format_count(axis_max)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .bottom(rems(-0.5))
                                    .right(space(2.))
                                    .child("0"),
                            )
                    },
                ))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(gridline(0.))
                        .when(peak.is_some(), |plot| plot.child(gridline(0.5)))
                        .child(gridline(1.).mt(gpui::px(-1.)))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .items_end()
                                .children(bars),
                        )
                        .when(peak.is_none(), |plot| {
                            plot.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .kp_text(TextSize::Sm)
                                    .child(i18n::t_args(
                                        "preferences:overview.trend.empty",
                                        &[("count", &days.to_string())],
                                    )),
                            )
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .h(rems(1.))
                .child(div().flex_none().w(TREND_GUTTER))
                .child(div().relative().flex_1().min_w_0().children(ticks)),
        );

    overview_card(tokens)
        .child(card_header(
            PrefIcon::ChartColumn,
            i18n::t("preferences:overview.trend.title"),
            Some(i18n::t_args(
                "preferences:overview.trend.subtitle",
                &[("count", &days.to_string())],
            )),
            peak.map(|peak| {
                div()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.secondary)
                    .child(i18n::t_args(
                        "preferences:overview.trend.peak",
                        &[
                            ("date", &peak.date.format("%m/%d").to_string()),
                            ("value", &format_count(peak.count)),
                        ],
                    ))
                    .into_any_element()
            }),
            tokens,
        ))
        .child(chart)
        .into_any_element()
}

/// 排行卡片（内容构成、来源应用）：没有数据时给一句空状态，可带一行底部说明。
fn ranking_card(
    icon: PrefIcon,
    title: SharedString,
    rows: Vec<AnyElement>,
    footer: Option<SharedString>,
    tokens: &SemanticTokens,
) -> AnyElement {
    let empty = rows.is_empty();
    overview_card(tokens)
        .child(card_header(icon, title, None, None, tokens))
        .child(
            div()
                .flex()
                .flex_col()
                .mt(space(2.))
                .mx(space(-2.))
                .when(empty, |list| {
                    list.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(rems(6.))
                            .kp_text(TextSize::Sm)
                            .text_color(tokens.text.muted)
                            .child(i18n::t("preferences:overview.empty")),
                    )
                })
                .children(rows),
        )
        .children(footer.map(|footer| {
            div()
                .mt_auto()
                .pt(space(3.))
                .kp_text(TextSize::Xs)
                .text_color(tokens.text.muted)
                .child(footer)
        }))
        .into_any_element()
}

/// 一行排行：图标、名称与条数、下面一条比例条；悬停整行时露出行尾的清理按钮。
#[allow(clippy::too_many_arguments)]
fn ranking_row(
    id: String,
    icon: AnyElement,
    name: SharedString,
    count: u64,
    max: u64,
    tooltip: SharedString,
    clear: AnyElement,
    tokens: &SemanticTokens,
) -> AnyElement {
    div()
        .group(ROW_GROUP)
        .flex()
        .items_center()
        .gap(space(2.))
        .h(rems(2.75))
        .px(space(2.))
        .rounded(theme::radius::MD)
        .hover(|style| style.bg(tokens.fill.hover))
        .child(
            div()
                .id(SharedString::from(id))
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(space(2.5))
                .kp_tooltip(tooltip)
                .child(icon)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(space(1.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(space(3.))
                                .kp_text(TextSize::Sm)
                                .child(div().min_w_0().truncate().child(name))
                                .child(
                                    div()
                                        .flex_none()
                                        .kp_text(TextSize::Xs)
                                        .text_color(tokens.text.secondary)
                                        .child(format_count(count)),
                                ),
                        )
                        .child(meter(count, max, tokens.accent.solid, tokens)),
                ),
        )
        .child(
            div()
                .flex_none()
                .opacity(0.)
                .group_hover(ROW_GROUP, |style| style.opacity(1.))
                .child(clear),
        )
        .into_any_element()
}

/// 分组与标记里的一行：图标、名称、比例条和条数，一行排开。
fn meter_row(
    icon: AnyElement,
    name: AnyElement,
    count: u64,
    max: u64,
    tokens: &SemanticTokens,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(space(2.5))
        .h(rems(2.25))
        .kp_text(TextSize::Sm)
        .child(icon)
        .child(div().flex().w(rems(6.)).flex_none().min_w_0().child(name))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(meter(count, max, tokens.accent.solid, tokens)),
        )
        .child(
            div()
                .flex_none()
                .min_w(rems(2.))
                .flex()
                .justify_end()
                .kp_text(TextSize::Xs)
                .text_color(tokens.text.secondary)
                .child(format_count(count)),
        )
        .into_any_element()
}

/// 横向比例条：有值但不足 2% 时按 2% 画，保证细条可见。
fn meter(value: u64, max: u64, color: Hsla, tokens: &SemanticTokens) -> gpui::Div {
    let ratio = if max == 0 || value == 0 {
        0.
    } else {
        (value as f32 / max as f32).clamp(0.02, 1.)
    };
    div()
        .h(rems(0.375))
        .w_full()
        .overflow_hidden()
        .rounded_full()
        .bg(tokens.fill.default)
        .when(ratio > 0., |track| {
            track.child(div().h_full().w(relative(ratio)).rounded_full().bg(color))
        })
}

fn column_title(title: SharedString, tokens: &SemanticTokens) -> gpui::Div {
    div()
        .kp_text(TextSize::Xs)
        .text_color(tokens.text.muted)
        .pb(space(1.))
        .child(title)
}

fn others_note(others: &OtherSourceApps) -> Option<SharedString> {
    (others.apps > 0).then(|| {
        i18n::t_args(
            "preferences:overview.sources.others",
            &[
                ("apps", &format_count(others.apps)),
                ("value", &format_count(others.count)),
            ],
        )
    })
}

fn category_icon(category: ContentCategory) -> PrefIcon {
    match category {
        ContentCategory::Text => PrefIcon::ClipboardType,
        ContentCategory::Html => PrefIcon::FileCode,
        ContentCategory::Rtf => PrefIcon::FileType,
        ContentCategory::Url => PrefIcon::Link,
        ContentCategory::Email => PrefIcon::Mail,
        ContentCategory::Color => PrefIcon::Palette,
        ContentCategory::Path => PrefIcon::FolderOpen,
        ContentCategory::Image => PrefIcon::FileImage,
        ContentCategory::Files => PrefIcon::Files,
    }
}

/// 整数加千分位：`12345` → `12,345`。
fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// 复数选择用的条数；超出 i64 的部分不影响单复数。
fn count_arg(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// 占比：取整，有值但不足 1% 时写 `<1%`。
fn share_label(value: u64, total: u64) -> String {
    if total == 0 || value == 0 {
        return "0%".to_owned();
    }
    let percent = value as f64 / total as f64 * 100.;
    if percent < 1. {
        "<1%".to_owned()
    } else {
        format!("{percent:.0}%")
    }
}

/// 已用比例的百分数：有占用但不足 1% 时写 `<1`。
fn percent_label(ratio: f64) -> String {
    let percent = ratio * 100.;
    if percent > 0. && percent < 1. {
        "<1".to_owned()
    } else {
        format!("{percent:.0}")
    }
}

/// 纵轴上限：不到 10 时就是最大值本身，否则取 1、1.2、1.5、2、2.5、3、4、5、6、8 × 10ⁿ 里
/// 不小于最大值的那个，刻度是整数又不会把柱子压得太矮。
fn nice_ceiling(value: u64) -> u64 {
    if value < 10 {
        return value.max(1);
    }
    let mut magnitude = 10_u64;
    while magnitude.saturating_mul(10) <= value {
        magnitude *= 10;
    }
    [10, 12, 15, 20, 25, 30, 40, 50, 60, 80, 100]
        .into_iter()
        .map(|step| step * (magnitude / 10))
        .find(|candidate| *candidate >= value)
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn axis_ceiling_is_a_round_number() {
        assert_eq!(nice_ceiling(0), 1);
        assert_eq!(nice_ceiling(3), 3);
        assert_eq!(nice_ceiling(10), 10);
        assert_eq!(nice_ceiling(18), 20);
        assert_eq!(nice_ceiling(53), 60);
        assert_eq!(nice_ceiling(100), 100);
        assert_eq!(nice_ceiling(101), 120);
        assert_eq!(nice_ceiling(1_234), 1_500);
    }

    #[test]
    fn small_shares_stay_visible() {
        assert_eq!(share_label(0, 10), "0%");
        assert_eq!(share_label(1, 1_000), "<1%");
        assert_eq!(share_label(1, 3), "33%");
        assert_eq!(percent_label(0.), "0");
        assert_eq!(percent_label(0.004), "<1");
        assert_eq!(percent_label(0.8), "80");
    }
}
