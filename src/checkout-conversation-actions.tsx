import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Conversation } from "@/bindings/Conversation";
import type { ConversationPipelineOption } from "@/bindings/ConversationPipelineOption";
import type { MenuGroups } from "@/components/ui/context-actions";
import { queueConversationDraftWrite } from "@/conversation-draft-writes";
import { conversationLibraryKey } from "@/conversation-library";
import { ConversationPipelineOptions, conversationPipelineKey, directConversationChoice } from "@/conversation-pipeline-picker";
import { excerptReference, markdownExcerpt, type EditorExcerpt } from "@/editor-context";
import { FileDialog, type FileTreeTarget } from "@/ide-file-workspace";
import { fileName } from "@/ide-knowledge-data";

const newConversation = "new";
type ContextText = { title: string; reference: string; text: string; language?: string; confirmSource?: () => Promise<void> };
type RecordedFile = { title: string; reference: string; sourceConversationId: string; runId: string; ordinal: number;
  messageRevisionId: string; sourceSha256: string };

const fileRequests = [
  ["Explain selected files…", "Read the selected checkout files and explain their purpose and how they work."],
  ["Find usages…", "Find code and workflows that use the selected checkout files or their exported symbols. Cite the relevant paths and lines."],
  ["Review selected files…", "Review the selected checkout files for defects and regressions. Cite the relevant paths and lines."],
  ["Suggest a fix…", "Read the selected checkout files, identify issues with concrete evidence, and suggest a fix."],
  ["Generate tests…", "Read the selected checkout files and propose tests that cover their behavior and edge cases."],
  ["Document selected files…", "Read the selected checkout files and draft documentation for their public behavior and usage."],
  ["Propose a refactor…", "Read the selected checkout files and propose a behavior-preserving refactor, including how to verify it."],
] as const;

export function useCheckoutConversationActions(checkoutId: string, simulated: boolean, openConversation: (id: string) => void) {
  const client = useQueryClient();
  const [targets, setTargets] = useState<readonly FileTreeTarget[]>();
  const [excerpt, setExcerpt] = useState<{ selection: ContextText; instruction?: string }>();
  const [recordedFile, setRecordedFile] = useState<RecordedFile>();
  const [fileRequest, setFileRequest] = useState<string>();
  const [destination, setDestination] = useState("");
  const [pipelineKey, setPipelineKey] = useState<string>();
  const [selected, setSelected] = useState<string[]>([]);
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const canAttach = Boolean(excerpt || recordedFile || (targets && selected.length));
  const conversationKey = ["conversations", checkoutId];
  const conversations = useQuery({
    queryKey: conversationKey,
    queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
    enabled: !simulated && Boolean(targets || excerpt || recordedFile),
    retry: false,
  });
  const pipelines = useQuery({
    queryKey: ["conversation-pipeline-options", checkoutId],
    queryFn: () => invoke<ConversationPipelineOption[]>("conversation_pipeline_options", { checkoutId }),
    enabled: !simulated && Boolean(excerpt) && destination === newConversation,
    retry: false,
  });
  const options = excerpt ? pipelines.data ?? [directConversationChoice] : [directConversationChoice];
  const pipeline = options.find((entry) => conversationPipelineKey(entry.pipeline) === pipelineKey)?.pipeline;
  const destinations = conversations.data?.filter(({ pipeline }) => excerpt || !pipeline || pipeline.kind === "direct");
  const folders = (targets ?? []).filter(({ kind }) => kind === "folder").map(({ path }) => path);
  const files = useQuery({
    queryKey: ["checkout-context-file-list", checkoutId],
    queryFn: () => invoke<string[]>("list_repository_source_files", { checkoutId }),
    enabled: !simulated && folders.length > 0,
    retry: false,
  });
  const matches = folders.length > 0
    ? (files.data ?? []).filter((path) => folders.some((folder) => path.startsWith(`${folder}/`)) && path.toLocaleLowerCase().includes(filter.toLocaleLowerCase()))
    : [];

  function start(items: readonly FileTreeTarget[], create: boolean, instruction?: string) {
    setSelected(items.filter(({ kind }) => kind === "file").map(({ path }) => path));
    setDestination(create ? newConversation : "");
    setPipelineKey(undefined);
    setFilter("");
    setError("");
    setExcerpt(undefined);
    setRecordedFile(undefined);
    setFileRequest(instruction);
    setTargets(items);
  }

  function startContextText(selection: ContextText, instruction?: string) {
    if (simulated || (!selection.text.trim() && !(selection.text && instruction?.trim()))) return;
    setTargets(undefined);
    setRecordedFile(undefined);
    setFileRequest(undefined);
    setExcerpt({ selection, instruction });
    setDestination(instruction ? newConversation : "");
    setPipelineKey(undefined);
    setError("");
  }

  function startExcerpt(selection: EditorExcerpt, instruction?: string) {
    startContextText({ title: fileName(selection.path), reference: excerptReference(selection), text: selection.text, language: selection.language }, instruction);
  }

  function startRecordedFile(file: RecordedFile) {
    if (simulated) return;
    setTargets(undefined);
    setExcerpt(undefined);
    setFileRequest(undefined);
    setRecordedFile(file);
    setDestination("");
    setError("");
  }

  function close() {
    setTargets(undefined);
    setExcerpt(undefined);
    setRecordedFile(undefined);
    setFileRequest(undefined);
  }

  function actions(item: FileTreeTarget, selection?: readonly FileTreeTarget[]): MenuGroups {
    if (simulated || item.kind === "root") return [];
    const items = selection ?? [item];
    return [[
      { label: !selection && item.kind === "file" ? "Add to conversation…" : "Add files to conversation…", run: () => start(items, false) },
      { label: "New conversation with context…", run: () => start(items, true) },
      { label: "Ask agent", items: fileRequests.map(([label, instruction]) => ({ label, run: () => start(items, true, instruction) })) },
    ]];
  }

  async function attach() {
    if (busy || !canAttach || !destination || (destination === newConversation && !pipeline)) return;
    setBusy(true);
    setError("");
    try {
      await excerpt?.selection.confirmSource?.();
      let conversationId = destination;
      if (destination === newConversation) {
        const title = excerpt?.selection.title ?? recordedFile?.title ?? (targets?.length === 1 ? fileName(targets[0]!.path) : "Selected checkout files");
        const created = await invoke<Conversation>("create_conversation", { checkoutId, title, pipeline });
        conversationId = created.id;
        setDestination(created.id);
        client.setQueryData<Conversation[]>(conversationKey, (current) => [created, ...(current ?? [])]);
        void client.invalidateQueries({ queryKey: conversationLibraryKey });
      }
      if (excerpt || fileRequest) {
        const selection = excerpt?.selection ?? { reference: `Selected files in checkout ${checkoutId}`, text: selected.join("\n") };
        const instruction = excerpt?.instruction ?? fileRequest;
        const context = [instruction, `Source: ${selection.reference}`, markdownExcerpt(selection)].filter(Boolean).join("\n\n");
        await queueConversationDraftWrite(conversationId, () => invoke<void>("append_checkout_context_to_conversation", {
          checkoutId, conversationId, context,
        }));
        await client.invalidateQueries({ queryKey: ["conversation", conversationId, "draft"] });
      } else if (recordedFile) {
        const { title: _title, reference: _reference, ...source } = recordedFile;
        await invoke("reuse_saved_conversation_attachment", { request: { ...source, destinationConversationId: conversationId } });
        await client.invalidateQueries({ queryKey: ["conversation", conversationId, "attachments"] });
      } else {
        await invoke("add_checkout_files_to_conversation", { checkoutId, conversationId, paths: selected });
        void client.invalidateQueries({ queryKey: ["conversation", conversationId, "attachments"] });
      }
      close();
      openConversation(conversationId);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  }

  const description = excerpt ? excerpt.selection.reference : recordedFile ? recordedFile.reference
    : targets?.length === 1 ? targets[0]!.path : `${targets?.length ?? 0} selected items`;
  const dialog = (targets || excerpt || recordedFile) && <FileDialog title={recordedFile ? "Reuse saved attachment" : "Add checkout context"} description={description}
    onClose={() => { if (!busy) close(); }}>
    <form className="grid gap-2" onSubmit={(event) => { event.preventDefault(); void attach(); }}>
      <label>Conversation <select className="delivery-filter" value={destination} disabled={busy}
        onChange={(event) => setDestination(event.currentTarget.value)}>
        <option value="">Choose a conversation</option>
        <option value={newConversation}>New conversation</option>
        {destinations?.map(({ id, title }) => <option key={id} value={id}>{title}</option>)}
      </select></label>
      {destination === newConversation && <>
        <ConversationPipelineOptions options={options} selected={pipelineKey} onSelect={setPipelineKey} disabled={busy} />
        {excerpt && pipelines.isPending && <p role="status">Loading published pipelines…</p>}
        {excerpt && pipelines.error && <p role="alert">{String(pipelines.error)}</p>}
      </>}
      {targets && <p className="file-help">File attachments use a direct conversation.</p>}
      {conversations.isFetching && <p role="status">Loading conversations…</p>}
      {conversations.error && <p role="alert">{String(conversations.error)}</p>}
      {fileRequest && <p>{fileRequest}</p>}
      {folders.length > 0 && <>
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
      {excerpt && <>
        {excerpt.instruction && <p>{excerpt.instruction}</p>}
        <pre className="max-h-48 overflow-auto" aria-label="Selected text">{excerpt.selection.text}</pre>
      </>}
      {recordedFile && <p>Review delivery options in the destination draft before sending.</p>}
      {targets && targets.length > 1 && selected.length > 0 && <details>
        <summary>Selected files</summary>
        <ul className="max-h-36 overflow-auto">{selected.map((path) => <li key={path}>{path}</li>)}</ul>
      </details>}
      {error && <p className="file-help" role="alert">{error}</p>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={close}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={busy || !destination || !canAttach || (destination === newConversation && !pipeline)}>
          {busy ? "Adding…" : "Add to draft"}
        </button>
      </div>
    </form>
  </FileDialog>;
  return { actions, dialog, startExcerpt, startContextText, startRecordedFile };
}
