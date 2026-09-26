import type { FC, ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { HistoryOverview } from "@/commands";
import { cn } from "@/utils/cn";
import {
  formatCount,
  formatLongDate,
  inclusiveDaySpan,
} from "../../utils/storageOverview";

interface OverviewStatTilesProps {
  history: HistoryOverview;
}

/**
 * 概览数字行：记录总数、近 30 天新增、复用次数与记录跨度。
 */
const OverviewStatTiles: FC<OverviewStatTilesProps> = (props) => {
  const { t, i18n } = useTranslation("preferences");
  const { history } = props;
  const language = i18n.language;
  const { daily, oldestDate, totals } = history;
  const today = daily[daily.length - 1];
  const recentTotal = daily.reduce((total, day) => {
    return total + day.count;
  }, 0);
  const dailyAverage = daily.length > 0 ? recentTotal / daily.length : 0;
  const averageLabel = new Intl.NumberFormat(language, {
    maximumFractionDigits: dailyAverage < 10 ? 1 : 0,
  }).format(dailyAverage);
  const spanDays =
    oldestDate && today ? inclusiveDaySpan(oldestDate, today.date) : null;

  return (
    <div className="grid grid-cols-4 gap-3">
      <StatTile
        icon="i-lucide:layers"
        label={t("overview.tiles.total.label")}
        sub={t("overview.tiles.total.today", {
          value: formatCount(today?.count ?? 0, language),
        })}
        value={formatCount(totals.total, language)}
      />
      <StatTile
        icon="i-lucide:calendar-plus"
        label={t("overview.tiles.recent.label")}
        sub={t("overview.tiles.recent.average", { value: averageLabel })}
        value={formatCount(recentTotal, language)}
      />
      <StatTile
        icon="i-lucide:repeat"
        label={t("overview.tiles.reuses.label")}
        sub={t("overview.tiles.reuses.hint")}
        value={formatCount(totals.reuses, language)}
      />
      <StatTile
        icon="i-lucide:history"
        label={t("overview.tiles.span.label")}
        sub={
          oldestDate
            ? t("overview.tiles.span.since", {
                date: formatLongDate(oldestDate, language),
              })
            : t("overview.tiles.span.empty")
        }
        value={
          spanDays === null
            ? "--"
            : t("overview.tiles.span.value", {
                count: spanDays,
                days: formatCount(spanDays, language),
              })
        }
      />
    </div>
  );
};

interface StatTileProps {
  icon: string;
  label: ReactNode;
  sub: ReactNode;
  value: ReactNode;
}

/**
 * 单个数字卡：标签、主数值与一行补充说明。
 */
const StatTile: FC<StatTileProps> = (props) => {
  const { icon, label, sub, value } = props;

  return (
    <div className="kp-preference-panel min-w-0 rounded-2 border border-ant-border-secondary px-4 py-3">
      <div className="flex items-center gap-1.5 text-ant-secondary text-xs">
        <i aria-hidden="true" className={cn("shrink-0", icon)} />
        <span className="truncate">{label}</span>
      </div>
      <div className="mt-1.5 truncate font-semibold text-2xl text-ant-text leading-tight">
        {value}
      </div>
      <div className="mt-1 truncate text-ant-secondary text-xs">{sub}</div>
    </div>
  );
};

export default OverviewStatTiles;
