import type { QuickPasteModifiers } from "@/types/settings";

/**
 * 快速粘贴修饰键组合对应的快捷键前缀，与 Rust `QuickPasteModifiers::accelerator` 保持一致。
 */
export const QUICK_PASTE_MODIFIER_ACCELERATORS: Record<
  QuickPasteModifiers,
  string
> = {
  alt: "Alt",
  altShift: "Alt+Shift",
  control: "Control",
  controlAlt: "Control+Alt",
  controlShift: "Control+Shift",
};

/**
 * 快速粘贴的数字键，依次对应历史第 1–10 条，与 Rust `shortcut::QUICK_PASTE_KEYS` 保持一致。
 */
export const QUICK_PASTE_KEYS = [
  "1",
  "2",
  "3",
  "4",
  "5",
  "6",
  "7",
  "8",
  "9",
  "0",
] as const;

const QUICK_PASTE_MODIFIER_ORDER: QuickPasteModifiers[] = [
  "controlShift",
  "controlAlt",
  "altShift",
  "alt",
  "control",
];

export const QUICK_PASTE_MODIFIER_OPTIONS = QUICK_PASTE_MODIFIER_ORDER.map(
  (value) => {
    return { shortcut: QUICK_PASTE_MODIFIER_ACCELERATORS[value], value };
  },
);

/**
 * 列出快速粘贴实际注册的全部组合键，供偏好页做全局快捷键冲突检测。
 */
export function resolveQuickPasteBindings(modifiers: QuickPasteModifiers) {
  const prefix = QUICK_PASTE_MODIFIER_ACCELERATORS[modifiers];

  return QUICK_PASTE_KEYS.map((key) => {
    return `${prefix}+${key}`;
  });
}
