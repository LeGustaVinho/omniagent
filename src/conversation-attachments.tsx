import { useEffect, useRef, useState, type RefObject } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Paperclip, X } from "lucide-react";

export type AttachmentDelivery = { kind: "native" | "reference" } | { kind: "excerpt"; start: number; end: number };
export type AttachmentSource = { kind: "checkout"; relativePath: string } | { kind: "selected_file"; path: string };
type Attachment = { id: string; source: AttachmentSource; delivery: AttachmentDelivery | null };
type AttachmentDraft = { revision: string | null; files: Attachment[] };
type DropReceipt = { id: string; x: number; y: number };
type Edit = { kind: "remove" } | { kind: "set_delivery"; delivery: AttachmentDelivery | null };
type AttachmentAction = { kind: "pick" } | { kind: "drop"; dropId: string } | { kind: "edit"; fileId: string; edit: Edit };

const empty: AttachmentDraft = { revision: null, files: [] };

function pathOf(source: AttachmentSource) {
  return source.kind === "checkout" ? source.relativePath : source.path;
}

function containsPoint(element: HTMLElement | null, x: number, y: number) {
  const bounds = element?.getBoundingClientRect();
  const scale = window.devicePixelRatio || 1;
  return Boolean(bounds && bounds.width > 0 && bounds.height > 0 &&
    x / scale >= bounds.left && x / scale < bounds.right && y / scale >= bounds.top && y / scale < bounds.bottom);
}

export function useConversationAttachments(conversationId: string, enabled: boolean, visible: boolean, dropTarget: RefObject<HTMLElement | null>) {
  const client = useQueryClient();
  const key = ["conversation", conversationId, "attachments"];
  const query = useQuery({
    queryKey: key,
    queryFn: () => invoke<AttachmentDraft>("conversation_attachment_draft", { conversationId }),
    enabled: enabled && visible,
  });
  const draft = query.data ?? empty;
  const action = useMutation({
    scope: { id: `conversation-attachments:${conversationId}` },
    mutationFn: (action: AttachmentAction) => {
      switch (action.kind) {
        case "pick": return invoke<AttachmentDraft>("pick_conversation_attachments", { conversationId });
        case "drop": return invoke<AttachmentDraft>("accept_conversation_attachment_drop", { conversationId, dropId: action.dropId });
        case "edit": return invoke<AttachmentDraft>("edit_conversation_attachment_draft", {
          conversationId, expectedRevision: draft.revision, fileId: action.fileId, edit: action.edit,
        });
      }
    },
    onSuccess: (data) => client.setQueryData(key, data),
    onError: () => client.invalidateQueries({ queryKey: key }),
  });
  const accept = useRef(action.mutate);
  accept.current = action.mutate;
  const [hover, setHover] = useState(false);
  const [dropError, setDropError] = useState<unknown>();
  useEffect(() => {
    if (!enabled || !visible || !("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    const removers: (() => void)[] = [];
    const install = async () => {
      const webview = getCurrentWebview();
      const unlisten = await webview.listen<DropReceipt>("conversation-attachment-drop", ({ payload }) => {
        setHover(false);
        if (!disposed && containsPoint(dropTarget.current, payload.x, payload.y)) accept.current({ kind: "drop", dropId: payload.id });
      });
      if (disposed) { unlisten(); return; }
      removers.push(unlisten);
      const removeHover = await webview.onDragDropEvent(({ payload }) => {
        if (disposed) return;
        setHover((payload.type === "enter" || payload.type === "over") &&
          containsPoint(dropTarget.current, payload.position.x, payload.position.y));
      });
      if (disposed) removeHover(); else removers.push(removeHover);
    };
    void install().catch((error: unknown) => { if (!disposed) setDropError(error); });
    return () => {
      disposed = true;
      for (const remove of removers) remove();
    };
  }, [dropTarget, enabled, visible]);

  return { draft, enabled, hover: visible && hover, busy: action.isPending || (enabled && query.isPending), ready: !enabled || query.isSuccess,
    error: query.error || action.error || dropError,
    pick: () => action.mutate({ kind: "pick" }),
    change: (fileId: string, edit: Edit) => action.mutate({ kind: "edit", fileId, edit }),
    refresh: () => client.invalidateQueries({ queryKey: key }),
  };
}

export function ConversationAttachments({ attachments, disabled }: {
  attachments: ReturnType<typeof useConversationAttachments>;
  disabled: boolean;
}) {
  if (!attachments.enabled) return null;
  const unavailable = disabled || attachments.busy;
  return <div className="grid gap-1">
    <button type="button" className="conv-quiet-button w-fit" onClick={attachments.pick} disabled={unavailable}>
      <Paperclip className="size-3.5" aria-hidden /> Attach files
    </button>
    {attachments.draft.files.length > 0 && <ul aria-label="Draft files" className="grid gap-1">
      {attachments.draft.files.map(({ id, source }) => {
        const path = pathOf(source);
        const name = path.split(/[\\/]/).at(-1) ?? path;
        return <li key={id} className="flex min-w-0 items-center justify-between rounded border border-border px-2 py-1 text-xs">
          <span title={path} className="truncate">{name} · {source.kind === "checkout" ? "Checkout" : "Selected external file"}</span>
          <button type="button" className="conv-icon-button" aria-label={`Remove file ${name}`} disabled={unavailable}
            onClick={() => attachments.change(id, { kind: "remove" })}><X size={14} aria-hidden /></button>
        </li>;
      })}
    </ul>}
    {attachments.error != null && <p role="alert" className="conv-alert">{String(attachments.error)}</p>}
  </div>;
}
