import type { FC } from "react";
import { useTranslation } from "react-i18next";
import type { StorageUsage } from "@/commands";
import { cn } from "@/utils/cn";
import type { PreferenceStorageState } from "../types/preferences";
import {
  formatBytes,
  storageLimitBytes,
  storageMeterClass,
  storageToneClass,
} from "../utils/storageUsage";

interface PreferenceStorageUsagePanelProps {
  state: PreferenceStorageState;
  storageLimitMb: number;
  storageUsage: StorageUsage | null;
  onClick: () => void;
}

/**
 * 侧栏里的本地存储摘要：展示数据目录占用与用户设定的存储上限，点击跳到存储设置。
 */
const PreferenceStorageUsagePanel: FC<PreferenceStorageUsagePanelProps> = (
  props,
) => {
  const { t } = useTranslation("preferences");
  const { state, storageLimitMb, storageUsage, onClick } = props;
  const limitBytes = storageLimitBytes(storageLimitMb);
  const isReady = state === "ready" && storageUsage !== null;
  const isOverLimit = isReady && storageUsage.totalBytes > limitBytes;
  const totalLabel = storageUsage ? formatBytes(storageUsage.totalBytes) : "--";
  const usageLabel =
    state === "loading"
      ? t("storage.loading")
      : t("storage.usage", {
          target: formatBytes(limitBytes),
          total: totalLabel,
        });
  const meterClassName = isReady
    ? storageMeterClass(storageUsage.totalBytes, limitBytes)
    : "w-1/10";
  const storageToneClassName = isReady
    ? storageToneClass(storageUsage.totalBytes, limitBytes)
    : { bg: "bg-ant-success", text: "text-ant-success" };

  return (
    <div className="px-3 pb-3">
      <button
        className="block w-full cursor-pointer rounded-2 border border-ant-border-secondary bg-ant-fill-quaternary px-3 py-3 text-left transition-colors hover:bg-ant-fill-tertiary focus-visible:ring-1 focus-visible:ring-ant-primary motion-reduce:transition-none"
        onClick={onClick}
        type="button"
      >
        <div className="flex min-w-0 items-start gap-2.5">
          <span
            className={cn(
              "flex size-7 shrink-0 items-center justify-center text-lg",
              state === "error" ? "text-ant-error" : storageToneClassName.text,
            )}
          >
            <i aria-hidden="true" className="i-lucide:hard-drive" />
          </span>
          <div className="min-w-0 flex-1">
            <div className="truncate font-medium text-ant-text text-sm leading-tight">
              {t("storage.title")}
            </div>
            <div
              className={cn(
                "mt-1 truncate font-medium text-xs leading-tight",
                state === "error" || isOverLimit
                  ? "text-ant-error"
                  : "text-ant-secondary",
              )}
            >
              {state === "error" ? t("storage.error") : usageLabel}
            </div>
          </div>
        </div>

        <div className="mt-3 h-1 overflow-hidden rounded-full bg-ant-fill-secondary">
          <span
            className={cn(
              "block h-full rounded-full transition-all motion-reduce:transition-none",
              state === "error" ? "bg-ant-error" : storageToneClassName.bg,
              meterClassName,
            )}
          />
        </div>
      </button>
    </div>
  );
};

export default PreferenceStorageUsagePanel;
