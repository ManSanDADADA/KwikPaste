import { Progress } from "antd";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import type { FC, ReactNode } from "react";
import LanguageSwitcher from "@/components/LanguageSwitcher";
import ScrollArea from "@/components/ScrollArea";
import WindowMaterialSurface from "@/components/WindowMaterialSurface";
import { cn } from "@/utils/cn";
import { isMac } from "@/utils/is";
import type { OnboardingStepDefinition } from "../types";

interface OnboardingShellProps {
  actions: ReactNode;
  activeIndex: number;
  activeStepId: string;
  children: ReactNode;
  direction: 1 | -1;
  steps: OnboardingStepDefinition[];
}

const OnboardingShell: FC<OnboardingShellProps> = (props) => {
  const { actions, activeIndex, activeStepId, children, direction, steps } =
    props;
  const shouldReduceMotion = useReducedMotion();
  const reduceMotion = shouldReduceMotion === true;

  const progress = Math.round(((activeIndex + 1) / steps.length) * 100);
  const contentVariants = reduceMotion
    ? {
        animate: { opacity: 1, scale: 1, x: 0 },
        exit: { opacity: 0, scale: 1, x: 0 },
        initial: { opacity: 0, scale: 1, x: 0 },
      }
    : {
        animate: { opacity: 1, scale: 1, x: 0 },
        exit: { opacity: 0, scale: 0.988, x: direction > 0 ? -28 : 28 },
        initial: { opacity: 0, scale: 0.988, x: direction > 0 ? 28 : -28 },
      };

  return (
    <main
      className={cn("h-screen overflow-hidden text-ant-text", {
        "bg-ant-container": !isMac,
        "bg-transparent": isMac,
      })}
    >
      <WindowMaterialSurface
        className={cn("relative flex h-full min-h-0 flex-col overflow-hidden", {
          "rounded-4": isMac,
        })}
        tone="layout"
      >
        <header
          className="flex shrink-0 items-center justify-between gap-4 px-5 pt-4"
          data-tauri-drag-region="deep"
        >
          <div className="flex min-w-0 items-center gap-3">
            <div className="relative overflow-hidden rounded-full">
              <Progress
                className="w-48"
                percent={progress}
                showInfo={false}
                size="small"
              />
            </div>
            <span className="flex items-center whitespace-nowrap font-medium text-ant-text text-sm">
              <AnimatePresence initial={false} mode="popLayout">
                <motion.span
                  animate={{ opacity: 1, y: 0 }}
                  className="inline-block min-w-[1ch]"
                  exit={{ opacity: 0, y: direction > 0 ? -10 : 10 }}
                  initial={{ opacity: 0, y: direction > 0 ? 10 : -10 }}
                  key={`${activeIndex}`}
                  transition={{
                    duration: reduceMotion ? 0.12 : 0.22,
                    ease: "easeOut",
                  }}
                >
                  {activeIndex + 1}
                </motion.span>
              </AnimatePresence>
              <span>/{steps.length}</span>
            </span>
          </div>

          <div data-tauri-drag-region="false">
            <LanguageSwitcher />
          </div>
        </header>

        <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
          <AnimatePresence initial={false} mode="wait">
            <motion.div
              animate={contentVariants.animate}
              className="flex min-h-0 flex-1 flex-col"
              exit={contentVariants.exit}
              initial={contentVariants.initial}
              key={activeStepId}
              transition={
                reduceMotion
                  ? {
                      duration: 0.14,
                      ease: "linear",
                    }
                  : {
                      duration: 0.34,
                      ease: [0.22, 1, 0.36, 1],
                    }
              }
            >
              <ScrollArea
                className="min-h-0 flex-1"
                contentClassName="flex min-h-full flex-col"
              >
                {children}
              </ScrollArea>
            </motion.div>
          </AnimatePresence>
        </div>

        {actions}
      </WindowMaterialSurface>
    </main>
  );
};

export default OnboardingShell;
