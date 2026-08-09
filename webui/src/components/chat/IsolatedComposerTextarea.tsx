import {
  forwardRef,
  memo,
  useCallback,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

export interface IsolatedComposerTextareaHandle {
  focus: (options?: FocusOptions) => void;
  setValue: (value: string) => void;
}

interface IsolatedComposerTextareaProps {
  disabled: boolean;
  placeholder: string;
  minHeight: number;
  maxHeight: number;
  onValueChange: (value: string) => void;
  onPaste: React.ClipboardEventHandler<HTMLTextAreaElement>;
  onKeyDown: React.KeyboardEventHandler<HTMLTextAreaElement>;
  onCompositionStart: React.CompositionEventHandler<HTMLTextAreaElement>;
  onCompositionEnd: React.CompositionEventHandler<HTMLTextAreaElement>;
}

const IsolatedComposerTextareaImpl = forwardRef<
  IsolatedComposerTextareaHandle,
  IsolatedComposerTextareaProps
>(function IsolatedComposerTextarea(
  {
    disabled,
    placeholder,
    minHeight,
    maxHeight,
    onValueChange,
    onPaste,
    onKeyDown,
    onCompositionStart,
    onCompositionEnd,
  },
  forwardedRef,
) {
  const [value, setValue] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const resize = useCallback(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    const rawHeight = Math.max(el.scrollHeight, minHeight);
    el.style.height = `${Math.min(rawHeight, maxHeight)}px`;
    el.style.overflowY = rawHeight > maxHeight ? "auto" : "hidden";
  }, [maxHeight, minHeight]);

  useLayoutEffect(resize, [resize, value]);

  useImperativeHandle(
    forwardedRef,
    () => ({
      focus: (options?: FocusOptions) => textareaRef.current?.focus(options),
      setValue,
    }),
    [],
  );

  return (
    <textarea
      ref={textareaRef}
      value={value}
      disabled={disabled}
      onChange={(event) => {
        const next = event.currentTarget.value;
        setValue(next);
        onValueChange(next);
      }}
      onPaste={onPaste}
      onKeyDown={onKeyDown}
      onCompositionStart={onCompositionStart}
      onCompositionEnd={onCompositionEnd}
      placeholder={placeholder}
      rows={1}
      style={{
        flex: 1,
        minWidth: 0,
        borderRadius: 10,
        fontSize: 14,
        border: "1px solid var(--border-default)",
        background: "var(--bg-elevated)",
        color: "var(--text-primary)",
        resize: "none",
        fontFamily: "var(--font-ui)",
        padding: "10px 14px",
        outline: "none",
        overflow: "hidden",
        lineHeight: 1.5,
        minHeight,
        maxHeight,
        cursor: disabled ? "not-allowed" : "text",
        opacity: 1,
      }}
    />
  );
});

export const IsolatedComposerTextarea = memo(IsolatedComposerTextareaImpl);
