import { AnimatePresence, motion } from "motion/react";
import type { FC, ReactNode } from "react";
import { PREVIEW_CONTENT_TRANSITION } from "../constants";

interface PreviewContentTransitionProps {
  children: ReactNode;
  contentKey: string;
}

/**
 * 内容切换用交叉淡入淡出：新内容立刻挂上来、盖在旧内容上淡入，旧内容同时淡出。
 *
 * 不能先淡出再淡入（`mode="wait"`）：两段之间正文会空掉几帧，指针在卡片间来回扫时
 * 面板一闪一闪。图层绝对定位铺满正文区，交叉期间布局不抖；正文区由调用方提供定位上下文。
 */
const PreviewContentTransition: FC<PreviewContentTransitionProps> = (props) => {
  const { children, contentKey } = props;

  return (
    <AnimatePresence initial={false} mode="sync">
      <motion.div
        animate={{ opacity: 1 }}
        className="absolute inset-0 flex flex-col"
        exit={{ opacity: 0 }}
        initial={{ opacity: 0 }}
        key={contentKey}
        transition={PREVIEW_CONTENT_TRANSITION}
      >
        {children}
      </motion.div>
    </AnimatePresence>
  );
};

export default PreviewContentTransition;
