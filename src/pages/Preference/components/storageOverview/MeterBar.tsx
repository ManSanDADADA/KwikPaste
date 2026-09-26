import type { FC } from "react";
import { cn } from "@/utils/cn";
import { visiblePercent } from "../../utils/storageOverview";

interface MeterBarProps {
  className?: string;
  max: number;
  value: number;
}

/**
 * 排行列表里的横向数值条：SVG 百分比宽度，免去按数据写行内尺寸。
 */
const MeterBar: FC<MeterBarProps> = (props) => {
  const { className, max, value } = props;
  const percent = visiblePercent(value, max);

  return (
    <svg
      aria-hidden="true"
      className={cn("block h-1.5 w-full overflow-visible", className)}
    >
      <rect
        className="fill-ant-fill-tertiary"
        height="100%"
        rx="3"
        width="100%"
      />
      {percent > 0 ? (
        <rect
          className="fill-ant-primary"
          height="100%"
          rx="3"
          width={`${percent}%`}
        />
      ) : null}
    </svg>
  );
};

export default MeterBar;
