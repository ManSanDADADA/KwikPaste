import { findPreferenceSectionSettings } from "@/pages/Preference/config/preferenceSchema";
import DoneStep from "./components/DoneStep";
import IgnoreAppsStep from "./components/IgnoreAppsStep";
import PermissionsStep from "./components/PermissionsStep";
import ShortcutsStep from "./components/ShortcutsStep";
import WelcomeStep from "./components/WelcomeStep";
import type { OnboardingStepDefinition } from "./types";

// Windows 便携版没有需要处理的权限项，整步跳过。
const HAS_PERMISSION_SETTINGS =
  findPreferenceSectionSettings("permissions").length > 0;

export const ONBOARDING_STEPS: OnboardingStepDefinition[] = [
  {
    component: WelcomeStep,
    icon: "i-lucide:sparkles",
    id: "welcome",
  },
  ...(HAS_PERMISSION_SETTINGS
    ? [
        {
          component: PermissionsStep,
          icon: "i-lucide:shield-check",
          id: "permissions",
        } as const,
      ]
    : []),
  {
    component: ShortcutsStep,
    icon: "i-lucide:keyboard",
    id: "shortcuts",
  },
  {
    component: IgnoreAppsStep,
    icon: "i-lucide:ban",
    id: "ignoreApps",
  },
  {
    component: DoneStep,
    icon: "i-lucide:badge-check",
    id: "done",
  },
];
