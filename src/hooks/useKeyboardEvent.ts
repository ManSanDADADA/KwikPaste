import { useEventListener, useLatest } from "ahooks";
import { useEffect } from "react";
import { TAURI_EVENT } from "@/constants/events";
import { isWinClipboardWindow } from "@/utils/is";
import { useTauriListen } from "./useTauriListen";

type KeyboardEventType = "keydown" | "keyup";

const EDITABLE_GLOBAL_KEYBOARD_ATTRIBUTE = "data-allow-global-keyboard";
const EDITABLE_GLOBAL_KEYBOARD_SELECTOR = `[${EDITABLE_GLOBAL_KEYBOARD_ATTRIBUTE}="true"]`;
const EDITABLE_GLOBAL_HANDOFF_KEYS = new Set([
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  "Control",
  "Enter",
  "Escape",
  "Tab",
]);

interface NavEventPayload {
  code?: string;
  type: KeyboardEventType;
  key: string;
  ctrlKey?: boolean;
  shiftKey?: boolean;
}

interface KeyboardLayerEntry {
  layer: string;
}

/**
 * 独占键盘的浮层栈（每个 webview 一份）。栈顶浮层打开期间，keydown 只交给登记在该浮层上的处理器，
 * 被遮住的列表、分组栏和快捷键提示一律跳过；keyup 不拦，底层靠它复位修饰键状态。
 */
const keyboardLayers: KeyboardLayerEntry[] = [];

/**
 * 判断某个处理器所在的层当前能否收到 keydown：没有浮层时只有底层（`layer` 为空）生效。
 */
const isKeyboardLayerActive = (layer?: string) => {
  return keyboardLayers[keyboardLayers.length - 1]?.layer === layer;
};

/**
 * 声明一个独占键盘的浮层：挂载期间压栈，卸载后出栈。
 *
 * 出栈推迟到下一个任务：关闭浮层的那次按键（如 Escape）还会继续派发给其它监听器，
 * 此时浮层必须仍在栈顶，否则底层会接着把同一个按键再处理一遍（例如直接隐藏窗口）。
 */
export const useKeyboardLayer = (layer: string) => {
  useEffect(() => {
    const entry: KeyboardLayerEntry = { layer };

    keyboardLayers.push(entry);

    return () => {
      window.setTimeout(() => {
        const index = keyboardLayers.indexOf(entry);
        if (index !== -1) keyboardLayers.splice(index, 1);
      });
    };
  }, [layer]);
};

/**
 * 跨平台键盘事件监听 hook。
 *
 * macOS 与可聚焦窗口直接监听浏览器键盘事件；Windows 剪贴板窗口默认不可聚焦，
 * 导航键通常来自 Rust 低级钩子，但输入控件仍保留浏览器原生输入行为。
 * `layer` 表示处理器所属的独占浮层（见 {@link useKeyboardLayer}），不传即底层界面。
 */
export const useKeyboardEvent = (
  type: KeyboardEventType,
  handler: (event: KeyboardEvent) => void,
  layer?: string,
) => {
  const isWindowsClipboardWindow = isWinClipboardWindow();
  const handlerRef = useLatest(handler);

  const shouldHandle = () => {
    return type === "keyup" || isKeyboardLayerActive(layer);
  };

  const handleBrowserEvent = (event: KeyboardEvent) => {
    if (!shouldHandle()) return;

    if (isWindowsClipboardWindow) {
      const editableTarget = findEditableElement(event.target);
      if (editableTarget) {
        if (!shouldHandoffEditableKeyboard(editableTarget, event)) return;

        editableTarget.blur();
      }
    }

    handlerRef.current(event);
  };

  useEventListener(type, handleBrowserEvent);

  useTauriListen<NavEventPayload>(TAURI_EVENT.KEYBOARD_NAV, (event) => {
    if (!isWindowsClipboardWindow) return;
    if (shouldUseNativeEditableKeyboard(document.activeElement)) return;

    const { type: payloadType, ...rest } = event.payload;

    if (payloadType !== type || !rest.key) return;
    if (!shouldHandle()) return;

    handlerRef.current(
      new KeyboardEvent(payloadType, { cancelable: true, ...rest }),
    );
  });
};

function findEditableElement(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof Element)) return null;

  let element: Element | null = target;
  while (element) {
    if (element instanceof HTMLElement) {
      if (element.isContentEditable) return element;

      const tagName = element.tagName.toLowerCase();
      if (tagName === "input" || tagName === "textarea") return element;
    }

    element = element.parentElement;
  }

  return null;
}

function shouldUseNativeEditableKeyboard(target: EventTarget | null) {
  return findEditableElement(target) !== null;
}

function shouldHandoffEditableKeyboard(
  target: HTMLElement,
  event: KeyboardEvent,
) {
  if (event.type !== "keydown") return false;
  if (!target.closest(EDITABLE_GLOBAL_KEYBOARD_SELECTOR)) return false;

  return EDITABLE_GLOBAL_HANDOFF_KEYS.has(event.key);
}
