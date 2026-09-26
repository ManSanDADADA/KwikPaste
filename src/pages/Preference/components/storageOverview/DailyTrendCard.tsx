import { useSize } from "ahooks";
import type { FC } from "react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { DailyCount } from "@/commands";
import Tooltip from "@/components/Tooltip";
import { cn } from "@/utils/cn";
import {
  formatCount,
  formatLongDate,
  formatShortDate,
  niceCeiling,
} from "../../utils/storageOverview";
import OverviewCard from "./OverviewCard";

const Y_GUTTER = 32;
const PLOT_TOP = 8;
const PLOT_HEIGHT = 104;
const AXIS_LABEL_Y = PLOT_TOP + PLOT_HEIGHT + 17;
const CHART_HEIGHT = PLOT_TOP + PLOT_HEIGHT + 24;
const MAX_BAR_WIDTH = 24;
const BAR_RADIUS = 4;
/** 从今天往前每隔几天标一个日期刻度。 */
const X_TICK_STEP = 7;

interface DailyTrendCardProps {
  daily: DailyCount[];
}

/**
 * 近 30 天每日新增记录的柱状图；悬停柱子查看当天条数。
 */
const DailyTrendCard: FC<DailyTrendCardProps> = (props) => {
  const { t, i18n } = useTranslation("preferences");
  const { daily } = props;
  const language = i18n.language;
  const peak = daily.reduce<DailyCount | null>((current, day) => {
    if (day.count === 0) return current;
    if (!current || day.count >= current.count) return day;

    return current;
  }, null);

  return (
    <OverviewCard
      extra={
        peak ? (
          <span className="text-ant-secondary text-xs">
            {t("overview.trend.peak", {
              count: peak.count,
              date: formatLongDate(peak.date, language),
              value: formatCount(peak.count, language),
            })}
          </span>
        ) : null
      }
      icon="i-lucide:chart-column"
      subtitle={t("overview.trend.subtitle", { count: daily.length })}
      title={t("overview.trend.title")}
    >
      <TrendChart daily={daily} empty={peak === null} />
    </OverviewCard>
  );
};

interface TrendChartProps {
  daily: DailyCount[];
  empty: boolean;
}

/**
 * 按容器实际宽度绘制的 SVG 柱状图，柱宽封顶 24px、顶端 4px 圆角、底部贴基线。
 */
const TrendChart: FC<TrendChartProps> = (props) => {
  const { t, i18n } = useTranslation("preferences");
  const { daily, empty } = props;
  const language = i18n.language;
  const containerRef = useRef<HTMLDivElement | null>(null);
  const size = useSize(containerRef);
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null);
  const width = size?.width ?? 0;
  const maxCount = daily.reduce((max, day) => {
    return Math.max(max, day.count);
  }, 0);
  const axisMax = niceCeiling(maxCount);
  const plotWidth = Math.max(width - Y_GUTTER, 0);
  const slot = daily.length > 0 ? plotWidth / daily.length : 0;
  const barWidth = Math.min(MAX_BAR_WIDTH, Math.max(slot * 0.62, 2));
  // 中间那条只作参考线不标数，避免 2.5 这类非整数刻度。
  const gridValues = [0, axisMax / 2, axisMax];
  const lastIndex = daily.length - 1;

  return (
    <div className="relative h-34 w-full" ref={containerRef}>
      {width > 0 ? (
        <svg
          aria-label={t("overview.trend.title")}
          className="block overflow-visible"
          height={CHART_HEIGHT}
          role="img"
          viewBox={`0 0 ${width} ${CHART_HEIGHT}`}
          width={width}
        >
          {gridValues.map((value) => {
            const y = Math.round(valueToY(value, axisMax)) + 0.5;

            return (
              <g key={value}>
                <line
                  className="stroke-ant-split"
                  x1={Y_GUTTER}
                  x2={width}
                  y1={y}
                  y2={y}
                />
                {value === axisMax / 2 || (empty && value > 0) ? null : (
                  <text
                    className="fill-ant-text-tertiary text-xs tabular-nums"
                    dominantBaseline="middle"
                    textAnchor="end"
                    x={Y_GUTTER - 8}
                    y={y}
                  >
                    {formatCount(value, language)}
                  </text>
                )}
              </g>
            );
          })}

          {daily.map((day, index) => {
            const slotX = Y_GUTTER + index * slot;
            const barX = slotX + (slot - barWidth) / 2;
            const barHeight = (day.count / axisMax) * PLOT_HEIGHT;
            const dimmed = hoveredIndex !== null && hoveredIndex !== index;
            const showTick =
              (lastIndex - index) % X_TICK_STEP === 0 && index > 0;
            const tooltip = t("overview.trend.tooltip", {
              count: day.count,
              date: formatLongDate(day.date, language),
              value: formatCount(day.count, language),
            });
            const handleTooltipOpenChange = (open: boolean) => {
              setHoveredIndex((current) => {
                if (open) return index;

                return current === index ? null : current;
              });
            };

            return (
              <g key={day.date}>
                {day.count > 0 ? (
                  <path
                    className={cn(
                      "fill-ant-primary transition-opacity motion-reduce:transition-none",
                      { "opacity-40": dimmed },
                    )}
                    d={roundedTopBarPath(barX, barWidth, barHeight)}
                  />
                ) : null}

                {showTick ? (
                  <text
                    className="fill-ant-text-tertiary text-xs tabular-nums"
                    textAnchor="middle"
                    x={slotX + slot / 2}
                    y={AXIS_LABEL_Y}
                  >
                    {index === lastIndex
                      ? t("overview.trend.today")
                      : formatShortDate(day.date, language)}
                  </text>
                ) : null}

                <Tooltip
                  mouseEnterDelay={0}
                  mouseLeaveDelay={0}
                  onOpenChange={handleTooltipOpenChange}
                  title={tooltip}
                >
                  <rect
                    className="cursor-default fill-transparent"
                    height={PLOT_HEIGHT}
                    width={slot}
                    x={slotX}
                    y={PLOT_TOP}
                  />
                </Tooltip>
              </g>
            );
          })}
        </svg>
      ) : null}

      {empty ? (
        <div className="pointer-events-none absolute inset-x-0 top-0 flex h-28 items-center justify-center text-ant-secondary text-xs">
          {t("overview.trend.empty", { count: daily.length })}
        </div>
      ) : null}
    </div>
  );
};

/**
 * 数值映射到绘图区的纵坐标（基线在下）。
 */
function valueToY(value: number, axisMax: number) {
  return PLOT_TOP + PLOT_HEIGHT - (value / axisMax) * PLOT_HEIGHT;
}

/**
 * 顶端圆角、底部直角的柱子路径；矮柱子的圆角随高度收小。
 */
function roundedTopBarPath(x: number, width: number, height: number) {
  const baseline = PLOT_TOP + PLOT_HEIGHT;
  const top = baseline - height;
  const radius = Math.min(BAR_RADIUS, width / 2, height);

  return [
    `M ${x} ${baseline}`,
    `V ${top + radius}`,
    `Q ${x} ${top} ${x + radius} ${top}`,
    `H ${x + width - radius}`,
    `Q ${x + width} ${top} ${x + width} ${top + radius}`,
    `V ${baseline}`,
    "Z",
  ].join(" ");
}

export default DailyTrendCard;
