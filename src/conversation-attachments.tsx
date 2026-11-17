import { useEffect, useRef, useState, type RefObject } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Paperclip, X } from "lucide-react";
import { ContextActions } from "@/components/ui/context-actions";
import type { AttachmentLimits } from "@/bindings/AttachmentLimits";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { useAttachmentActions } from "@/conversation-attachment-actions";
import { SafeAttachmentMedia } from "@/repository-note-media";

export type AttachmentDelivery = { kind: "native" | "reference" } | { kind: "excerpt"; start: number; end: number };
export type AttachmentSource = { kind: "checkout"; relativePath: string } | { kind: "selected_file"; path: string };
type Attachment = { id: string; source: AttachmentSource; delivery: AttachmentDelivery | null };
type AttachmentDraft = { revision: string | null; files: Attachment[] };
type DropReceipt = { id: string; x: number; y: number };
type Edit = { kind: "remove" } | { kind: "set_delivery"; delivery: AttachmentDelivery | null };
type AttachmentAction = { kind: "pick" } | { kind: "drop"; dropId: string } | { kind: "edit"; fileId: string; edit: Edit };

type Format = "png" | "jpeg" | "gif" | "webp" | "pdf" | "wav" | "mp3" | "text" | "unknown";
type FileReview = {
  id: string;
  metadata: { source: AttachmentSource; format: Format; size: number; sha256: string } | null;
  delivery: AttachmentDelivery | null;
  alternatives: AttachmentDelivery[];
  nativeUnavailable: string | null;
  blocker: string | null;
  excerpt: { start: number; end: number; text: string } | null;
  reference: { kind: "checkout_path" | "read_only_copy_of"; path: string } | null;
  media: string | null;
};
type Review = {
  previewId: string | null;
  files: FileReview[];
  projectLimits: AttachmentLimits;
  qualifiedLimits: {
    filesPerTurn: number | null;
    encodedRequestBytes: number | null;
    formats: { format: Format; fileBytes: number | null; inputTokens: number | null }[];
    context: { windowTokens: number; reservedOutputTokens: number; fixedInputTokens: number } | null;
  } | null;
  minimumEncodedBytes: number;
  estimatedContextInputTokens: number | null;
};
type ReviewInput = {
  conversationId: string; expectedRevision: string; selection: ModelSelection; effort: string | null; mode: "read_only" | "plan"; text: string;
  targetCheckoutId?: string;
};

const deliveryLabels = { native: "Native content", excerpt: "Text excerpt", reference: "File reference" };
const mime: Record<Format, string> = { png: "image/png", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp",
  pdf: "application/pdf", wav: "audio/wav", mp3: "audio/mpeg", text: "text/plain", unknown: "application/octet-stream" };

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

export function useConversationAttachmentReview({ conversationId, revision, contextId, selection, effort, text, mode, enabled, targetCheckoutId }: {
  conversationId: string; revision: string | null; contextId?: string; selection: ModelSelection | null;
  effort: string | null; text: string; mode: "read_only" | "plan" | "goal"; enabled: boolean; targetCheckoutId?: string;
}) {
  const key = enabled && revision && contextId && selection && mode !== "goal"
    ? JSON.stringify([revision, contextId, selection.provider, selection.profileId, selection.modelId, effort, mode, text, targetCheckoutId]) : null;
  const mutation = useMutation({
    scope: { id: `conversation-attachment-review:${conversationId}` },
    mutationFn: async ({ input, key }: { input: ReviewInput; key: string }) => ({
      key, review: await invoke<Review>("review_conversation_attachment_draft", input),
    }),
  });
  const request = useRef<(() => void) | undefined>(undefined);
  request.current = key && revision && selection && mode !== "goal" ? () => mutation.mutate({ key,
    input: { conversationId, expectedRevision: revision, selection, effort, mode, text, ...(targetCheckoutId && { targetCheckoutId }) },
  }) : undefined;
  useEffect(() => {
    if (!key) return;
    const timer = window.setTimeout(() => request.current?.(), 350);
    return () => window.clearTimeout(timer);
  }, [key]);
  const review = mutation.data?.key === key ? mutation.data.review : undefined;
  return { review, pending: Boolean(key && !review && !mutation.error), eligible: Boolean(key),
    error: mutation.variables?.key === key ? mutation.error : null, refresh: () => request.current?.() };
}

export function ConversationAttachments({ attachments, disabled, preview, checkoutId }: {
  attachments: ReturnType<typeof useConversationAttachments>;
  disabled: boolean;
  preview?: ReturnType<typeof useConversationAttachmentReview>;
  checkoutId?: string;
}) {
  if (!attachments.enabled) return null;
  const unavailable = disabled || attachments.busy;
  return <div className="grid gap-1">
    <button type="button" className="conv-quiet-button w-fit" onClick={attachments.pick} disabled={unavailable}>
      <Paperclip className="size-3.5" aria-hidden /> Attach files
    </button>
    {attachments.draft.files.length > 0 && <ul aria-label="Draft files" className="grid gap-1">
      {attachments.draft.files.map((file) => <DraftAttachmentRow key={file.id} file={file} checkoutId={checkoutId}
        review={preview?.review?.files.find((entry) => entry.id === file.id)} disabled={unavailable}
        change={(edit) => attachments.change(file.id, edit)} />)}
    </ul>}
    {attachments.draft.files.length > 0 && preview && <>
      {!preview.eligible && <p className="text-xs">Choose a conversation model to review file delivery.</p>}
      {preview.pending && <p role="status" className="text-xs">Reviewing files…</p>}
      {preview.review && <AttachmentLimitsOverview review={preview.review} />}
      {(preview.error || preview.review?.files.some(({ blocker }) => blocker)) && <button
        type="button" className="conv-quiet-button w-fit" disabled={unavailable} onClick={preview.refresh}>Review files again</button>}
      {preview.error && <p role="alert" className="conv-alert">{String(preview.error)}</p>}
    </>}
    {attachments.error != null && <p role="alert" className="conv-alert">{String(attachments.error)}</p>}
  </div>;
}

function DraftAttachmentRow({ file, checkoutId, review, disabled, change }: {
  file: Attachment; checkoutId?: string; review?: FileReview; disabled: boolean; change: (edit: Edit) => void;
}) {
  const path = pathOf(file.source);
  const name = path.split(/[\\/]/).at(-1) ?? path;
  const details = useRef<HTMLDetailsElement>(null);
  const { checkout, clipboard, copy, feedback } = useAttachmentActions(file.source, name, checkoutId);
  const excerpt = review?.excerpt?.text;
  const sha = review?.metadata?.sha256;
  const setDelivery = (delivery: AttachmentDelivery | null) => change({ kind: "set_delivery", delivery });
  const remove = () => change({ kind: "remove" });
  return <li className="grid min-w-0 gap-1 rounded border border-border px-2 py-1 text-xs">
    <ContextActions label={path} groups={[checkout, review ? [
      { label: "Show attachment review", run: () => { if (details.current) details.current.open = true; } },
      { label: "Delivery", disabled, items: [
        { label: "Automatic", disabled: file.delivery === null, run: () => setDelivery(null) },
        ...review.alternatives.map((delivery) => ({ label: deliveryLabels[delivery.kind],
          disabled: file.delivery?.kind === delivery.kind, run: () => setDelivery(delivery) })),
      ] },
    ] : [], [...clipboard,
      ...(sha ? [{ label: "Copy reviewed SHA-256", run: () => copy(sha) }] : []),
      ...(excerpt !== undefined ? [{ label: "Copy reviewed excerpt", run: () => copy(excerpt) }] : []),
    ], [{ label: "Remove from draft", disabled, run: remove }]]}>
      <span title={path} className="min-w-0 flex-1 truncate">{name} · {file.source.kind === "checkout" ? "Checkout" : "Selected external file"}</span>
      <button type="button" className="conv-icon-button" aria-label={`Remove file ${name}`} disabled={disabled}
        onClick={remove}><X size={14} aria-hidden /></button>
    </ContextActions>
    {feedback && <p role={feedback.failed ? "alert" : "status"} className="text-muted-foreground">{feedback.text}</p>}
    {review && <ReviewedAttachment key={`${file.id}:${review.delivery?.kind}:${review.excerpt?.start}:${review.excerpt?.end}`}
      details={details} name={name} path={path} file={file} review={review} disabled={disabled} onDelivery={setDelivery} />}
  </li>;
}

function ReviewedAttachment({ name, path, file, review, disabled, onDelivery, details }: {
  name: string; path: string; file: Attachment; review: FileReview; disabled: boolean;
  onDelivery: (delivery: AttachmentDelivery | null) => void;
  details: RefObject<HTMLDetailsElement | null>;
}) {
  const current = review.delivery;
  const [start, setStart] = useState(current?.kind === "excerpt" ? String(current.start) : "0");
  const [end, setEnd] = useState(current?.kind === "excerpt" ? String(current.end) : "");
  const from = Number(start);
  const to = Number(end);
  const alternatives = review.alternatives.filter((candidate) => candidate.kind !== file.delivery?.kind);
  return <details ref={details} open={Boolean(review.blocker)} className="min-w-0">
    <summary className="cursor-pointer">{current ? deliveryLabels[current.kind] : "Delivery unavailable"}
      {review.metadata && ` · ${review.metadata.size.toLocaleString()} bytes`}</summary>
    <div className="grid gap-2 py-1">
      <p className="break-all">{path}{review.metadata && ` · ${mime[review.metadata.format]}`}</p>
      <label>Delivery for {name} <select disabled={disabled} value={file.delivery?.kind ?? "auto"}
        onChange={(event) => onDelivery(alternatives.find(({ kind }) => kind === event.currentTarget.value) ?? null)}>
        <option value="auto">Automatic</option>
        {file.delivery && <option value={file.delivery.kind}>{deliveryLabels[file.delivery.kind]}</option>}
        {alternatives.map(({ kind }) => <option key={kind} value={kind}>{deliveryLabels[kind]}</option>)}
      </select></label>
      {review.nativeUnavailable && current?.kind !== "native" && <p>{review.nativeUnavailable}</p>}
      {current?.kind === "excerpt" && <div className="flex flex-wrap items-end gap-2">
        <label>Start byte <input min={0} type="number" value={start} onChange={(event) => setStart(event.currentTarget.value)} /></label>
        <label>End byte <input min={1} type="number" value={end} onChange={(event) => setEnd(event.currentTarget.value)} /></label>
        <button type="button" className="conv-quiet-button" disabled={disabled || !Number.isSafeInteger(from) || !Number.isSafeInteger(to) || from < 0 || to <= from}
          onClick={() => onDelivery({ kind: "excerpt", start: from, end: to })}>Use byte range</button>
      </div>}
      {review.excerpt && <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words">{review.excerpt.text}</pre>}
      {review.reference && <p className="break-all">{review.reference.kind === "checkout_path" ? "Reference target" : "Scoped read-only copy of"} {review.reference.path}</p>}
      {review.media !== null && review.metadata && <SafeAttachmentMedia base64={review.media} mimeType={mime[review.metadata.format]} title={name} />}
      {current?.kind === "native" && review.media === null && !review.blocker && <p>Inline preview unavailable for this file.</p>}
      {review.blocker && <p role="alert" className="conv-alert">{review.blocker}</p>}
    </div>
  </details>;
}

function AttachmentLimitsOverview({ review }: { review: Review }) {
  const { projectLimits: project, qualifiedLimits: provider } = review;
  return <details className="text-xs">
    <summary>Attachment limits</summary>
    <p>{Math.min(project.filesPerTurn, provider?.filesPerTurn ?? project.filesPerTurn)} files per turn · {project.bytesPerFile.toLocaleString()} bytes per file · {project.bytesPerTurn.toLocaleString()} bytes combined</p>
    <p>{project.utf8BytesPerExcerpt.toLocaleString()} UTF-8 bytes per excerpt</p>
    {provider?.formats.map(({ format, fileBytes }, index) => fileBytes !== null && <p key={`${format}:${index}`}>{mime[format]} · {Math.min(fileBytes, project.bytesPerFile).toLocaleString()} bytes per native file</p>)}
    {provider?.encodedRequestBytes != null && <p>Encoded request limit {provider.encodedRequestBytes.toLocaleString()} bytes · at least {review.minimumEncodedBytes.toLocaleString()} bytes estimated</p>}
    {provider?.context && <p>Context {provider.context.windowTokens.toLocaleString()} tokens · reserve {provider.context.reservedOutputTokens.toLocaleString()} for output
      {review.estimatedContextInputTokens !== null && ` · ${review.estimatedContextInputTokens.toLocaleString()} input tokens estimated before provider history`}</p>}
  </details>;
}
