import { invoke } from "@tauri-apps/api/core";
import { useRef, useState } from "react";
import { FileDialog, type FilePanelApi } from "@/ide-file-workspace";
import type { ReviewedNoteRevision, ReviewedNotesResult } from "@/repository-note-buffers";

type TagRenamePreview = {
  id: string;
  source: string;
  destination: string;
  files: (ReviewedNoteRevision & { occurrences: { line: number; before: string; after: string; property: boolean }[] })[];
  blocked: string[];
  warnings: { path: string; line: number | null; message: string }[];
};

export function RepositoryTagRename({ checkoutId, source, ownerPanel, onClose, onApply, onOpen, onSaved }: {
  checkoutId: string;
  source: string;
  ownerPanel?: FilePanelApi;
  onClose: () => void;
  onApply: (previewId: string, files: readonly ReviewedNoteRevision[], saveNow: boolean) => Promise<ReviewedNotesResult>;
  onOpen: (path: string) => void;
  onSaved: (tag: string) => void;
}) {
  const [destination, setDestination] = useState("");
  const [includeNested, setIncludeNested] = useState(false);
  const [preview, setPreview] = useState<TagRenamePreview>();
  const [result, setResult] = useState<ReviewedNotesResult>();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const running = useRef(false);
  const close = () => { if (!running.current) onClose(); };

  async function review() {
    if (!destination.trim() || running.current) return;
    running.current = true;
    setBusy(true);
    setError("");
    try {
      setPreview(await invoke<TagRenamePreview>("preview_markdown_tag_rename", { checkoutId, source, destination, includeNested }));
    } catch (failure) {
      setError(String(failure));
    } finally {
      running.current = false;
      setBusy(false);
    }
  }

  async function apply(saveNow: boolean) {
    if (!preview || preview.blocked.length || !preview.files.length || running.current) return;
    running.current = true;
    setBusy(true);
    setError("");
    try {
      const outcome = await onApply(preview.id, preview.files, saveNow);
      setResult(outcome);
      if (outcome.saved.length) onSaved(preview.destination);
    } catch (failure) {
      setError(String(failure));
      setPreview(undefined);
    } finally {
      running.current = false;
      setBusy(false);
    }
  }

  const description = preview ? "#" + preview.source + " → #" + preview.destination : "#" + source;
  return <FileDialog title={result ? "Tag rename result" : preview ? "Review tag rename" : "Rename tag"} description={description}
    ownerPanel={ownerPanel} onClose={close}>
    {result ? <>
      {result.saved.length > 0 && <ul aria-label="Saved notes" className="note-rewrite-list">
        {result.saved.map((path) => <li key={path}>{path} · Saved</li>)}
      </ul>}
      {result.failure && <p role="alert" className="break-words">{result.failure}</p>}
      {result.pending.length > 0 && <>
        <p>Open a pending draft to review it and use Save to write it to disk.</p>
        <ul aria-label="Pending tag drafts" className="note-rewrite-list">{result.pending.map((path) => <li key={path}>
          <button type="button" className="fw-btn" onClick={() => onOpen(path)}>{path}</button>
        </li>)}</ul>
      </>}
      <div className="file-dialog-footer"><button type="button" className="fw-btn" onClick={close}>Close</button></div>
    </> : preview ? <>
      {preview.files.length ? <ul aria-label="Tag occurrences" className="note-rewrite-list">{preview.files.map(({ path, occurrences }) => <li key={path}>
        <details open><summary>{path} · {occurrences.length}</summary>
          {occurrences.map(({ line, before, after, property }, index) => <div key={index} className="file-dialog-comparison">
            <div><h3>Line {line}{property ? " · Property" : ""}</h3><pre>{before}</pre></div>
            <div><h3>Proposed</h3><pre>{after}</pre></div>
          </div>)}
        </details>
      </li>)}</ul> : <p>No saved occurrences were found.</p>}
      {preview.blocked.length > 0 && <>
        <p role="alert">Save or discard these drafts and review the rename again.</p>
        <ul aria-label="Affected drafts" className="note-rewrite-list">{preview.blocked.map((path) => <li key={path}>
          <button type="button" className="fw-btn" onClick={() => onOpen(path)}>{path}</button>
        </li>)}</ul>
      </>}
      {preview.warnings.length > 0 && <ul aria-label="Tags requiring review" className="note-rewrite-list">
        {preview.warnings.map(({ path, line, message }, index) => <li key={index}>{path}{line === null ? "" : " · Line " + line} · {message}</li>)}
      </ul>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={() => setPreview(undefined)}>Back</button>
        <button type="button" className="fw-btn" disabled={busy || !!preview.blocked.length || !preview.files.length} onClick={() => void apply(false)}>Stage drafts</button>
        <button type="button" className="fw-btn fw-primary" disabled={busy || !!preview.blocked.length || !preview.files.length} onClick={() => void apply(true)}>Rename and save</button>
      </div>
    </> : <form onSubmit={(event) => { event.preventDefault(); void review(); }}>
      <label className="file-field">New tag name
        <input autoFocus required value={destination} disabled={busy} onChange={(event) => setDestination(event.currentTarget.value)} />
      </label>
      <label className="flex items-center gap-2 py-2 text-xs">
        <input type="checkbox" checked={includeNested} disabled={busy} onChange={(event) => setIncludeNested(event.currentTarget.checked)} />
        Include nested tags
      </label>
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={close}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={busy || !destination.trim()}>Review occurrences</button>
      </div>
    </form>}
    {busy && <p role="status">{preview ? "Applying the reviewed revisions…" : "Finding tags in this checkout…"}</p>}
    {error && <p role="alert">{error}</p>}
  </FileDialog>;
}
