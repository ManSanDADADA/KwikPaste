import { useSize } from "ahooks";
import { Button, Tag } from "antd";
import type { FC } from "react";
import { useId, useRef } from "react";
import { useTranslation } from "react-i18next";
import type {
  ReclaimableCache,
  StorageBreakdown,
  StorageUsage,
} from "@/commands";
import CustomIconButton from "@/components/CustomIconButton";
import Tooltip from "@/components/Tooltip";
import type { StorageLimitAction } from "@/types/settings";
import { cn } from "@/utils/cn";
import { STORAGE_WARNING_RATIO } from "../../constants";
import { BREAKDOWN_SEGMENTS } from "../../utils/storageOverview";
import { formatBytes } from "../../utils/storageUsage";
import OverviewCard from "./OverviewCard";

const BAR_TOP = 4;
const BAR_HEIGHT = 12;
const CHART_HEIGHT = BAR_TOP * 2 + BAR_HEIGHT;
const SEGMENT_GAP = 2;

type SpaceStatus = "ok" | "warning" | "over";

interface SpaceStatusMeta {
  color: string;
  icon: string;
}

const SPACE_STATUS_META: Record<SpaceStatus, SpaceStatusMeta> = {
  ok: { color: "success", icon: "i-lucide:circle-check" },
  over: { color: "error", icon: "i-lucide:circle-alert" },
  warning: { color: "warning", icon: "i-lucide:triangle-alert" },
};

interface StorageSpaceCardProps {
  breakdown: StorageBreakdown;
  cleaningCache: boolean;
  limitAction: StorageLimitAction;
  limitBytes: number;
  reclaimable: ReclaimableCache;
  refreshing: boolean;
  usage: StorageUsage;
  onAdjustLimit: () => void;
  onCleanCache: () => void;
  onOpenDirectory: () => void;
  onRefresh: () => void;
}

/**
 * 数据概览首屏：已用空间、相对上限的分段占用条，以及上限策略与可清理缓存。
 */
const StorageSpaceCard: FC<StorageSpaceCardProps> = (props) => {
  const { t } = useTranslation("preferences");
  const {
    breakdown,
    cleaningCache,
    limitAction,
    limitBytes,
    reclaimable,
    refreshing,
    usage,
    onAdjustLimit,
    onCleanCache,
    onOpenDirectory,
    onRefresh,
  } = props;
  const totalBytes = usage.totalBytes;
  const status = resolveSpaceStatus(totalBytes, limitBytes);
  const statusMeta = SPACE_STATUS_META[status];
  const percent = usagePercentLabel(totalBytes, limitBytes);

  return (
    <OverviewCard
      extra={
        <div className="flex items-center gap-1">
          <Tooltip title={t("overview.refresh")}>
            <CustomIconButton
              aria-label={t("overview.refresh")}
              icon={<i aria-hidden="true" className="i-lucide:refresh-cw" />}
              loading={refreshing}
              onClick={onRefresh}
              size="small"
              type="text"
            />
          </Tooltip>
          <Tooltip title={t("overview.openDirectory")}>
            <CustomIconButton
              aria-label={t("overview.openDirectory")}
              icon={<i aria-hidden="true" className="i-lucide:folder-open" />}
              onClick={onOpenDirectory}
              size="small"
              type="text"
            />
          </Tooltip>
        </div>
      }
      icon="i-lucide:hard-drive"
      title={t("overview.space.title")}
    >
      <div className="flex items-end justify-between gap-4">
        <div className="min-w-0">
          <div className="flex items-baseline gap-2">
            <span className="font-semibold text-3xl text-ant-text leading-none">
              {formatBytes(totalBytes)}
            </span>
            <span className="truncate text-ant-secondary text-sm">
              {t("overview.space.limit", { limit: formatBytes(limitBytes) })}
            </span>
          </div>
        </div>

        <Tag
          bordered={false}
          className="me-0"
          color={statusMeta.color}
          icon={
            <i aria-hidden="true" className={cn("me-1", statusMeta.icon)} />
          }
        >
          {t(`overview.space.status.${status}`, { percent })}
        </Tag>
      </div>

      <SpaceBar
        breakdown={breakdown}
        limitBytes={limitBytes}
        totalBytes={totalBytes}
      />

      <ul className="m-0 mt-2 flex list-none flex-wrap gap-x-5 gap-y-1.5 p-0">
        {BREAKDOWN_SEGMENTS.map((segment) => {
          return (
            <li className="flex items-center gap-1.5 text-xs" key={segment.key}>
              <span
                aria-hidden="true"
                className={cn("size-2 rounded-full", segment.swatchClass)}
              />
              <span className="text-ant-secondary">{t(segment.labelKey)}</span>
              <span className="font-medium text-ant-text tabular-nums">
                {formatBytes(breakdown[segment.key])}
              </span>
            </li>
          );
        })}
      </ul>

      <div className="mt-4 flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-ant-split border-t pt-3 text-xs">
        <div className="flex min-w-0 items-center gap-1.5 text-ant-secondary">
          <i aria-hidden="true" className="i-lucide:info shrink-0" />
          <span className="truncate">
            {t(`overview.space.limitAction.${limitAction}`)}
          </span>
          <Button
            className="h-auto p-0 text-xs"
            onClick={onAdjustLimit}
            size="small"
            type="link"
          >
            {t("overview.space.adjustLimit")}
          </Button>
        </div>

        {reclaimable.bytes > 0 ? (
          <div className="flex items-center gap-2">
            <span className="text-ant-secondary">
              {t("overview.space.reclaimable", {
                count: reclaimable.files,
                size: formatBytes(reclaimable.bytes),
              })}
            </span>
            <Button
              autoInsertSpace={false}
              loading={cleaningCache}
              onClick={onCleanCache}
              size="small"
            >
              {t("overview.space.cleanCache")}
            </Button>
          </div>
        ) : (
          <div className="flex items-center gap-1.5 text-ant-secondary">
            <i aria-hidden="true" className="i-lucide:sparkles" />
            <span>{t("overview.space.cacheClean")}</span>
          </div>
        )}
      </div>
    </OverviewCard>
  );
};

interface SpaceBarProps {
  breakdown: StorageBreakdown;
  limitBytes: number;
  totalBytes: number;
}

/**
 * 分段占用条：满格代表上限，超限时满格改为实际占用并标出上限位置。
 */
const SpaceBar: FC<SpaceBarProps> = (props) => {
  const { t } = useTranslation("preferences");
  const { breakdown, limitBytes, totalBytes } = props;
  const containerRef = useRef<HTMLDivElement | null>(null);
  const size = useSize(containerRef);
  const clipId = useId();
  const width = size?.width ?? 0;
  const scaleBytes = Math.max(limitBytes, totalBytes, 1);
  const segments = layoutSegments(breakdown, scaleBytes, width);
  const filledWidth = segments.reduce((total, segment) => {
    return Math.max(total, segment.x + segment.width);
  }, 0);
  const limitX =
    totalBytes > limitBytes
      ? Math.round((limitBytes / scaleBytes) * width)
      : null;

  return (
    <div className="mt-4 h-5 w-full" ref={containerRef}>
      {width > 0 ? (
        <svg
          aria-label={t("overview.space.title")}
          className="block overflow-visible"
          height={CHART_HEIGHT}
          role="img"
          viewBox={`0 0 ${width} ${CHART_HEIGHT}`}
          width={width}
        >
          <defs>
            <clipPath id={clipId}>
              <rect
                height={BAR_HEIGHT}
                rx={BAR_HEIGHT / 2}
                width={filledWidth}
                y={BAR_TOP}
              />
            </clipPath>
          </defs>

          <rect
            className="fill-ant-fill-tertiary"
            height={BAR_HEIGHT}
            rx={BAR_HEIGHT / 2}
            width={width}
            y={BAR_TOP}
          />

          <g clipPath={`url(#${clipId})`}>
            {segments.map((segment) => {
              return (
                <Tooltip
                  key={segment.meta.key}
                  title={`${t(segment.meta.labelKey)} · ${formatBytes(segment.bytes)} · ${t(segment.meta.hintKey)}`}
                >
                  <rect
                    className={segment.meta.fillClass}
                    height={BAR_HEIGHT}
                    width={segment.width}
                    x={segment.x}
                    y={BAR_TOP}
                  />
                </Tooltip>
              );
            })}
          </g>

          {limitX !== null ? (
            <Tooltip title={t("overview.space.limitMarker")}>
              <rect
                className="fill-ant-text"
                height={CHART_HEIGHT}
                width={2}
                x={limitX - 1}
              />
            </Tooltip>
          ) : null}
        </svg>
      ) : null}
    </div>
  );
};

interface LaidOutSegment {
  bytes: number;
  meta: (typeof BREAKDOWN_SEGMENTS)[number];
  width: number;
  x: number;
}

/**
 * 按占用把各分段换算成像素位置，相邻分段之间留 2px 间隙；再小的分段也至少占 1px。
 */
function layoutSegments(
  breakdown: StorageBreakdown,
  scaleBytes: number,
  width: number,
): LaidOutSegment[] {
  const visible = BREAKDOWN_SEGMENTS.filter((meta) => {
    return breakdown[meta.key] > 0;
  });
  let cursor = 0;

  return visible.map((meta, index) => {
    const bytes = breakdown[meta.key];
    const span = (bytes / scaleBytes) * width;
    const isLast = index === visible.length - 1;
    const segment = {
      bytes,
      meta,
      width: Math.max(isLast ? span : span - SEGMENT_GAP, 1),
      x: cursor,
    };

    cursor += Math.max(span, 1 + SEGMENT_GAP);

    return segment;
  });
}

/**
 * 占用相对上限的百分数；有占用但不足 1% 时显示 `<1`，避免读成空。
 */
function usagePercentLabel(totalBytes: number, limitBytes: number) {
  const percent = (totalBytes / limitBytes) * 100;

  if (totalBytes > 0 && percent < 1) return "<1";

  return String(Math.round(percent));
}

/**
 * 占用相对上限的状态：宽裕、接近上限（与侧栏同一阈值）或已超出。
 */
function resolveSpaceStatus(
  totalBytes: number,
  limitBytes: number,
): SpaceStatus {
  if (totalBytes > limitBytes) return "over";
  if (totalBytes >= limitBytes * STORAGE_WARNING_RATIO) return "warning";

  return "ok";
}

export default StorageSpaceCard;
