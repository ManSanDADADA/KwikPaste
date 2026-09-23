import type { ComponentProps, FC } from "react";
import { useWindowMaterial } from "@/hooks/useWindowMaterial";
import { cn } from "@/utils/cn";

/** 材质为「默认」时的纯色底色层级；云母 / 亚克力下由材质规则接管。 */
export type WindowMaterialTone = "container" | "layout" | "elevated";

interface WindowMaterialSurfaceProps extends ComponentProps<"div"> {
  tone?: WindowMaterialTone;
}

/** Applies one window-level material token without adding blur to child rows. */
const WindowMaterialSurface: FC<WindowMaterialSurfaceProps> = (props) => {
  const { children, className, tone = "container", ...rest } = props;
  const material = useWindowMaterial();

  return (
    <div
      {...rest}
      className={cn("kp-material-surface", className)}
      data-material={material}
      data-tone={tone}
    >
      {children}
    </div>
  );
};

export default WindowMaterialSurface;
