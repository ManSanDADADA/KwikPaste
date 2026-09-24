import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { platform } from "@tauri-apps/plugin-os";
import { RUNTIME_GLOBAL } from "@/constants/runtime";
import { WINDOW_LABEL } from "@/constants/windows";

interface KwikPasteRuntime {
  portable: boolean;
}

/**
 * 当前是否运行在 macOS 平台。
 */
export const isMac = platform() === "macos";

/**
 * 当前是否运行在 Windows 平台。
 */
export const isWin = platform() === "windows";

/**
 * 当前是否为 Windows 便携版：数据保存在程序文件夹，不支持改数据目录、自动以管理员运行。
 * 由 Rust 在页面脚本执行前注入，可在模块顶层同步读取。
 */
export const isPortable =
  (Reflect.get(window, RUNTIME_GLOBAL) as KwikPasteRuntime | undefined)
    ?.portable === true;

/**
 * 当前是否为 Vite dev 构建（开发模式）。生产构建为 false。
 */
export const isDev = import.meta.env.DEV;

/**
 * 当前是否为 Windows 平台的剪贴板窗口（focusable=false，需要低级键盘钩子）。
 */
export const isWinClipboardWindow = () => {
  return isWin && getCurrentWebviewWindow().label === WINDOW_LABEL.CLIPBOARD;
};

/**
 * 判断路径/文件名是否为常见图片类型（按扩展名匹配，大小写不敏感）。
 */
export const isImage = (value: string) => {
  const regex = /\.(jpe?g|png|webp|avif|gif|svg|bmp|ico|tiff?|heic|apng)$/i;

  return regex.test(value);
};
