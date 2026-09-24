import type { FC, MouseEvent, SyntheticEvent } from "react";
import Tooltip from "@/components/Tooltip";
import { cn } from "@/utils/cn";

/** 超过这个长度的片段（多为链接）大概率会被截断，悬停时补一个完整内容的提示。 */
const SNIPPET_TOOLTIP_MIN_CHARS = 32;

interface QuickSnippetsProps {
  snippets: string[];
  /**
   * 卡片右下角的置顶 / 敏感水印个数，给它们让出位置，避免压住片段文字。
   */
  indicatorCount: number;
  onPick: (text: string) => void;
}

interface SnippetChipProps {
  text: string;
  onPick: (text: string) => void;
}

/**
 * 文本卡片下方的快捷信息：只显示一行放得下的片段，放不下的整个隐藏，不截出半个。
 */
const QuickSnippets: FC<QuickSnippetsProps> = (props) => {
  const { indicatorCount, onPick, snippets } = props;

  return (
    <div
      className={cn("flex h-6 flex-wrap gap-1 overflow-hidden", {
        "pr-6": indicatorCount === 1,
        "pr-11": indicatorCount > 1,
      })}
    >
      {snippets.map((snippet) => {
        return <SnippetChip key={snippet} onPick={onPick} text={snippet} />;
      })}
    </div>
  );
};

export default QuickSnippets;

/**
 * 单个快捷信息；按下时拦住事件，避免触发卡片的选中、自动粘贴和拖拽。
 */
const SnippetChip: FC<SnippetChipProps> = (props) => {
  const { onPick, text } = props;

  const stopCardEvent = (event: SyntheticEvent<HTMLButtonElement>) => {
    event.preventDefault();
    event.stopPropagation();
  };

  const handleClick = (event: MouseEvent<HTMLButtonElement>) => {
    stopCardEvent(event);

    onPick(text);
  };

  const chip = (
    <button
      className="h-6 max-w-full cursor-pointer truncate rounded-1.5 border-0 bg-ant-fill-tertiary px-2 text-ant-secondary text-xs transition-colors hover:bg-ant-fill-secondary hover:text-ant-text motion-reduce:transition-none"
      onClick={handleClick}
      onDoubleClick={stopCardEvent}
      onMouseDown={stopCardEvent}
      onPointerDown={stopCardEvent}
      tabIndex={-1}
      type="button"
    >
      {text}
    </button>
  );

  if (text.length < SNIPPET_TOOLTIP_MIN_CHARS) return chip;

  return <Tooltip title={text}>{chip}</Tooltip>;
};
