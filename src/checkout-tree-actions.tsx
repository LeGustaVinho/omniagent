import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { FileDialog, type FileTreeTarget } from "@/ide-file-workspace";
import type { MenuGroups } from "@/components/ui/context-actions";
import { ignoredTreeKey } from "@/checkout-file-tree";
import { containingFolder } from "@/file-context-actions";
import { fileName } from "@/ide-knowledge-data";

type Kind = "file" | "note" | "folder";
const labels = { file: "New file", note: "New note", folder: "New folder" };
const noteTemplate = { kind: "note", label: "Markdown note", name: "note.md", contents: "# Title\n\n" } as const;
const taskTemplate = { kind: "note", label: "Markdown task notes", name: "task-notes.md",
  contents: "# Task notes\n\n## Tasks\n\n- [ ] \n\n## Notes\n" } as const;
const jsonTemplate = { kind: "file", label: "JSON object", name: "data.json", contents: "{\n  \n}\n" } as const;

export function useCheckoutTreeActions({ checkoutId, simulated, knowledge, refresh, onCreated, report }: {
  checkoutId: string;
  simulated: boolean;
  knowledge?: boolean;
  refresh: () => Promise<unknown>;
  onCreated: (path: string, kind: Kind) => void;
  report: (message: string) => void;
}) {
  const client = useQueryClient();
  const folders = useQuery({
    queryKey: ["checkout-folders", checkoutId],
    queryFn: () => invoke<string[]>("list_checkout_folders", { checkoutId }),
    enabled: !simulated,
    retry: false,
  });
  const [request, setRequest] = useState<{ path: string; kind: Kind; contents?: string; copy?: boolean }>();
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const refreshTree = () => Promise.all([refresh(), folders.refetch({ throwOnError: true }),
    client.invalidateQueries({ queryKey: ignoredTreeKey(checkoutId) })]);

  function start(next: NonNullable<typeof request>, suggested = "") {
    setName(suggested);
    setError("");
    setRequest(next);
  }

  function saveCopy(path: string, contents: string) {
    if (simulated) return;
    const original = fileName(path);
    const extension = original.lastIndexOf(".");
    const suggested = extension > 0 ? `${original.slice(0, extension)} copy${original.slice(extension)}` : `${original} copy`;
    const kind = /\.(?:md|markdown)$/i.test(path) ? "note" : "file";
    start({ path: containingFolder(path), kind, contents, copy: true }, suggested);
  }

  function actions(item: FileTreeTarget): MenuGroups {
    if (simulated || item.kind === "file") return [];
    const kinds: Kind[] = knowledge ? ["note", "folder"] : ["file", "note", "folder"];
    const templates = [noteTemplate, taskTemplate, ...(knowledge ? [] : [jsonTemplate])];
    return [[...kinds.map((kind) => ({ label: `${labels[kind]}…`, run: () => start({ path: item.path, kind }) })), {
      label: "New from template", items: templates.map(({ kind, label, name, contents }) => ({ label, run: () => start({ path: item.path, kind, contents }, name) })),
    }], item.kind === "root" ? [{ label: "Refresh tree", run: () => {
      report("");
      void refreshTree().catch((failure: unknown) => report(String(failure)));
    } }] : []];
  }

  async function create() {
    if (!request || busy || !name.trim()) return;
    const { kind, path, contents } = request;
    const filename = kind === "note" && !/\.(?:md|markdown)$/i.test(name) ? `${name}.md` : name;
    setBusy(true);
    setError("");
    try {
      const created = await invoke<string>("create_checkout_item", {
        checkoutId, relativeDirectory: path, name: filename, directory: kind === "folder", ...(contents === undefined ? {} : { contents }),
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

  const dialog = request && <FileDialog title={request.copy ? "Save a copy" : labels[request.kind]} description={request.path || "Checkout root"}
    onClose={() => { if (!busy) setRequest(undefined); }}>
    <form onSubmit={(event) => { event.preventDefault(); void create(); }}>
      {request.copy && <label className="grid gap-1 py-2 text-xs">Destination folder<select className="delivery-filter" disabled={busy}
        value={request.path} onChange={(event) => setRequest({ ...request, path: event.currentTarget.value })}>
        <option value="">Checkout root</option>
        {[...new Set([...(folders.data ?? []), ...(request.path ? [request.path] : [])])].map((path) => <option key={path} value={path}>{path}</option>)}
      </select></label>}
      <label className="file-field"><input autoFocus aria-label="Name" value={name} disabled={busy}
        onChange={(event) => setName(event.currentTarget.value)} /></label>
      {error && <p role="alert" className="file-help">{error}</p>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={() => setRequest(undefined)}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={busy || !name.trim()}>
          {request.copy ? busy ? "Saving…" : "Save copy" : busy ? "Creating…" : "Create"}
        </button>
      </div>
    </form>
  </FileDialog>;

  return { folders: folders.data, refreshTree, actions, saveCopy, dialog };
}
