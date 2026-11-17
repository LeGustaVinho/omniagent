import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { FileDialog, type FileTreeTarget } from "@/ide-file-workspace";
import type { MenuGroups } from "@/components/ui/context-actions";

type Kind = "file" | "note" | "folder";
const labels = { file: "New file", note: "New note", folder: "New folder" };

export function useCheckoutTreeActions({ checkoutId, simulated, knowledge, refresh, onCreated, report }: {
  checkoutId: string;
  simulated: boolean;
  knowledge?: boolean;
  refresh: () => Promise<unknown>;
  onCreated: (path: string, kind: Kind) => void;
  report: (message: string) => void;
}) {
  const folders = useQuery({
    queryKey: ["checkout-folders", checkoutId],
    queryFn: () => invoke<string[]>("list_checkout_folders", { checkoutId }),
    enabled: !simulated,
    retry: false,
  });
  const [request, setRequest] = useState<{ path: string; kind: Kind }>();
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const refreshTree = () => Promise.all([refresh(), folders.refetch({ throwOnError: true })]);

  function actions(item: FileTreeTarget): MenuGroups {
    if (simulated || item.kind === "file") return [];
    const kinds: Kind[] = knowledge ? ["note", "folder"] : ["file", "note", "folder"];
    return [kinds.map((kind) => ({ label: `${labels[kind]}…`, run: () => {
      setName("");
      setError("");
      setRequest({ path: item.path, kind });
    } })), item.kind === "root" ? [{ label: "Refresh tree", run: () => {
      report("");
      void refreshTree().catch((failure: unknown) => report(String(failure)));
    } }] : []];
  }

  async function create() {
    if (!request || busy || !name.trim()) return;
    const { kind, path } = request;
    const filename = kind === "note" && !/\.(?:md|markdown)$/i.test(name) ? `${name}.md` : name;
    setBusy(true);
    setError("");
    try {
      const created = await invoke<string>("create_checkout_item", {
        checkoutId, relativeDirectory: path, name: filename, directory: kind === "folder",
      });
      setRequest(undefined);
      await refreshTree().catch((failure: unknown) => report(String(failure)));
      onCreated(created, kind);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  }

  const dialog = request && <FileDialog title={labels[request.kind]} description={request.path || "Checkout root"}
    onClose={() => { if (!busy) setRequest(undefined); }}>
    <form onSubmit={(event) => { event.preventDefault(); void create(); }}>
      <label className="file-field"><input autoFocus aria-label="Name" value={name} disabled={busy}
        onChange={(event) => setName(event.currentTarget.value)} /></label>
      {error && <p role="alert" className="file-help">{error}</p>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={() => setRequest(undefined)}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={busy || !name.trim()}>{busy ? "Creating…" : "Create"}</button>
      </div>
    </form>
  </FileDialog>;

  return { folders: folders.data, refreshTree, actions, dialog };
}
