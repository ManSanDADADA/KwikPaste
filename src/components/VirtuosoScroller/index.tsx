import type { PartialOptions } from "overlayscrollbars";
import { useOverlayScrollbars } from "overlayscrollbars-react";
import type { FC, ReactNode } from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import { cn } from "@/utils/cn";

const VIRTUOSO_SCROLLBAR_OPTIONS = {
  scrollbars: {
    autoHide: "move",
  },
} satisfies PartialOptions;

export type VirtuosoScrollerRef = HTMLElement | Window | null;

export interface VirtuosoScrollerChildrenProps {
  scrollerRef: (ref: VirtuosoScrollerRef) => void;
}

interface VirtuosoScrollerProps {
  children: (props: VirtuosoScrollerChildrenProps) => ReactNode;
  className?: string;
}

/**
 * Bridges OverlayScrollbars with react-virtuoso while keeping Virtuoso's own
 * scroll viewport as the element that receives all scroll calls and events.
 */
const VirtuosoScroller: FC<VirtuosoScrollerProps> = (props) => {
  const { children, className } = props;
  const rootRef = useRef<HTMLDivElement>(null);
  const [scroller, setScroller] = useState<VirtuosoScrollerRef>(null);
  const [initialize, osInstance] = useOverlayScrollbars({
    defer: true,
    events: {
      initialized(instance) {
        const { viewport } = instance.elements();

        viewport.style.overflowX = "var(--os-viewport-overflow-x)";
        viewport.style.overflowY = "var(--os-viewport-overflow-y)";
      },
    },
    options: VIRTUOSO_SCROLLBAR_OPTIONS,
  });

  useEffect(() => {
    const { current: root } = rootRef;
    if (!root || !(scroller instanceof HTMLElement)) return;

    initialize({
      elements: {
        viewport: scroller,
      },
      target: root,
    });

    return () => {
      osInstance()?.destroy();
    };
  }, [initialize, osInstance, scroller]);

  const handleScrollerRef = useCallback((ref: VirtuosoScrollerRef) => {
    // OverlayScrollbars 是 defer 初始化的，接管之前这个元素还是原生 overflow，
    // 会先闪一下系统滚动条。它自带的 initialize 标记就是干这个的，拿到 ref 就打上。
    if (ref instanceof HTMLElement) {
      ref.setAttribute("data-overlayscrollbars-initialize", "");
    }

    setScroller(ref);
  }, []);

  return (
    <div
      className={cn("size-full", className)}
      data-overlayscrollbars-initialize=""
      ref={rootRef}
    >
      {children({ scrollerRef: handleScrollerRef })}
    </div>
  );
};

export default VirtuosoScroller;
