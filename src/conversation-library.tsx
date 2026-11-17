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
import { ConversationLibraryGroup, ConversationLibraryLayout } from "@/conversation-layout";
import { ConversationRow, type ConversationAction } from "@/conversation-menu";
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

function libraryGroups(entries: ConversationLibraryEntry[], allProjects: boolean, archived: boolean) {
  const groups = new Map<string, { id: string; title: string; pinned: boolean; entries: ConversationLibraryEntry[] }>();
  for (const entry of entries) {
    const { pinned, section } = entry.organization;
    const isPinned = pinned && !archived;
    const id = isPinned ? "pinned" : `${entry.checkout.projectId}:${section?.id ?? ""}`;
    const title = isPinned ? "Pinned"
      : [allProjects ? entry.projectName : null, section?.name ?? "Conversations"].filter(Boolean).join(" · ");
    const group = groups.get(id);
    if (group) group.entries.push(entry);
    else groups.set(id, { id, title, pinned: isPinned, entries: [entry] });
  }
  return [...groups.values()].sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.title.localeCompare(b.title));
}

export function ConversationLibrary({ checkout, activeId, startPending, startError, onStart, onOpen }: {
  checkout: ProjectCheckout;
  activeId?: string;
  startPending: boolean;
  startError?: Error | null;
  onStart: () => void;
  onOpen: OpenLibraryConversation;
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
  const [sectionName, setSectionName] = useState("");
  const [notice, setNotice] = useState("");
  const list = useConversationLibrary(query, projectId, true, archived);
  const sections = useQuery({
    queryKey: [...sectionKey, projectId],
    queryFn: () => invoke<ConversationLibrarySection[]>("conversation_library_sections", { projectId }),
  });
  const open = useMutation({ mutationFn: (entry: ConversationLibraryEntry) => openEntry(onOpen, entry) });
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
  const registered = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
  });
  const projects = new Map([checkout, ...(registered.data ?? [])].map((item) => [
    item.projectId, item.repositoryRoot.split(/[\\/]/).filter(Boolean).at(-1) ?? item.repositoryRoot,
  ]));
  const error = open.error ?? inspect.error ?? openFork.error ?? list.error ?? registered.error ?? sections.error ?? startError ??
    (!sectionConversation ? organize.error : undefined);

  function rowAction(action: ConversationAction, entry: ConversationLibraryEntry, sectionId?: string) {
    const { conversation, organization } = entry;
    if (action === "open") open.mutate(entry);
    if (action === "activity") inspect.mutate(entry);
    if (action === "fork") setForking(entry);
    if (action === "rename") setRenaming(conversation);
    if (action === "new-section") {
      organize.reset();
      setSectionName("");
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
    onCreate={() => { setArchived(false); setQuery(""); setProjectId(checkout.projectId); onStart(); }} creating={startPending} filters={<>
      <select className="conv-select conv-project-filter" aria-label="Project" value={projectId ?? ""}
        onChange={(event) => setProjectId(event.currentTarget.value || null)}>
        <option value="">All projects</option>
        {[...projects].map(([id, name]) => <option key={id} value={id}>{name}</option>)}
      </select>
      <button type="button" className="conv-icon-button" aria-label="Archived" aria-pressed={archived}
        title="Archived" onClick={() => setArchived(!archived)}><Archive aria-hidden /></button>
    </>}>
    {error && <p role="alert" className="conv-alert mt-2">{String(error)}</p>}
    {list.isPending && <p role="status" className="conv-sidebar-note">Loading conversations…</p>}
    {list.data?.length === 0 && <p className="conv-sidebar-note">{archived ? "No archived conversations match."
      : query.trim() ? "No matching conversations." : "No conversations in this view."}</p>}
    {libraryGroups(list.data ?? [], projectId === null, archived).map(({ id, title, entries }) =>
      <ConversationLibraryGroup key={id} title={title}>
        {entries.map((entry) => {
          const { conversation, organization } = entry;
          const metadataId = `library-metadata-${conversation.id}`;
          const unreadId = `library-unread-${conversation.id}`;
          return <li key={conversation.id}>
            {renaming?.id === conversation.id
              ? <ConversationTitleEditor conversation={renaming} onClose={() => setRenaming(undefined)} />
              : <ConversationRow title={conversation.title} {...organization}
                sections={sections.data?.filter(({ projectId }) => projectId === entry.checkout.projectId)}
                available={(action) => ["rename", "copy-title", "copy-id", "copy-worktree"].includes(action) ||
                  (action === "open" && !open.isPending) || (action === "activity" && !inspect.isPending && Boolean(inspector && entry.status && entry.checkout.projectId === checkout.projectId)) ||
                  (action === "fork" && !openFork.isPending) ||
                  (!organize.isPending && ["pin", "unread", "archive", "move", "new-section"].includes(action))}
                onAction={(action, sectionId) => rowAction(action, entry, sectionId)}>
                <button type="button" className="conv-session-button items-start" disabled={open.isPending}
                  data-conversation-id={conversation.id} aria-description={organization.unread ? "Unread" : undefined}
                  aria-label={conversation.title} aria-describedby={organization.unread ? `${unreadId} ${metadataId}` : metadataId}
                  aria-current={conversation.id === activeId ? "page" : undefined} onClick={() => open.mutate(entry)}>
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
    {sectionConversation && <Dialog.Root open onOpenChange={(shown) => {
      if (!shown && !organize.isPending) { setSectionConversation(undefined); organize.reset(); }
    }}>
      <Dialog.Portal>
        <Dialog.Overlay className="conv-dialog-scrim" />
        <Dialog.Content className="conv-dialog" onCloseAutoFocus={(event) => {
          const library = libraryRef.current;
          const row = Array.from(library?.querySelectorAll<HTMLElement>("[data-conversation-id]") ?? [])
            .find(({ dataset }) => dataset.conversationId === sectionConversation.id);
          const focus = row ?? library?.querySelector<HTMLElement>('input[type="search"]');
          if (focus) { event.preventDefault(); focus.focus(); }
        }}>
          <Dialog.Title>Create section</Dialog.Title>
          <Dialog.Description>{sectionConversation.title}</Dialog.Description>
          <form className="conv-dialog-form" onSubmit={(event) => {
            event.preventDefault();
            if (sectionName.trim() && !organize.isPending) organize.mutate({ conversation: sectionConversation,
              change: { type: "newSection", name: sectionName.trim() } });
          }}>
            <label>Section name<input value={sectionName} maxLength={96} required disabled={organize.isPending}
              onChange={(event) => setSectionName(event.currentTarget.value)} /></label>
            {organize.error && <p role="alert" className="text-destructive">{String(organize.error)}</p>}
            <footer>
              <Dialog.Close asChild><button type="button" className="conv-quiet-button" disabled={organize.isPending}>Cancel</button></Dialog.Close>
              <button type="submit" className="conv-primary-button" disabled={!sectionName.trim() || organize.isPending}>
                Save <ArrowUp size={14} aria-hidden />
              </button>
            </footer>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>}
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
