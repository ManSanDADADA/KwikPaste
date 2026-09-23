import { lazy } from "react";
import { createHashRouter } from "react-router";

const Clipboard = lazy(async () => import("@/pages/Clipboard"));
const Preference = lazy(async () => import("@/pages/Preference"));
const Onboarding = lazy(async () => import("@/pages/Onboarding"));
const ContextMenu = lazy(async () => import("@/pages/ContextMenu"));
const ContextSubmenu = lazy(async () => {
  const module = await import("@/pages/ContextMenu");

  return { default: module.ContextSubmenu };
});
const Preview = lazy(async () => import("@/pages/Preview"));
const Update = lazy(async () => import("@/pages/Update"));

export const router = createHashRouter([
  {
    Component: Clipboard,
    path: "/",
  },
  {
    Component: Preference,
    path: "/preference",
  },
  {
    Component: Onboarding,
    path: "/onboarding",
  },
  {
    Component: ContextMenu,
    path: "/context-menu",
  },
  {
    Component: ContextSubmenu,
    path: "/context-submenu",
  },
  {
    Component: Preview,
    path: "/preview",
  },
  {
    Component: Update,
    path: "/update",
  },
]);
