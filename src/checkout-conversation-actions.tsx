import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Conversation } from "@/bindings/Conversation";
import type { MenuGroups } from "@/components/ui/context-actions";
import { conversationLibraryKey } from "@/conversation-library";
import { FileDialog, type FileTreeTarget } from "@/ide-file-workspace";
import { fileName } from "@/ide-knowledge-data";

const newConversation = "new";

export function useCheckoutConversationActions(checkoutId: string, simulated: boolean, openConversation: (id: string) => void) {
  const client = useQueryClient();
  const [target, setTarget] = useState<FileTreeTarget>();
  const [destination, setDestination] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const conversationKey = ["conversations", checkoutId];
  const conversations = useQuery({
    queryKey: conversationKey,
    queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
    enabled: !simulated && !!target,
    retry: false,
  });
  const files = useQuery({
    queryKey: ["checkout-context-file-list", checkoutId],
    queryFn: () => invoke<string[]>("list_repository_source_files", { checkoutId }),
    enabled: !simulated && target?.kind === "folder",
    retry: false,
  });
  const matches = target?.kind === "folder"
    ? (files.data ?? []).filter((path) => path.startsWith(`${target.path}/`) && path.toLocaleLowerCase().includes(filter.toLocaleLowerCase()))
    : [];

  function start(item: FileTreeTarget, create: boolean) {
    setSelected(item.kind === "file" ? [item.path] : []);
    setDestination(create ? newConversation : "");
    setFilter("");
    setError("");
    setTarget(item);
  }

  function actions(item: FileTreeTarget): MenuGroups {
    if (simulated || item.kind === "root") return [];
    return [[
      { label: item.kind === "file" ? "Add to conversation…" : "Add files to conversation…", run: () => start(item, false) },
      { label: "New conversation with context…", run: () => start(item, true) },
    ]];
  }

  async function attach() {
    if (busy || !target || !selected.length || !destination) return;
    setBusy(true);
    setError("");
    try {
      let conversationId = destination;
      if (destination === newConversation) {
        const created = await invoke<Conversation>("create_conversation", { checkoutId, title: fileName(target.path) });
        conversationId = created.id;
        setDestination(created.id);
        client.setQueryData<Conversation[]>(conversationKey, (current) => [created, ...(current ?? [])]);
        void client.invalidateQueries({ queryKey: conversationLibraryKey });
      }
      await invoke("add_checkout_files_to_conversation", { checkoutId, conversationId, paths: selected });
      void client.invalidateQueries({ queryKey: ["conversation", conversationId, "attachments"] });
      setTarget(undefined);
      openConversation(conversationId);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  }

  const dialog = target && <FileDialog title="Add checkout context" description={target.path} onClose={() => { if (!busy) setTarget(undefined); }}>
    <form className="grid gap-2" onSubmit={(event) => { event.preventDefault(); void attach(); }}>
      <label>Conversation <select className="delivery-filter" value={destination} disabled={busy}
        onChange={(event) => setDestination(event.currentTarget.value)}>
        <option value="">Choose a conversation</option>
        <option value={newConversation}>New conversation</option>
        {conversations.data?.map(({ id, title }) => <option key={id} value={id}>{title}</option>)}
      </select></label>
      {conversations.isFetching && <p role="status">Loading conversations…</p>}
      {conversations.error && <p role="alert">{String(conversations.error)}</p>}
      {target.kind === "folder" && <>
        <label className="file-field"><input type="search" aria-label="Filter files" value={filter}
          onChange={(event) => setFilter(event.currentTarget.value)} /></label>
        {files.isPending && <p role="status">Loading files…</p>}
        {files.error && <p role="alert">{String(files.error)}</p>}
        {files.isSuccess && !matches.length && <p>No matching files in this folder.</p>}
        <ul className="max-h-56 overflow-auto">
          {matches.slice(0, 100).map((path) => <li key={path}>
            <label className="flex gap-2 wrap-anywhere"><input type="checkbox" checked={selected.includes(path)} disabled={busy}
              onChange={({ currentTarget }) => {
                const checked = currentTarget.checked;
                setSelected((before) => checked ? [...before, path] : before.filter((entry) => entry !== path));
              }} />{path}</label>
          </li>)}
        </ul>
        {matches.length > 100 && <p>Showing 100 of {matches.length} files. Filter to narrow the list.</p>}
        <p role="status">{selected.length} selected</p>
      </>}
      {error && <p className="file-help" role="alert">{error}</p>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={() => setTarget(undefined)}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={busy || !destination || !selected.length}>
          {busy ? "Adding…" : "Add to draft"}
        </button>
      </div>
    </form>
  </FileDialog>;
  return { actions, dialog };
}
