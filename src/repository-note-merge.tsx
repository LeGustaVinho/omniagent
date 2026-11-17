import { invoke } from "@tauri-apps/api/core";
import { useRef, useState } from "react";
import { FileDialog } from "@/ide-file-workspace";
import type { MergeRevision, SavedNoteMerge } from "@/repository-note-buffers";

type MergePreview = {
  id: string;
  source: string;
  destination: string;
  mergedMarkdown: string;
  sourceRedirect: string;
  files: MergeRevision[];
  references: { path: string; edits: { line: number; before: string; after: string }[] }[];
  warnings: { path: string; line: number | null; message: string }[];
};

export function RepositoryNoteMerge({ checkoutId, source, suggestedDestination, destinations, onClose, onApply, onOpen, onSaved }: {
  checkoutId: string;
  source: string;
  suggestedDestination?: string;
  destinations: readonly string[];
  onClose: () => void;
  onApply: (previewId: string, files: readonly MergeRevision[], saveNow: boolean) => Promise<SavedNoteMerge>;
  onOpen: (path: string) => void;
  onSaved: () => void;
}) {
  const [destination, setDestination] = useState(suggestedDestination ?? "");
  const [preview, setPreview] = useState<MergePreview>();
  const [result, setResult] = useState<SavedNoteMerge>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const submitting = useRef(false);
  const close = () => { if (!submitting.current) onClose(); };

  async function run<T>(operation: () => Promise<T>) {
    if (submitting.current) return;
    submitting.current = true;
    setBusy(true);
    setError("");
    try {
      return await operation();
    } catch (failure) {
      setError(String(failure));
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  }

  async function apply(saveNow: boolean) {
    if (!preview || submitting.current) return;
    const outcome = await run(() => onApply(preview.id, preview.files, saveNow));
    if (outcome) {
      setResult(outcome);
      if (outcome.saved.length) onSaved();
    } else if (!submitting.current) {
      setPreview(undefined);
    }
  }

  return <FileDialog title={result ? "Merge result" : preview ? "Review note merge" : "Merge notes"}
    description={preview ? preview.source + " → " + preview.destination : source} onClose={close}>
    {result ? <>
      {result.saved.length > 0 && <ul className="note-rewrite-list" aria-label="Saved notes">
        {result.saved.map((path) => <li key={path}>{path} · Saved</li>)}
      </ul>}
      {result.failure && <p role="alert" className="break-words">{result.failure}</p>}
      {result.pending.length > 0 && <>
        <p>Open each pending draft to review it and use Save to write it to disk.</p>
        <ul className="note-rewrite-list" aria-label="Pending merge drafts">{result.pending.map((path) => <li key={path}>
          <button type="button" className="fw-btn" onClick={() => onOpen(path)}>{path}</button>
        </li>)}</ul>
      </>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" onClick={close}>Close</button>
        <button type="button" className="fw-btn fw-primary" onClick={() => onOpen(destination)}>Open merged note</button>
      </div>
    </> : preview ? <>
      <p>The source becomes a note linking to the destination. Its properties stay with the source.</p>
      <div className="note-rewrite-list">
        <details><summary>Merged destination</summary>
          <pre className="max-h-52 overflow-auto whitespace-pre-wrap">{preview.mergedMarkdown}</pre>
        </details>
        <details><summary>Source redirect</summary>
          <pre className="max-h-40 overflow-auto whitespace-pre-wrap">{preview.sourceRedirect}</pre>
        </details>
        {preview.references.length > 0 && <ul className="note-rewrite-list" aria-label="Reference rewrites">
          {preview.references.map(({ path, edits }) => <li key={path}>
            <details open><summary>{path}</summary>{edits.map(({ line, before, after }, index) => <div key={index} className="file-dialog-comparison">
              <div><h3>Line {line} · Current</h3><pre>{before}</pre></div>
              <div><h3>Proposed</h3><pre>{after}</pre></div>
            </div>)}</details>
          </li>)}
        </ul>}
        {preview.warnings.length > 0 && <ul aria-label="Links requiring review">
          {preview.warnings.map(({ path, line, message }, index) => <li key={index}>{path}{line === null ? "" : " · Line " + line} · {message}</li>)}
        </ul>}
      </div>
      <p>Stage drafts to review them in the editors, or merge and save the reviewed files now.</p>
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={() => setPreview(undefined)}>Back</button>
        <button type="button" className="fw-btn" disabled={busy} onClick={() => void apply(false)}>Stage drafts</button>
        <button type="button" className="fw-btn fw-primary" disabled={busy} onClick={() => void apply(true)}>Merge and save</button>
      </div>
    </> : <form onSubmit={(event) => {
      event.preventDefault();
      if (destination) void run(() => invoke<MergePreview>("preview_markdown_merge", { checkoutId, source, destination }))
        .then((value) => { if (value) setPreview(value); });
    }}>
      <label className="grid gap-1 py-2 text-xs">Destination note
        <select className="delivery-filter" value={destination} disabled={busy} onChange={(event) => setDestination(event.currentTarget.value)}>
          <option value="">Choose a note</option>
          {destinations.map((path) => <option key={path} value={path}>{path}</option>)}
        </select>
      </label>
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={close}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={busy || !destination}>Review merge</button>
      </div>
    </form>}
    {busy && <p role="status">{preview ? "Applying the reviewed revisions…" : "Checking references in this checkout…"}</p>}
    {error && <p role="alert">{error}</p>}
  </FileDialog>;
}
