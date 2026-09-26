/* @unocss-include */
import type { ContentCategory, StorageBreakdown } from "@/commands";

export type BreakdownSegmentKey = keyof StorageBreakdown;

interface BreakdownSegmentMeta {
  key: BreakdownSegmentKey;
  labelKey: string;
  hintKey: string;
  fillClass: string;
  swatchClass: string;
}

interface CategoryMeta {
  icon: string;
  labelKey: string;
}

/**
 * 存储空间分段的固定顺序与配色；颜色按实体固定，不随占用排名变化。
 * 蓝 / 青 / 橙三色已按亮暗两套 antd 调色板校验过色觉障碍区分度，其他归为中性灰。
 */
export const BREAKDOWN_SEGMENTS: BreakdownSegmentMeta[] = [
  {
    fillClass: "fill-ant-blue-6",
    hintKey: "overview.space.hints.database",
    key: "databaseBytes",
    labelKey: "overview.space.segments.database",
    swatchClass: "bg-ant-blue-6",
  },
  {
    fillClass: "fill-ant-cyan-6",
    hintKey: "overview.space.hints.image",
    key: "imageBytes",
    labelKey: "overview.space.segments.image",
    swatchClass: "bg-ant-cyan-6",
  },
  {
    fillClass: "fill-ant-orange-6",
    hintKey: "overview.space.hints.icon",
    key: "iconBytes",
    labelKey: "overview.space.segments.icon",
    swatchClass: "bg-ant-orange-6",
  },
  {
    fillClass: "fill-ant-text-quaternary",
    hintKey: "overview.space.hints.other",
    key: "otherBytes",
    labelKey: "overview.space.segments.other",
    swatchClass: "bg-ant-text-quaternary",
  },
];

export const CATEGORY_META: Record<ContentCategory, CategoryMeta> = {
  color: {
    icon: "i-lucide:palette",
    labelKey: "overview.categories.names.color",
  },
  email: {
    icon: "i-lucide:mail",
    labelKey: "overview.categories.names.email",
  },
  files: {
    icon: "i-lucide:files",
    labelKey: "overview.categories.names.files",
  },
  html: {
    icon: "i-lucide:file-code-2",
    labelKey: "overview.categories.names.html",
  },
  image: {
    icon: "i-lucide:file-image",
    labelKey: "overview.categories.names.image",
  },
  path: {
    icon: "i-lucide:folder-open",
    labelKey: "overview.categories.names.path",
  },
  rtf: {
    icon: "i-lucide:file-type",
    labelKey: "overview.categories.names.rtf",
  },
  text: {
    icon: "i-lucide:clipboard-type",
    labelKey: "overview.categories.names.text",
  },
  url: {
    icon: "i-lucide:link",
    labelKey: "overview.categories.names.url",
  },
};

/**
 * 按界面语言格式化整数计数，带千分位。
 */
export function formatCount(value: number, language: string) {
  return new Intl.NumberFormat(language, { maximumFractionDigits: 0 }).format(
    value,
  );
}

/**
 * 把 Rust 回传的本地日期 `YYYY-MM-DD` 解析成本地零点，避免被当成 UTC 解析后跨日。
 */
export function parseLocalDate(value: string) {
  const [year, month, day] = value.split("-").map(Number);

  return new Date(year, month - 1, day);
}

/**
 * 坐标轴用的短日期，如 `9/25`。
 */
export function formatShortDate(value: string, language: string) {
  return new Intl.DateTimeFormat(language, {
    day: "numeric",
    month: "numeric",
  }).format(parseLocalDate(value));
}

/**
 * 提示与说明用的完整日期，如 `2026年9月25日` / `Sep 25, 2026`。
 */
export function formatLongDate(value: string, language: string) {
  return new Intl.DateTimeFormat(language, {
    day: "numeric",
    month: "short",
    year: "numeric",
  }).format(parseLocalDate(value));
}

/**
 * 两个本地日期之间相隔的自然日数（含首尾两天）。
 */
export function inclusiveDaySpan(from: string, to: string) {
  const dayMs = 24 * 60 * 60 * 1000;
  const diff = Math.round(
    (parseLocalDate(to).getTime() - parseLocalDate(from).getTime()) / dayMs,
  );

  return Math.max(diff, 0) + 1;
}

/**
 * 把坐标轴上限取整到 1 / 2 / 5 × 10^n，刻度读起来是整数。
 */
export function niceCeiling(value: number) {
  if (value <= 0) return 1;

  const magnitude = 10 ** Math.floor(Math.log10(value));
  const normalized = value / magnitude;
  const step = [1, 2, 5, 10].find((candidate) => {
    return normalized <= candidate;
  });

  return (step ?? 10) * magnitude;
}

/**
 * 占比转成 0–100 的百分数；有值但不足 1% 时按 1% 显示，保证细条可见。
 */
export function visiblePercent(value: number, max: number) {
  if (value <= 0 || max <= 0) return 0;

  return Math.min(Math.max((value / max) * 100, 1), 100);
}
