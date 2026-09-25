import type { FC, ReactNode } from "react";
import { cn } from "@/utils/cn";
import StepHero from "./StepHero";

interface OnboardingStepLayoutProps {
  children: ReactNode;
  contentClassName?: string;
  description: ReactNode;
  icon: ReactNode;
  iconClassName?: string;
  title: ReactNode;
}

const OnboardingStepLayout: FC<OnboardingStepLayoutProps> = (props) => {
  const {
    children,
    contentClassName,
    description,
    icon,
    iconClassName,
    title,
  } = props;

  return (
    <div className="flex flex-1 flex-col justify-center px-6 pt-2 pb-6 md:px-20">
      <StepHero
        description={description}
        icon={icon}
        iconClassName={iconClassName}
        title={title}
      />

      <div className={cn("mx-auto mt-8 w-full", contentClassName)}>
        {children}
      </div>
    </div>
  );
};

export default OnboardingStepLayout;
