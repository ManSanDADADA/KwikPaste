import { Empty, Segmented } from "antd";
import type { FC } from "react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { CategoryStat } from "@/commands";
import Tooltip from "@/components/Tooltip";
import { cn } from "@/utils/cn";
import { CATEGORY_META, formatCount } from "../../utils/storageOverview";
import { formatBytes } from "../../utils/storageUsage";
import ClearScopeButton from "./ClearScopeButton";
import MeterBar from "./MeterBar";
import OverviewCard from "./OverviewCard";

type CategoryMetric = "count" | "bytes";

interface CategoryCardProps {
  categories: CategoryStat[];
  clearingKey: string | null;
  onClear: (stat: CategoryStat) => void;
}

/**
 * 内容构成：按类别比较条数或内容大小，行尾可清理该类普通记录。
 */
const CategoryCard: FC<CategoryCardProps> = (props) => {
  const { t, i18n } = useTranslation("preferences");
  const { categories, clearingKey, onClear } = props;
  const [metric, setMetric] = useState<CategoryMetric>("count");
  const language = i18n.language;
  const totalCount = categories.reduce((total, stat) => {
    return total + stat.count;
  }, 0);
  const totalBytes = categories.reduce((total, stat) => {
    return total + stat.bytes;
  }, 0);
  const rows = categories
    .filter((stat) => {
      return stat.count > 0;
    })
    .sort((left, right) => {
      return right[metric] - left[metric];
    });
  const maxValue = rows[0]?.[metric] ?? 0;
  const metricOptions: { label: string; value: CategoryMetric }[] = [
    { label: t("overview.categories.byCount"), value: "count" },
    { label: t("overview.categories.byBytes"), value: "bytes" },
  ];

  const handleMetricChange = (value: CategoryMetric) => {
    setMetric(value);
  };

  return (
    <OverviewCard
      extra={
        <Segmented<CategoryMetric>
          onChange={handleMetricChange}
          options={metricOptions}
          size="small"
          value={metric}
        />
      }
      icon="i-lucide:shapes"
      title={t("overview.categories.title")}
    >
      {rows.length === 0 ? (
        <Empty
          className="my-auto"
          description={t("overview.empty")}
          image={Empty.PRESENTED_IMAGE_SIMPLE}
        />
      ) : (
        <>
          <ul className="m-0 flex list-none flex-col p-0">
            {rows.map((stat) => {
              const meta = CATEGORY_META[stat.category];
              const label = t(meta.labelKey);
              const value =
                metric === "count"
                  ? formatCount(stat.count, language)
                  : formatBytes(stat.bytes);
              const share = shareLabel(
                metric === "count" ? stat.count : stat.bytes,
                metric === "count" ? totalCount : totalBytes,
              );
              const handleClear = () => {
                onClear(stat);
              };

              return (
                <li
                  className="group flex h-8 items-center gap-2"
                  key={stat.category}
                >
                  <Tooltip
                    placement="topLeft"
                    title={t("overview.categories.tooltip", {
                      count: stat.count,
                      share,
                      size: formatBytes(stat.bytes),
                      value: formatCount(stat.count, language),
                    })}
                  >
                    <div className="flex min-w-0 flex-1 items-center gap-2">
                      <i
                        aria-hidden="true"
                        className={cn(
                          "shrink-0 text-ant-secondary text-sm",
                          meta.icon,
                        )}
                      />
                      <span className="w-14 shrink-0 truncate text-sm">
                        {label}
                      </span>
                      <MeterBar
                        className="min-w-0 flex-1"
                        max={maxValue}
                        value={stat[metric]}
                      />
                      <span className="w-16 shrink-0 text-right text-ant-text text-xs tabular-nums">
                        {value}
                      </span>
                    </div>
                  </Tooltip>

                  <ClearScopeButton
                    label={label}
                    loading={clearingKey === `category:${stat.category}`}
                    onClick={handleClear}
                    removable={stat.removable}
                  />
                </li>
              );
            })}
          </ul>

          {metric === "bytes" ? (
            <p className="m-0 mt-auto pt-3 text-ant-secondary text-xs">
              {t("overview.categories.bytesNote")}
            </p>
          ) : null}
        </>
      )}
    </OverviewCard>
  );
};

/**
 * 占比文案：保留到整数，有值但不足 1% 时显示 `<1%`。
 */
function shareLabel(value: number, total: number) {
  if (total <= 0 || value <= 0) return "0%";

  const percent = (value / total) * 100;

  if (percent < 1) return "<1%";

  return `${Math.round(percent)}%`;
}

export default CategoryCard;
