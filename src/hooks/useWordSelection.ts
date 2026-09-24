import type { PointerEvent } from "react";
import { useRef, useState } from "react";

/** 可点选的词元素上的序号属性。 */
export const WORD_INDEX_ATTRIBUTE = "data-token-index";

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
 * 拆词选区：点击切换单个词，按住拖过一串词整段选中或取消（按下的词原来已选就是取消）。
 * 返回的指针处理器挂在词的公共容器上，词元素用 {@link WORD_INDEX_ATTRIBUTE} 标出序号。
 */
export function useWordSelection(count: number) {
  const [selected, setSelected] = useState<ReadonlySet<number>>(() => {
    return new Set();
  });
  const dragRef = useRef<WordDrag | null>(null);
  const allSelected = count > 0 && selected.size === count;

  const toggleAll = () => {
    if (allSelected) {
      setSelected(new Set());
      return;
    }

    setSelected(
      new Set(
        Array.from({ length: count }, (_value, index) => {
          return index;
        }),
      ),
    );
  };

  const clear = () => {
    setSelected(new Set());
  };

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

  const onPointerDown = (event: PointerEvent<HTMLElement>) => {
    if (event.button !== 0) return;

    const index = findWordIndex(event.target);
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
  const onPointerMove = (event: PointerEvent<HTMLElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;

    const index = findWordIndex(
      document.elementFromPoint(event.clientX, event.clientY),
    );
    if (index === null || index === drag.last) return;

    applyDrag(drag, index);
  };

  const endDrag = () => {
    dragRef.current = null;
  };

  return {
    allSelected,
    clear,
    pointerHandlers: {
      onLostPointerCapture: endDrag,
      onPointerCancel: endDrag,
      onPointerDown,
      onPointerMove,
      onPointerUp: endDrag,
    },
    selected,
    toggleAll,
  };
}

/**
 * 从事件目标或坐标命中的元素向上找到所在的词，返回词序号。
 */
function findWordIndex(target: EventTarget | null) {
  if (!(target instanceof Element)) return null;

  const word = target.closest<HTMLElement>(`[${WORD_INDEX_ATTRIBUTE}]`);
  if (!word) return null;

  return Number(word.getAttribute(WORD_INDEX_ATTRIBUTE));
}
