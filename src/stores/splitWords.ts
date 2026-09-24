import { proxy } from "valtio";

interface SplitWordsState {
  /** 正在拆词的记录 id；为 null 时拆词面板关闭。 */
  itemId: string | null;
}

/**
 * 剪贴板窗口的拆词面板状态：列表（右键菜单 / 悬停动作 / 快捷键）写入，面板读取后自行拉取分词结果。
 * 单独成 store：`clipboardViewState` 的任何变化都会触发列表重置选中与重新查询。
 */
export const splitWordsState = proxy<SplitWordsState>({
  itemId: null,
});

/**
 * 打开拆词面板。
 */
export const openSplitWords = (itemId: string) => {
  splitWordsState.itemId = itemId;
};

/**
 * 关闭拆词面板。
 */
export const closeSplitWords = () => {
  splitWordsState.itemId = null;
};
