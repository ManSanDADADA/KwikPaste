import { Empty } from "antd";
import type { FC } from "react";
import { useTranslation } from "react-i18next";
import type { OtherSourceApps, SourceAppStat } from "@/commands";
import AssetImage from "@/components/AssetImage";
import Tooltip from "@/components/Tooltip";
import { cn } from "@/utils/cn";
import { formatCount } from "../../utils/storageOverview";
import { formatBytes } from "../../utils/storageUsage";
import ClearScopeButton from "./ClearScopeButton";
import MeterBar from "./MeterBar";
import OverviewCard from "./OverviewCard";

interface SourceAppsCardProps {
  apps: SourceAppStat[];
  clearingKey: string | null;
  others: OtherSourceApps;
  onClear: (stat: SourceAppStat) => void;
}

/**
 * 来源应用排行：记录最多的几个应用，其余合并成一行说明。
 */
const SourceAppsCard: FC<SourceAppsCardProps> = (props) => {
  const { t, i18n } = useTranslation("preferences");
  const { apps, clearingKey, others, onClear } = props;
  const language = i18n.language;
  const maxCount = apps[0]?.count ?? 0;

  return (
    <OverviewCard
      icon="i-lucide:app-window"
      title={t("overview.sources.title")}
    >
      {apps.length === 0 ? (
        <Empty
          className="my-auto"
          description={t("overview.empty")}
          image={Empty.PRESENTED_IMAGE_SIMPLE}
        />
      ) : (
        <>
          <ul className="m-0 flex list-none flex-col p-0">
            {apps.map((stat) => {
              const name = stat.name ?? t("overview.sources.unknown");
              const handleClear = () => {
                onClear(stat);
              };

              return (
                <li
                  className="group flex h-10 items-center gap-2"
                  key={stat.appId ?? ""}
                >
                  <Tooltip
                    placement="topLeft"
                    title={t("overview.sources.tooltip", {
                      count: stat.count,
                      name,
                      size: formatBytes(stat.bytes),
                      value: formatCount(stat.count, language),
                    })}
                  >
                    <div className="flex min-w-0 flex-1 items-center gap-2.5">
                      <SourceAppIcon
                        iconPath={stat.iconPath}
                        known={stat.appId !== null}
                      />
                      <div className="min-w-0 flex-1">
                        <div className="flex items-baseline justify-between gap-3">
                          <span className="truncate text-sm">{name}</span>
                          <span className="shrink-0 text-ant-text text-xs tabular-nums">
                            {formatCount(stat.count, language)}
                          </span>
                        </div>
                        <MeterBar
                          className="mt-1"
                          max={maxCount}
                          value={stat.count}
                        />
                      </div>
                    </div>
                  </Tooltip>

                  <ClearScopeButton
                    label={name}
                    loading={clearingKey === `sourceApp:${stat.appId ?? ""}`}
                    onClick={handleClear}
                    removable={stat.removable}
                  />
                </li>
              );
            })}
          </ul>

          {others.apps > 0 ? (
            <p className="m-0 mt-auto pt-3 text-ant-secondary text-xs">
              {t("overview.sources.others", {
                apps: formatCount(others.apps, language),
                count: others.count,
                value: formatCount(others.count, language),
              })}
            </p>
          ) : null}
        </>
      )}
    </OverviewCard>
  );
};

interface SourceAppIconProps {
  iconPath: string | null;
  known: boolean;
}

/**
 * 来源应用图标；没有图标时用通用应用图标，未知来源用问号图标。
 */
const SourceAppIcon: FC<SourceAppIconProps> = (props) => {
  const { iconPath, known } = props;

  if (iconPath) {
    return (
      <AssetImage
        alt=""
        className="size-5 shrink-0 object-contain"
        src={iconPath}
      />
    );
  }

  return (
    <i
      aria-hidden="true"
      className={cn("shrink-0 text-ant-secondary text-lg", {
        "i-lucide:app-window": known,
        "i-lucide:circle-help": !known,
      })}
    />
  );
};

export default SourceAppsCard;
