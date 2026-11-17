import { useRef, useState, type ReactNode } from "react";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";

export type RecordedTextSelection = { text: string; firstLine: number; lastLine: number };

export function readRecordedTextSelection(root: HTMLElement): RecordedTextSelection | undefined {
  const selection = root.ownerDocument.getSelection();
  const range = selection?.rangeCount === 1 ? selection.getRangeAt(0) : undefined;
  if (!range || range.collapsed || !root.contains(range.startContainer) || !root.contains(range.endContainer)) return;
  const text = range.toString();
  if (!text.trim()) return;
  const prefix = root.ownerDocument.createRange();
  prefix.selectNodeContents(root);
  prefix.setEnd(range.startContainer, range.startOffset);
  const firstLine = prefix.toString().split(/\r\n|\r|\n/).length;
  const lastLine = firstLine + text.replace(/(?:\r\n|\r|\n)$/, "").split(/\r\n|\r|\n/).length - 1;
  return { text, firstLine, lastLine };
}

export function RecordedTextActions({ label, text, groups, className, bodyClassName, excerptAction, onExcerpt, children }: {
  label: string; text: string; groups: MenuGroups; className?: string; bodyClassName: string;
  excerptAction: string; onExcerpt?: (selection: RecordedTextSelection) => void;
  children: (body: ReactNode) => ReactNode;
}) {
  const body = useRef<HTMLPreElement>(null);
  const [selected, select] = useState<RecordedTextSelection>();
  function capture(eligible = true) {
    select(eligible && body.current && onExcerpt ? readRecordedTextSelection(body.current) : undefined);
  }
  return <div onContextMenuCapture={(event) => capture(body.current?.contains(event.target as Node) === true)}
    onKeyDownCapture={(event) => { if (event.key === "ContextMenu" || event.shiftKey && event.key === "F10") capture(); }}
    onPointerDownCapture={(event) => { if (event.target instanceof Element && event.target.closest(".context-actions-more")) capture(); }}>
    <ContextActions label={label} className={className} groups={[...groups, selected && onExcerpt
      ? [{ label: excerptAction, run: () => onExcerpt(selected) }] : []]}>
      {children(<pre ref={body} tabIndex={onExcerpt ? 0 : undefined} className={bodyClassName}>{text}</pre>)}
    </ContextActions>
  </div>;
}
