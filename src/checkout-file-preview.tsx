import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { checkoutFileLiveText, readCheckoutFileText } from "@/file-context-actions";
import { useContextActionFeedback } from "@/components/ui/context-actions";
import { FileDialog } from "@/ide-file-workspace";
import { fileName } from "@/ide-knowledge-data";
import type { NoteAttachmentContent } from "@/repository-note-buffers";
import { downloadRepositoryAttachment, isNoteMediaPath, isNoteMediaType, readExactRepositoryAttachment, SafeAttachmentMedia } from "@/repository-note-media";

type Preview = { kind: "text"; text: string } | { kind: "media"; file: NoteAttachmentContent } | { kind: "unavailable" };
type Request = { path: string; liveText?: string };
const previewTextLimit = 256 * 1024;

async function readPreview(checkoutId: string, path: string): Promise<Preview> {
  if (isNoteMediaPath(path)) {
    const file = await readExactRepositoryAttachment(checkoutId, path);
    if (file && isNoteMediaType(file.mimeType)) return { kind: "media", file };
  }
  const result = await readCheckoutFileText(checkoutId, path);
  return result.status === "text" ? { kind: "text", text: result.text } : { kind: "unavailable" };
}

export function useCheckoutFilePreview({ checkoutId, currentText, open }: {
  checkoutId: string;
  currentText: (path: string) => string | undefined;
  open: (path: string) => void;
}) {
  const [selection, setSelection] = useState<Request>();
  const close = () => setSelection(undefined);
  return {
    action: (path: string) => ({
      label: "Preview…",
      run: () => setSelection({ path, liveText: checkoutFileLiveText(checkoutId, path, currentText) }),
    }),
    dialog: selection && <CheckoutFilePreview key={selection.path} checkoutId={checkoutId} request={selection}
      close={close} open={() => { close(); open(selection.path); }} />,
  };
}

function CheckoutFilePreview({ checkoutId, request, close, open }: {
  checkoutId: string;
  request: Request;
  close: () => void;
  open: () => void;
}) {
  const { path, liveText } = request;
  const { run, feedback } = useContextActionFeedback();
  const query = useQuery({
    queryKey: ["checkout-file-preview", checkoutId, path],
    queryFn: () => readPreview(checkoutId, path),
    enabled: liveText === undefined,
    retry: false,
    networkMode: "always",
    gcTime: 0,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
  });
  const launch = useMutation({
    networkMode: "always",
    mutationFn: () => invoke("open_checkout_item_in_shell", { checkoutId, relativePath: path, action: "open" }),
    onSuccess: close,
  });
  const preview: Preview | undefined = liveText !== undefined ? { kind: "text", text: liveText }
    : query.isFetching || query.error ? undefined : query.data;
  const text = preview?.kind === "text" ? preview.text : undefined;
  const shortened = text !== undefined && text.length > previewTextLimit;
  const visibleText = shortened ? text.slice(0, previewTextLimit).replace(/[\uD800-\uDBFF]$/, "") : text;

  return <FileDialog title="Preview file" description={path} className="file-comparison-dialog"
    onClose={() => { if (!launch.isPending) close(); }}>
    {query.isFetching && <p role="status" className="file-help">Loading preview…</p>}
    {query.error && <p role="alert" className="file-help">{String(query.error)}</p>}
    {preview?.kind === "unavailable" && <p role="status" className="file-help">A preview is unavailable for this file format.</p>}
    {preview?.kind === "media" && <div className="max-h-[62vh] overflow-auto [&_img]:mx-auto [&_img]:max-h-[56vh] [&_video]:max-w-full">
      <SafeAttachmentMedia base64={preview.file.contentBase64} mimeType={preview.file.mimeType}
        title={fileName(path)} relativePath={preview.file.relativePath} />
    </div>}
    {visibleText !== undefined && <pre aria-label="Text preview" className="m-0 max-h-[62vh] overflow-auto whitespace-pre-wrap break-words p-3 text-xs">
      {visibleText}
    </pre>}
    {visibleText === "" && <p role="status" className="file-help">This file is empty.</p>}
    {shortened && <p role="status" className="file-help">Showing the start of this file. Open it in the editor for the full text.</p>}
    {launch.error && <p role="alert" className="file-help">{String(launch.error)}</p>}
    {feedback && <p role={feedback.failed ? "alert" : "status"} className="file-help">{feedback.text}</p>}
    <div className="file-dialog-footer">
      <button type="button" className="fw-btn" disabled={launch.isPending} onClick={close}>Close</button>
      {preview?.kind === "media" && <button type="button" className="fw-btn"
        onClick={() => void run(async () => downloadRepositoryAttachment(preview.file))}>Save a copy</button>}
      {preview?.kind === "text" && <button type="button" className="fw-btn fw-primary" onClick={open}>Open in editor</button>}
      {preview && preview.kind !== "text" && <button type="button" className="fw-btn fw-primary"
        disabled={launch.isPending} onClick={() => launch.mutate()}>Open in default application</button>}
    </div>
  </FileDialog>;
}
