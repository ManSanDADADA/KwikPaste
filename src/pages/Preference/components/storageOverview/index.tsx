import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useDebounceFn, useMount } from "ahooks";
import { Button, Spin } from "antd";
import type { FC } from "react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  acquireWindowKeepalive,
  type CategoryStat,
  type ClearScope,
  cleanResourceCache,
  clearClipboardItemsInScope,
  getStorageOverview,
  openPreferenceDirectory,
  releaseWindowKeepalive,
  type SourceAppStat,
  type StorageOverview,
  type StorageUsage,
} from "@/commands";
import { TAURI_EVENT } from "@/constants/events";
import { useTauriListen } from "@/hooks/useTauriListen";
import type { Settings } from "@/types/settings";
import { getModalApi } from "@/utils/feedback";
import { log } from "@/utils/log";
import { formatCount } from "../../utils/storageOverview";
import { storageLimitBytes } from "../../utils/storageUsage";
import CategoryCard from "./CategoryCard";
import DailyTrendCard from "./DailyTrendCard";
import GroupsCard from "./GroupsCard";
import OverviewStatTiles from "./OverviewStatTiles";
import SourceAppsCard from "./SourceAppsCard";
import StorageSpaceCard from "./StorageSpaceCard";

export const STORAGE_OVERVIEW_SETTING_ID = "overview.dashboard";
const STORAGE_LIMIT_SETTING_ID = "localData.storageLimit";
const CUSTOM_GROUPS_SETTING_ID = "organizing.customGroups";
const KEEPALIVE_TIMEOUT_MS = 120_000;
/** 采集新记录时合并连续的刷新请求。 */
const REFRESH_DEBOUNCE_MS = 800;

type OverviewStatus = "loading" | "ready" | "error";

interface StorageOverviewPanelProps {
  settings: Settings;
  onNavigateSetting: (settingId: string) => void;
  onStorageUsageChange: (usage: StorageUsage) => void;
}

/**
 * 偏好页「数据概览」：存储空间、记录数量、每日趋势、内容构成、来源应用与分组，
 * 并提供按类别 / 来源清理和清理未引用缓存的入口。
 */
const StorageOverviewPanel: FC<StorageOverviewPanelProps> = (props) => {
  const { t, i18n } = useTranslation(["preferences", "common"]);
  const { settings, onNavigateSetting, onStorageUsageChange } = props;
  const [overview, setOverview] = useState<StorageOverview | null>(null);
  const [status, setStatus] = useState<OverviewStatus>("loading");
  const [refreshing, setRefreshing] = useState(false);
  const [cleaningCache, setCleaningCache] = useState(false);
  const [clearingKey, setClearingKey] = useState<string | null>(null);
  const requestIdRef = useRef(0);
  const windowLabel = getCurrentWebviewWindow().label;
  const history = settings.clipboard.history;
  const limitBytes = storageLimitBytes(history.storageLimitMb);

  /**
   * 拉取最新概览；并发时只采用最后一次请求的结果，失败时保留已有数据。
   */
  const loadOverview = async () => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;

    try {
      const next = await getStorageOverview();
      if (requestId !== requestIdRef.current) return;

      setOverview(next);
      setStatus("ready");
      onStorageUsageChange(next.usage);
    } catch (error) {
      log.warn("load storage overview failed", error);
      if (requestId !== requestIdRef.current) return;

      setStatus((current) => {
        return current === "ready" ? current : "error";
      });
    }
  };

  const { run: scheduleRefresh } = useDebounceFn(
    () => {
      void loadOverview();
    },
    { wait: REFRESH_DEBOUNCE_MS },
  );

  /**
   * 操作进行中保活偏好窗口；隐藏后 idle destroy 会等租约释放或超时兜底。
   */
  async function runWithKeepalive<T>(reason: string, task: () => Promise<T>) {
    const owner = `overview:${reason}`;

    await acquireWindowKeepalive(
      windowLabel,
      owner,
      reason,
      KEEPALIVE_TIMEOUT_MS,
    );
    try {
      return await task();
    } finally {
      await releaseWindowKeepalive(windowLabel, owner);
    }
  }

  const refresh = async () => {
    setRefreshing(true);
    try {
      await loadOverview();
    } finally {
      setRefreshing(false);
    }
  };

  const retry = async () => {
    setStatus("loading");
    await loadOverview();
  };

  const openDataDirectory = async () => {
    try {
      await runWithKeepalive("open-data-directory", () => {
        return openPreferenceDirectory("data");
      });
    } catch {
      // 错误 toast 已由 commands 层统一处理。
    }
  };

  const cleanCache = async () => {
    setCleaningCache(true);
    try {
      const result = await runWithKeepalive("clean-cache", cleanResourceCache);
      onStorageUsageChange(result.storageUsage);
      await loadOverview();
    } catch {
      // 错误 toast 已由 commands 层统一处理。
    } finally {
      setCleaningCache(false);
    }
  };

  const confirmCleanCache = () => {
    getModalApi().confirm({
      cancelText: t("common:actions.cancel"),
      centered: true,
      content: t("schema.settings.localData.cleanCache.confirmContent"),
      okText: t("schema.settings.localData.cleanCache.controlLabel"),
      onOk: cleanCache,
      title: t("schema.settings.localData.cleanCache.confirmTitle"),
    });
  };

  /**
   * 确认后清理某个范围内的普通记录；列表与侧栏占用由 Rust 的清理事件刷新。
   */
  const confirmClearScope = (
    key: string,
    scope: ClearScope,
    title: string,
    count: number,
    removable: number,
  ) => {
    const kept = count - removable;
    const clearScope = async () => {
      setClearingKey(key);
      try {
        await runWithKeepalive("clear-scope", () => {
          return clearClipboardItemsInScope(scope);
        });
        await loadOverview();
      } catch {
        // 错误 toast 已由 commands 层统一处理。
      } finally {
        setClearingKey(null);
      }
    };

    getModalApi().confirm({
      cancelText: t("common:actions.cancel"),
      centered: true,
      content: t(
        kept > 0 ? "overview.clear.contentWithKept" : "overview.clear.content",
        {
          count: removable,
          kept: formatCount(kept, i18n.language),
          value: formatCount(removable, i18n.language),
        },
      ),
      okButtonProps: { danger: true },
      okText: t("overview.clear.confirm"),
      onOk: clearScope,
      title,
    });
  };

  const clearCategory = (stat: CategoryStat) => {
    const name = t(`overview.categories.names.${stat.category}`);

    confirmClearScope(
      `category:${stat.category}`,
      { category: stat.category, type: "category" },
      t("overview.clear.categoryTitle", { name }),
      stat.count,
      stat.removable,
    );
  };

  const clearSourceApp = (stat: SourceAppStat) => {
    const name = stat.name ?? t("overview.sources.unknown");

    confirmClearScope(
      `sourceApp:${stat.appId ?? ""}`,
      { appId: stat.appId, type: "sourceApp" },
      t("overview.clear.sourceAppTitle", { name }),
      stat.count,
      stat.removable,
    );
  };

  const adjustLimit = () => {
    onNavigateSetting(STORAGE_LIMIT_SETTING_ID);
  };

  const manageGroups = () => {
    onNavigateSetting(CUSTOM_GROUPS_SETTING_ID);
  };

  useMount(() => {
    void loadOverview();
  });

  // 新采集、删除、清理和导入都会发这个事件，合并后刷新一次即可。
  useTauriListen(TAURI_EVENT.CLIPBOARD_UPDATED, () => {
    scheduleRefresh();
  });

  useTauriListen(TAURI_EVENT.CLIPBOARD_GROUPS_UPDATED, () => {
    scheduleRefresh();
  });

  if (!overview) {
    return (
      <div
        className="flex h-60 flex-col items-center justify-center gap-3"
        data-preference-setting-id={STORAGE_OVERVIEW_SETTING_ID}
      >
        {status === "error" ? (
          <>
            <span className="text-ant-secondary text-sm">
              {t("overview.loadError")}
            </span>
            <Button onClick={retry} size="small">
              {t("overview.retry")}
            </Button>
          </>
        ) : (
          <Spin />
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* 搜索跳转会把锚点滚到视野中央，锚在首张卡片上才能停在页顶。 */}
      <div data-preference-setting-id={STORAGE_OVERVIEW_SETTING_ID}>
        <StorageSpaceCard
          breakdown={overview.breakdown}
          cleaningCache={cleaningCache}
          limitAction={history.storageLimitAction}
          limitBytes={limitBytes}
          onAdjustLimit={adjustLimit}
          onCleanCache={confirmCleanCache}
          onOpenDirectory={openDataDirectory}
          onRefresh={refresh}
          reclaimable={overview.reclaimable}
          refreshing={refreshing}
          usage={overview.usage}
        />
      </div>

      <OverviewStatTiles history={overview.history} />

      <DailyTrendCard daily={overview.history.daily} />

      <div className="grid grid-cols-2 gap-4">
        <CategoryCard
          categories={overview.history.categories}
          clearingKey={clearingKey}
          onClear={clearCategory}
        />
        <SourceAppsCard
          apps={overview.history.sourceApps}
          clearingKey={clearingKey}
          onClear={clearSourceApp}
          others={overview.history.otherSourceApps}
        />
      </div>

      <GroupsCard
        groups={overview.history.groups}
        onManageGroups={manageGroups}
        totals={overview.history.totals}
      />
    </div>
  );
};

export default StorageOverviewPanel;
