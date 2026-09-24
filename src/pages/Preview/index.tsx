import { emitTo } from "@tauri-apps/api/event";
import { useMount } from "ahooks";
import { Spin } from "antd";
import { motion } from "motion/react";
import { type FC, useMemo, useState } from "react";
import { useSnapshot } from "valtio";
import {
  type ClipboardPreviewPayload,
  type ClipboardPreviewPlacement,
  type ClipboardPreviewState,
  getClipboardPreviewState,
} from "@/commands";
import WindowMaterialSurface from "@/components/WindowMaterialSurface";
import { TAURI_EVENT } from "@/constants/events";
import { WINDOW_LABEL } from "@/constants/windows";
import { useTauriListen } from "@/hooks/useTauriListen";
import { settingsState } from "@/stores/settings";
import { log } from "@/utils/log";
import { cacheKey } from "./cache";
import { PreviewContent, PreviewHeader } from "./components/PreviewContent";
import PreviewContentTransition from "./components/PreviewContentTransition";
import {
  PREVIEW_LOADING_INDICATOR_DELAY_MS,
  PREVIEW_PANEL_TRANSITION,
} from "./constants";
import {
  useDelayedLoadingIndicator,
  usePreviewPayload,
  usePreviewRenderState,
} from "./hooks";

/**
 * 内容从贴着卡片的那条边淡入。窗口自身没法做缩放（原生背板会整块直接出现），
 * 方向感只能由内容的位移传达：面板在卡片右边就从左边缘推出来，依此类推。
 */
const PANEL_ENTER_OFFSET: Record<
  ClipboardPreviewPlacement,
  { x: number; y: number }
> = {
  bottom: { x: 0, y: -6 },
  left: { x: 6, y: 0 },
  right: { x: -6, y: 0 },
  top: { x: 0, y: 6 },
};

interface BeforeDestroyPayload {
  label: string;
}

/**
 * 系统级剪贴板预览窗口。
 *
 * 窗口本身就是预览面板：位置、尺寸、原生材质都由 Rust 在显示前定好，
 * 这里只负责铺满窗口渲染内容，按 `itemId + updatedAt` 缓存最近 payload。
 */
const Preview: FC = () => {
  const [previewState, setPreviewState] =
    useState<ClipboardPreviewState | null>(null);
  const [payloadResetToken, setPayloadResetToken] = useState(0);
  const { clipboard } = useSnapshot(settingsState);
  const redactSecrets = clipboard.sensitive.redactSecrets;
  const renderState = usePreviewRenderState(previewState);
  const active = previewState !== null;
  const visibleState = previewState ?? renderState;
  const { loadingItemId, missing, payload } = usePreviewPayload(previewState, {
    panelVisible: visibleState !== null,
    resetToken: payloadResetToken,
  });
  const showLoading = useDelayedLoadingIndicator(
    loadingItemId !== null,
    PREVIEW_LOADING_INDICATOR_DELAY_MS,
  );
  const placement = visibleState?.placement ?? "right";
  const variants = useMemo(() => {
    return {
      closed: { opacity: 0, ...PANEL_ENTER_OFFSET[placement] },
      open: { opacity: 1, x: 0, y: 0 },
    };
  }, [placement]);

  useMount(async () => {
    try {
      const state = await getClipboardPreviewState();
      setPreviewState(state);
    } catch (error) {
      log.error("load preview state failed", error);
    }
  });

  useTauriListen<ClipboardPreviewState | null>(
    TAURI_EVENT.PREVIEW_UPDATED,
    (event) => {
      setPreviewState(event.payload);
    },
  );

  const handleBeforeDestroy = (event: { payload: BeforeDestroyPayload }) => {
    if (event.payload.label !== WINDOW_LABEL.PREVIEW) return;

    setPreviewState(null);
    setPayloadResetToken((current) => {
      return current + 1;
    });
  };

  useTauriListen<BeforeDestroyPayload>(
    TAURI_EVENT.WINDOW_BEFORE_DESTROY,
    handleBeforeDestroy,
  );

  /**
   * 指针进出面板时通知剪贴板窗口：停在面板上期间，松开 Space 或离开卡片都不收起预览。
   */
  const reportPointer = (inside: boolean) => {
    void emitTo(WINDOW_LABEL.CLIPBOARD, TAURI_EVENT.PREVIEW_POINTER, {
      inside,
    });
  };

  const handlePointerEnter = () => {
    reportPointer(true);
  };

  const handlePointerLeave = () => {
    reportPointer(false);
  };

  if (!visibleState) return <div className="size-screen bg-transparent" />;

  // 还没拿到内容（缓存未命中、请求在路上）时正文留白：既不能画上一条的内容，
  // 也不能先画一张「暂无内容」再换成正文。
  const hasContent = payload !== null || missing;
  const contentKey = resolveContentKey(payload, missing, redactSecrets);

  return (
    <motion.div
      animate={active ? "open" : "closed"}
      className="size-screen"
      initial="closed"
      onPointerEnter={handlePointerEnter}
      onPointerLeave={handlePointerLeave}
      transition={PREVIEW_PANEL_TRANSITION}
      variants={variants}
    >
      <WindowMaterialSurface
        className="flex size-full flex-col overflow-hidden rounded-2 border border-ant-border-secondary"
        tone="elevated"
      >
        <PreviewHeader payload={payload} />

        <div className="relative min-h-0 flex-1 overflow-hidden">
          <PreviewContentTransition contentKey={contentKey}>
            {hasContent && <PreviewContent payload={payload} />}
          </PreviewContentTransition>
        </div>

        {showLoading && (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center bg-ant-mask/10">
            <Spin size="small" />
          </div>
        )}
      </WindowMaterialSurface>
    </motion.div>
  );
};

/**
 * 交叉淡入淡出的图层 key：有内容时按 payload 缓存 key 区分；
 * 没内容时区分「还没拿到」和「记录已不存在」，前者是空图层，后者才画空态。
 */
function resolveContentKey(
  payload: ClipboardPreviewPayload | null,
  missing: boolean,
  redactSecrets: boolean,
) {
  if (payload) return cacheKey(payload, redactSecrets);

  return missing ? "missing" : "pending";
}

export default Preview;
