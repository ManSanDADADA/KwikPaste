/* @unocss-include */
import { STORAGE_LIMIT_MIN_MB, STORAGE_WARNING_RATIO } from "../constants";

interface StorageToneClass {
  bg: string;
  text: string;
}

const STORAGE_METER_WIDTH_CLASSES = [
  "w-1/10",
  "w-2/10",
  "w-3/10",
  "w-4/10",
  "w-5/10",
  "w-6/10",
  "w-7/10",
  "w-8/10",
  "w-9/10",
  "w-full",
];

/**
 * 把字节数格式化成侧栏里的紧凑存储标签。
 */
export function formatBytes(bytes: number) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let size = bytes;
  let unitIndex = 0;

  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }

  const digits = unitIndex === 0 || size >= 10 ? 0 : 1;

  return `${size.toFixed(digits)} ${units[unitIndex]}`;
}

/**
 * 存储上限（MB）换算成字节；低于下限的值按下限计，与 Rust 侧的兜底一致。
 */
export function storageLimitBytes(limitMb: number) {
  return Math.max(limitMb, STORAGE_LIMIT_MIN_MB) * 1024 * 1024;
}

/**
 * 用离散宽度表达占用相对上限的比例：按 10% 向上取整、至少一格，避免 inline style。
 */
export function storageMeterClass(totalBytes: number, limitBytes: number) {
  const steps = Math.ceil((totalBytes / limitBytes) * 10);
  const index = Math.min(
    Math.max(steps, 1),
    STORAGE_METER_WIDTH_CLASSES.length,
  );

  return STORAGE_METER_WIDTH_CLASSES[index - 1];
}

/**
 * 按占用相对上限返回状态色：宽裕时绿色，接近上限黄色，超出红色。
 */
export function storageToneClass(
  totalBytes: number,
  limitBytes: number,
): StorageToneClass {
  if (totalBytes > limitBytes) {
    return { bg: "bg-ant-error", text: "text-ant-error" };
  }

  if (totalBytes >= limitBytes * STORAGE_WARNING_RATIO) {
    return { bg: "bg-ant-warning", text: "text-ant-warning" };
  }

  return { bg: "bg-ant-success", text: "text-ant-success" };
}
