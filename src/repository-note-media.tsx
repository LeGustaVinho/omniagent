import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import pdfWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { fileName, normalizeRepositoryPath } from "@/ide-knowledge-data";
import { useContextActionFeedback } from "@/components/ui/context-actions";
import { decodeArtifactBytes, downloadArtifact } from "@/download-artifact";
import { isContextKey, useNoteContextMenu } from "@/knowledge-context";
import type { NoteAttachmentContent } from "@/repository-note-buffers";

const fileExtension = /\.(?:avif|bmp|gif|jpe?g|png|svg|webp|aac|flac|m4a|mp3|ogg|opus|wav|m4v|mov|mp4|ogv|webm|pdf)$/i;
const imageTypes = new Set(["image/avif", "image/bmp", "image/gif", "image/jpeg", "image/png", "image/svg+xml", "image/webp"]);
const audioTypes = new Set(["audio/aac", "audio/flac", "audio/mp4", "audio/mpeg", "audio/ogg", "audio/opus", "audio/wav", "audio/x-wav"]);
const videoTypes = new Set(["video/mp4", "video/ogg", "video/quicktime", "video/webm", "video/x-m4v"]);

export const isNoteMediaPath = (path: string) => fileExtension.test(path);

export function isNoteMediaType(mimeType: string) {
  return mimeType === "application/pdf" || imageTypes.has(mimeType) || audioTypes.has(mimeType) || videoTypes.has(mimeType);
}

export async function readExactRepositoryAttachment(checkoutId: string, path: string) {
  const file = await invoke<NoteAttachmentContent | null>("read_repository_attachment", { checkoutId, path });
  if (file && file.relativePath.toLowerCase() !== path.toLowerCase()) {
    throw new Error("The file now resolves to a different path.");
  }
  return file;
}

export function downloadRepositoryAttachment(file: NoteAttachmentContent) {
  downloadArtifact(fileName(file.relativePath), new Blob([decodeArtifactBytes(file.contentBase64)], { type: file.mimeType }));
}

export function noteAttachmentCandidates(target: string, from: string, wiki: boolean) {
  if (!isNoteMediaPath(target) || target.includes(":") || /^[\\/]{2}/.test(target)) return [];
  const path = target.replaceAll("\\", "/");
  const parent = from.slice(0, from.lastIndexOf("/") + 1);
  const reference = path.startsWith("/") ? path.slice(1) : parent + path;
  const candidates = wiki && !path.startsWith("/") && !/^\.\.?\//.test(path) ? [reference, path] : [reference];
  return [...new Set(candidates.flatMap((candidate) => {
    const normalized = normalizeRepositoryPath(candidate);
    return normalized && normalized.split("/")[0]?.toLowerCase() !== ".git" ? [normalized] : [];
  }))];
}

function PdfAttachment({ base64, title, relativePath }: { base64: string; title: string; relativePath?: string }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [page, setPage] = useState(1);
  const [pageCount, setPageCount] = useState(1);
  const [status, setStatus] = useState("Rendering PDF…");
  const [text, setText] = useState<{ base64: string; page: number; value: string }>();
  const { open: openMenu, overlay } = useNoteContextMenu();
  const { feedback, copy } = useContextActionFeedback();
  useEffect(() => {
    let alive = true;
    let loading: import("pdfjs-dist").PDFDocumentLoadingTask | undefined;
    let rendering: import("pdfjs-dist").RenderTask | undefined;
    const target = canvas.current;
    if (!target) return;
    setStatus("Rendering PDF…");
    void (async () => {
      try {
        const pdf = await import("pdfjs-dist");
        if (!alive) return;
        pdf.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;
        loading = pdf.getDocument({
          data: Uint8Array.from(atob(base64), (value) => value.charCodeAt(0)), enableXfa: false, useSystemFonts: true,
        });
        const document = await loading.promise;
        const renderedPage = await document.getPage(page);
        if (!alive) return;
        setPageCount(document.numPages);
        const original = renderedPage.getViewport({ scale: 1 });
        const maxDimension = Math.max(original.width, original.height);
        if (!Number.isFinite(maxDimension) || maxDimension <= 0) throw new Error("Invalid PDF page");
        const view = renderedPage.getViewport({ scale: Math.min(1, 1400 / maxDimension) });
        target.width = Math.max(1, Math.ceil(view.width));
        target.height = Math.max(1, Math.ceil(view.height));
        rendering = renderedPage.render({ canvas: target, viewport: view, background: "#ffffff" });
        await rendering.promise;
        if (!alive) return;
        setStatus("");
        void Promise.resolve().then(() => renderedPage.getTextContent()).then(({ items }) => {
          if (!alive) return;
          const value = items.map((item) => "str" in item ? item.str + (item.hasEOL ? "\n" : "") : "").join("");
          setText({ page, base64, value });
        }).catch(() => { if (alive) setText(undefined); });
      } catch {
        if (alive) setStatus("This PDF could not be rendered.");
      }
    })();
    return () => {
      alive = false;
      rendering?.cancel();
      if (loading) void loading.destroy().catch(() => undefined);
    };
  }, [base64, page]);

  const pageText = text?.page === page && text.base64 === base64 ? text.value.trim() : "";
  const quote = (point: { x: number; y: number }) => {
    if (!pageText || status) return false;
    return openMenu(title + " · Page " + page, [[{
      label: "Copy page as quote",
      run: () => copy(pageText.split(/\r?\n/).map((line) => "> " + line.trimEnd()).join("\n")
        + "\n\n— " + (relativePath ?? title) + " · Page " + page),
    }]], point, () => canvas.current?.focus());
  };

  return <span className="note-pdf-preview">
    <canvas ref={canvas} className="note-pdf" aria-label={title + " page " + page} hidden={Boolean(status)}
      tabIndex={pageText ? 0 : undefined} onContextMenu={(event) => {
        if (quote({ x: event.clientX, y: event.clientY })) { event.preventDefault(); event.stopPropagation(); }
      }} onKeyDown={(event) => {
        if (!isContextKey(event.nativeEvent)) return;
        const rect = event.currentTarget.getBoundingClientRect();
        if (quote({ x: rect.left, y: rect.bottom })) { event.preventDefault(); event.stopPropagation(); }
      }} />
    {overlay}
    {feedback && <span role={feedback.failed ? "alert" : "status"} className="file-help">{feedback.text}</span>}
    {status && <span role="status" className="file-help">{status}</span>}
    {pageCount > 1 && <span className="file-toolbar">
      <button type="button" className="fw-btn" disabled={page === 1} onClick={() => setPage(page - 1)}>Previous page</button>
      <span>Page {page} of {pageCount}</span>
      <button type="button" className="fw-btn" disabled={page === pageCount} onClick={() => setPage(page + 1)}>Next page</button>
    </span>}
  </span>;
}

export function RepositoryNoteMedia({ checkoutId, paths, title, allowSaveCopy }: {
  checkoutId: string; paths: string[]; title: string; allowSaveCopy?: boolean;
}) {
  const { run, feedback } = useContextActionFeedback();
  const attachment = useQuery({
    queryKey: ["repository-note-attachment", checkoutId, ...paths],
    enabled: paths.length > 0,
    queryFn: async () => {
      for (const path of paths) {
        const content = await invoke<NoteAttachmentContent | null>("read_repository_attachment", { checkoutId, path });
        if (content) return content;
      }
      return null;
    },
  });
  const file = attachment.data;
  if (!paths.length || attachment.error || file === null) return <span className="file-help">Attachment unavailable · {title}</span>;
  if (!file) return <span className="file-help" role="status">Loading attachment · {title}</span>;
  return <>
    <SafeAttachmentMedia key={file.relativePath} base64={file.contentBase64} mimeType={file.mimeType} title={title} relativePath={file.relativePath} />
    {allowSaveCopy && <div className="file-dialog-footer">
      {feedback && <span role={feedback.failed ? "alert" : "status"}>{feedback.text}</span>}
      <button type="button" className="fw-btn" onClick={() => void run(async () => downloadRepositoryAttachment(file))}>Save a copy</button>
    </div>}
  </>;
}

export function SafeAttachmentMedia({ base64, mimeType, title, relativePath }: { base64: string; mimeType: string; title: string; relativePath?: string }) {
  const [failed, setFailed] = useState<string>();
  if (failed === base64) return <span className="file-help">Attachment preview unavailable · {title}</span>;
  const dataUrl = `data:${mimeType};base64,${base64}`;
  const onError = () => setFailed(base64);
  if (mimeType === "application/pdf") return <PdfAttachment base64={base64} title={title} relativePath={relativePath} />;
  if (imageTypes.has(mimeType)) return <img className="note-media-image" src={dataUrl} alt={title} loading="lazy" onError={onError} />;
  if (audioTypes.has(mimeType)) return <audio src={dataUrl} aria-label={title} controls preload="metadata" onError={onError} />;
  if (videoTypes.has(mimeType)) return <video src={dataUrl} aria-label={title} controls preload="metadata" onError={onError} />;
  if (mimeType === "text/plain") {
    const bytes = Uint8Array.from(atob(base64), (byte) => byte.charCodeAt(0));
    return <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words">{new TextDecoder().decode(bytes)}</pre>;
  }
  return <span className="file-help">Attachment preview unavailable · {title}</span>;
}
