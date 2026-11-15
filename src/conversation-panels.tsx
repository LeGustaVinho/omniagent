import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { IDockviewPanelProps } from "dockview-react";
import { Activity, ArrowDown, ArrowUp, MessageSquare, Plus, Search } from "lucide-react";
import { Dialog } from "radix-ui";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type FormEvent } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { MessageRevision } from "@/bindings/MessageRevision";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { RunRecord } from "@/bindings/RunRecord";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { ConversationActivity, ConversationRequests, StopConversation, isLiveRun, useConversationRun } from "@/conversation-run";
import { ConversationMarkdown } from "@/conversation-markdown";
import { ConversationRow } from "@/conversation-menu";
import { openConversation } from "@/workspace-tabs";
import { useWorkspace } from "@/workspace-state";
import "./conversation.css";

const draftWrites = new Map<string, Promise<void>>();
const historyOffsets = new Map<string, number>();

function saveDraft(conversationId: string, text: string) {
  const pending = draftWrites.get(conversationId);
  const write = (pending?.catch(() => undefined) ?? Promise.resolve()).then(() =>
    invoke<void>("save_conversation_draft", { conversationId, text }),
  );
  draftWrites.set(conversationId, write);
  const release = () => {
    if (draftWrites.get(conversationId) === write) draftWrites.delete(conversationId);
  };
  void write.then(release, release);
  return write;
}

export function ConversationListPanel({ containerApi }: IDockviewPanelProps) {
  const checkoutId = useWorkspace((state) => state.checkout?.checkoutId);
  if (!checkoutId) return null;
  return <ConversationSidebar checkoutId={checkoutId} onOpen={(conversation) => openConversation(containerApi, conversation)} />;
}

export function ConversationSidebar({ checkoutId, onOpen, activeId, onSimulate, onRenamed }: {
  checkoutId: string;
  onOpen: (conversation: Conversation) => void;
  activeId?: string;
  onSimulate?: () => void;
  onRenamed?: (conversation: Conversation) => void;
}) {
  const queryClient = useQueryClient();
  const [filter, setFilter] = useState("");
  const [notice, setNotice] = useState("");
  const [renaming, setRenaming] = useState<Conversation>();
  const [title, setTitle] = useState("");
  const sidebar = useRef<HTMLElement>(null);
  const listKey = ["conversations", checkoutId] as const;
  const conversations = useQuery({
    queryKey: listKey,
    queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
    enabled: Boolean(checkoutId),
  });
  const start = useMutation({
    mutationFn: () => invoke<Conversation>("create_conversation", {
      checkoutId,
      title: `Conversation ${(conversations.data?.length ?? 0) + 1}`,
    }),
    onSuccess: (conversation) => {
      void queryClient.invalidateQueries({ queryKey: listKey });
      onOpen(conversation);
    },
  });
  const rename = useMutation({
    mutationFn: ({ conversationId, title }: { conversationId: string; title: string }) =>
      invoke<Conversation>("rename_conversation", { checkoutId, conversationId, title }),
    onSuccess: (updated) => {
      queryClient.setQueryData<Conversation[]>(listKey, (previous) =>
        previous?.map((conversation) => conversation.id === updated.id ? updated : conversation));
      void queryClient.invalidateQueries({ queryKey: listKey });
      onRenamed?.(updated);
      setRenaming(undefined);
    },
  });
  const error = start.error ?? conversations.error;
  const matching = conversations.data?.filter(({ title }) => title.toLowerCase().includes(filter.trim().toLowerCase()));
  return (
    <section ref={sidebar} className="conv-sessions" aria-label="Conversation list">
      <header className="conv-session-heading">
        <h2>Conversations</h2>
        <button className="conv-icon-button" type="button" title="New conversation" aria-label="New conversation"
          onClick={() => start.mutate()} disabled={conversations.isPending || start.isPending}>
          <Plus aria-hidden /><span className="sr-only">New conversation</span>
        </button>
      </header>
      <label className="conv-search">
        <Search aria-hidden /><input aria-label="Filter conversations" placeholder="Filter conversations" value={filter}
          onChange={(event) => setFilter(event.target.value)} />
      </label>
      {error && <p role="alert" className="mb-4 text-sm text-red-300">{String(error)}</p>}
      {conversations.isPending && <p role="status" className="text-sm text-muted-foreground">Loading conversations…</p>}
      {matching?.length === 0 && <p className="conv-sidebar-note">{filter ? "No matching conversations." : "New conversations appear here."}</p>}
      <ul className="conv-session-list">
        {matching?.map((conversation) => (
          <li key={conversation.id}>
            <ConversationRow title={conversation.title} available={(action) => action === "copy-title" || action === "rename"}
              onAction={(action) => {
                if (action === "rename") {
                  setRenaming(conversation);
                  setTitle(conversation.title);
                  rename.reset();
                  return;
                }
                void Promise.resolve().then(() => navigator.clipboard.writeText(conversation.title))
                  .then(() => setNotice("Title copied."))
                  .catch(() => setNotice("Couldn’t copy the title."));
              }}>
              <button type="button" className="conv-session-button" data-conversation-id={conversation.id}
                aria-current={activeId === conversation.id ? "page" : undefined}
                onClick={() => onOpen(conversation)}>
                <MessageSquare aria-hidden /><span>{conversation.title}</span>
              </button>
            </ConversationRow>
          </li>
        ))}
      </ul>
      {notice && <p role="status" className="conv-sidebar-note">{notice}</p>}
      {onSimulate && Boolean(matching?.length) && <p className="conv-sidebar-note">Try organization and history controls in <button type="button" className="conv-inline-action" onClick={onSimulate}>Simulation</button>.</p>}
      {renaming && <Dialog.Root open onOpenChange={(open) => { if (!open && !rename.isPending) setRenaming(undefined); }}>
        <Dialog.Portal>
          <Dialog.Overlay className="conv-dialog-scrim" />
          <Dialog.Content className="conv-dialog" onCloseAutoFocus={(event) => {
            const row = Array.from(sidebar.current?.querySelectorAll<HTMLElement>("[data-conversation-id]") ?? [])
              .find(({ dataset }) => dataset.conversationId === renaming.id);
            const focus = row ?? sidebar.current?.querySelector<HTMLElement>('input[aria-label="Filter conversations"]');
            if (focus) { event.preventDefault(); focus.focus(); }
          }}>
            <Dialog.Title>Rename conversation</Dialog.Title>
            <Dialog.Description>{renaming.title}</Dialog.Description>
            <form className="conv-dialog-form" onSubmit={(event) => {
              event.preventDefault();
              if (title.trim() && !rename.isPending) rename.mutate({ conversationId: renaming.id, title: title.trim() });
            }}>
              <label>Conversation title<input type="text" value={title} required disabled={rename.isPending}
                onChange={(event) => setTitle(event.currentTarget.value)} /></label>
              {rename.error && <p role="alert" className="conv-alert">{String(rename.error)}</p>}
              <footer>
                <Dialog.Close asChild><button type="button" className="conv-quiet-button" disabled={rename.isPending}>Cancel</button></Dialog.Close>
                <button type="submit" className="conv-primary-button" disabled={!title.trim() || rename.isPending}>
                  {rename.isPending ? "Saving…" : "Save"}
                </button>
              </footer>
            </form>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>}
    </section>
  );
}

type PanelParams = { conversationId: string; title: string };
type DesktopModel = { option: ModelOption; efforts: string[] };
type ProfileMode = "read_only" | "plan";
const selectorClass = "conv-select";

export function ConversationPanel({ params, api }: IDockviewPanelProps<PanelParams>) {
  const subscribe = useCallback((update: () => void) => {
    const listener = api.onDidVisibilityChange(update);
    return () => listener.dispose();
  }, [api]);
  const visible = useSyncExternalStore(subscribe, () => api.isVisible);
  useEffect(() => {
    if (api.renderer !== "always") api.setRenderer("always");
  }, [api]);
  return <ConversationContent key={params.conversationId} {...params} visible={visible} />;
}

function ConversationContent({ conversationId, title, visible }: PanelParams & { visible: boolean }) {
  const queryClient = useQueryClient();
  const draftKey = ["conversation", conversationId, "draft"];
  const draft = useQuery({
    queryKey: draftKey,
    queryFn: async () => {
      await draftWrites.get(conversationId);
      return invoke<string>("conversation_draft", { conversationId });
    },
    staleTime: Infinity,
  });
  const historyKey = ["conversation", conversationId, "messages"];
  const history = useQuery({
    queryKey: historyKey,
    queryFn: () => invoke<MessageRevision[]>("conversation_messages", { conversationId }),
  });
  const historyRegion = useRef<HTMLDivElement>(null);
  const historyContents = useRef<HTMLDivElement>(null);
  const stickToEnd = useRef(!historyOffsets.has(conversationId));
  const [showJump, setShowJump] = useState(false);
  useLayoutEffect(() => {
    if (!visible || !history.data || !historyRegion.current) return;
    const region = historyRegion.current;
    region.scrollTop = stickToEnd.current ? region.scrollHeight : historyOffsets.get(conversationId) ?? 0;
  }, [conversationId, visible, history.data]);
  useEffect(() => {
    const contents = historyContents.current;
    const region = historyRegion.current;
    if (!visible || !contents || !region) return;
    const observer = new ResizeObserver(() => {
      if (stickToEnd.current) region.scrollTop = region.scrollHeight;
    });
    observer.observe(contents);
    return () => observer.disconnect();
  }, [visible]);
  const activeEdit = useRef(0);
  const [draftError, setDraftError] = useState("");
  const [draftStatus, setDraftStatus] = useState("");
  const [provider, setProvider] = useState<Provider>("codex");
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const [effort, setEffort] = useState("");
  const [mode, setMode] = useState<ProfileMode>("read_only");
  const text = draft.data ?? "";
  const models = useQuery({
    queryKey: ["conversation-models", provider],
    queryFn: () => invoke<DesktopModel[]>("conversation_models", { provider }),
    staleTime: 60_000,
  });
  const selected = models.data?.find(({ option }) => option.selection.modelId === selection?.modelId
    && option.selection.provider === selection?.provider && option.selection.profileId === selection?.profileId);
  const runKey = ["conversation", conversationId, "latest-run"];
  const latest = useQuery({
    queryKey: runKey,
    queryFn: () => invoke<RunRecord | null>("latest_conversation_run", { conversationId }),
    refetchInterval: (query) => isLiveRun(query.state.data) ? 250 : false,
  });
  const run = latest.data;
  const activity = useConversationRun(conversationId, run);
  const { live } = activity;
  const [inspectedRun, setInspectedRun] = useState<string>();
  const activityButton = useRef<HTMLButtonElement>(null);
  const showingActivity = visible && run && inspectedRun === run.id;
  useEffect(() => {
    if (!visible) setInspectedRun(undefined);
  }, [visible]);
  function closeActivity() {
    setInspectedRun(undefined);
    activityButton.current?.focus();
  }
  useEffect(() => {
    if (run?.status) void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "messages"] });
  }, [run?.id, run?.status, conversationId, queryClient]);
  const send = useMutation({
    mutationFn: async (submitted: string) => {
      if (!selection) throw new Error("Choose a model");
      await saveDraft(conversationId, submitted);
      return invoke<RunRecord>("start_conversation_turn", {
        conversationId, selection, effort: effort || null, mode, text: submitted,
      });
    },
    onSuccess: (run, submitted) => {
      activeEdit.current++;
      queryClient.setQueryData<string>(draftKey, (current) => current === submitted ? "" : current);
      setDraftStatus("");
      setDraftError("");
      queryClient.setQueryData(runKey, run);
      void queryClient.invalidateQueries({ queryKey: historyKey });
    },
  });

  function editDraft(text: string) {
    const edit = ++activeEdit.current;
    setDraftError("");
    setDraftStatus("Saving draft…");
    queryClient.setQueryData(draftKey, text);
    void saveDraft(conversationId, text).then(() => {
      if (edit === activeEdit.current) setDraftStatus(text ? "Draft saved" : "");
    }, (error: unknown) => {
      if (edit === activeEdit.current) {
        setDraftStatus("");
        setDraftError(String(error));
      }
    });
  }

  const canSend = Boolean(text.trim() && !live && !latest.isPending && selected?.option.selectable && !send.isPending);

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!canSend) return;
    stickToEnd.current = true;
    if (historyRegion.current) historyRegion.current.scrollTop = historyRegion.current.scrollHeight;
    setShowJump(false);
    send.mutate(text);
  }

  return (
    <section className="conv-pane" aria-label={title}>
      <header className="conv-heading">
        <MessageSquare aria-hidden />
        <h1>{title}</h1>
        {run && <button ref={activityButton} type="button" className="conv-quiet-button ml-auto" aria-expanded={Boolean(showingActivity)}
          onClick={() => showingActivity ? closeActivity() : setInspectedRun(run.id)}>
          <Activity aria-hidden /> Activity
        </button>}
      </header>
      <div className="conv-layout">
      <div className="conv-chat">
      <div ref={historyRegion} className="conv-history" role="region" aria-label="Conversation history"
        onScroll={(event) => {
          const region = event.currentTarget;
          if (!visible || !region.clientHeight) return;
          historyOffsets.set(conversationId, region.scrollTop);
          stickToEnd.current = region.scrollHeight - region.clientHeight - region.scrollTop < 32;
          setShowJump(!stickToEnd.current);
        }}>
        <div ref={historyContents} className="conv-reading">
        {history.isPending && <p role="status" className="text-sm text-muted-foreground">Loading messages…</p>}
        {history.error && <p role="alert" className="text-sm text-red-300">{String(history.error)}</p>}
        {history.data?.length === 0 && !run && <div className="conv-empty">
          <MessageSquare aria-hidden />
          <h2>Start a conversation</h2>
          <p>Choose a model and ask about this repository.</p>
        </div>}
        <ol className="conv-messages">
          {history.data?.map((message) => (
            <li key={message.revisionId} className={message.role === "user" ? "conv-user" : "conv-assistant"}>
              <p className="sr-only">{message.role === "user" ? "You" : "Assistant"}</p>
              {message.role === "user" ? <p>{message.text}</p> : <ConversationMarkdown>{message.text}</ConversationMarkdown>}
            </li>
          ))}
        </ol>
        {latest.error && <p role="alert" className="text-sm text-red-300">{String(latest.error)}</p>}
        {activity.error && <p role="alert" className="text-sm text-red-300">{String(activity.error)}</p>}
        {live && activity.output && <div className="conv-stream" aria-label="Streaming response">
          <ConversationMarkdown>{activity.output}</ConversationMarkdown>
        </div>}
        {run && <ConversationRequests activity={activity} conversationId={conversationId} runId={run.id} />}
        </div>
      </div>
      <div className="conv-composer-area">
        {showJump && <button type="button" className="conv-jump" onClick={() => {
          stickToEnd.current = true;
          if (historyRegion.current) historyRegion.current.scrollTop = historyRegion.current.scrollHeight;
          setShowJump(false);
        }}><ArrowDown aria-hidden /> Jump to latest</button>}
        {Boolean(activity.live && activity.inputs.length) && <button className="conv-request-link" type="button" onClick={() => {
          const request = historyRegion.current?.querySelector('[aria-label="Pending requests"]');
          request?.scrollIntoView({ block: "nearest" });
          request?.querySelector<HTMLElement>("button, input, select")?.focus({ preventScroll: true });
        }}>Waiting for input <ArrowUp aria-hidden /></button>}
      <form onSubmit={submit} className="conv-composer">
          <label htmlFor={`draft-${conversationId}`} className="sr-only">Message draft</label>
          <textarea id={`draft-${conversationId}`} placeholder="Ask about this repository…"
            value={text} onChange={(event) => editDraft(event.target.value)} disabled={draft.isPending || Boolean(draft.error) || send.isPending}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.nativeEvent.isComposing) {
                event.preventDefault();
                event.currentTarget.form?.requestSubmit();
              }
            }} />
          <div className="conv-composer-toolbar">
              <select aria-label="Provider" className={selectorClass} value={provider} onChange={(event) => {
                setProvider(event.target.value as Provider);
                setSelection(null);
                setEffort("");
              }}>
                <option value="codex">Codex</option>
                <option value="claude">Claude Code</option>
                <option value="antigravity">Antigravity</option>
              </select>
              <select aria-label="Thinking level" className={selectorClass} value={effort} onChange={(event) => setEffort(event.target.value)}>
                <option value="">Default effort</option>
                {selected?.efforts.map((level) => <option key={level} value={level}>{level}</option>)}
              </select>
              <select aria-label="Profile" className={selectorClass} value={mode} onChange={(event) => setMode(event.target.value as ProfileMode)}>
                <option value="read_only">Read-only</option>
                <option value="plan">Planning</option>
              </select>
              {live ? <StopConversation {...activity} /> :
                <Button type="submit" className="conv-send" disabled={!canSend}>
                  {send.isPending ? "Starting…" : "Send"} <ArrowUp className="size-3.5" aria-hidden />
                </Button>}
          </div>
          <div className="conv-model">
          <ModelPicker label="Model" options={models.data?.map(({ option }) => option) ?? []}
            selection={{ mode: "single", selected: selection, onChange: (selection) => { setSelection(selection); setEffort(""); } }} />
          </div>
          {models.isPending && <p role="status" className="text-xs text-muted-foreground">Checking CLI models…</p>}
          {models.error && <div className="flex flex-wrap items-center gap-2">
            <p role="alert" className="text-xs text-red-300">{String(models.error)}</p>
            <Button type="button" variant="outline" onClick={() => void models.refetch()}>Refresh models</Button>
          </div>}
          {draftStatus && <p role="status" className="text-xs text-muted-foreground">{draftStatus}</p>}
          {(draftError || draft.error) && <p role="alert" className="text-sm text-red-300">{draftError || String(draft.error)}</p>}
          {send.error && <p role="alert" className="text-sm text-red-300">{String(send.error)}</p>}
      </form>
      </div>
      </div>
      {showingActivity && <ConversationActivity key={run.id} conversationId={conversationId} run={run} projection={activity.projection} onClose={closeActivity} />}
      </div>
    </section>
  );
}
