export const PREVIEW_CACHE_LIMIT = 16;
export const PREVIEW_TEXT_SOFT_WRAP_CHARS = 32;
export const PREVIEW_EXIT_ANIMATION_MS = 160;
export const PREVIEW_PANEL_TRANSITION = {
  duration: 0.18,
  ease: [0.22, 1, 0.36, 1],
} as const;
export const PREVIEW_CONTENT_TRANSITION = {
  duration: 0.1,
  ease: "easeOut",
} as const;
/** payload 通常十几毫秒就回来，加载指示等到这么久还没回来才出现，免得每次换条目都闪一下遮罩。 */
export const PREVIEW_LOADING_INDICATOR_DELAY_MS = 200;
