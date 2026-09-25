import type { DragEvent, FC, MouseEvent, PointerEvent } from "react";
import { memo, useState } from "react";
import { useTranslation } from "react-i18next";
import { popupClipboardItemMenu, startDragClipboardItem } from "@/commands";
import AssetImage from "@/components/AssetImage";
import KeyHint from "@/components/KeyHint";
import type { ItemActionLabels } from "@/constants/itemActions";
import type { ClipboardAction, ClipboardItem } from "@/types/clipboard";
import type { ItemAction } from "@/types/settings";
import { cn } from "@/utils/cn";
import { isMac } from "@/utils/is";
import ClipboardQuickActions from "./ClipboardQuickActions";
import FilesCard from "./FilesCard";
import ImageCard from "./ImageCard";
import NoteContentSwitcher from "./NoteContentSwitcher";
import QuickSnippets from "./QuickSnippets";
import TextCard from "./TextCard";

/**
 * 回调都把条目作为参数回传：列表对所有卡片传同一组稳定函数，`memo` 才能跳过没变的卡片。
 */
interface ClipboardCardProps {
  item: ClipboardItem;
  isSelected?: boolean;
  /**
   * 快捷键提示字符（"1"–"9" / "0"），存在时在 app 图标上叠加 KeyHint；
   * 按下修饰键（macOS ⌘ / Windows Ctrl）+ 该数字键触发快速粘贴。
   */
  hintKey?: string;
  /**
   * 快捷键触发时执行的粘贴操作，由父级列表注入。
   */
  onQuickPaste?: (item: ClipboardItem) => void;
  /**
   * MOD 键按下时，URL / Email 文本以链接态展示。
   */
  isLinkActive?: boolean;
  /**
   * 点击 URL / Email 文本时打开外部链接。
   */
  onOpenLink?: (item: ClipboardItem) => void;
  onPointerEnter?: (
    item: ClipboardItem,
    event: PointerEvent<HTMLDivElement>,
  ) => void;
  onPointerLeave?: () => void;
  onPointerMove?: (
    item: ClipboardItem,
    event: PointerEvent<HTMLDivElement>,
  ) => void;
  onMouseDown?: (
    item: ClipboardItem,
    event: MouseEvent<HTMLDivElement>,
  ) => void;
  onAuxClick?: (event: MouseEvent<HTMLDivElement>) => void;
  onDoubleClick?: (item: ClipboardItem) => void;
  /**
   * 受收藏 / 置顶保护规则约束时为 false，右键菜单和快捷动作都去掉删除。
   */
  canDelete: boolean;
  quickActions?: readonly ItemAction[];
  quickActionLabels?: ItemActionLabels;
  onQuickAction?: (
    item: ClipboardItem,
    action: ItemAction,
  ) => Promise<void> | void;
  /**
   * 点击卡片下方的快捷信息时单独粘贴 / 复制该片段，由列表层按左键设置决定。
   */
  onPickSnippet?: (item: ClipboardItem, text: string) => void;
  showOriginalOnHover?: boolean;
  /**
   * 登记卡片根节点，预览打开时用它采集 anchor rect；卸载时传 null。
   */
  onRootElement?: (id: string, node: HTMLDivElement | null) => void;
}

/**
 * 按 `kind` 分发到具体卡片组件，统一外层 padding / 时间戳 / 来源应用图标。
 * `isSelected` 为 true 时加一圈柔和外环；指针事件由列表注入用于 hover preview；
 * 右键根节点弹出 Rust 端原生菜单（避免 tauri-apps/tauri#9470 的 muda use-after-free），
 * 点击菜单项后由列表层订阅 `clipboard://menu-action` 派发到实际处理逻辑。
 */
const ClipboardCard: FC<ClipboardCardProps> = (props) => {
  const {
    item,
    isSelected,
    hintKey,
    onQuickPaste,
    isLinkActive,
    onOpenLink,
    onPointerEnter,
    onPointerLeave,
    onPointerMove,
    onMouseDown,
    onAuxClick,
    onDoubleClick,
    canDelete,
    quickActions = [],
    quickActionLabels,
    onQuickAction,
    onPickSnippet,
    showOriginalOnHover = true,
    onRootElement,
  } = props;
  const {
    kind,
    quickSnippets = [],
    sourceAppId,
    subKind,
    sourceAppIconPath,
    sourceAppName,
  } = item;
  const { t } = useTranslation("clipboard");
  const [hovered, setHovered] = useState(false);
  const typeKey = subKind ?? kind;
  const typeLabel = t(`types.${typeKey}`);
  const visibleQuickActions = canDelete
    ? quickActions
    : quickActions.filter(isNotDeleteAction);

  const handleOpenLink = () => {
    onOpenLink?.(item);
  };

  const body = renderBody(item, isLinkActive, handleOpenLink);
  const showSensitiveIndicator = item.isSensitive && item.kind === "text";
  const showStatusIndicators = item.isPinned || showSensitiveIndicator;
  const indicatorCount = Number(item.isPinned) + Number(showSensitiveIndicator);
  const sourceAppIcon = sourceAppId ? (
    <AssetImage
      alt={sourceAppName}
      className="size-4"
      src={sourceAppIconPath}
    />
  ) : (
    <img
      alt="KwikPaste"
      className="pointer-events-none size-4"
      src={isMac ? "/logo-mac.png" : "/logo.png"}
    />
  );

  const handleDragStart = async (event: DragEvent) => {
    event.preventDefault();

    await startDragClipboardItem(item.id);
  };

  const handleContextMenu = async (event: MouseEvent) => {
    event.preventDefault();

    const allActions = item.availableActions ?? [];
    const actions = canDelete
      ? allActions
      : allActions.filter(isNotDeleteAction);
    const { isFavorite, isPinned, note } = item;

    if (actions.length === 0) return;

    await popupClipboardItemMenu(
      item.id,
      [...actions],
      item.groupId,
      isFavorite,
      isPinned,
      Boolean(note),
    );
  };

  const handlePointerEnter = (event: PointerEvent<HTMLDivElement>) => {
    setHovered(true);
    onPointerEnter?.(item, event);
  };

  const handlePointerLeave = () => {
    setHovered(false);
    onPointerLeave?.();
  };

  const handlePointerMove = (event: PointerEvent<HTMLDivElement>) => {
    onPointerMove?.(item, event);
  };

  const handleMouseDown = (event: MouseEvent<HTMLDivElement>) => {
    onMouseDown?.(item, event);
  };

  const handleDoubleClick = () => {
    onDoubleClick?.(item);
  };

  const handleQuickPaste = () => {
    onQuickPaste?.(item);
  };

  const handleQuickAction = (action: ItemAction) => {
    return onQuickAction?.(item, action);
  };

  const handlePickSnippet = (text: string) => {
    onPickSnippet?.(item, text);
  };

  const registerRoot = (node: HTMLDivElement | null) => {
    onRootElement?.(item.id, node);
  };

  return (
    <div
      aria-selected={isSelected}
      className={cn(
        "relative flex flex-col gap-1 overflow-hidden rounded-2 border border-ant-border-secondary p-2 transition-colors duration-150 ease-out motion-reduce:transition-none",
        {
          // 外环和边框互不干扰，置顶项被选中时两种标记可以同时读出来。
          "border-ant-primary bg-ant-container": item.isPinned,
          // 选中只加一圈柔和外环，底色和边框都不动：整圈亮蓝框在深色下太跳，
          // 而且会和置顶项的 primary 边框撞在一起分不出来。
          "ring-2 ring-ant-primary/35": isSelected,
        },
      )}
      draggable
      onAuxClick={onAuxClick}
      onContextMenu={handleContextMenu}
      onDoubleClick={handleDoubleClick}
      onDragStart={handleDragStart}
      onMouseDown={handleMouseDown}
      onPointerEnter={handlePointerEnter}
      onPointerLeave={handlePointerLeave}
      onPointerMove={handlePointerMove}
      ref={registerRoot}
      role="option"
      tabIndex={-1}
    >
      <div className="flex items-center justify-between text-ant-secondary text-xs">
        <div className="flex min-w-0 items-center gap-1 overflow-hidden">
          {hintKey ? (
            <KeyHint hintKey={hintKey} onKeyPress={handleQuickPaste}>
              {sourceAppIcon}
            </KeyHint>
          ) : (
            sourceAppIcon
          )}

          <span className="truncate">{typeLabel}</span>
        </div>

        <ClipboardQuickActions
          item={item}
          labels={quickActionLabels}
          onQuickAction={onQuickAction ? handleQuickAction : void 0}
          quickActions={visibleQuickActions}
          visible={hovered}
        />
      </div>

      {item.note ? (
        <NoteContentSwitcher
          note={item.note}
          showOriginal={showOriginalOnHover && hovered}
        >
          {body}
        </NoteContentSwitcher>
      ) : (
        body
      )}
      {quickSnippets.length > 0 && onPickSnippet ? (
        <QuickSnippets
          indicatorCount={indicatorCount}
          onPick={handlePickSnippet}
          snippets={quickSnippets}
        />
      ) : null}
      {showStatusIndicators
        ? renderStatusIndicators(item.isPinned, showSensitiveIndicator)
        : null}
    </div>
  );
};

const isNotDeleteAction = (action: ClipboardAction | ItemAction) => {
  return action !== "delete";
};

/**
 * 渲染卡片右下角的状态水印；仅表达状态，不参与交互。
 */
function renderStatusIndicators(isPinned: boolean, isSensitive: boolean) {
  return (
    <div className="pointer-events-none absolute right-2 bottom-2 flex items-end gap-1 text-ant-quaternary">
      {isPinned ? (
        <i aria-hidden="true" className="i-ph:push-pin-bold size-5" />
      ) : null}
      {isSensitive ? (
        <i aria-hidden="true" className="i-lucide:key-round size-5" />
      ) : null}
    </div>
  );
}

const renderBody = (
  item: ClipboardItem,
  isLinkActive?: boolean,
  onOpenLink?: () => void,
) => {
  if (item.kind === "image") return <ImageCard {...item} />;

  if (item.kind === "files") return <FilesCard {...item} />;

  return (
    <TextCard {...item} isLinkActive={isLinkActive} onOpenLink={onOpenLink} />
  );
};

export default memo(ClipboardCard);
