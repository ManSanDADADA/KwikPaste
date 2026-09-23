import { useSnapshot } from "valtio";
import { settingsState } from "@/stores/settings";
import { windowMaterialSupportState } from "@/stores/windowMaterial";
import type { Material } from "@/types/settings";

/**
 * 当前窗口实际应渲染的材质：设置值经系统支持情况收敛，与 Rust 原生效果的判定保持一致，
 * 避免原生效果没生效时前端仍按半透明渲染而透出未模糊的桌面。
 */
export const useWindowMaterial = (): Material => {
  const settings = useSnapshot(settingsState);
  const support = useSnapshot(windowMaterialSupportState);
  const material = settings.appearance.material ?? "default";

  if (material === "mica" && !support.mica) return "default";
  if (material === "acrylic" && !support.acrylic) return "default";

  return material;
};
