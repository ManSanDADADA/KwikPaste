import { useMount, useUnmount } from "ahooks";
import { useRef, useState } from "react";
import { getClipboardImageThumbnailPath } from "@/commands";

/** 已解析缩略图路径的内存缓存上限；超出后按插入顺序淘汰，长期驻留不会无界增长。 */
const RESOLVED_CACHE_MAX = 512;

const resolvedPaths = new Map<string, string>();
const pendingRequests = new Map<string, Promise<string | null>>();

const rememberResolved = (fileName: string, path: string) => {
  resolvedPaths.delete(fileName);
  resolvedPaths.set(fileName, path);
  if (resolvedPaths.size <= RESOLVED_CACHE_MAX) return;

  const oldest = resolvedPaths.keys().next().value;
  if (oldest !== void 0) resolvedPaths.delete(oldest);
};

/**
 * 向 Rust 请求一张图片的缩略图路径。同一文件的并发请求合并为一次 IPC；
 * 解码并发由 Rust 侧的信号量限制，这里不再额外排队。
 */
const requestThumbnail = async (fileName: string): Promise<string | null> => {
  const cached = resolvedPaths.get(fileName);
  if (cached) return cached;

  const pending = pendingRequests.get(fileName);
  if (pending) return pending;

  const request = getClipboardImageThumbnailPath(fileName);
  pendingRequests.set(fileName, request);

  try {
    const path = await request;
    if (path) rememberResolved(fileName, path);

    return path;
  } finally {
    pendingRequests.delete(fileName);
  }
};

/**
 * 图片卡片的缩略图路径：列表已带路径时直接用；否则按需让 Rust 生成，
 * 生成期间返回 null 让卡片显示同尺寸占位。卡片卸载后到达的结果直接丢弃。
 */
export const useImageThumbnail = (
  fileName: string | null,
  knownPath: string | null,
): string | null => {
  const [resolvedPath, setResolvedPath] = useState<string | null>(() => {
    if (fileName === null) return null;

    return resolvedPaths.get(fileName) ?? null;
  });
  const mountedRef = useRef(true);

  useMount(async () => {
    if (knownPath !== null || resolvedPath !== null || fileName === null) {
      return;
    }

    const path = await requestThumbnail(fileName);
    if (!mountedRef.current) return;

    setResolvedPath(path);
  });

  useUnmount(() => {
    mountedRef.current = false;
  });

  return knownPath ?? resolvedPath;
};
