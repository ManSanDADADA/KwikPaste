import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useSnapshot } from "valtio";
import {
  type ClipboardPreviewPayload,
  type ClipboardPreviewState,
  getClipboardPreviewPayload,
} from "@/commands";
import { settingsState } from "@/stores/settings";
import { log } from "@/utils/log";
import {
  cacheKey,
  payloadEntryKey,
  readCachedPayload,
  writeCachedPayload,
} from "./cache";
import { PREVIEW_EXIT_ANIMATION_MS } from "./constants";

/**
 * 保留退出动画期间的最后一帧 preview state，动画结束后再清空渲染树。
 */
export function usePreviewRenderState(
  previewState: ClipboardPreviewState | null,
) {
  const [renderState, setRenderState] = useState<ClipboardPreviewState | null>(
    null,
  );
  const exitTimerRef = useRef<number | null>(null);

  useEffect(() => {
    if (exitTimerRef.current !== null) {
      window.clearTimeout(exitTimerRef.current);
      exitTimerRef.current = null;
    }

    if (previewState) {
      setRenderState(previewState);
      return;
    }

    exitTimerRef.current = window.setTimeout(() => {
      setRenderState(null);
      exitTimerRef.current = null;
    }, PREVIEW_EXIT_ANIMATION_MS);

    return () => {
      if (exitTimerRef.current === null) return;

      window.clearTimeout(exitTimerRef.current);
      exitTimerRef.current = null;
    };
  }, [previewState]);

  return renderState;
}

export interface UsePreviewPayloadOptions {
  /** 面板树是否还挂着。退出动画放完、面板卸载后清掉留存的 payload，下次打开不会先闪一下上一条的内容。 */
  panelVisible: boolean;
  /** 销毁前清缓存的纯触发器。 */
  resetToken: number;
}

/**
 * 按预览状态加载 payload，并用 LRU cache 复用最近内容。
 *
 * 回源按「条目 + 脱敏模式」去重：同一条目在一次预览里只读一次库，
 * 换条目才重新回源，顺带让缓存里的内容在下次悬停时被最新记录刷新。
 *
 * 换条目期间沿用上一条的 payload，等新内容到了再由调用方交叉淡入；
 * `missing` 区分「还没拿到」和「记录已经不存在」，前者正文留白，后者才画空态。
 */
export function usePreviewPayload(
  previewState: ClipboardPreviewState | null,
  options: UsePreviewPayloadOptions,
) {
  const { panelVisible, resetToken } = options;
  const [payload, setPayload] = useState<ClipboardPreviewPayload | null>(null);
  const [missing, setMissing] = useState(false);
  const [loadingItemId, setLoadingItemId] = useState<string | null>(null);
  const activeEntryKeyRef = useRef<string | null>(null);
  const loadedEntryKeyRef = useRef<string | null>(null);
  const cacheRef = useRef(new Map<string, ClipboardPreviewPayload>());
  const { clipboard } = useSnapshot(settingsState);
  const redactSecrets = clipboard.sensitive.redactSecrets;

  // biome-ignore lint/correctness/useExhaustiveDependencies: resetToken 是销毁前清缓存的纯触发器。
  useEffect(() => {
    cacheRef.current.clear();
    activeEntryKeyRef.current = null;
    loadedEntryKeyRef.current = null;
    setLoadingItemId(null);
    setMissing(false);
    setPayload(null);
  }, [resetToken]);

  useEffect(() => {
    if (panelVisible) return;

    setMissing(false);
    setPayload(null);
  }, [panelVisible]);

  // 用 layout effect：缓存命中要赶在这一帧绘制前把内容换上。普通 effect 在绘制之后才跑，
  // 换条目会先画一帧旧内容、打开会先画一帧空白，再换成命中的内容。
  useLayoutEffect(() => {
    if (!previewState) {
      activeEntryKeyRef.current = null;
      // 预览关闭即结束本轮去重：下次打开重新回源一次，缓存只用来消除首帧空白。
      loadedEntryKeyRef.current = null;
      setLoadingItemId(null);
      return;
    }

    const { itemId } = previewState;
    const entryKey = payloadEntryKey(itemId, redactSecrets);
    const cached = readCachedPayload(cacheRef.current, itemId, redactSecrets);

    activeEntryKeyRef.current = entryKey;

    if (cached) {
      setPayload(cached);
      setMissing(false);
      setLoadingItemId(null);
    }

    // 已经为这个条目回过源就不再重复：状态每次广播都会重跑本 effect，若也跟着回源，
    // 前一次的响应会被后一次作废，库也被反复读。
    if (loadedEntryKeyRef.current === entryKey) return;

    loadedEntryKeyRef.current = entryKey;

    if (!cached) setLoadingItemId(itemId);

    /**
     * 回源读取完整 payload；用户已经切到别的条目时丢弃响应。
     */
    async function loadPayload() {
      try {
        const nextPayload = await getClipboardPreviewPayload(itemId);

        if (activeEntryKeyRef.current !== entryKey) return;

        if (!nextPayload) {
          setPayload(null);
          setMissing(true);
          setLoadingItemId(null);
          return;
        }

        writeCachedPayload(cacheRef.current, nextPayload, redactSecrets);
        setMissing(false);
        setPayload((current) => {
          if (
            current &&
            cacheKey(current, redactSecrets) ===
              cacheKey(nextPayload, redactSecrets)
          ) {
            return current;
          }

          return nextPayload;
        });
        setLoadingItemId(null);
      } catch (error) {
        if (activeEntryKeyRef.current !== entryKey) return;

        log.error("load preview payload failed", error);
        setLoadingItemId(null);
      }
    }

    void loadPayload();
  }, [previewState, redactSecrets]);

  return { loadingItemId, missing, payload };
}

/**
 * 加载指示延迟出现：加载持续超过 `delayMs` 才显示，加载结束立即撤掉。
 */
export function useDelayedLoadingIndicator(loading: boolean, delayMs: number) {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    if (!loading) {
      setVisible(false);
      return;
    }

    const timer = window.setTimeout(() => {
      setVisible(true);
    }, delayMs);

    return () => {
      window.clearTimeout(timer);
    };
  }, [delayMs, loading]);

  return visible;
}
