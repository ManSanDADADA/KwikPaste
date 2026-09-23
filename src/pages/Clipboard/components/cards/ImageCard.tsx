import type { CSSProperties, FC } from "react";
import { useSnapshot } from "valtio";
import AssetImage from "@/components/AssetImage";
import { IMAGE_THUMBNAIL_MAX_EDGE } from "@/constants/clipboard";
import { settingsState } from "@/stores/settings";
import type { ClipboardItem } from "@/types/clipboard";
import { useImageThumbnail } from "../../hooks/useImageThumbnail";

/**
 * 图片类卡片：列表已带缩略图路径时直接渲染；缩略图尚未生成时先显示同尺寸占位，
 * 再按需让 Rust 生成，避免为了一个几十像素高的卡片解码整张原图。
 * files 类型的单图预览也复用本组件，但直接加载给定路径，不走缩略图生成。
 */
const ImageCard: FC<ClipboardItem> = (props) => {
  const { content, height, imageThumbnailPath, kind, width } = props;
  const { clipboard } = useSnapshot(settingsState);
  const maxHeight = clipboard.display.imageMaxHeight;
  const thumbnailPath = useImageThumbnail(
    kind === "image" ? content : null,
    imageThumbnailPath ?? null,
  );
  const style: CSSProperties = { maxHeight };

  if (thumbnailPath) {
    return (
      <AssetImage className="self-start" src={thumbnailPath} style={style} />
    );
  }

  if (kind !== "image") return null;

  return (
    <div
      aria-hidden="true"
      className="animate-pulse self-start rounded-1 bg-ant-fill-tertiary motion-reduce:animate-none"
      style={resolvePlaceholderSize(width, height, maxHeight)}
    />
  );
};

/**
 * 按 Rust 缩略图规则（最长边不超过 IMAGE_THUMBNAIL_MAX_EDGE）和显示高度上限，
 * 算出缩略图最终的显示尺寸，让占位与图片到达后的布局一致。
 */
const resolvePlaceholderSize = (
  width: number | null,
  height: number | null,
  maxHeight: number,
): CSSProperties => {
  if (!width || !height) {
    return { height: maxHeight, width: maxHeight };
  }

  const scale = Math.min(1, IMAGE_THUMBNAIL_MAX_EDGE / Math.max(width, height));
  const thumbHeight = height * scale;
  const displayHeight = Math.min(maxHeight, thumbHeight);
  const displayWidth = (width * scale * displayHeight) / thumbHeight;

  return {
    height: Math.round(displayHeight),
    width: Math.round(displayWidth),
  };
};

export default ImageCard;
