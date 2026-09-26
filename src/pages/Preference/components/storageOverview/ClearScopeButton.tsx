import type { FC } from "react";
import { useTranslation } from "react-i18next";
import CustomIconButton from "@/components/CustomIconButton";
import Tooltip from "@/components/Tooltip";

interface ClearScopeButtonProps {
  label: string;
  loading: boolean;
  removable: number;
  onClick: () => void;
}

/**
 * 排行行尾的清理按钮：悬停或键盘聚焦该行时显现；全是收藏 / 置顶时禁用并说明原因。
 */
const ClearScopeButton: FC<ClearScopeButtonProps> = (props) => {
  const { t } = useTranslation("preferences");
  const { label, loading, removable, onClick } = props;
  const disabled = removable === 0;

  return (
    <Tooltip
      title={
        disabled ? t("overview.clear.nothing") : t("overview.clear.tooltip")
      }
    >
      <CustomIconButton
        aria-label={t("overview.clear.ariaLabel", { name: label })}
        className="opacity-0 transition-opacity focus-visible:opacity-100 group-focus-within:opacity-100 group-hover:opacity-100 motion-reduce:transition-none"
        disabled={disabled}
        icon={<i aria-hidden="true" className="i-lucide:trash-2" />}
        loading={loading}
        onClick={onClick}
        size="small"
        type="text"
      />
    </Tooltip>
  );
};

export default ClearScopeButton;
