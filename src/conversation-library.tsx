import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { MessageSquare, Search } from "lucide-react";
import { Popover } from "radix-ui";
import { createContext, useState } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";
import { ConversationLibraryLayout } from "@/conversation-layout";
import { ConversationRow, type ConversationAction } from "@/conversation-menu";
import { ConversationReferenceSelection, type ConversationReferencePreview } from "@/conversation-reference";

type ConversationLibraryEntry = {
  conversation: Conversation;
  checkout: ProjectCheckout;
  projectName: string;
  agent: string | null;
  model: string | null;
  status: string | null;
  lastActivityAt: number | null;
};

type ConversationIdentity = Pick<Conversation, "id" | "checkoutId" | "title">;
export type OpenLibraryConversation = (checkout: ProjectCheckout, conversationId: string) => Promise<void>;
export const conversationLibraryKey = ["conversation-library"];
export const ConversationLibraryNavigation = createContext<{
  checkoutId: string;
  projectId: string;
  open: OpenLibraryConversation;
} | null>(null);

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

function useConversationLibrary(query: string, projectId: string | null, enabled = true) {
  const title = query.trim();
  return useQuery({
    queryKey: [...conversationLibraryKey, projectId, title],
    queryFn: () => invoke<ConversationLibraryEntry[]>("conversation_library", { query: title || null, projectId }),
    enabled,
    refetchInterval: 5_000,
  });
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

const libraryActionAvailable = (action: ConversationAction) => action === "rename" || action === "copy-title";

export function ConversationLibrary({ checkout, activeId, startPending, startError, onStart, onOpen }: {
  checkout: ProjectCheckout;
  activeId?: string;
  startPending: boolean;
  startError?: Error | null;
  onStart: () => void;
  onOpen: OpenLibraryConversation;
}) {
  const [projectId, setProjectId] = useState<string | null>(checkout.projectId);
  const [query, setQuery] = useState("");
  const [renaming, setRenaming] = useState<ConversationIdentity>();
  const [notice, setNotice] = useState("");
  const list = useConversationLibrary(query, projectId);
  const open = useMutation({ mutationFn: (entry: ConversationLibraryEntry) => openEntry(onOpen, entry) });
  const registered = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
  });
  const projects = new Map([checkout, ...(registered.data ?? [])].map((item) => [
    item.projectId, item.repositoryRoot.split(/[\\/]/).filter(Boolean).at(-1) ?? item.repositoryRoot,
  ]));
  const error = open.error ?? list.error ?? registered.error ?? startError;

  function rowAction(action: ConversationAction, conversation: ConversationIdentity) {
    if (action === "rename") setRenaming(conversation);
    if (action === "copy-title") {
      setNotice("");
      void Promise.resolve().then(() => navigator.clipboard.writeText(conversation.title))
        .then(() => setNotice("Title copied."))
        .catch(() => setNotice("Couldn’t copy the title."));
    }
  }

  return <ConversationLibraryLayout query={query} searchLabel="Search conversation titles" onQuery={setQuery}
    onCreate={onStart} creating={startPending} filters={
      <select className="conv-select conv-project-filter" aria-label="Project" value={projectId ?? ""}
        onChange={(event) => setProjectId(event.currentTarget.value || null)}>
        <option value="">All projects</option>
        {[...projects].map(([id, name]) => <option key={id} value={id}>{name}</option>)}
      </select>
    }>
    {error && <p role="alert" className="conv-alert mt-2">{String(error)}</p>}
    {list.isPending && <p role="status" className="conv-sidebar-note">Loading conversations…</p>}
    {list.data?.length === 0 && <p className="conv-sidebar-note">{query.trim() ? "No matching conversations." : "No conversations in this view."}</p>}
    <ul className="conv-session-list">
      {list.data?.map((entry) => <li key={entry.conversation.id}>
        {renaming?.id === entry.conversation.id
          ? <ConversationTitleEditor conversation={renaming} onClose={() => setRenaming(undefined)} />
          : <ConversationRow title={entry.conversation.title} available={libraryActionAvailable}
            onAction={(action) => rowAction(action, entry.conversation)}>
            <button type="button" className="conv-session-button items-start" disabled={open.isPending}
              aria-label={entry.conversation.title} aria-describedby={`library-metadata-${entry.conversation.id}`}
              aria-current={entry.conversation.id === activeId ? "page" : undefined} onClick={() => open.mutate(entry)}>
              <MessageSquare className="mt-0.5" aria-hidden />
              <span className="min-w-0">
                <span className="block font-medium">{entry.conversation.title}</span>
                <ConversationMetadata entry={entry} id={`library-metadata-${entry.conversation.id}`} />
              </span>
            </button>
          </ConversationRow>}
      </li>)}
    </ul>
    {notice && <p className="conv-sidebar-note" role="status">{notice}</p>}
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
