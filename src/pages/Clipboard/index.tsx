import WindowMaterialSurface from "@/components/WindowMaterialSurface";
import { useClipboardWindowEditableFocus } from "@/hooks/useClipboardWindowEditableFocus";
import Footer from "./components/Footer";
import Group from "./components/Group";
import Header from "./components/Header";
import List from "./components/List";

const Clipboard = () => {
  useClipboardWindowEditableFocus();

  return (
    <WindowMaterialSurface
      className="flex size-screen flex-col overflow-hidden"
      data-density="compact"
      data-tauri-drag-region
    >
      <Header />

      <Group />

      <List />

      <Footer />
    </WindowMaterialSurface>
  );
};

export default Clipboard;
