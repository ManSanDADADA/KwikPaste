import { useSnapshot } from "valtio";
import WindowMaterialSurface from "@/components/WindowMaterialSurface";
import { useClipboardWindowEditableFocus } from "@/hooks/useClipboardWindowEditableFocus";
import { splitWordsState } from "@/stores/splitWords";
import { cn } from "@/utils/cn";
import Footer from "./components/Footer";
import Group from "./components/Group";
import Header from "./components/Header";
import List from "./components/List";
import SplitWordsPanel from "./components/SplitWordsPanel";

const Clipboard = () => {
  useClipboardWindowEditableFocus();

  const { itemId: splitItemId } = useSnapshot(splitWordsState);

  // 拆词面板打开时只把列表界面藏起来而不卸载，关闭后滚动位置、已加载分页和选中项原样保留。
  return (
    <WindowMaterialSurface
      className="flex size-screen flex-col overflow-hidden"
      data-density="compact"
      data-tauri-drag-region
    >
      <div
        className={cn("flex min-h-0 flex-1 flex-col", {
          invisible: splitItemId !== null,
        })}
        data-tauri-drag-region
      >
        <Header />

        <Group />

        <List />

        <Footer />
      </div>

      <SplitWordsPanel />
    </WindowMaterialSurface>
  );
};

export default Clipboard;
