import { Archive, ArrowUp, CircleHelp, FileText, GitFork, MessageSquare, X } from "lucide-react";
import { Dialog } from "radix-ui";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { ConversationEmptyState, ConversationLibraryGroup, ConversationLibraryLayout, ConversationNavigation, ConversationWorkspaceLayout } from "@/conversation-layout";
import { ConversationRow, type ConversationAction } from "@/conversation-menu";
import {
  createLibrary, createPreviewConversation, primaryProject,
  type PreviewConversation, type PreviewLibrary, type PreviewMessage,
} from "@/conversation-preview-data";
import { PreviewConversationPanel } from "@/conversation-preview-panel";
import type { WorkspaceProps } from "@/workbench-contract";

type PreviewWorkspaceProps = WorkspaceProps & { compact?: boolean };

type LibraryDialog = {
  action: "rename" | "new-section" | "fork" | "delete" | "rewind";
  conversationId: string;
  title: string;
  revision: number;
  messageId?: string;
};

function selectConversation(library: PreviewLibrary, conversationId: string): PreviewLibrary {
  if (!library.conversations.some(({ id }) => id === conversationId)) return library;
  return {
    ...library,
    activeId: conversationId,
    tabIds: library.tabIds.includes(conversationId) ? library.tabIds : [...library.tabIds, conversationId],
    conversations: library.conversations.map((conversation) => (
      conversation.id === conversationId && conversation.unread ? { ...conversation, unread: false } : conversation
    )),
  };
}

export default function ConversationsPreview(props: PreviewWorkspaceProps) {
  if (!props.simulation) return (
    <ConversationEmptyState title="Conversations" description="Try conversation history, planning and agent controls with example data.">
      <button type="button" className="conv-inline-action" onClick={() => props.startSimulation("delivery")}>Simulate</button>
    </ConversationEmptyState>
  );
  return <PreviewSession key={props.simulation.epoch} {...props} simulation={props.simulation} />;
}

function PreviewSession(props: PreviewWorkspaceProps & { simulation: NonNullable<WorkspaceProps["simulation"]> }) {
  const { target, compact = false } = props;
  const [library, setLibrary] = useState(createLibrary);
  const [filter, setFilter] = useState("");
  const [allProjects, setAllProjects] = useState(false);
  const [archived, setArchived] = useState(false);
  const [mobileLibrary, setMobileLibrary] = useState(false);
  const [notice, setNotice] = useState("");
  const [dialog, setDialog] = useState<LibraryDialog>();
  const [dialogValue, setDialogValue] = useState("");
  const [dialogError, setDialogError] = useState("");
  const [forkTarget, setForkTarget] = useState("current");
  const [deleteCopies, setDeleteCopies] = useState(false);
  const libraryRef = useRef<HTMLElement>(null);
  const libraryToggle = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (target) setLibrary((current) => selectConversation(current, target));
  }, [target]);

  const current = library.conversations.find(({ id }) => id === library.activeId);
  const scope = current?.project ?? primaryProject;
  const matching = library.conversations.filter((conversation) =>
    conversation.archived === archived && (allProjects || conversation.project === scope)
    && conversation.title.toLowerCase().includes(filter.trim().toLowerCase()));
  const groups = new Map<string, PreviewConversation[]>();
  for (const conversation of matching) {
    const group = conversation.pinned && !archived ? "Pinned"
      : [allProjects ? conversation.project : null, conversation.section ?? "Conversations"].filter(Boolean).join(" · ");
    groups.set(group, [...(groups.get(group) ?? []), conversation]);
  }

  function updateConversation(id: string, update: (conversation: PreviewConversation) => PreviewConversation) {
    setLibrary((current) => ({
      ...current,
      conversations: current.conversations.map((conversation) => conversation.id === id ? update(conversation) : conversation),
    }));
  }

  function openConversation(id: string) {
    setLibrary((current) => selectConversation(current, id));
    changeMobileLibrary(false);
  }

  function changeMobileLibrary(open: boolean) {
    setMobileLibrary(open);
    if (!open && mobileLibrary) requestAnimationFrame(() => libraryToggle.current?.focus());
  }

  function closeTab(id: string) {
    setLibrary((current) => {
      const tabIds = current.tabIds.filter((tab) => tab !== id);
      const index = current.tabIds.indexOf(id);
      return { ...current, tabIds, activeId: current.activeId === id ? (tabIds[index] ?? tabIds[index - 1] ?? null) : current.activeId };
    });
  }

  function createConversation() {
    setLibrary((current) => {
      const id = `conversation-${current.nextId}`;
      const next = createPreviewConversation(id, `New conversation ${current.nextId}`, scope);
      return selectConversation({
        ...current, nextId: current.nextId + 1, conversations: [...current.conversations, next],
      }, id);
    });
    setArchived(false);
    setFilter("");
    changeMobileLibrary(false);
  }

  function openDialog(action: LibraryDialog["action"], conversation: PreviewConversation, message?: PreviewMessage) {
    setDialog({ action, conversationId: conversation.id, title: conversation.title, revision: conversation.revision, messageId: message?.id });
    setDialogValue(action === "rename" ? conversation.title : "");
    setDialogError("");
    setForkTarget("current");
    setDeleteCopies(false);
  }

  function rowAction(conversation: PreviewConversation, action: ConversationAction, section?: string) {
    if (action === "rename" || action === "new-section" || action === "fork" || action === "delete") {
      openDialog(action, conversation);
      return;
    }
    const id = conversation.id;
    if (action === "pin") updateConversation(id, (item) => ({ ...item, pinned: !item.pinned }));
    if (action === "unread") updateConversation(id, (item) => ({ ...item, unread: !item.unread }));
    if (action === "move") updateConversation(id, (item) => ({ ...item, section }));
    if (action === "archive") {
      updateConversation(id, (item) => ({ ...item, archived: !item.archived }));
      setNotice(conversation.archived ? "Conversation restored." : "Conversation moved to Archived.");
    }
    if (action === "copy-title" || action === "copy-reference") {
      const value = action === "copy-title" ? conversation.title : `@conversation:${id}`;
      void Promise.resolve().then(() => navigator.clipboard.writeText(value))
        .then(() => setNotice(action === "copy-title" ? "Title copied." : "Example reference copied."))
        .catch(() => setNotice(`Copy: ${value}`));
    }
  }

  function confirmDialog(event: FormEvent) {
    event.preventDefault();
    const selected = library.conversations.find(({ id }) => id === dialog?.conversationId);
    if (!dialog || !selected) {
      setDialogError("This conversation is no longer available.");
      return;
    }
    const { action, revision, messageId } = dialog;
    if (selected.revision !== revision) {
      setDialogError("This conversation changed. Reopen the action to refresh its preview.");
      return;
    }
    const value = dialogValue.trim();
    if ((action === "rename" || action === "new-section") && !value) return;
    if (action === "rename") updateConversation(selected.id, (item) => ({ ...item, title: value }));
    if (action === "new-section") {
      const equivalent = library.sections[selected.project]?.find((section) => section.toLowerCase() === value.toLowerCase());
      if (equivalent) {
        setDialogError(`A section named ${equivalent} already exists in this project.`);
        return;
      }
      setLibrary((current) => ({ ...current, sections: {
        ...current.sections, [selected.project]: [...(current.sections[selected.project] ?? []), value],
      } }));
      updateConversation(selected.id, (item) => ({ ...item, section: value }));
    }
    if (action === "fork") {
      const lastIndex = messageId ? selected.messages.findIndex(({ id }) => id === messageId) : selected.messages.length - 1;
      setLibrary((current) => {
        const id = `conversation-${current.nextId}`;
        const next = {
          ...createPreviewConversation(id, `Fork of ${selected.title}`, selected.project),
          messages: selected.messages.slice(0, lastIndex + 1), forkOf: selected.id,
          worktree: forkTarget === "new" ? `example-fork-${current.nextId}` : selected.worktree,
        };
        return selectConversation({
          ...current, nextId: current.nextId + 1, conversations: [...current.conversations, next],
        }, id);
      });
      setNotice(forkTarget === "new" ? "Fork opened in an example worktree." : "Fork opened in the current example worktree.");
    }
    if (action === "rewind") {
      const end = selected.messages.findIndex(({ id }) => id === messageId);
      updateConversation(selected.id, (item) => ({
        ...item, former: [...item.former, item.messages], messages: item.messages.slice(0, end + 1), revision: item.revision + 1,
      }));
    }
    if (action === "delete") {
      const deleted = new Set([selected.id]);
      if (deleteCopies) {
        for (const conversation of library.conversations) {
          if (conversation.forkOf && deleted.has(conversation.forkOf)) deleted.add(conversation.id);
        }
      }
      setLibrary((current) => {
        const tabIds = current.tabIds.filter((id) => !deleted.has(id));
        return {
          ...current,
          activeId: current.activeId && deleted.has(current.activeId) ? (tabIds[0] ?? null) : current.activeId,
          tabIds,
          conversations: current.conversations.filter(({ id }) => !deleted.has(id)),
        };
      });
      setNotice("Example history deleted.");
    }
    setDialog(undefined);
  }

  const orderedGroups = [...groups.entries()].sort(([a], [b]) =>
    a === "Pinned" ? -1 : b === "Pinned" ? 1 : a.localeCompare(b));
  const dialogTitle = dialog ? {
    rename: "Rename conversation", "new-section": "Create section", fork: "Create fork",
    delete: "Delete conversation history", rewind: "Rewind conversation",
  }[dialog.action] : "";
  const navigation = <ConversationNavigation compact={compact} open={!compact || mobileLibrary} onOpenChange={changeMobileLibrary}>
    <ConversationLibraryLayout sectionRef={libraryRef} className="conv-example-sidebar" query={filter} searchLabel="Filter conversations"
      onQuery={setFilter} onCreate={createConversation} filters={<>
        <select className="conv-select conv-project-filter" aria-label="Conversation project scope" value={allProjects ? "all" : "current"}
          onChange={(event) => setAllProjects(event.currentTarget.value === "all")}>
          <option value="current">{scope}</option><option value="all">All projects</option>
        </select>
        <button type="button" className="conv-icon-button" aria-label="Archived" aria-pressed={archived}
          title="Archived" onClick={() => setArchived(!archived)}><Archive aria-hidden /></button>
      </>}>
        {orderedGroups.map(([group, conversations]) => (
          <ConversationLibraryGroup key={group} title={group}>
            {conversations.map((conversation) => (
              <li key={conversation.id}>
                <ConversationRow {...conversation}
                  sections={library.sections[conversation.project]?.map((name) => ({ id: name, name }))}
                  onAction={(action, section) => rowAction(conversation, action, section)}>
                  <button type="button" className="conv-session-button" data-conversation-id={conversation.id}
                    aria-description={conversation.unread ? "Unread" : undefined} onClick={() => openConversation(conversation.id)}
                    aria-current={current?.id === conversation.id ? "page" : undefined}>
                    <MessageSquare aria-hidden /><span>{conversation.title}</span>
                    {conversation.unread && <i className="conv-unread" aria-hidden />}
                  </button>
                </ConversationRow>
              </li>
            ))}
          </ConversationLibraryGroup>
        ))}
        {!matching.length && <p className="conv-sidebar-note">{archived ? "No archived conversations match." : "No conversations match."}</p>}
        {notice && <p className="conv-sidebar-note" role="status">{notice}</p>}
    </ConversationLibraryLayout>
  </ConversationNavigation>;
  return (
    <ConversationWorkspaceLayout navigation={navigation} controls={compact &&
      <button ref={libraryToggle} type="button" className="conv-mobile-library conv-quiet-button" onClick={() => changeMobileLibrary(true)}
        aria-expanded={mobileLibrary}><MessageSquare aria-hidden /> Conversations</button>
    }>
        <nav className="conv-example-tabs" aria-label="Open conversations">
          {library.tabIds.map((id) => {
            const tab = library.conversations.find((conversation) => conversation.id === id);
            if (!tab) return null;
            return (
              <div className="conv-example-tab" key={id} data-active={current?.id === id}>
                <button type="button" aria-current={current?.id === id ? "page" : undefined} onClick={() => openConversation(id)}>
                  <MessageSquare aria-hidden /><span>{tab.title}</span>
                </button>
                <button type="button" className="conv-icon-button" aria-label={`Close ${tab.title}`} onClick={() => closeTab(id)}><X aria-hidden /></button>
              </div>
            );
          })}
        </nav>
        {current ? <PreviewConversationPanel key={current.id} {...props} conversation={current}
          references={library.conversations.filter(({ id, project, archived }) => id !== current.id && project === current.project && !archived)}
          update={(update) => updateConversation(current.id, update)}
          onFork={(message) => openDialog("fork", current, message)}
          onRewind={(message) => openDialog("rewind", current, message)} />
          : <ConversationEmptyState title="No conversation open">
            <button type="button" className="conv-primary-button" onClick={createConversation}>New conversation</button>
          </ConversationEmptyState>}
      {dialog && <Dialog.Root open onOpenChange={(open) => { if (!open) setDialog(undefined); }}>
        <Dialog.Portal>
          <Dialog.Overlay className="conv-dialog-scrim" />
          <Dialog.Content className="conv-dialog" onCloseAutoFocus={(event) => {
            const library = libraryRef.current;
            const row = Array.from(library?.querySelectorAll<HTMLElement>("[data-conversation-id]") ?? [])
              .find(({ dataset }) => dataset.conversationId === dialog.conversationId);
            const focus = row ?? library?.querySelector<HTMLElement>('input[aria-label="Filter conversations"]');
            if (focus) { event.preventDefault(); focus.focus(); }
          }}>
            <Dialog.Title>{dialogTitle}</Dialog.Title>
            <Dialog.Description>{dialog.title} · Example revision {dialog.revision}</Dialog.Description>
            <form onSubmit={confirmDialog} className="conv-dialog-form">
              {(dialog.action === "rename" || dialog.action === "new-section") && <label>
                {dialog.action === "rename" ? "Conversation title" : "Section name"}
                <input value={dialogValue} required maxLength={96} onChange={(event) => setDialogValue(event.currentTarget.value)} />
              </label>}
              {dialog.action === "fork" && <label><span className="flex items-center gap-2"><GitFork size={16} aria-hidden /> Destination</span>
                <select value={forkTarget} onChange={(event) => setForkTarget(event.currentTarget.value)}>
                  <option value="current">Reuse current checkout</option><option value="new">New example worktree</option>
                </select>
                <small>Base: {library.conversations.find(({ id }) => id === dialog.conversationId)?.worktree} · {dialog.messageId ? "Selected message" : "Current conversation head"}</small>
              </label>}
              {dialog.action === "delete" && <>
                <p className="flex items-center gap-2"><FileText size={16} aria-hidden /> Remove transcript, drafts and indexed history for this example.</p>
                <label className="conv-checkbox"><input type="checkbox" checked={deleteCopies} onChange={(event) => setDeleteCopies(event.currentTarget.checked)} /> Include derived copies</label>
              </>}
              {dialog.action === "rewind" && <p className="flex items-center gap-2"><CircleHelp size={16} aria-hidden /> Restore the conversation at the selected message. The previous revision remains in Activity.</p>}
              {dialogError && <p role="alert" className="text-red-300">{dialogError}</p>}
              <footer>
                <Dialog.Close asChild><button type="button" className="conv-quiet-button">Cancel</button></Dialog.Close>
                <button type="submit" className={dialog.action === "delete" ? "conv-danger-button" : "conv-primary-button"}>
                  {dialog.action === "delete" ? "Delete permanently" : dialog.action === "fork" ? "Create fork" : dialog.action === "rewind" ? "Rewind" : "Save"}
                  <ArrowUp size={14} aria-hidden />
                </button>
              </footer>
            </form>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>}
    </ConversationWorkspaceLayout>
  );
}
