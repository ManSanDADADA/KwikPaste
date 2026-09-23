/* @unocss-include */
import type { PreferenceTabId } from "./types/preferences";

export const APP_NAME_PLACEHOLDER = "KwikPaste";

/** 与 Rust `MIN_STORAGE_LIMIT_MB` 保持一致。 */
export const STORAGE_LIMIT_MIN_MB = 100;
export const STORAGE_LIMIT_MAX_MB = 1024 * 1024;
/** 占用达到上限的这个比例后侧栏转为警示色。 */
export const STORAGE_WARNING_RATIO = 0.8;

interface PreferenceTabMeta {
  activeClass: string;
  icon: string;
}

export const PREFERENCE_TAB_META: Record<PreferenceTabId, PreferenceTabMeta> = {
  about: {
    activeClass: "bg-ant-fill-secondary text-ant-text",
    icon: "i-lucide:info",
  },
  data: {
    activeClass: "bg-ant-fill-secondary text-ant-text",
    icon: "i-lucide:database",
  },
  organize: {
    activeClass: "bg-ant-fill-secondary text-ant-text",
    icon: "i-lucide:history",
  },
  record: {
    activeClass: "bg-ant-fill-secondary text-ant-text",
    icon: "i-lucide:clipboard-plus",
  },
  reuse: {
    activeClass: "bg-ant-fill-secondary text-ant-text",
    icon: "i-lucide:mouse-pointer-click",
  },
  shortcuts: {
    activeClass: "bg-ant-fill-secondary text-ant-text",
    icon: "i-lucide:keyboard",
  },
  workflow: {
    activeClass: "bg-ant-fill-secondary text-ant-text",
    icon: "i-lucide:panel-top",
  },
};
