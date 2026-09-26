import type { FC, ReactNode } from "react";
import { cn } from "@/utils/cn";

interface OverviewCardProps {
  children: ReactNode;
  className?: string;
  extra?: ReactNode;
  icon: string;
  subtitle?: ReactNode;
  title: ReactNode;
}

/**
 * 数据概览里的一块卡片：图标标题 + 右侧操作 + 内容区，沿用偏好面板的材质底色。
 */
const OverviewCard: FC<OverviewCardProps> = (props) => {
  const { children, className, extra, icon, subtitle, title } = props;

  return (
    <section
      className={cn(
        "kp-preference-panel flex min-w-0 flex-col rounded-2 border border-ant-border-secondary",
        className,
      )}
    >
      <header className="flex min-h-12 items-center justify-between gap-3 px-4 pt-3">
        <div className="flex min-w-0 items-center gap-2">
          <i
            aria-hidden="true"
            className={cn("shrink-0 text-ant-primary text-base", icon)}
          />
          <h3 className="m-0 truncate font-semibold text-ant-text text-sm">
            {title}
          </h3>
          {subtitle ? (
            <span className="truncate text-ant-secondary text-xs">
              {subtitle}
            </span>
          ) : null}
        </div>

        {extra ? (
          <div className="flex shrink-0 items-center">{extra}</div>
        ) : null}
      </header>

      <div className="flex min-h-0 flex-1 flex-col px-4 pt-2 pb-4">
        {children}
      </div>
    </section>
  );
};

export default OverviewCard;
