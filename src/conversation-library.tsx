import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Archive, ArrowUp, MessageSquare, Search } from "lucide-react";
import { Dialog, Popover } from "radix-ui";
import { useCallback, useContext, useRef, useState } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { ConversationHistory } from "@/bindings/ConversationHistory";
import type { JobInspection } from "@/bindings/JobInspection";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { RunRecord } from "@/bindings/RunRecord";
import { Button } from "@/components/ui/button";
import type { MenuGroups } from "@/components/ui/context-actions";
import { ConversationLibraryGroup, ConversationLibraryLayout } from "@/conversation-layout";
import { LibraryBatchMoveDialog, type LibraryMoveItem, type LibraryMoveTarget } from "@/conversation-library-batch";
import { ConversationRow, type ConversationAction } from "@/conversation-menu";
import { useConversationTranscriptExport } from "@/conversation-transcript-export";
import { ConversationHistoryControls } from "@/conversation-history-controls";
import { ConversationLibraryNavigation, type OpenLibraryConversation } from "@/conversation-library-navigation";
import { ConversationReferenceSelection, type ConversationReferencePreview } from "@/conversation-reference";
import { isLiveRun } from "@/conversation-run";
import { RunInspectorNavigation } from "@/run-inspector-navigation";

type ConversationLibraryEntry = {
  conversation: Conversation;
  checkout: ProjectCheckout;
  projectName: string;
  agent: string | null;
  model: string | null;
  status: string | null;
  lastActivityAt: number | null;
  organization: { pinned: boolean; unread: boolean; archived: boolean; section: ConversationLibrarySection | null };
};

type ConversationLibrarySection = { id: string; projectId: string; name: string };
type SectionChange = { type: "rename"; expectedName: string; name: string }
  | { type: "move"; neighborId: string; before: boolean };
type BatchSelection = { projectId: string; items: LibraryMoveItem[] };
type OrganizationChange =
  | { type: "pin"; pinned: boolean }
  | { type: "read"; unread: boolean }
  | { type: "archive"; archived: boolean }
  | { type: "move"; sectionId: string | null }
  | { type: "newSection"; name: string };

type ConversationIdentity = Pick<Conversation, "id" | "checkoutId" | "title">;
export { ConversationLibraryNavigation, type OpenLibraryConversation } from "@/conversation-library-navigation";
export const conversationLibraryKey = ["conversation-library"];
const sectionKey = ["conversation-library-sections"];

const titleKey = (checkoutId: string, conversationId: string) => ["checkout-conversation", checkoutId, conversationId];

export function useConversationTitle(checkoutId: string | undefined, conversationId: string) {
  return useQuery({
    queryKey: titleKey(checkoutId ?? "", conversationId),
    queryFn: () => invoke<Conversation>("checkout_conversation", { checkoutId, conversationId }),
    enabled: Boolean(checkoutId),
  });
}

export function useConversationRename(conversation: ConversationIdentity | undefined, onRenamed?: (conversation: Conversation) => void) {
  const client = useQueryClient();
  return useMutation({
    scope: conversation && { id: `rename-conversation:${conversation.id}` },
    mutationFn: (title: string) => {
      if (!conversation) throw new Error("Choose a conversation to rename.");
      return invoke<Conversation>("rename_conversation", {
        checkoutId: conversation.checkoutId, conversationId: conversation.id, title,
      });
    },
    onSuccess: async (updated) => {
      const details = titleKey(updated.checkoutId, updated.id);
      const list = ["conversations", updated.checkoutId];
      await client.cancelQueries({ queryKey: details });
      client.setQueryData(details, updated);
      client.setQueryData<Conversation[]>(list, (previous) => previous?.map((item) => item.id === updated.id ? updated : item));
      void client.invalidateQueries({ queryKey: list });
      void client.invalidateQueries({ queryKey: conversationLibraryKey });
      onRenamed?.(updated);
    },
  });
}

function useConversationLibrary(query: string, projectId: string | null, enabled = true, archived = false) {
  const title = query.trim();
  return useQuery({
    queryKey: [...conversationLibraryKey, projectId, title, archived],
    queryFn: () => invoke<ConversationLibraryEntry[]>("conversation_library", { query: title || null, projectId, ...(archived ? { archived } : {}) }),
    enabled,
    refetchInterval: 5_000,
  });
}

function organizeConversation({ checkoutId, id }: Pick<Conversation, "checkoutId" | "id">, change: OrganizationChange) {
  return invoke<boolean>("organize_conversation", { checkoutId, conversationId: id, change });
}

export function useConversationLibraryRead() {
  const client = useQueryClient();
  return useCallback((checkoutId: string, id: string) => {
    void organizeConversation({ checkoutId, id }, { type: "read", unread: false })
      .then((changed) => { if (changed) return client.invalidateQueries({ queryKey: conversationLibraryKey }); })
      .catch(() => { /* Opening a conversation remains available if updating its read status fails. */ });
  }, [client]);
}

const activityTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const agentNames: Record<string, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };

function ConversationMetadata({ entry, id }: { entry: ConversationLibraryEntry; id: string }) {
  const { projectName, agent, model, status, lastActivityAt } = entry;
  const activity = lastActivityAt === null ? null : new Date(lastActivityAt);
  const details = [projectName, agent && (agentNames[agent] ?? agent), model, status?.replace(/_/g, " ")].filter(Boolean);
  return <span id={id} className="block min-w-0 text-xs text-muted-foreground">
    <small className="block truncate" title={details.join(" · ")}>{details.join(" · ")}</small>
    {activity && <time className="block truncate" dateTime={activity.toISOString()}>{activityTime.format(activity)}</time>}
  </span>;
}

export function ConversationTitleEditor({ conversation, onClose }: { conversation: ConversationIdentity; onClose: () => void }) {
  const [title, setTitle] = useState(conversation.title);
  const rename = useConversationRename(conversation, onClose);
  const value = title.trim();
  return <form aria-label="Rename conversation" className="grid min-w-0 gap-1 p-1" onSubmit={(event) => {
    event.preventDefault();
    if (value && value !== conversation.title && !rename.isPending) rename.mutate(value);
  }} onKeyDown={(event) => {
    if (event.key === "Escape" && !rename.isPending) { event.stopPropagation(); onClose(); }
  }}>
    <label className="conv-search">
      <span className="sr-only">Conversation title</span>
      <input value={title} onChange={(event) => setTitle(event.currentTarget.value)} autoFocus required disabled={rename.isPending} />
    </label>
    {rename.error && <p className="conv-alert" role="alert">{String(rename.error)}</p>}
    <div className="flex items-center justify-end gap-1">
      <button type="button" className="conv-quiet-button" onClick={onClose} disabled={rename.isPending}>Cancel</button>
      <button type="submit" className="conv-primary-button" disabled={!value || value === conversation.title || rename.isPending}>
        {rename.isPending ? "Saving…" : "Save"}
      </button>
    </div>
  </form>;
}

function openEntry(onOpen: OpenLibraryConversation, { checkout, conversation }: ConversationLibraryEntry) {
  return onOpen(checkout, conversation.id);
}

function LibraryFork({ entry, onClose, onOpen }: {
  entry: ConversationLibraryEntry; onClose: () => void; onOpen: OpenLibraryConversation;
}) {
  const { conversation, checkout } = entry;
  const conversationId = conversation.id;
  const history = useQuery({ queryKey: ["conversation", conversationId, "messages"], refetchOnMount: "always",
    queryFn: () => invoke<ConversationHistory>("conversation_history", { conversationId }) });
  const run = useQuery({ queryKey: ["conversation", conversationId, "latest-run"], refetchOnMount: "always",
    queryFn: () => invoke<RunRecord | null>("latest_conversation_run", { conversationId }),
    refetchInterval: (query) => isLiveRun(query.state.data) ? 1000 : false });
  const job = useQuery({ queryKey: ["conversation", conversationId, "latest-job"], refetchOnMount: "always",
    queryFn: () => invoke<JobInspection | null>("latest_conversation_job", { conversationId }) });
  if (!history.data) return <Dialog.Root open onOpenChange={(open) => { if (!open) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog">
        <Dialog.Title>Load conversation history</Dialog.Title>
        <Dialog.Description>{conversation.title}</Dialog.Description>
        {history.error ? <p role="alert">{String(history.error)}</p> : <p role="status">Loading messages…</p>}
        <Dialog.Close className="conv-quiet-button">Close</Dialog.Close>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
  return <>
    {(run.error || job.error) && <p role="alert" className="conv-alert">{String(run.error || job.error)}</p>}
    <ConversationLibraryNavigation.Provider value={{ checkoutId: checkout.checkoutId, projectId: checkout.projectId, open: onOpen }}>
      <ConversationHistoryControls conversationId={conversationId} title={conversation.title} history={history.data}
        disabled={!history.isSuccess || history.isFetching || !run.isSuccess || !job.isSuccess || run.isFetching || job.isFetching ||
          isLiveRun(run.data) || isLiveRun(job.data?.job.latestAttempt)} mode="read_only"
        requestedAction={{ kind: "fork", revisionId: "" }} standaloneAction onRequestedActionClose={onClose}
        openFork={(source) => void onOpen(checkout, source.id)} />
    </ConversationLibraryNavigation.Provider>
  </>;
}

function libraryGroups(entries: ConversationLibraryEntry[], sections: ConversationLibrarySection[], projects: Map<string, string>,
  allProjects: boolean, archived: boolean, showEmpty: boolean) {
  type Group = { id: string; title: string; pinned: boolean; project: string; projectId: string;
    section?: ConversationLibrarySection; entries: ConversationLibraryEntry[] };
  const groups = new Map<string, Group>();
  const sectionOrder = new Map(sections.map((section, index) => [section.id, index]));
  const names = new Map(sections.map((section) => [section.id, section]));
  for (const entry of entries) {
    const { pinned, section } = entry.organization;
    const isPinned = pinned && !archived;
    const projectId = entry.checkout.projectId;
    const id = isPinned ? "pinned" : `${projectId}:${section?.id ?? ""}`;
    const currentSection = section && (names.get(section.id) ?? section);
    const title = isPinned ? "Pinned" : [allProjects ? entry.projectName : null, currentSection?.name ?? "Conversations"].filter(Boolean).join(" · ");
    const group = groups.get(id);
    if (group) group.entries.push(entry);
    else groups.set(id, { id, title, pinned: isPinned, project: entry.projectName, projectId, section: isPinned ? undefined : currentSection ?? undefined, entries: [entry] });
  }
  if (showEmpty) for (const section of sections) {
    const id = `${section.projectId}:${section.id}`;
    if (groups.has(id)) continue;
    const project = projects.get(section.projectId) ?? section.projectId;
    groups.set(id, { id, title: allProjects ? `${project} · ${section.name}` : section.name, pinned: false,
      project, projectId: section.projectId, section, entries: [] });
  }
  const rank = ({ section }: Group) => section ? sectionOrder.get(section.id) ?? Number.MAX_SAFE_INTEGER : -1;
  return [...groups.values()].sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.project.localeCompare(b.project)
    || a.projectId.localeCompare(b.projectId) || rank(a) - rank(b) || a.title.localeCompare(b.title));
}

function SectionNameDialog({ title, description, initialName = "", busy, error, restoreFocus, onClose, onSave }: {
  title: string; description: string; initialName?: string; busy: boolean; error: Error | null;
  restoreFocus: (event: Event) => void; onClose: () => void; onSave: (name: string) => void;
}) {
  const [name, setName] = useState(initialName);
  const trimmed = name.trim();
  const canSave = Boolean(trimmed && trimmed !== initialName && !busy);
  return <Dialog.Root open onOpenChange={(shown) => { if (!shown && !busy) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog" onCloseAutoFocus={restoreFocus}>
        <Dialog.Title>{title}</Dialog.Title>
        <Dialog.Description>{description}</Dialog.Description>
        <form className="conv-dialog-form" onSubmit={(event) => {
          event.preventDefault();
          if (canSave) onSave(trimmed);
        }}>
          <label>Section name<input value={name} maxLength={96} required disabled={busy}
            onChange={(event) => setName(event.currentTarget.value)} /></label>
          {error && <p role="alert" className="text-destructive">{String(error)}</p>}
          <footer>
            <Dialog.Close asChild><button type="button" className="conv-quiet-button" disabled={busy}>Cancel</button></Dialog.Close>
            <button type="submit" className="conv-primary-button" disabled={!canSave}>Save <ArrowUp size={14} aria-hidden /></button>
          </footer>
        </form>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}

export function ConversationLibrary({ checkout, activeId, startPending, startError, onStart, onOpen, onOpenBeside, onOpenCheckoutInIde }: {
  checkout: ProjectCheckout;
  activeId?: string;
  startPending: boolean;
  startError?: Error | null;
  onStart: () => void;
  onOpen: OpenLibraryConversation;
  onOpenBeside?: OpenLibraryConversation;
  onOpenCheckoutInIde?: (checkout: ProjectCheckout) => Promise<void>;
}) {
  const client = useQueryClient();
  const inspector = useContext(RunInspectorNavigation);
  const libraryRef = useRef<HTMLElement>(null);
  const [projectId, setProjectId] = useState<string | null>(checkout.projectId);
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  const [renaming, setRenaming] = useState<ConversationIdentity>();
  const [forking, setForking] = useState<ConversationLibraryEntry>();
  const [sectionConversation, setSectionConversation] = useState<ConversationIdentity>();
  const [renamingSection, setRenamingSection] = useState<ConversationLibrarySection>();
  const [batch, setBatch] = useState<BatchSelection>();
  const [showBatchDialog, setShowBatchDialog] = useState(false);
  const [notice, setNotice] = useState("");
  const exportTranscript = useConversationTranscriptExport(setNotice);
  const list = useConversationLibrary(query, projectId, true, archived);
  const sections = useQuery({
    queryKey: [...sectionKey, projectId],
    queryFn: () => invoke<ConversationLibrarySection[]>("conversation_library_sections", { projectId }),
  });
  const open = useMutation({
    mutationFn: ({ entry, beside }: { entry: ConversationLibraryEntry; beside?: boolean }) =>
      openEntry(beside && onOpenBeside ? onOpenBeside : onOpen, entry),
  });
  const openWorktree = useMutation({
    mutationFn: async ({ source, folder }: { source: ProjectCheckout; folder: boolean }) => {
      if (folder) await invoke<void>("open_checkout_item_in_shell", { checkoutId: source.checkoutId, relativePath: "", action: "open" });
      else await onOpenCheckoutInIde?.(source);
    },
  });
  const openFork = useMutation({ mutationFn: ({ source, id }: { source: ProjectCheckout; id: string }) => onOpen(source, id) });
  const inspect = useMutation({
    mutationFn: async ({ conversation, checkout }: ConversationLibraryEntry) => {
      const run = await invoke<RunRecord | null>("latest_conversation_run", { conversationId: conversation.id });
      if (run && run.projectId !== checkout.projectId) throw new Error("The attempt is no longer in this project.");
      if (run) inspector?.openActivity(run.id, conversation.id);
      else setNotice("No attempt is available for this conversation.");
    },
  });
  const organize = useMutation({
    mutationFn: ({ conversation, change }: { conversation: ConversationIdentity; change: OrganizationChange }) =>
      organizeConversation(conversation, change),
    onSuccess: async (_, { change }) => {
      await client.invalidateQueries({ queryKey: conversationLibraryKey });
      if (change.type === "newSection") {
        await client.invalidateQueries({ queryKey: sectionKey });
        setSectionConversation(undefined);
      }
      if (change.type === "archive") setNotice(change.archived ? "Conversation moved to Archived." : "Conversation restored.");
    },
  });
  const organizeSection = useMutation({
    mutationFn: ({ section, change }: { section: ConversationLibrarySection; change: SectionChange }) =>
      invoke<void>("organize_conversation_library_section", { projectId: section.projectId, sectionId: section.id, change }),
    onSuccess: async (_, { change }) => {
      await Promise.all([client.invalidateQueries({ queryKey: sectionKey }), client.invalidateQueries({ queryKey: conversationLibraryKey })]);
      if (change.type === "rename") setRenamingSection(undefined);
      setNotice(change.type === "rename" ? "Section renamed." : "Section order updated.");
    },
    onError: () => { void client.invalidateQueries({ queryKey: sectionKey }); },
  });
  const moveBatch = useMutation({
    mutationFn: (request: { projectId: string; sectionId: string | null; selection: LibraryMoveTarget[] }) =>
      invoke<number>("move_conversations_to_library_section", request),
    onSuccess: async (moved) => {
      await client.invalidateQueries({ queryKey: conversationLibraryKey });
      setShowBatchDialog(false);
      setBatch(undefined);
      setNotice(moved === 1 ? "Moved 1 conversation." : `Moved ${moved} conversations.`);
    },
    onError: () => { void client.invalidateQueries({ queryKey: conversationLibraryKey }); },
  });
  const registered = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
  });
  const projects = new Map([checkout, ...(registered.data ?? [])].map((item) => [
    item.projectId, item.repositoryRoot.split(/[\\/]/).filter(Boolean).at(-1) ?? item.repositoryRoot,
  ]));
  const error = open.error ?? openWorktree.error ?? inspect.error ?? openFork.error ?? exportTranscript.error ?? list.error ?? registered.error ?? sections.error ?? startError ??
    (!sectionConversation ? organize.error : undefined) ?? (!renamingSection ? organizeSection.error : undefined);
  const groups = libraryGroups(list.data ?? [], sections.data ?? [], projects, projectId === null, archived, !query.trim() && !archived);

  function restoreLibraryFocus(event: Event, selector: string, matches: (element: HTMLElement) => boolean) {
    const library = libraryRef.current;
    const target = Array.from(library?.querySelectorAll<HTMLElement>(selector) ?? []).find(matches)
      ?? library?.querySelector<HTMLElement>('input[type="search"]');
    if (target) { event.preventDefault(); target.focus(); }
  }

  function selectForMove(entries: readonly ConversationLibraryEntry[]) {
    const projectId = entries[0]?.checkout.projectId;
    if (!projectId || (batch && batch.projectId !== projectId)) return;
    setNotice("");
    setBatch((current) => {
      if (current && current.projectId !== projectId) return current;
      const items = new Map(current?.items.map((item) => [item.conversationId, item]));
      for (const { conversation, checkout, organization } of entries) {
        if (checkout.projectId === projectId && !items.has(conversation.id)) {
          items.set(conversation.id, { title: conversation.title, checkoutId: conversation.checkoutId, worktree: checkout.checkoutRoot,
            conversationId: conversation.id, expectedSectionId: organization.section?.id ?? null });
        }
      }
      return { projectId, items: [...items.values()] };
    });
  }

  function sectionActions(entries: readonly ConversationLibraryEntry[], section?: ConversationLibrarySection): MenuGroups | undefined {
    if (!section) return;
    const siblings = sections.data?.filter(({ projectId }) => projectId === section.projectId) ?? [];
    const index = siblings.findIndex(({ id }) => id === section.id);
    if (index < 0) return;
    const neighborBefore = siblings[index - 1];
    const neighborAfter = siblings[index + 1];
    const pending = organizeSection.isPending;
    return [[{ label: "Rename section…", disabled: pending, run: () => {
      setNotice("");
      organizeSection.reset();
      setRenamingSection(section);
    } }], [
      { label: "Move section up", disabled: pending || !neighborBefore, run: () => {
        if (neighborBefore) organizeSection.mutate({ section, change: { type: "move", neighborId: neighborBefore.id, before: true } });
      } },
      { label: "Move section down", disabled: pending || !neighborAfter, run: () => {
        if (neighborAfter) organizeSection.mutate({ section, change: { type: "move", neighborId: neighborAfter.id, before: false } });
      } },
    ], [{ label: "Select visible conversations to move…", disabled: !entries.length || Boolean(batch && batch.projectId !== section.projectId),
      run: () => selectForMove(entries) }]];
  }

  function rowAction(action: ConversationAction, entry: ConversationLibraryEntry, sectionId?: string) {
    const { conversation, organization } = entry;
    if (action === "open") open.mutate({ entry });
    if (action === "open-beside" && onOpenBeside && entry.checkout.checkoutId === checkout.checkoutId) open.mutate({ entry, beside: true });
    if (action === "open-worktree" && onOpenCheckoutInIde) openWorktree.mutate({ source: entry.checkout, folder: false });
    if (action === "open-worktree-folder") openWorktree.mutate({ source: entry.checkout, folder: true });
    if (action === "activity") inspect.mutate(entry);
    if (action === "fork") setForking(entry);
    if (action === "export-markdown") exportTranscript.mutate(conversation);
    if (action === "rename") setRenaming(conversation);
    if (action === "select-to-move") selectForMove([entry]);
    if (action === "new-section") {
      organize.reset();
      setSectionConversation(conversation);
    }
    if (action === "copy-title" || action === "copy-id" || action === "copy-worktree") {
      setNotice("");
      const [value, confirmation] = action === "copy-title" ? [conversation.title, "Title copied."]
        : action === "copy-id" ? [conversation.id, "Conversation ID copied."] : [entry.checkout.checkoutRoot, "Worktree path copied."];
      void Promise.resolve().then(() => navigator.clipboard.writeText(value))
        .then(() => setNotice(confirmation))
        .catch(() => setNotice("Couldn’t copy to the clipboard."));
    }
    if (action === "pin") organize.mutate({ conversation, change: { type: "pin", pinned: !organization.pinned } });
    if (action === "unread") organize.mutate({ conversation, change: { type: "read", unread: !organization.unread } });
    if (action === "archive") organize.mutate({ conversation, change: { type: "archive", archived: !organization.archived } });
    if (action === "move") organize.mutate({ conversation, change: { type: "move", sectionId: sectionId ?? null } });
  }

  return <ConversationLibraryLayout sectionRef={libraryRef} query={query} searchLabel="Search conversation titles" onQuery={setQuery}
    onCreate={() => { setBatch(undefined); setArchived(false); setQuery(""); setProjectId(checkout.projectId); onStart(); }} creating={startPending} filters={<>
      <select className="conv-select conv-project-filter" aria-label="Project" value={projectId ?? ""}
        onChange={(event) => { setBatch(undefined); setProjectId(event.currentTarget.value || null); }}>
        <option value="">All projects</option>
        {[...projects].map(([id, name]) => <option key={id} value={id}>{name}</option>)}
      </select>
      <button type="button" className="conv-icon-button" aria-label="Archived" aria-pressed={archived}
        title="Archived" onClick={() => { setBatch(undefined); setArchived(!archived); }}><Archive aria-hidden /></button>
    </>} tools={batch && <div role="group" aria-label="Conversation selection" className="flex flex-wrap items-center gap-1 py-1">
      <span role="status" className="pl-1 text-xs text-muted-foreground">{batch.items.length} selected</span>
      <button type="button" className="conv-quiet-button" disabled={!batch.items.length} onClick={() => {
        moveBatch.reset();
        setShowBatchDialog(true);
      }}>Move selected conversations…</button>
      <button type="button" className="conv-quiet-button" onClick={() => setBatch(undefined)}>Cancel selection</button>
    </div>}>
    {error && <p role="alert" className="conv-alert mt-2">{String(error)}</p>}
    {list.isPending && <p role="status" className="conv-sidebar-note">Loading conversations…</p>}
    {list.data?.length === 0 && groups.length === 0 && <p className="conv-sidebar-note">{archived ? "No archived conversations match."
      : query.trim() ? "No matching conversations." : "No conversations in this view."}</p>}
    {groups.map(({ id, title, section, entries }) =>
      <ConversationLibraryGroup key={id} title={title} sectionId={section?.id} actions={sectionActions(entries, section)}>
        {!entries.length && <li className="conv-sidebar-note">No conversations in this section.</li>}
        {entries.map((entry) => {
          const { conversation, organization } = entry;
          const metadataId = `library-metadata-${conversation.id}`;
          const unreadId = `library-unread-${conversation.id}`;
          const batchProject = batch?.projectId === entry.checkout.projectId;
          const selected = Boolean(batchProject && batch?.items.some(({ conversationId }) => conversationId === conversation.id));
          return <li key={conversation.id}>
            {renaming?.id === conversation.id
              ? <ConversationTitleEditor conversation={renaming} onClose={() => setRenaming(undefined)} />
              : <ConversationRow title={conversation.title} {...organization}
                sections={sections.data?.filter(({ projectId }) => projectId === entry.checkout.projectId)}
                available={(action) => ["rename", "copy-title", "copy-id", "copy-worktree"].includes(action) ||
                  (action === "open" && !open.isPending) ||
                  (action === "open-beside" && !open.isPending && Boolean(onOpenBeside && entry.checkout.checkoutId === checkout.checkoutId)) ||
                  (action === "open-worktree" && !openWorktree.isPending && Boolean(onOpenCheckoutInIde)) ||
                  (action === "open-worktree-folder" && !openWorktree.isPending) ||
                  (action === "activity" && !inspect.isPending && Boolean(inspector && entry.status && entry.checkout.projectId === checkout.projectId)) ||
                  (action === "fork" && !openFork.isPending) ||
                  (action === "export-markdown" && !exportTranscript.isPending) ||
                  (action === "select-to-move" && (!batch || batchProject) && !selected) ||
                  (!organize.isPending && ["pin", "unread", "archive", "move", "new-section"].includes(action))}
                onAction={(action, sectionId) => rowAction(action, entry, sectionId)}>
                {batchProject && <input type="checkbox" className="ml-2 shrink-0 accent-primary" aria-label={`Select ${conversation.title}`}
                  checked={selected} onChange={({ currentTarget }) => {
                    if (currentTarget.checked) selectForMove([entry]);
                    else setBatch((current) => current && ({ ...current, items: current.items.filter(({ conversationId }) => conversationId !== conversation.id) }));
                  }} />}
                <button type="button" className="conv-session-button items-start" disabled={open.isPending}
                  data-conversation-id={conversation.id} aria-description={organization.unread ? "Unread" : undefined}
                  aria-label={conversation.title} aria-describedby={organization.unread ? `${unreadId} ${metadataId}` : metadataId}
                  aria-current={conversation.id === activeId ? "page" : undefined} onClick={() => open.mutate({ entry })}>
                  <MessageSquare className="mt-0.5" aria-hidden />
                  <span className="min-w-0">
                    <span className="block font-medium">{conversation.title}</span>
                    <ConversationMetadata entry={entry} id={metadataId} />
                  </span>
                  {organization.unread && <>
                    <i className="conv-unread mt-1.5" aria-hidden />
                    <span id={unreadId} className="sr-only">Unread</span>
                  </>}
                </button>
              </ConversationRow>}
          </li>;
        })}
      </ConversationLibraryGroup>)}
    {notice && <p className="conv-sidebar-note" role="status">{notice}</p>}
    {forking && <LibraryFork key={forking.conversation.id} entry={forking} onClose={() => setForking(undefined)}
      onOpen={async (source, id) => { openFork.mutate({ source, id }); }} />}
    {sectionConversation && <SectionNameDialog title="Create section" description={sectionConversation.title}
      busy={organize.isPending} error={organize.error}
      onSave={(name) => organize.mutate({ conversation: sectionConversation, change: { type: "newSection", name } })}
      onClose={() => { setSectionConversation(undefined); organize.reset(); }}
      restoreFocus={(event) => restoreLibraryFocus(event, "[data-conversation-id]", ({ dataset }) => dataset.conversationId === sectionConversation.id)} />}
    {renamingSection && <SectionNameDialog title="Rename section" description={renamingSection.name} initialName={renamingSection.name}
      busy={organizeSection.isPending} error={organizeSection.error}
      onSave={(name) => organizeSection.mutate({ section: renamingSection, change: { type: "rename", expectedName: renamingSection.name, name } })}
      onClose={() => { setRenamingSection(undefined); organizeSection.reset(); }}
      restoreFocus={(event) => restoreLibraryFocus(event, "[data-library-section-id]", ({ dataset }) => dataset.librarySectionId === renamingSection.id)} />}
    {batch && showBatchDialog && <LibraryBatchMoveDialog items={batch.items}
      sections={(sections.data ?? []).filter(({ projectId }) => projectId === batch.projectId)} busy={moveBatch.isPending} error={moveBatch.error}
      onClose={() => { setShowBatchDialog(false); moveBatch.reset(); }}
      onMove={(sectionId) => moveBatch.mutate({ projectId: batch.projectId, sectionId,
        selection: batch.items.map(({ checkoutId, conversationId, expectedSectionId }) => ({ checkoutId, conversationId, expectedSectionId })) })}
      restoreFocus={(event) => restoreLibraryFocus(event, "[data-conversation-id]", ({ dataset }) => dataset.conversationId === batch.items[0]?.conversationId)} />}
  </ConversationLibraryLayout>;
}

export function ConversationReferencePicker({ projectId, excludeId, onOpen, onAttach }: {
  projectId: string; excludeId: string; onOpen: OpenLibraryConversation;
  onAttach?: (reference: ConversationReferencePreview) => void;
}) {
  const [shown, setShown] = useState(false);
  const [query, setQuery] = useState("");
  const [source, setSource] = useState<ConversationLibraryEntry>();
  const list = useConversationLibrary(query, projectId, shown && !source);
  function changeShown(next: boolean) {
    setShown(next);
    if (!next) setSource(undefined);
  }
  const open = useMutation({
    mutationFn: (entry: ConversationLibraryEntry) => openEntry(onOpen, entry),
    onSuccess: () => changeShown(false),
  });
  const matches = list.data?.filter(({ conversation, checkout }) =>
    conversation.id !== excludeId && checkout.projectId === projectId);
  const error = list.error ?? open.error;
  return <Popover.Root open={shown} onOpenChange={changeShown}>
    <Popover.Trigger asChild>
      <button className="conv-quiet-button" type="button" aria-label="Find conversation references">@conversation</button>
    </Popover.Trigger>
    <Popover.Portal>
      <Popover.Content align="start" sideOffset={6} className="work-project-popover" aria-label="Conversation references">
        {error && <p role="alert" className="conv-alert">{String(error)}</p>}
        {source && onAttach ? <ConversationReferenceSelection
          key={source.conversation.id}
          destinationConversationId={excludeId}
          source={source}
          onBack={() => setSource(undefined)}
          onAttach={(reference) => { onAttach(reference); changeShown(false); }}
        /> : <>
          <label className="conv-search"><Search aria-hidden />
            <input type="search" aria-label="Search conversation references" placeholder="Search this project"
              value={query} onChange={(event) => setQuery(event.currentTarget.value)} />
          </label>
          {list.isPending && <p role="status">Loading conversations…</p>}
          {matches?.length === 0 && <p>No matching conversations.</p>}
          <ul className="max-h-64 overflow-auto">
            {matches?.map((entry) => <li key={entry.conversation.id} className="grid gap-1 border-b border-border last:border-0">
              <Button type="button" variant="ghost" className="h-auto justify-start" aria-label={`Open source ${entry.conversation.title}`}
                aria-describedby={`reference-metadata-${excludeId}-${entry.conversation.id}`} disabled={open.isPending}
                onClick={() => open.mutate(entry)}>
                <MessageSquare aria-hidden />
                <span className="min-w-0">
                  <strong className="block truncate">{entry.conversation.title}</strong>
                  <ConversationMetadata entry={entry} id={`reference-metadata-${excludeId}-${entry.conversation.id}`} />
                </span>
              </Button>
              {onAttach && <Button type="button" variant="outline" aria-label={`Choose excerpt or summary from ${entry.conversation.title}`}
                disabled={open.isPending} onClick={() => setSource(entry)}>Choose excerpt or summary</Button>}
            </li>)}
          </ul>
        </>}
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}
