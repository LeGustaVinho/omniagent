import { useEffect, useRef, useState } from "react";
import { FileDialog } from "@/ide-file-workspace";
import { lineEndingLabels, sourceEncodingLabels, type ReviewedSourceFormat, type SourceFormatChange } from "@/source-file-format";

export function SourceFormatDialog({ path, change, preview, save, onClose, onSaved }: {
  path: string;
  change: SourceFormatChange;
  preview: (path: string, change: SourceFormatChange) => Promise<ReviewedSourceFormat>;
  save: (reviewed: ReviewedSourceFormat) => Promise<void>;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [reviewed, setReviewed] = useState<ReviewedSourceFormat>();
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const confirming = useRef(false);
  useEffect(() => {
    let live = true;
    void preview(path, change).then(
      (result) => { if (live) setReviewed(result); },
      (failure: unknown) => { if (live) setError(String(failure)); },
    );
    return () => { live = false; };
  }, [change, path, preview]);

  const confirm = async () => {
    if (!reviewed || confirming.current || error) return;
    confirming.current = true;
    setSaving(true);
    try {
      await save(reviewed);
      onSaved();
    } catch (failure) {
      setError(String(failure));
    } finally {
      confirming.current = false;
      setSaving(false);
    }
  };
  const bytes = reviewed?.preview;
  return <FileDialog title="Review file format conversion" description={path} onClose={() => { if (!saving) onClose(); }}>
    {!bytes && !error && <p role="status" className="file-help">Checking the saved file…</p>}
    {error && <p role="alert" className="file-help">{error}</p>}
    {bytes && <>
      <p className="file-help">The conversion changes how the saved text is written to disk.</p>
      <div className="file-dialog-comparison">
        <div><h3>Current format</h3><p>{sourceEncodingLabels[bytes.originalEncoding]} · {lineEndingLabels[bytes.originalLineEndings]} · {bytes.originalBytes} bytes</p>
          <code className="block break-all" aria-label="Current initial bytes">{bytes.originalPrefix || "Empty file"}</code></div>
        <div><h3>Converted format</h3><p>{sourceEncodingLabels[bytes.encoding]} · {lineEndingLabels[bytes.lineEndings]} · {bytes.convertedBytes} bytes</p>
          <code className="block break-all" aria-label="Converted initial bytes">{bytes.convertedPrefix || "Empty file"}</code></div>
      </div>
    </>}
    <div className="file-dialog-footer">
      <button type="button" className="fw-btn" disabled={saving} onClick={onClose}>Cancel</button>
      <button type="button" className="fw-btn fw-primary" disabled={!reviewed || Boolean(error) || saving}
        onClick={() => void confirm()}>{saving ? "Converting…" : "Convert and save"}</button>
    </div>
  </FileDialog>;
}
