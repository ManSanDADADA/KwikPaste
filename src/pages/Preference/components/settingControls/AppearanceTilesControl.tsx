import type { FC } from "react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSnapshot } from "valtio";
import { windowMaterialSupportState } from "@/stores/windowMaterial";
import { cn } from "@/utils/cn";
import type { PreferenceSetting } from "../../types/preferences";
import type { ControlProps } from "./types";

type AppearanceTilesKind = "material" | "theme";

interface AppearanceTilesControlProps extends ControlProps {
  kind: AppearanceTilesKind;
  setting: PreferenceSetting;
  value: string;
}

const TILE_VALUES: Record<AppearanceTilesKind, string[]> = {
  material: ["default", "mica", "acrylic"],
  theme: ["auto", "light", "dark"],
};

/**
 * 用原生 radio 语义呈现外观选项，保留 Tab、方向键和 Space 的键盘行为。
 */
const AppearanceTilesControl: FC<AppearanceTilesControlProps> = (props) => {
  const { t } = useTranslation("preferences");
  const { disabled, kind, onChange, setting, value } = props;
  const [saving, setSaving] = useState(false);
  const support = useSnapshot(windowMaterialSupportState);
  const values = TILE_VALUES[kind];
  const selectedValue = values.includes(value) ? value : values[0];
  const labelKey = `schema.settings.${setting.id}.options`;

  const isUnsupported = (option: string) => {
    if (kind !== "material") return false;
    if (option === "mica") return !support.mica;
    if (option === "acrylic") return !support.acrylic;

    return false;
  };

  const handleChange = async (nextValue: string) => {
    if (disabled || saving || nextValue === selectedValue) return;

    setSaving(true);
    try {
      await onChange(setting, nextValue);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div aria-busy={saving} className="w-full min-w-0">
      <div
        aria-label={t(`schema.settings.${setting.id}.title`)}
        className="grid w-full grid-cols-1 gap-2.5 sm:grid-cols-3"
        role="radiogroup"
      >
        {values.map((option) => {
          const checked = option === selectedValue;
          const unsupported = isUnsupported(option);
          const optionLabel = t(`${labelKey}.${option}.label`);
          const optionDescription = unsupported
            ? t(`schema.settings.${setting.id}.unsupported`)
            : t(`${labelKey}.${option}.description`);

          return (
            <label
              className={cn(
                "group relative flex min-w-0 cursor-pointer flex-col overflow-hidden rounded-2 border bg-ant-container transition-colors focus-within:border-ant-primary focus-within:ring-2 focus-within:ring-ant-primary/25 hover:border-ant-primary motion-reduce:transition-none",
                checked
                  ? "border-ant-primary bg-ant-primary-bg"
                  : "border-ant-border-secondary",
                {
                  "cursor-not-allowed opacity-55 hover:border-ant-border-secondary":
                    disabled || saving || unsupported,
                },
              )}
              key={option}
            >
              <input
                aria-describedby={`${setting.id}-${option}-description`}
                aria-label={optionLabel}
                checked={checked}
                className="sr-only"
                disabled={disabled || saving || unsupported}
                name={setting.id}
                onChange={() => {
                  void handleChange(option);
                }}
                type="radio"
                value={option}
              />
              <AppearanceTilePreview kind={kind} option={option} />
              <span className="flex min-w-0 flex-1 flex-col gap-0.5 p-2.5">
                <span className="flex items-center gap-1.5 font-medium text-ant-text text-sm">
                  <span className="truncate">{optionLabel}</span>
                  {checked ? (
                    <i
                      aria-hidden="true"
                      className="i-lucide:check-circle-2 shrink-0 text-ant-primary"
                    />
                  ) : null}
                </span>
                <span
                  className="text-ant-secondary text-xs leading-relaxed"
                  id={`${setting.id}-${option}-description`}
                >
                  {optionDescription}
                </span>
              </span>
            </label>
          );
        })}
      </div>
      <span
        aria-live="polite"
        className="mt-1.5 flex items-center justify-end gap-1 text-ant-tertiary text-xs"
      >
        {saving ? (
          <>
            <i
              aria-hidden="true"
              className="i-lucide:loader-circle animate-spin"
            />
            {t("controls.saving")}
          </>
        ) : null}
      </span>
    </div>
  );
};

export default AppearanceTilesControl;

interface AppearanceTilePreviewProps {
  kind: AppearanceTilesKind;
  option: string;
}

const AppearanceTilePreview: FC<AppearanceTilePreviewProps> = (props) => {
  const { kind, option } = props;
  const isDark = option === "dark";
  const isAcrylic = option === "acrylic";
  const isMica = option === "mica";

  return (
    <span
      aria-hidden="true"
      className={cn(
        "relative block h-16 overflow-hidden border-ant-border-secondary border-b",
        kind === "theme" && isDark
          ? "bg-ant-bg-layout"
          : kind === "theme"
            ? "bg-ant-container"
            : "bg-ant-fill-tertiary",
      )}
    >
      <span className="absolute inset-x-3 top-3 h-2 rounded-full bg-ant-container/70" />
      <span className="absolute inset-x-3 top-7 h-2 w-2/3 rounded-full bg-ant-container/45" />
      <span
        className={cn(
          "absolute bottom-0 left-0 h-5 w-2/3 rounded-tr-2 bg-ant-primary/30",
          {
            "bg-ant-container/15": kind === "theme" && isDark,
            "bg-ant-container/20 backdrop-blur-sm": isAcrylic,
            "bg-ant-fill-secondary":
              kind === "material" && option === "default",
            "bg-gradient-to-br from-ant-primary/45 via-ant-container/25 to-transparent":
              isMica,
          },
        )}
      />
      {isAcrylic ? (
        <span className="absolute inset-0 bg-gradient-to-r from-ant-primary/20 via-ant-container/25 to-ant-fill-secondary/20" />
      ) : null}
    </span>
  );
};
