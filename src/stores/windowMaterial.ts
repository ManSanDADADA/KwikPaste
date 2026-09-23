import { proxy } from "valtio";

import { getWindowMaterialSupport } from "@/commands";
import type { WindowMaterialSupport } from "@/types/settings";

/**
 * 当前系统原生材质支持情况的镜像，真相源在 Rust（`window::material_support`）。
 *
 * 系统版本在进程生命周期内不变，因此只在启动时拉取一次。初值按全部不支持处理，
 * 让窗口在快照灌入前保持纯色，而不是透出未模糊的桌面。
 */
export const windowMaterialSupportState = proxy<WindowMaterialSupport>({
  acrylic: false,
  mica: false,
});

/**
 * 启动期一次性拉取支持情况；由 React `use(windowMaterialSupportReady)` 在 Suspense 中等待。
 * 命令包装内部已把失败收敛为全部不支持，这里无需再 try/catch。
 */
export const windowMaterialSupportReady: Promise<void> = (async () => {
  const support = await getWindowMaterialSupport();

  Object.assign(windowMaterialSupportState, support);
})();
