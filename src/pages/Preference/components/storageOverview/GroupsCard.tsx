import { Button } from "antd";
import type { FC } from "react";
import { useTranslation } from "react-i18next";
import type { GroupStat, ItemTotals } from "@/commands";
import ClipboardGroupIcon from "@/components/ClipboardGroupIcon";
import Tooltip from "@/components/Tooltip";
import { cn } from "@/utils/cn";
import { formatCount } from "../../utils/storageOverview";
import MeterBar from "./MeterBar";
import OverviewCard from "./OverviewCard";

interface MarkRow {
  icon: string;
  key: string;
  value: number;
}

interface GroupsCardProps {
  groups: GroupStat[];
  totals: ItemTotals;
  onManageGroups: () => void;
}

/**
 * 分组与标记：自定义分组的记录分布，以及收藏、置顶、备注等标记的数量。
 */
const GroupsCard: FC<GroupsCardProps> = (props) => {
  const { t, i18n } = useTranslation("preferences");
  const { groups, totals, onManageGroups } = props;
  const language = i18n.language;
  const maxGroupCount = groups.reduce((max, group) => {
    return Math.max(max, group.count);
  }, 0);
  const marks: MarkRow[] = [
    { icon: "i-lucide:star", key: "favorites", value: totals.favorites },
    { icon: "i-lucide:pin", key: "pinned", value: totals.pinned },
    { icon: "i-lucide:notebook-pen", key: "noted", value: totals.noted },
    {
      icon: "i-lucide:shield-alert",
      key: "sensitive",
      value: totals.sensitive,
    },
    {
      icon: "i-lucide:inbox",
      key: "ungrouped",
      value: Math.max(totals.total - totals.grouped, 0),
    },
  ];

  return (
    <OverviewCard
      extra={
        <Button
          className="h-auto p-0 text-xs"
          onClick={onManageGroups}
          size="small"
          type="link"
        >
          {t("overview.organize.manageGroups")}
        </Button>
      }
      icon="i-lucide:folder-tree"
      title={t("overview.organize.title")}
    >
      <div className="grid grid-cols-2 gap-x-8">
        <div className="min-w-0">
          <h4 className="m-0 mb-1 font-normal text-ant-secondary text-xs">
            {t("overview.organize.groups")}
          </h4>

          {groups.length === 0 ? (
            <p className="m-0 flex h-8 items-center text-ant-secondary text-sm">
              {t("overview.organize.noGroups")}
            </p>
          ) : (
            <ul className="m-0 flex list-none flex-col p-0">
              {groups.map((group) => {
                return (
                  <li className="flex h-8 items-center gap-2" key={group.id}>
                    <ClipboardGroupIcon
                      className="text-sm"
                      disabled={group.isHidden}
                      icon={group.icon}
                    />
                    <span
                      className={cn("w-20 shrink-0 truncate text-sm", {
                        "text-ant-secondary": group.isHidden,
                      })}
                    >
                      {group.name}
                    </span>
                    {group.isHidden ? (
                      <Tooltip title={t("overview.organize.hidden")}>
                        <i
                          aria-label={t("overview.organize.hidden")}
                          className="i-lucide:eye-off shrink-0 text-ant-tertiary text-xs"
                          role="img"
                        />
                      </Tooltip>
                    ) : null}
                    <MeterBar
                      className="min-w-0 flex-1"
                      max={maxGroupCount}
                      value={group.count}
                    />
                    <span className="w-12 shrink-0 text-right text-ant-text text-xs tabular-nums">
                      {formatCount(group.count, language)}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <div className="min-w-0">
          <h4 className="m-0 mb-1 font-normal text-ant-secondary text-xs">
            {t("overview.organize.marks")}
          </h4>

          <ul className="m-0 flex list-none flex-col p-0">
            {marks.map((mark) => {
              return (
                <li className="flex h-8 items-center gap-2" key={mark.key}>
                  <i
                    aria-hidden="true"
                    className={cn(
                      "shrink-0 text-ant-secondary text-sm",
                      mark.icon,
                    )}
                  />
                  <span className="w-20 shrink-0 truncate text-sm">
                    {t(`overview.organize.${mark.key}`)}
                  </span>
                  <MeterBar
                    className="min-w-0 flex-1"
                    max={totals.total}
                    value={mark.value}
                  />
                  <span className="w-12 shrink-0 text-right text-ant-text text-xs tabular-nums">
                    {formatCount(mark.value, language)}
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
      </div>
    </OverviewCard>
  );
};

export default GroupsCard;
