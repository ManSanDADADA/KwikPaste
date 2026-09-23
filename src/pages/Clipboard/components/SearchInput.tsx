import {
  ConfigProvider,
  Input,
  type InputProps,
  type InputRef,
  theme,
} from "antd";
import type { ChangeEvent, CompositionEvent, FC } from "react";
import { useCallback, useEffect, useMemo, useRef } from "react";
import KeyHint from "@/components/KeyHint";
import { prepareClipboardWindowEditableFocus } from "@/hooks/useClipboardWindowEditableFocus";

interface SearchInputProps extends Omit<InputProps, "prefix"> {
  blurToken?: number;
  clearToken?: number;
  focusToken?: number;
}

/**
 * 带快捷键提示的搜索输入框，支持 ⌘F / Ctrl+F 聚焦。
 * IME 拼音/日文组合输入期间抑制 onChange，待 compositionend 再补发一次，
 * 避免上层防抖/受控逻辑被中间态拼字串污染。
 */
const SearchInput: FC<SearchInputProps> = (props) => {
  const {
    blurToken = 0,
    clearToken = 0,
    focusToken = 0,
    onChange,
    onCompositionStart,
    onCompositionEnd,
    ...rest
  } = props;

  const inputRef = useRef<InputRef>(null);
  const composingRef = useRef(false);
  const { token } = theme.useToken();
  // 搜索框在「打开即聚焦」下几乎常驻聚焦态，antd 默认的高亮描边 + 光晕会一直亮着抢视线；
  // 这里把聚焦收敛成一层淡一档的描边，去掉光晕，hover 也不再变蓝。
  const searchTheme = useMemo(() => {
    return {
      components: {
        Input: {
          activeBorderColor: token.colorPrimaryBorder,
          activeShadow: "none",
          hoverBorderColor: token.colorBorder,
        },
      },
    };
  }, [token.colorBorder, token.colorPrimaryBorder]);

  /**
   * 聚焦搜索框并选中已有内容，便于直接覆盖输入。
   */
  const focusSearch = useCallback(async () => {
    if (!inputRef.current) return;

    await prepareClipboardWindowEditableFocus();
    inputRef.current?.focus({ cursor: "all" });
  }, []);

  useEffect(() => {
    if (blurToken <= 0) return;

    inputRef.current?.blur();
  }, [blurToken]);

  useEffect(() => {
    if (focusToken <= 0) return;

    const frame = requestAnimationFrame(() => {
      void focusSearch();
    });

    return () => {
      cancelAnimationFrame(frame);
    };
  }, [focusToken, focusSearch]);

  const handleChange = (event: ChangeEvent<HTMLInputElement>) => {
    if (composingRef.current) return;

    onChange?.(event);
  };

  const handleCompositionStart = (
    event: CompositionEvent<HTMLInputElement>,
  ) => {
    composingRef.current = true;

    onCompositionStart?.(event);
  };

  const handleCompositionEnd = (event: CompositionEvent<HTMLInputElement>) => {
    composingRef.current = false;

    onCompositionEnd?.(event);
    // composition 结束时浏览器已派发最后一次 input，但被上面挡掉了，这里补一次。
    onChange?.(event as unknown as ChangeEvent<HTMLInputElement>);
  };

  return (
    <ConfigProvider theme={searchTheme}>
      <Input
        autoCapitalize="off"
        autoCorrect="off"
        data-allow-global-keyboard="true"
        key={clearToken}
        onChange={handleChange}
        onCompositionEnd={handleCompositionEnd}
        onCompositionStart={handleCompositionStart}
        prefix={
          <KeyHint
            hintKey="F"
            iconName="i-lucide:search"
            onKeyPress={focusSearch}
          />
        }
        ref={inputRef}
        spellCheck={false}
        {...rest}
      />
    </ConfigProvider>
  );
};

export default SearchInput;
