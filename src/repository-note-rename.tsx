import { invoke } from "@tauri-apps/api/core";
import { useState, type FormEvent } from "react";
import { FileDialog } from "@/ide-file-workspace";
import { normalizeRepositoryPath } from "@/ide-knowledge-data";

type Recovery = { message: string; editedPath?: string | null; displacedPath?: string | null };
type Warning = { path: string; line: number | null; message: string };
type Preview = {
  id: string;
  source: string;
  destination: string;
  files: { path: string; edits: { line: number; before: string; after: string }[] }[];
  warnings: Warning[];
};
type FileResult = { status: "rewritten"; count: number } | { status: "changed" | "failed"; message: string; recovery?: Recovery | null }
  | { status: "recovery_required"; recovery: Recovery };
type RenameReport = {
  source: string;
  destination: string;
  outcome: { status: "moved" } | { status: "conflict" | "failed"; message: string }
    | { status: "recovery_required"; recovery: Recovery };
  partial: boolean;
  files: (FileResult & { path: string })[];
  warnings: Warning[];
};

function RecoveryFiles({ recovery }: { recovery: Recovery }) {
  return <>
    <p>{recovery.message}</p>
    {recovery.editedPath && <code className="break-all">{recovery.editedPath}</code>}
    {recovery.displacedPath && <code className="break-all">{recovery.displacedPath}</code>}
  </>;
}

function Warnings({ warnings }: { warnings: Warning[] }) {
  if (!warnings.length) return null;
  return <ul aria-label="Links requiring review" className="grid gap-1">
    {warnings.map(({ path, line, message }, index) => <li key={index}>{path}{line === null ? "" : ` · Line ${line}`} · {message}</li>)}
  </ul>;
}

export function RepositoryNoteRename({ checkoutId, path, sourceRevisionId, canApply, onClose, onSettled }: {
  checkoutId: string;
  path: string;
  sourceRevisionId: string;
  canApply: boolean;
  onClose: () => void;
  onSettled: (destination?: string) => void;
}) {
  const [destination, setDestination] = useState(path);
  const [preview, setPreview] = useState<Preview>();
  const [report, setReport] = useState<RenameReport>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const close = () => { if (!pending) onClose(); };

  async function review(event: FormEvent) {
    event.preventDefault();
    if (pending) return;
    const target = normalizeRepositoryPath(destination.trim());
    if (!target || !/\.(?:md|markdown)$/i.test(target) || target === path) {
      setError("Enter a different path inside the checkout ending in .md or .markdown.");
      return;
    }
    setPending(true);
    setError("");
    try {
      setPreview(await invoke<Preview>("preview_markdown_rename", {
        checkoutId, source: path, destination: target, expectedSourceRevision: sourceRevisionId,
      }));
    } catch (failure) {
      setError(String(failure));
    } finally {
      setPending(false);
    }
  }

  async function apply() {
    if (!preview || pending || !canApply) return;
    setPending(true);
    setError("");
    try {
      const settled = await invoke<RenameReport>("apply_markdown_rename", { checkoutId, previewId: preview.id });
      setReport(settled);
      onSettled(settled.outcome.status === "moved" ? settled.destination : undefined);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setPending(false);
    }
  }

  if (report) return <FileDialog title="Rename result" description={`${report.source} → ${report.destination}`} onClose={close}>
    {report.outcome.status === "moved" ? <p>{report.partial ? "Note moved. Some link rewrites require attention." : "Note moved."}</p>
      : report.outcome.status === "recovery_required" ? <RecoveryFiles recovery={report.outcome.recovery} />
        : <p role="alert">{report.outcome.message}</p>}
    <ul className="note-rewrite-list" aria-label="Affected files">{report.files.map((file) => <li key={file.path}>
      <strong>{file.path}</strong>
      {file.status === "rewritten" ? <p>{file.count} link {file.count === 1 ? "rewrite" : "rewrites"} applied.</p>
        : file.status === "recovery_required" ? <RecoveryFiles recovery={file.recovery} />
          : file.recovery ? <RecoveryFiles recovery={file.recovery} /> : <p>{file.message}</p>}
    </li>)}</ul>
    <Warnings warnings={report.warnings} />
    <div className="file-dialog-footer"><button type="button" className="fw-btn" onClick={close}>Close</button></div>
  </FileDialog>;

  return <FileDialog title={preview ? "Review link rewrites" : "Rename note"}
    description={preview ? `${preview.source} → ${preview.destination}` : `Choose a new path within the checkout for ${path}.`} onClose={close}>
    {!preview ? <form onSubmit={(event) => void review(event)} className="grid gap-3">
      <label>New path<input value={destination} onChange={(event) => setDestination(event.currentTarget.value)} disabled={pending} /></label>
      <div className="file-dialog-footer"><button type="submit" className="fw-btn fw-primary" disabled={pending}>Preview link rewrites</button></div>
    </form> : <>
      {preview.files.length ? <ul className="note-rewrite-list">{preview.files.map(({ path, edits }) => <li key={path}>
        <details open><summary>{path}</summary>{edits.map(({ line, before, after }, index) => <div key={index} className="file-dialog-comparison">
          <div><h3>Line {line} · Current</h3><pre>{before}</pre></div><div><h3>Proposed</h3><pre>{after}</pre></div>
        </div>)}</details>
      </li>)}</ul> : <p>No links need to change.</p>}
      <Warnings warnings={preview.warnings} />
      {!canApply && <p role="alert">The note changed. Close this preview and review a new one.</p>}
      <div className="file-dialog-footer"><button type="button" className="fw-btn fw-primary" disabled={pending || !canApply}
        onClick={() => void apply()}>Apply reviewed changes</button></div>
    </>}
    {pending && <p role="status">{preview ? "Applying the reviewed revisions…" : "Checking links in this checkout…"}</p>}
    {error && <p role="alert">{error}</p>}
  </FileDialog>;
}
