import { invoke } from "@tauri-apps/api/core";
import { useEffect, useEffectEvent, useRef, useState } from "react";
import { create } from "zustand";
import type { MenuAction, MenuGroups } from "@/components/ui/context-actions";
import { containingFolder } from "@/file-context-actions";
import { fileName } from "@/ide-knowledge-data";
import { FileDialog, type FileTreeTarget } from "@/ide-file-workspace";
import { reconcileFavoriteItems } from "@/checkout-favorites";

type Operation = "copy" | "move" | "recycle";
type Item = { kind: "file" | "folder"; path: string };
type Clipboard = { checkoutId: string; operation: "copy" | "move"; items: readonly Item[] };
type Request = { kind: "duplicate" | "rename" | "copy" | "move" | "recycle" | "paste"; items: readonly Item[]; paste?: Clipboard };
export type ItemEdit = { source: string; directory: boolean; destination: string | null };
export type ItemResult = { completed: ItemEdit[]; error: string | null };
type ItemChange = { origin: symbol; checkoutId: string; operation: Operation; completed: ItemEdit[] };

const useFileClipboard = create<{ value?: Clipboard }>(() => ({}));
const itemsChangedEvent = "omniagent:checkout-items-changed";

export function publishCheckoutItemChanges(checkoutId: string, operation: Operation, completed: ItemEdit[], origin = Symbol()) {
  if (operation !== "copy") reconcileFavoriteItems(checkoutId, (path) => changedItemPath(path, completed));
  window.dispatchEvent(new CustomEvent<ItemChange>(itemsChangedEvent, { detail: { origin, checkoutId, operation, completed } }));
}

export function itemIncludesPath(item: { path: string; kind: "file" | "folder" }, path: string) {
  const original = item.path.toLowerCase();
  const relative = path.toLowerCase();
  return relative === original || item.kind === "folder" && relative.startsWith(`${original}/`);
}

export function changedItemPath(path: string, edits: readonly ItemEdit[]): string | null | undefined {
  for (const { source, destination, directory } of edits) {
    if (itemIncludesPath({ path: source, kind: directory ? "folder" : "file" }, path)) {
      return destination === null ? null : destination + path.slice(source.length);
    }
  }
}

const labels = { rename: "Rename", duplicate: "Duplicate", copy: "Copy to", move: "Move to", recycle: "Move to Recycle Bin", paste: "Paste" };
const checkoutPath = (folder: string, name: string) => folder ? `${folder}/${name}` : name;

function topLevel(items: readonly Item[]) {
  const folders = items.filter((item) => item.kind === "folder").map(({ path }) => path.toLowerCase());
  return items.filter(({ path }) => !folders.some((folder) => path.toLowerCase().startsWith(`${folder}/`)));
}

function itemOf(target: FileTreeTarget): Item | undefined {
  return target.kind === "root" ? undefined : { kind: target.kind, path: target.path };
}

function copyName(item: Item) {
  const name = fileName(item.path);
  const extension = item.kind === "file" ? name.lastIndexOf(".") : -1;
  return extension > 0 ? `${name.slice(0, extension)} copy${name.slice(extension)}` : `${name} copy`;
}

export function useCheckoutItemOperations({ checkoutId, folders, refreshTree, report, status, isDirty, onChanged, onReveal, onRenameNote }: {
  checkoutId: string;
  folders?: readonly string[];
  refreshTree: () => Promise<unknown>;
  report: (message: string) => void;
  status: (message: string) => void;
  isDirty: (items: readonly Item[]) => boolean;
  onChanged: (edits: readonly ItemEdit[]) => void;
  onReveal: (path: string) => void;
  onRenameNote: (path: string) => void;
}) {
  const clipboard = useFileClipboard(({ value }) => value?.checkoutId === checkoutId ? value : undefined);
  const [request, setRequest] = useState<Request>();
  const [folder, setFolder] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const origin = useRef(Symbol());

  const receive = useEffectEvent((change: ItemChange) => {
    if (change.checkoutId !== checkoutId || change.origin === origin.current) return;
    if (change.operation !== "copy") onChanged(change.completed);
    void refreshTree().catch((failure: unknown) => report(String(failure)));
  });
  useEffect(() => {
    const handle = (event: Event) => receive((event as CustomEvent<ItemChange>).detail);
    window.addEventListener(itemsChangedEvent, handle);
    return () => window.removeEventListener(itemsChangedEvent, handle);
  }, []);

  function start(kind: Request["kind"], items: readonly Item[], targetFolder = "", paste?: Clipboard) {
    setError("");
    setFolder(targetFolder);
    setName(kind === "duplicate" ? copyName(items[0]!) : fileName(items[0]!.path));
    setRequest({ kind, items, paste });
  }

  function putOnClipboard(operation: Clipboard["operation"], items: readonly Item[]) {
    useFileClipboard.setState({ value: { checkoutId, operation, items } });
    status(`${operation === "copy" ? "Copied" : "Cut"} ${items.length} ${items.length === 1 ? "item" : "items"}`);
  }

  async function reflectChanges(operation: Operation, completed: ItemEdit[], verb: string) {
    if (!completed.length) return;
    if (operation !== "copy") onChanged(completed);
    publishCheckoutItemChanges(checkoutId, operation, completed, origin.current);
    const count = completed.length;
    status(`${verb} ${count} ${count === 1 ? "item" : "items"}`);
    await refreshTree().catch((failure: unknown) => report(String(failure)));
    const destination = completed.at(-1)?.destination;
    if (destination) onReveal(destination);
  }

  async function importItems(relativeDirectory: string, folder: boolean) {
    if (busy) return;
    setBusy(true);
    report("");
    try {
      const result = await invoke<ItemResult>("import_checkout_items", { checkoutId, relativeDirectory, folder });
      await reflectChanges("copy", result.completed, "Imported");
      if (result.error) report(result.error);
    } catch (failure) {
      report(String(failure));
    } finally {
      setBusy(false);
    }
  }

  function actions(target: FileTreeTarget, selected?: readonly FileTreeTarget[]): MenuGroups {
    const item = itemOf(target);
    const items = topLevel((selected?.length ? selected : [target]).flatMap((row) => itemOf(row) ?? []));
    const single = (selected?.length ?? 1) === 1 ? items[0] : undefined;
    const note = single?.kind === "file" && /\.(?:md|markdown)$/i.test(single.path);
    const clean = items.length > 0 && !isDirty(items) && !busy;
    const commands: MenuAction[] = items.length ? [
      ...(single && !note ? [{ label: "Rename…", disabled: !clean, run: () => start("rename", items) }] : []),
      ...(single ? [{ label: "Duplicate…", disabled: busy, run: () => start("duplicate", items) }] : []),
      { label: "Move to folder…", disabled: !clean, run: () => {
        if (note && single) onRenameNote(single.path);
        else start("move", items);
      } },
      { label: "Copy to folder…", disabled: busy, run: () => start("copy", items) },
      { label: "Cut", disabled: !clean, run: () => putOnClipboard("move", items) },
      { label: "Copy items", disabled: busy, run: () => putOnClipboard("copy", items) },
    ] : [];
    if (item?.kind !== "file" && !selected) {
      if (clipboard) commands.push({ label: `Paste ${clipboard.items.length} ${clipboard.items.length === 1 ? "item" : "items"}…`, disabled: busy,
        run: () => start("paste", clipboard.items, target.path, clipboard) });
      commands.push({ label: "Import files…", disabled: busy, run: () => { void importItems(target.path, false); } },
        { label: "Import folder…", disabled: busy, run: () => { void importItems(target.path, true); } });
    }
    return [commands, items.length ? [{ label: "Move to Recycle Bin…", destructive: true, disabled: !clean, run: () => start("recycle", items) }] : []];
  }

  async function submit() {
    if (!request || busy) return;
    const { kind, items, paste } = request;
    const operation: Operation = kind === "paste" ? paste!.operation : kind === "duplicate" ? "copy" : kind === "rename" ? "move" : kind;
    if (operation !== "copy" && isDirty(items)) {
      setError("Save or resolve changes in the selected items before moving them.");
      return;
    }
    const nameOnly = kind === "duplicate" || kind === "rename";
    const edits = items.map((item): ItemEdit => ({ source: item.path, directory: item.kind === "folder",
      destination: operation === "recycle" ? null : nameOnly ? checkoutPath(containingFolder(item.path), name)
        : checkoutPath(folder, fileName(item.path)),
    }));
    setError("");
    setBusy(true);
    try {
      const result = await invoke<ItemResult>("mutate_checkout_items", { checkoutId, operation, edits });
      if (result.completed.length) {
        if (paste?.operation === "move" && paste === useFileClipboard.getState().value) {
          const completed = new Set(result.completed.map(({ source }) => source));
          const remaining = paste.items.filter(({ path }) => !completed.has(path));
          useFileClipboard.setState({ value: remaining.length ? { ...paste, items: remaining } : undefined });
        }
        await reflectChanges(operation, result.completed, operation === "copy" ? "Copied" : operation === "move" ? "Moved" : "Recycled");
      }
      if (result.error && result.completed.length === 0) setError(result.error);
      else {
        if (result.error) report(result.error);
        setRequest(undefined);
      }
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  }

  const nameOnly = request?.kind === "rename" || request?.kind === "duplicate";
  const chooseFolder = request?.kind === "move" || request?.kind === "copy" || request?.kind === "paste";
  const destructive = request?.kind === "recycle";
  const dialog = request && <FileDialog title={labels[request.kind]} description={request.items.length === 1 ? request.items[0]!.path : `${request.items.length} selected items`}
    onClose={() => { if (!busy) setRequest(undefined); }}>
    <form className="grid gap-2" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
      {nameOnly && <label className="file-field"><input autoFocus aria-label="New name" value={name} disabled={busy}
        onChange={(event) => setName(event.currentTarget.value)} /></label>}
      {chooseFolder && <label>Destination folder <select className="delivery-filter" value={folder} disabled={busy}
        onChange={(event) => setFolder(event.currentTarget.value)}>
        <option value="">Checkout root</option>
        {[...new Set([...(folders ?? []), ...(folder ? [folder] : [])])].sort().map((path) => <option key={path} value={path}>{path}</option>)}
      </select></label>}
      {destructive && <p>You can restore these items from the Windows Recycle Bin.</p>}
      {request.items.length > 1 && <ul className="note-rewrite-list" aria-label="Selected items">
        {request.items.map(({ path }) => <li key={path}>{path}</li>)}
      </ul>}
      {error && <p role="alert">{error}</p>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={() => setRequest(undefined)}>Cancel</button>
        <button type="submit" className={`fw-btn ${destructive ? "fw-destructive" : "fw-primary"}`} disabled={busy || nameOnly && !name.trim()}>
          {busy ? "Working…" : labels[request.kind]}
        </button>
      </div>
    </form>
  </FileDialog>;
  return { actions, dialog };
}
