import { emitTo } from "@tauri-apps/api/event";
import { useUnmount } from "ahooks";
import { Button, ConfigProvider, Empty, Segmented, theme } from "antd";
import type { TFunction } from "i18next";
import type { FC } from "react";
import { Fragment, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Virtuoso } from "react-virtuoso";
import { useSnapshot } from "valtio";
import {
  type ClipboardPreviewFileEntry,
  type ClipboardPreviewPayload,
  copyClipboardFragment,
  pasteClipboardFragment,
} from "@/commands";
import AssetImage from "@/components/AssetImage";
import ScrollArea from "@/components/ScrollArea";
import VirtuosoScroller, {
  type VirtuosoScrollerChildrenProps,
} from "@/components/VirtuosoScroller";
import { TAURI_EVENT } from "@/constants/events";
import { WINDOW_LABEL } from "@/constants/windows";
import { useWordSelection } from "@/hooks/useWordSelection";
import { settingsState, updateSettings } from "@/stores/settings";
import type { PreviewTextView } from "@/types/settings";
import { cn } from "@/utils/cn";
import { log } from "@/utils/log";
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

interface PlainTextViewerProps {
  text: string;
}

interface TextViewSwitchProps {
  value: PreviewTextView;
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
  const { clipboard } = useSnapshot(settingsState);

  return (
    <div className="flex h-12 shrink-0 items-center justify-between gap-3 border-ant-border border-b px-4">
      <div className="min-w-0">
        <div className="truncate font-medium text-sm">{title}</div>
        <div className="truncate text-ant-secondary text-xs">{meta}</div>
      </div>

      {payload && (
        <div className="flex shrink-0 items-center gap-2">
          {canPickWords(payload) ? (
            <TextViewSwitch value={clipboard.preview.textView} />
          ) : null}

          <span className="rounded-1 bg-ant-fill-secondary px-2 py-0.5 text-ant-secondary text-xs">
            {typeLabel}
          </span>
        </div>
      )}
    </div>
  );
};

/**
 * 标题栏里的文本预览方式切换。选择随设置保存，面板尺寸由剪贴板窗口按新方式重新开窗。
 */
const TextViewSwitch: FC<TextViewSwitchProps> = (props) => {
  const { value } = props;
  const { t } = useTranslation("preview");
  const { token } = theme.useToken();
  // 默认轨道和选中块都是实色底，在 Mica / Acrylic 上是两块不透明的色块；
  // 换成半透明填充，和旁边的类型标签一样透出窗口材质。
  const switchTheme = useMemo(() => {
    return {
      components: {
        Segmented: {
          itemSelectedBg: token.colorFill,
          trackBg: token.colorFillTertiary,
        },
      },
    };
  }, [token.colorFill, token.colorFillTertiary]);
  const labelStyles = useMemo(() => {
    return { label: { fontSize: token.fontSizeSM } };
  }, [token.fontSizeSM]);
  const options = [
    { label: t("view.plain"), value: "plain" as const },
    { label: t("view.words"), value: "words" as const },
  ];

  const switchTextView = async (next: PreviewTextView) => {
    try {
      await updateSettings({ clipboard: { preview: { textView: next } } });
    } catch (error) {
      log.error("switch preview text view failed", error);
    }
  };

  return (
    <ConfigProvider theme={switchTheme}>
      <Segmented<PreviewTextView>
        onChange={switchTextView}
        options={options}
        size="small"
        styles={labelStyles}
        value={value}
      />
    </ConfigProvider>
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
 * 文本预览：选词方式下有词可拆时按词块排版，其余情况展示原文。
 */
const TextViewer: FC<PayloadViewerProps> = (props) => {
  const { payload } = props;
  const { t } = useTranslation("preview");
  const { clipboard } = useSnapshot(settingsState);
  const text = payload.text ?? "";

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

  if (resolveTextView(payload, clipboard.preview.textView) === "words") {
    return <WordChipsViewer payload={payload} />;
  }

  return <PlainTextViewer text={text} />;
};

/**
 * 原文预览：所有文本族内容都按纯文本虚拟行展示，避免长 HTML / RTF 构造大 DOM。
 */
const PlainTextViewer: FC<PlainTextViewerProps> = (props) => {
  const { text } = props;
  const rows = useMemo(() => {
    return buildTextPreviewRows(text);
  }, [text]);

  return <VirtuosoScroller>{renderTextVirtuoso}</VirtuosoScroller>;

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
        {!row || row.start === row.end ? " " : text.slice(row.start, row.end)}
      </div>
    );
  }
};

/**
 * 选词预览：词按原文顺序排成词块、原文换段处另起一行，点选、拖选后单独粘贴或复制。
 * 原文过长时只拆了开头，词块末尾给出提示。
 */
const WordChipsViewer: FC<PayloadViewerProps> = (props) => {
  const { payload } = props;
  const { t } = useTranslation("preview");
  const text = payload.text ?? "";
  const words = payload.words ?? NO_WORDS;
  const { clear, pointerHandlers, selected } = useWordSelection(words.length);
  const [submitting, setSubmitting] = useState(false);
  const selectedCount = selected.size;

  // 选区同步给剪贴板窗口：预览里选了词时，Enter / Cmd+C 作用于选中的词。
  useEffect(() => {
    reportWordSelection(payload.id, [...selected]);
  }, [payload.id, selected]);

  // 切回原文或收起面板时撤掉选区，Enter / Cmd+C 重新作用于整条记录。
  useUnmount(() => {
    reportWordSelection(payload.id, []);
  });

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

  // 选区操作栏占据面板底部一行而不是浮在内容上：它不铺自己的底色，和标题栏一样透出窗口材质。
  return (
    <div className="flex size-full select-none flex-col" {...pointerHandlers}>
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-wrap content-start gap-1 p-4">
          {words.map(([start, end], index) => {
            const isSelected = selected.has(index);
            const lineBreak =
              index > 0 &&
              text.slice(words[index - 1][1], start).includes("\n");

            return (
              // biome-ignore lint/suspicious/noArrayIndexKey: 词序即原文位置，列表不会重排
              <Fragment key={index}>
                {lineBreak ? (
                  <span aria-hidden="true" className="h-0 basis-full" />
                ) : null}
                <span
                  className={cn(
                    "min-w-6 max-w-full cursor-pointer whitespace-pre-wrap break-all rounded-1.5 px-1.5 py-0.5 text-center text-sm leading-5 transition-colors duration-100 motion-reduce:transition-none",
                    {
                      "bg-ant-fill-tertiary hover:bg-ant-fill-secondary":
                        !isSelected,
                      "bg-ant-primary text-ant-light-solid": isSelected,
                    },
                  )}
                  data-token-index={index}
                >
                  {text.slice(start, end)}
                </span>
              </Fragment>
            );
          })}
        </div>

        {payload.wordsTruncated ? (
          <p className="px-4 pb-4 text-ant-secondary text-xs">
            {t("words.truncated")}
          </p>
        ) : null}
      </ScrollArea>

      {selectedCount > 0 ? (
        <div className="flex shrink-0 items-center gap-2 border-ant-border border-t py-2 pr-3 pl-4">
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
 * 这条预览能否切到选词方式：文本里有词可拆（脱敏展示的敏感内容不给词）。
 */
function canPickWords(payload: ClipboardPreviewPayload) {
  return payload.kind === "text" && (payload.words?.length ?? 0) > 0;
}

/**
 * 这条预览实际采用的文本视图：选词方式下有词可拆才按词块排，与 Rust `preview_text_metrics`
 * 的判断一致，面板尺寸才对得上。
 */
export function resolveTextView(
  payload: ClipboardPreviewPayload,
  textView: PreviewTextView,
): PreviewTextView {
  return textView === "words" && canPickWords(payload) ? "words" : "plain";
}

/**
 * 把预览里选中的词同步给剪贴板窗口，由它决定 Enter / Cmd+C 作用于选中的词还是整条记录。
 */
function reportWordSelection(itemId: string, indices: number[]) {
  void emitTo(WINDOW_LABEL.CLIPBOARD, TAURI_EVENT.PREVIEW_SELECTION, {
    indices,
    itemId,
  });
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
