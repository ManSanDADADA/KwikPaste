import type { ClipboardPreviewPayload } from "@/commands";
import { PREVIEW_CACHE_LIMIT } from "./constants";

/**
 * 从 LRU cache 读取同 item 的最新 payload，命中后刷新插入顺序。
 *
 * 读取时拿不到 `updatedAt`（预览状态只带 itemId），所以按「id + 脱敏模式」匹配；
 * 写入侧保证同一条目在同一模式下只留一份，这里不会命中旧版本。
 */
export function readCachedPayload(
  cache: Map<string, ClipboardPreviewPayload>,
  itemId: string,
  redactSecrets: boolean,
) {
  const key = [...cache.keys()].find((entryKey) => {
    return belongsToEntry(entryKey, itemId, redactSecrets);
  });

  if (!key) return null;

  const cached = cache.get(key) ?? null;
  if (cached) {
    cache.delete(key);
    cache.set(key, cached);
  }

  return cached;
}

/**
 * 写入最近预览 payload，key 绑定 updatedAt 避免内容复用过期。
 *
 * 同一条目在同一脱敏模式下只保留最新的一份：旧版本留在 Map 里，
 * 按 id 匹配的读取会在新旧之间反复命中，等于让缓存供出过期内容。
 */
export function writeCachedPayload(
  cache: Map<string, ClipboardPreviewPayload>,
  nextPayload: ClipboardPreviewPayload,
  redactSecrets: boolean,
) {
  const key = cacheKey(nextPayload, redactSecrets);

  for (const entryKey of [...cache.keys()]) {
    if (entryKey === key) continue;
    if (!belongsToEntry(entryKey, nextPayload.id, redactSecrets)) continue;

    cache.delete(entryKey);
  }

  cache.set(key, nextPayload);

  while (cache.size > PREVIEW_CACHE_LIMIT) {
    const [oldestKey] = cache.keys();
    if (!oldestKey) return;

    cache.delete(oldestKey);
  }
}

/**
 * 生成预览 payload 的缓存 key。
 */
export function cacheKey(
  payload: ClipboardPreviewPayload,
  redactSecrets = payload.isSensitive,
) {
  return `${payload.id}:${payload.updatedAt}:${redactSuffix(redactSecrets)}`;
}

/**
 * 同一条目 + 同一脱敏模式的稳定标识，不含 updatedAt；
 * 用于判断某条目在本窗口实例里是否已经回源过。
 */
export function payloadEntryKey(itemId: string, redactSecrets: boolean) {
  return `${itemId}:${redactSuffix(redactSecrets)}`;
}

/**
 * 判断缓存 key 是否属于指定条目的指定脱敏模式。
 */
function belongsToEntry(
  entryKey: string,
  itemId: string,
  redactSecrets: boolean,
) {
  return (
    entryKey.startsWith(`${itemId}:`) &&
    entryKey.endsWith(`:${redactSuffix(redactSecrets)}`)
  );
}

/**
 * 脱敏模式在 key 里的字面量。
 */
function redactSuffix(redactSecrets: boolean) {
  return redactSecrets ? "redacted" : "full";
}
