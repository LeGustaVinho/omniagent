import { useRef, useState } from "react";
import { containingFolder } from "@/file-context-actions";
import { FileDialog, type FilePanelApi } from "@/ide-file-workspace";
import type { NoteSelection } from "@/knowledge-context";
import type { NoteExtractionDestination } from "@/repository-note-buffers";

export function ExtractNoteDialog({ path, selection, folders = [], onClose, onExtract, onCreated, ownerPanel }: {
  ownerPanel?: FilePanelApi;
  path: string;
  selection: NoteSelection;
  folders?: readonly string[];
  onClose: () => void;
  onExtract: (destination: NoteExtractionDestination) => Promise<string>;
  onCreated: (path: string, replaced: boolean) => void;
}) {
  const sourceFolder = containingFolder(path);
  const [relativeDirectory, setDirectory] = useState(sourceFolder);
  const [name, setName] = useState("");
  const [replaceWithLink, setReplace] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const submitting = useRef(false);

  async function extract() {
    if (!name.trim() || submitting.current) return;
    const filename = /\.(?:md|markdown)$/i.test(name) ? name : `${name}.md`;
    submitting.current = true;
    setBusy(true);
    setError("");
    let created: string;
    try {
      created = await onExtract({ relativeDirectory, name: filename, replaceWithLink });
    } catch (failure) {
      setError(String(failure));
      setBusy(false);
      submitting.current = false;
      return;
    }
    onCreated(created, replaceWithLink);
  }

  return <FileDialog title="Extract selection to a new note" description={path} ownerPanel={ownerPanel}
    onClose={() => { if (!submitting.current) onClose(); }}>
    <form onSubmit={(event) => { event.preventDefault(); void extract(); }}>
      <label className="grid gap-1 py-2 text-xs">Destination folder
        <select className="delivery-filter" value={relativeDirectory} disabled={busy}
          onChange={(event) => setDirectory(event.currentTarget.value)}>
          <option value="">Checkout root</option>
          {[...new Set([sourceFolder, ...folders].filter(Boolean))].map((folder) => <option key={folder} value={folder}>{folder}</option>)}
        </select>
      </label>
      <label className="file-field">New note name
        <input autoFocus required value={name} disabled={busy} onChange={(event) => setName(event.currentTarget.value)} />
      </label>
      <label className="flex items-center gap-2 py-2 text-xs">
        <input type="checkbox" checked={replaceWithLink} disabled={busy} onChange={(event) => setReplace(event.currentTarget.checked)} />
        Replace selection with a link in the source draft
      </label>
      <pre aria-label="Selected Markdown" className="max-h-40 overflow-auto whitespace-pre-wrap text-xs">{selection.text}</pre>
      {error && <p role="alert" className="file-help">{error}</p>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={onClose}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={busy || !name.trim()}>{busy ? "Extracting…" : "Extract selection"}</button>
      </div>
    </form>
  </FileDialog>;
}
