import { emitTo } from "@tauri-apps/api/event";
import { Button, Empty } from "antd";
import type { TFunction } from "i18next";
import type { FC, ReactNode } from "react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Virtuoso } from "react-virtuoso";
import {
  type ClipboardPreviewFileEntry,
  type ClipboardPreviewPayload,
  copyClipboardFragment,
  pasteClipboardFragment,
} from "@/commands";
import AssetImage from "@/components/AssetImage";
import VirtuosoScroller, {
  type VirtuosoScrollerChildrenProps,
} from "@/components/VirtuosoScroller";
import { TAURI_EVENT } from "@/constants/events";
import { WINDOW_LABEL } from "@/constants/windows";
import { useWordSelection } from "@/hooks/useWordSelection";
import { cn } from "@/utils/cn";
import { PREVIEW_TEXT_SOFT_WRAP_CHARS } from "../constants";

export interface PreviewContentProps {
  payload: ClipboardPreviewPayload | null;
}

export interface PreviewHeaderProps {
  payload: ClipboardPreviewPayload | null;
}

interface PayloadViewerProps {
  payload: ClipboardPreviewPayload;
}

interface FilePreviewRowProps {
  file: ClipboardPreviewFileEntry;
}

/** 一行虚拟文本在原文里的下标区间。 */
interface TextRow {
  start: number;
  end: number;
}

const NO_WORDS: [number, number][] = [];

const TEXT_VIRTUOSO_COMPONENTS = {
  Footer: PreviewTextPadding,
  Header: PreviewTextPadding,
};

const FILES_VIRTUOSO_COMPONENTS = {
  Footer: PreviewFilesPadding,
  Header: PreviewFilesPadding,
};

/**
 * Content Viewer 顶部元信息区。
 */
export const PreviewHeader: FC<PreviewHeaderProps> = (props) => {
  const { payload } = props;
  const { t } = useTranslation(["preview", "clipboard"]);
  const title = payload ? previewTitle(t, payload) : t("title.loading");
  const meta = payload ? previewMeta(t, payload) : t("meta.contentViewer");
  const typeKey = payload ? (payload.subKind ?? payload.kind) : null;
  const typeLabel = typeKey ? t(`clipboard:types.${typeKey}`) : "";

  return (
    <div className="flex h-12 shrink-0 items-center justify-between gap-3 border-ant-border border-b px-4">
      <div className="min-w-0">
        <div className="truncate font-medium text-sm">{title}</div>
        <div className="truncate text-ant-secondary text-xs">{meta}</div>
      </div>

      {payload && (
        <span className="shrink-0 rounded-1 bg-ant-fill-secondary px-2 py-0.5 text-ant-secondary text-xs">
          {typeLabel}
        </span>
      )}
    </div>
  );
};

/**
 * 按 payload kind 分发到基础 viewer。
 */
export const PreviewContent: FC<PreviewContentProps> = (props) => {
  const { payload } = props;
  const { t } = useTranslation("preview");

  if (!payload) {
    return (
      <div className="flex min-h-24 items-center justify-center">
        <Empty
          description={t("empty.content")}
          image={Empty.PRESENTED_IMAGE_SIMPLE}
        />
      </div>
    );
  }

  if (payload.kind === "image") return <ImageViewer payload={payload} />;

  if (payload.kind === "files") return <FilesViewer payload={payload} />;

  return <TextViewer payload={payload} />;
};

/**
 * 文本预览：所有文本族内容都按纯文本虚拟行展示，避免长 HTML / RTF 构造大 DOM。
 * 带词区间时可以直接在原文上点选、拖选词语，选好后单独粘贴或复制。
 */
const TextViewer: FC<PayloadViewerProps> = (props) => {
  const { payload } = props;
  const { t } = useTranslation("preview");
  const text = payload.text ?? "";
  const words = payload.words ?? NO_WORDS;
  const rows = useMemo(() => {
    return buildTextPreviewRows(text);
  }, [text]);
  const { clear, pointerHandlers, selected } = useWordSelection(words.length);
  const [submitting, setSubmitting] = useState(false);
  const selectedCount = selected.size;

  // 选区同步给剪贴板窗口：预览里选了词时，Enter / Cmd+C 作用于选中的词。
  useEffect(() => {
    void emitTo(WINDOW_LABEL.CLIPBOARD, TAURI_EVENT.PREVIEW_SELECTION, {
      indices: [...selected],
      itemId: payload.id,
    });
  }, [payload.id, selected]);

  const pasteSelection = async () => {
    setSubmitting(true);

    try {
      await pasteClipboardFragment(payload.id, {
        indices: [...selected],
        kind: "words",
      });
    } catch {
      // 失败原因已由命令层提示。
    }

    setSubmitting(false);
  };

  const copySelection = async () => {
    setSubmitting(true);

    try {
      await copyClipboardFragment(payload.id, {
        indices: [...selected],
        kind: "words",
      });
    } catch {
      // 失败原因已由命令层提示。
    }

    setSubmitting(false);
  };

  if (text.length === 0) {
    return (
      <div className="flex min-h-24 items-center justify-center">
        <Empty
          description={t("empty.text")}
          image={Empty.PRESENTED_IMAGE_SIMPLE}
        />
      </div>
    );
  }

  return (
    <div className="relative size-full select-none" {...pointerHandlers}>
      <VirtuosoScroller>{renderTextVirtuoso}</VirtuosoScroller>

      {selectedCount > 0 ? (
        <div className="absolute inset-x-2 bottom-2 flex items-center gap-2 rounded-2 border border-ant-border-secondary bg-ant-bg-elevated py-1.5 pr-1.5 pl-3">
          <span className="min-w-0 flex-1 truncate text-ant-secondary text-xs">
            {t("words.selected", { count: selectedCount })}
          </span>

          <Button onClick={clear} size="small" type="text">
            {t("words.clear")}
          </Button>
          <Button disabled={submitting} onClick={copySelection} size="small">
            {t("words.copy")}
          </Button>
          <Button
            disabled={submitting}
            onClick={pasteSelection}
            size="small"
            type="primary"
          >
            {t("words.paste")}
          </Button>
        </div>
      ) : null}
    </div>
  );

  function renderTextVirtuoso(props: VirtuosoScrollerChildrenProps) {
    const { scrollerRef } = props;

    return (
      <Virtuoso
        components={TEXT_VIRTUOSO_COMPONENTS}
        computeItemKey={computeTextRowKey}
        itemContent={renderTextRow}
        scrollerRef={scrollerRef}
        totalCount={rows.length}
      />
    );
  }

  function computeTextRowKey(index: number) {
    return index;
  }

  function renderTextRow(index: number) {
    const row = rows[index];

    return (
      <div className="min-h-5.5 whitespace-pre px-4 font-mono text-xs leading-5.5">
        {!row || row.start === row.end ? " " : renderRowSegments(row)}
      </div>
    );
  }

  /**
   * 把一行切成普通文本和可点选的词；被折行切开的词两段带同一个序号，一起高亮。
   */
  function renderRowSegments(row: TextRow) {
    const segments: ReactNode[] = [];
    let cursor = row.start;

    for (
      let index = findFirstWordEndingAfter(words, row.start);
      index < words.length;
      index += 1
    ) {
      const [start, end] = words[index];
      if (start >= row.end) break;

      const from = Math.max(start, row.start);
      const to = Math.min(end, row.end);
      const isSelected = selected.has(index);

      if (from > cursor) segments.push(text.slice(cursor, from));
      segments.push(
        <span
          className={cn("cursor-pointer rounded-0.5", {
            "bg-ant-primary text-ant-light-solid": isSelected,
            "hover:bg-ant-fill-secondary": !isSelected,
          })}
          data-token-index={index}
          key={index}
        >
          {text.slice(from, to)}
        </span>,
      );
      cursor = to;
    }

    if (cursor < row.end) segments.push(text.slice(cursor, row.end));

    return segments;
  }
};

/**
 * 图片预览：使用原图路径渲染，缺失时降级为空状态。
 */
const ImageViewer: FC<PayloadViewerProps> = (props) => {
  const { payload } = props;
  const { t } = useTranslation("preview");
  const imageWidth = payload.imageWidth ?? void 0;
  const imageHeight = payload.imageHeight ?? void 0;

  if (!payload.imagePath || !payload.imageExists) {
    return (
      <div className="flex min-h-24 items-center justify-center">
        <Empty
          description={t("empty.imageMissing")}
          image={Empty.PRESENTED_IMAGE_SIMPLE}
        />
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 items-center justify-center p-4">
      <AssetImage
        alt={t("image.alt")}
        className="h-auto max-h-full max-w-full object-contain"
        draggable={false}
        height={imageHeight}
        src={payload.imagePath}
        width={imageWidth}
      />
    </div>
  );
};

/**
 * 文件预览：虚拟列表展示路径、文件名、存在状态与基础大小。
 */
const FilesViewer: FC<PayloadViewerProps> = (props) => {
  const { payload } = props;
  const { t } = useTranslation("preview");

  if (payload.files.length === 0) {
    return (
      <div className="flex min-h-24 items-center justify-center">
        <Empty
          description={t("empty.files")}
          image={Empty.PRESENTED_IMAGE_SIMPLE}
        />
      </div>
    );
  }

  return <VirtuosoScroller>{renderFilesVirtuoso}</VirtuosoScroller>;

  function renderFilesVirtuoso(props: VirtuosoScrollerChildrenProps) {
    const { scrollerRef } = props;
    const components =
      payload.totalFiles > payload.files.length
        ? {
            Footer: renderFilesFooter,
            Header: PreviewFilesPadding,
          }
        : FILES_VIRTUOSO_COMPONENTS;

    return (
      <Virtuoso
        components={components}
        computeItemKey={computeFileRowKey}
        itemContent={renderFileRow}
        scrollerRef={scrollerRef}
        totalCount={payload.files.length}
      />
    );
  }

  function computeFileRowKey(index: number) {
    return payload.files[index]?.path ?? index;
  }

  function renderFileRow(index: number) {
    const file = payload.files[index];
    if (!file) return <div className="h-10" />;

    return (
      <div className="px-2">
        <FilePreviewRow file={file} />
      </div>
    );
  }

  function renderFilesFooter() {
    return (
      <div className="px-4 py-2 text-ant-secondary text-xs">
        {t("file.shownCount", {
          shown: payload.files.length,
          total: payload.totalFiles,
        })}
      </div>
    );
  }
};

/**
 * 虚拟文本列表上下留白。
 */
function PreviewTextPadding() {
  return <div className="h-4" />;
}

/**
 * 虚拟文件列表顶部留白。
 */
function PreviewFilesPadding() {
  return <div className="h-2" />;
}

/**
 * 文件 viewer 的单行展示。
 */
const FilePreviewRow: FC<FilePreviewRowProps> = (props) => {
  const { file } = props;
  const { t } = useTranslation("preview");
  const kindLabel = file.isDir ? t("file.folder") : t("file.item");
  const sizeLabel = file.size === null ? kindLabel : formatBytes(file.size);

  return (
    <div
      className={cn(
        "flex min-h-10 items-center gap-2 rounded-1.5 px-2 py-1.5",
        { "opacity-50": !file.exists },
      )}
      title={file.path}
    >
      {file.iconPath ? (
        <AssetImage className="size-6 shrink-0" src={file.iconPath} />
      ) : (
        <i
          aria-hidden
          className="i-lucide:file size-5 shrink-0 text-ant-secondary"
        />
      )}

      <div className="min-w-0 flex-1">
        <div
          className={cn("truncate text-xs", {
            "line-through": !file.exists,
          })}
        >
          {file.name}
        </div>
        <div className="truncate text-ant-secondary text-xs">
          {file.exists ? file.path : t("file.missingPath")}
        </div>
      </div>

      <span className="shrink-0 text-ant-secondary text-xs">{sizeLabel}</span>
    </div>
  );
};

/**
 * 将长文本拆成虚拟行（原文下标区间），超长单行按固定字符数软切块。
 * 放不下的整字符挪到下一行，不从 emoji 的代理对中间切开；规则与 Rust
 * `count_preview_text_rows` 一致，面板高度才对得上。
 */
function buildTextPreviewRows(text: string) {
  const rows: TextRow[] = [];
  let lineStart = 0;

  for (const line of text.split("\n")) {
    let rowStart = lineStart;
    let offset = lineStart;

    for (const char of line) {
      if (offset + char.length - rowStart > PREVIEW_TEXT_SOFT_WRAP_CHARS) {
        rows.push({ end: offset, start: rowStart });
        rowStart = offset;
      }
      offset += char.length;
    }

    rows.push({ end: offset, start: rowStart });
    lineStart = offset + 1;
  }

  return rows;
}

/**
 * 二分查找第一个结束位置在 `offset` 之后的词，词区间按原文顺序排列且互不重叠。
 */
function findFirstWordEndingAfter(
  words: readonly [number, number][],
  offset: number,
) {
  let low = 0;
  let high = words.length;

  while (low < high) {
    const middle = (low + high) >> 1;

    if (words[middle][1] <= offset) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }

  return low;
}

/**
 * 生成 Content Viewer 标题。
 */
function previewTitle(
  t: TFunction<"preview">,
  payload: ClipboardPreviewPayload,
) {
  if (payload.kind === "files") {
    return t("title.files", { count: payload.totalFiles });
  }

  if (payload.kind === "image") {
    return t("title.image");
  }

  return t("title.text");
}

/**
 * 生成 Content Viewer 元信息。
 */
function previewMeta(
  t: TFunction<"preview">,
  payload: ClipboardPreviewPayload,
) {
  if (payload.kind === "files") {
    return t("meta.filesLoaded", { count: payload.files.length });
  }

  if (payload.kind === "image") {
    const dimensions =
      payload.imageWidth && payload.imageHeight
        ? `${payload.imageWidth} x ${payload.imageHeight}`
        : t("meta.unknownSize");
    const size = payload.size === null ? "" : ` · ${formatBytes(payload.size)}`;

    return `${dimensions}${size}`;
  }

  return t("meta.characters", {
    count: payload.size ?? payload.text?.length ?? 0,
  });
}

/**
 * 格式化字节大小为紧凑文本。
 */
function formatBytes(value: number) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let size = value;
  let unitIndex = 0;

  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }

  const fractionDigits = unitIndex === 0 || size >= 10 ? 0 : 1;

  return `${size.toFixed(fractionDigits)} ${units[unitIndex]}`;
}
