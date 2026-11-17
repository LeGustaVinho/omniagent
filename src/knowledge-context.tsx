import { useCallback, useState } from "react";
import { PositionedContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { useFileEditorDocument } from "@/file-editor-popout";
import type { EditorExcerpt } from "@/editor-context";
import type { NoteAnchorKind, NoteTarget } from "@/knowledge-document";

export type NoteSelection = { from: number; to: number; text: string; source: string };

export type NoteContextHit = {
  path: string;
  line?: number;
  anchor?: string;
  anchorKind?: NoteAnchorKind;
  anchorLine?: number;
  link?: NoteTarget;
  attachment?: { paths: readonly string[]; label: string };
  tag?: string;
  excerpt?: EditorExcerpt;
  selection?: NoteSelection;
  property?: { name: string; value: string };
  mermaid?: string;
  diagramImage?: HTMLImageElement;
};

export type NoteContextActions = (hit: NoteContextHit) => MenuGroups;

export function noteWikilink(path: string, anchor?: string) {
  if (/[#\[\]|\r\n]/.test(path) || anchor && /[\[\]|\r\n]/.test(anchor)) return;
  return `[[${path}${anchor ? `#${anchor}` : ""}]]`;
}

export function isContextKey(event: KeyboardEvent) {
  return event.key === "ContextMenu" || event.shiftKey && event.key === "F10";
}

export function useNoteContextMenu() {
  const owner = useFileEditorDocument();
  const [menu, setMenu] = useState<{ label: string; groups: MenuGroups; point: { x: number; y: number }; focus: () => void }>();
  const open = useCallback((label: string, groups: MenuGroups | undefined, point: { x: number; y: number }, focus: () => void) => {
    if (!groups?.some((group) => group.length)) return false;
    setMenu({ label, groups, point, focus });
    return true;
  }, []);
  return { open, overlay: menu && <PositionedContextActions label={menu.label} groups={menu.groups} point={menu.point}
    onClose={() => setMenu(undefined)} restoreFocus={menu.focus} container={owner?.body} /> };
}
