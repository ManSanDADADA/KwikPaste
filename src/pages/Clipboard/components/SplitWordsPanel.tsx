import { useMount } from "ahooks";
import { Button, Empty, Spin } from "antd";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import type { FC, PointerEvent } from "react";
import { Fragment, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useSnapshot } from "valtio";
import {
  copyClipboardFragment,
  pasteClipboardFragment,
  splitClipboardItem,
} from "@/commands";
import CustomIconButton from "@/components/CustomIconButton";
import ScrollArea from "@/components/ScrollArea";
import Tooltip from "@/components/Tooltip";
import { TAURI_EVENT } from "@/constants/events";
import { WINDOW_LABEL } from "@/constants/windows";
import { useKeyboardEvent, useKeyboardLayer } from "@/hooks/useKeyboardEvent";
import { useTauriListen } from "@/hooks/useTauriListen";
import { closeSplitWords, splitWordsState } from "@/stores/splitWords";
import type { WordSplit } from "@/types/clipboard";
import { cn } from "@/utils/cn";
import { isMac } from "@/utils/is";
import { formatShortcutDisplay } from "@/utils/shortcut";
import type { WindowVisibilityPayload } from "../hooks/previewController";

const SPLIT_WORDS_LAYER = "splitWords";

interface SplitWordsSheetProps {
  itemId: string;
}

interface WordDrag {
  /** 按下时所在的词。 */
  anchor: number;
  /** 按下前的选区；拖拽只改 anchor 到当前词这一段，其余保持原样。 */
  base: ReadonlySet<number>;
  /** 这次拖拽是选中还是取消选中，由按下的词原来的状态决定。 */
  select: boolean;
  /** 最近一次应用到的词，指针停在同一个词上时不重复更新。 */
  last: number;
  pointerId: number;
}

/**
 * 拆词面板：覆盖整个剪贴板窗口，按记录 id 挂载内容，换一条记录时选区随之重置。
 */
const SplitWordsPanel: FC = () => {
  const { itemId } = useSnapshot(splitWordsState);
  const shouldReduceMotion = useReducedMotion();
  const offset = shouldReduceMotion ? 0 : 8;

  return (
    <AnimatePresence>
      {itemId ? (
        <motion.div
          animate={{ opacity: 1, y: 0 }}
          className="absolute inset-0 z-20 flex flex-col"
          exit={{ opacity: 0, y: offset }}
          initial={{ opacity: 0, y: offset }}
          key={itemId}
          transition={{
            duration: shouldReduceMotion ? 0 : 0.16,
            ease: "easeOut",
          }}
        >
          <SplitWordsSheet itemId={itemId} />
        </motion.div>
      ) : null}
    </AnimatePresence>
  );
};

export default SplitWordsPanel;

/**
 * 一条记录的拆词内容：点击切换单个词，按住拖过一串词整段选中或取消，选好后粘贴或复制。
 */
const SplitWordsSheet: FC<SplitWordsSheetProps> = (props) => {
  const { itemId } = props;
  const { t } = useTranslation("clipboard");
  const [split, setSplit] = useState<WordSplit | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<number>>(() => {
    return new Set();
  });
  const [submitting, setSubmitting] = useState(false);
  const dragRef = useRef<WordDrag | null>(null);

  const tokens = split?.tokens ?? [];
  const selectedCount = selected.size;
  const allSelected = tokens.length > 0 && selectedCount === tokens.length;
  const actionDisabled = selectedCount === 0 || submitting;

  useKeyboardLayer(SPLIT_WORDS_LAYER);

  /**
   * 拉取分词结果；失败时命令层已提示原因，直接收起面板。
   */
  const loadSplit = async () => {
    try {
      setSplit(await splitClipboardItem(itemId));
    } catch {
      closeSplitWords();
    }
  };

  useMount(() => {
    void loadSplit();
  });

  /**
   * 剪贴板窗口隐藏后收起面板，下次打开回到列表。
   */
  const handleWindowVisibility = (event: {
    payload: WindowVisibilityPayload;
  }) => {
    const { label, visible } = event.payload;
    if (label !== WINDOW_LABEL.CLIPBOARD || visible) return;

    closeSplitWords();
  };

  useTauriListen<WindowVisibilityPayload>(
    TAURI_EVENT.WINDOW_VISIBILITY,
    handleWindowVisibility,
  );

  const toggleSelectAll = () => {
    if (allSelected) {
      setSelected(new Set());
      return;
    }

    setSelected(
      new Set(
        tokens.map((_token, index) => {
          return index;
        }),
      ),
    );
  };

  const pasteSelection = async () => {
    if (actionDisabled) return;

    setSubmitting(true);

    try {
      await pasteClipboardFragment(itemId, {
        indices: [...selected],
        kind: "words",
      });
    } catch {
      setSubmitting(false);
      return;
    }

    closeSplitWords();
  };

  const copySelection = async () => {
    if (actionDisabled) return;

    setSubmitting(true);

    try {
      await copyClipboardFragment(itemId, {
        indices: [...selected],
        kind: "words",
      });
    } catch {
      // 失败原因已由命令层提示，面板保持原样方便重试。
    }

    setSubmitting(false);
  };

  const handleKeyDown = (event: KeyboardEvent) => {
    const modifierPressed = isMac ? event.metaKey : event.ctrlKey;
    const key = event.key.toLowerCase();

    if (event.key === "Escape" || (modifierPressed && key === "s")) {
      event.preventDefault();
      closeSplitWords();
      return;
    }

    if (event.key === "Enter") {
      event.preventDefault();
      void pasteSelection();
      return;
    }

    if (!modifierPressed) return;

    if (key === "a") {
      event.preventDefault();
      toggleSelectAll();
      return;
    }

    if (key === "c") {
      event.preventDefault();
      void copySelection();
    }
  };

  useKeyboardEvent("keydown", handleKeyDown, SPLIT_WORDS_LAYER);

  /**
   * 把拖拽起点到 `index` 之间的词统一设为这次拖拽的状态，其余词保持按下前的选区。
   */
  const applyDrag = (drag: WordDrag, index: number) => {
    const next = new Set(drag.base);
    const from = Math.min(drag.anchor, index);
    const to = Math.max(drag.anchor, index);

    for (let current = from; current <= to; current += 1) {
      if (drag.select) {
        next.add(current);
      } else {
        next.delete(current);
      }
    }

    drag.last = index;
    setSelected(next);
  };

  const handlePointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;

    const index = findTokenIndex(event.target);
    if (index === null) return;

    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);

    const drag: WordDrag = {
      anchor: index,
      base: selected,
      last: index,
      pointerId: event.pointerId,
      select: !selected.has(index),
    };

    dragRef.current = drag;
    applyDrag(drag, index);
  };

  // 指针被捕获后事件目标恒为容器，要按坐标找出指针下的词。
  const handlePointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;

    const index = findTokenIndex(
      document.elementFromPoint(event.clientX, event.clientY),
    );
    if (index === null || index === drag.last) return;

    applyDrag(drag, index);
  };

  const endDrag = () => {
    dragRef.current = null;
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-1 p-3 pb-2" data-tauri-drag-region>
        <Tooltip title={t("splitWords.back")}>
          <CustomIconButton
            aria-label={t("splitWords.back")}
            icon={<i className="i-lucide:arrow-left text-base" />}
            onClick={closeSplitWords}
            size="small"
            type="text"
          />
        </Tooltip>

        <span
          className="min-w-0 flex-1 truncate font-medium text-sm"
          data-tauri-drag-region
        >
          {t("splitWords.title")}
        </span>

        <Tooltip title={formatShortcutDisplay("CmdOrCtrl+A")}>
          <Button
            disabled={tokens.length === 0}
            onClick={toggleSelectAll}
            size="small"
            type="text"
          >
            {t(allSelected ? "splitWords.clear" : "splitWords.selectAll")}
          </Button>
        </Tooltip>
      </div>

      {renderBody()}

      <div className="flex items-center justify-between gap-2 px-3 pt-2 pb-3">
        <span className="min-w-0 truncate text-ant-secondary text-xs">
          {selectedCount > 0
            ? t("splitWords.selected", { count: selectedCount })
            : t("splitWords.hint")}
        </span>

        <div className="flex shrink-0 items-center gap-2">
          <Tooltip title={formatShortcutDisplay("CmdOrCtrl+C")}>
            <Button
              disabled={actionDisabled}
              onClick={copySelection}
              size="small"
            >
              {t("splitWords.copy")}
            </Button>
          </Tooltip>

          <Tooltip title={formatShortcutDisplay("Enter")}>
            <Button
              disabled={actionDisabled}
              onClick={pasteSelection}
              size="small"
              type="primary"
            >
              {t("splitWords.paste")}
            </Button>
          </Tooltip>
        </div>
      </div>
    </div>
  );

  function renderBody() {
    if (!split) {
      return (
        <div className="flex min-h-0 flex-1 items-center justify-center">
          <Spin />
        </div>
      );
    }

    if (tokens.length === 0) {
      return (
        <div className="flex min-h-0 flex-1 items-center justify-center">
          <Empty
            description={t("splitWords.empty")}
            image={Empty.PRESENTED_IMAGE_SIMPLE}
          />
        </div>
      );
    }

    return (
      <ScrollArea className="min-h-0 flex-1 px-3">
        <div
          aria-label={t("splitWords.title")}
          aria-multiselectable
          className="flex flex-wrap content-start gap-1.5 pb-1"
          onLostPointerCapture={endDrag}
          onPointerCancel={endDrag}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={endDrag}
          role="listbox"
          tabIndex={-1}
        >
          {tokens.map((token, index) => {
            const isSelected = selected.has(index);

            return (
              // biome-ignore lint/suspicious/noArrayIndexKey: 词序即原文位置，列表不会重排
              <Fragment key={index}>
                {token.lineBreak ? (
                  <span aria-hidden="true" className="h-0 basis-full" />
                ) : null}
                <span
                  aria-selected={isSelected}
                  className={cn(
                    "min-w-8 max-w-full cursor-pointer select-none whitespace-pre-wrap break-all rounded-1.5 px-2 py-1 text-center text-base transition-colors duration-100 motion-reduce:transition-none",
                    {
                      "bg-ant-fill-tertiary hover:bg-ant-fill-secondary":
                        !isSelected,
                      "bg-ant-primary text-ant-light-solid": isSelected,
                    },
                  )}
                  data-token-index={index}
                  role="option"
                  tabIndex={-1}
                >
                  {token.text}
                </span>
              </Fragment>
            );
          })}
        </div>

        {split.truncated ? (
          <p className="mt-2 mb-1 text-ant-secondary text-xs">
            {t("splitWords.truncated")}
          </p>
        ) : null}
      </ScrollArea>
    );
  }
};

/**
 * 从事件目标或坐标命中的元素向上找到所在的词，返回词序号。
 */
function findTokenIndex(target: EventTarget | null) {
  if (!(target instanceof Element)) return null;

  const token = target.closest<HTMLElement>("[data-token-index]");
  if (!token) return null;

  return Number(token.dataset.tokenIndex);
}
