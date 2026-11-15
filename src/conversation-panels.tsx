import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { IDockviewPanelProps } from "dockview-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { MessageRevision } from "@/bindings/MessageRevision";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { RunRecord } from "@/bindings/RunRecord";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { ConversationRun, isLiveRun } from "@/conversation-run";
import { openConversation } from "@/workspace-tabs";
import { useWorkspace } from "@/workspace-state";

const draftWrites = new Map<string, Promise<void>>();

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
  const queryClient = useQueryClient();
  const conversations = useQuery({
    queryKey: ["conversations", checkoutId],
    queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
    enabled: Boolean(checkoutId),
  });
  const start = useMutation({
    mutationFn: () => invoke<Conversation>("create_conversation", {
      checkoutId,
      title: `Conversation ${(conversations.data?.length ?? 0) + 1}`,
    }),
    onSuccess: (conversation) => {
      void queryClient.invalidateQueries({ queryKey: ["conversations", checkoutId] });
      openConversation(containerApi, conversation);
    },
  });
  const error = start.error ?? conversations.error;
  return (
    <section className="h-full overflow-auto p-6">
      <header className="mb-5 flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-semibold">Conversations</h1>
        <Button onClick={() => start.mutate()} disabled={conversations.isPending || start.isPending}>
          New conversation
        </Button>
      </header>
      {error && <p role="alert" className="mb-4 text-sm text-red-300">{String(error)}</p>}
      {conversations.isPending && <p role="status" className="text-sm text-muted-foreground">Loading conversations…</p>}
      <ul className="grid max-w-2xl gap-2">
        {conversations.data?.map((conversation) => (
          <li key={conversation.id}>
            <Button className="w-full justify-start" variant="outline" onClick={() => openConversation(containerApi, conversation)}>
              {conversation.title}
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}

type PanelParams = { conversationId: string; title: string };
type DesktopModel = { option: ModelOption; efforts: string[] };
type ProfileMode = "read_only" | "plan";
const selectorClass = "rounded-md border border-input bg-background px-2 py-2 text-sm";

export function ConversationPanel({ params }: IDockviewPanelProps<PanelParams>) {
  return <ConversationContent key={params.conversationId} {...params} />;
}

function ConversationContent({ conversationId, title }: PanelParams) {
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
  const selected = models.data?.find(({ option }) => option.selection.modelId === selection?.modelId && option.selection.provider === selection?.provider);
  const runKey = ["conversation", conversationId, "latest-run"];
  const latest = useQuery({
    queryKey: runKey,
    queryFn: () => invoke<RunRecord | null>("latest_conversation_run", { conversationId }),
    refetchInterval: (query) => isLiveRun(query.state.data) ? 250 : false,
  });
  const run = latest.data;
  const live = isLiveRun(run);
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

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!live && text.trim() && selected?.option.selectable && !send.isPending) send.mutate(text);
  }

  return (
    <section className="flex h-full min-h-0 flex-col" aria-label={title}>
      <header className="border-b border-border px-6 py-4">
        <h1 className="text-xl font-semibold">{title}</h1>
      </header>
      <div className="min-h-0 flex-1 overflow-auto px-6 py-4" role="region" aria-label="Conversation history">
        {history.isPending && <p role="status" className="text-sm text-muted-foreground">Loading messages…</p>}
        {history.error && <p role="alert" className="text-sm text-red-300">{String(history.error)}</p>}
        <ol className="mx-auto grid max-w-3xl gap-4">
          {history.data?.map((message) => (
            <li key={message.revisionId} className="rounded-md border border-border bg-card px-4 py-3">
              <p className="mb-1 text-xs font-medium text-muted-foreground">{message.role === "user" ? "You" : "Assistant"}</p>
              <p className="whitespace-pre-wrap break-words text-sm">{message.text}</p>
            </li>
          ))}
        </ol>
        {latest.error && <p role="alert" className="text-sm text-red-300">{String(latest.error)}</p>}
        {run && <ConversationRun key={run.id} conversationId={conversationId} run={run} />}
      </div>
      <form onSubmit={submit} className="max-h-[55%] overflow-auto border-t border-border p-4">
        <div className="mx-auto grid max-w-3xl gap-2">
          <div className="flex flex-wrap items-end gap-2">
            <label className="grid gap-1 text-xs">Provider
              <select className={selectorClass} value={provider} onChange={(event) => {
                setProvider(event.target.value as Provider);
                setSelection(null);
                setEffort("");
              }}>
                <option value="codex">Codex</option>
                <option value="claude">Claude Code</option>
                <option value="antigravity">Antigravity</option>
              </select>
            </label>
            <label className="grid gap-1 text-xs">Thinking level
              <select className={selectorClass} value={effort} onChange={(event) => setEffort(event.target.value)}>
                <option value="">Default</option>
                {selected?.efforts.map((level) => <option key={level} value={level}>{level}</option>)}
              </select>
            </label>
            <label className="grid gap-1 text-xs">Profile
              <select className={selectorClass} value={mode} onChange={(event) => setMode(event.target.value as ProfileMode)}>
                <option value="read_only">Read-only</option>
                <option value="plan">Planning</option>
              </select>
            </label>
          </div>
          <ModelPicker label="Model" options={models.data?.map(({ option }) => option) ?? []}
            selection={{ mode: "single", selected: selection, onChange: (selection) => { setSelection(selection); setEffort(""); } }} />
          {models.isPending && <p role="status" className="text-xs text-muted-foreground">Checking CLI models…</p>}
          {models.error && <div className="flex flex-wrap items-center gap-2">
            <p role="alert" className="text-xs text-red-300">{String(models.error)}</p>
            <Button type="button" variant="outline" onClick={() => void models.refetch()}>Refresh models</Button>
          </div>}
          <label htmlFor={`draft-${conversationId}`} className="text-sm font-medium">Message draft</label>
          <textarea id={`draft-${conversationId}`} className="min-h-24 rounded-md border border-input bg-background p-3 text-sm focus-visible:outline-ring"
            value={text} onChange={(event) => editDraft(event.target.value)} disabled={draft.isPending || Boolean(draft.error) || send.isPending} />
          <div className="flex flex-wrap items-center justify-between gap-2">
            <Button type="submit" disabled={!text.trim() || live || latest.isPending || !selected?.option.selectable || send.isPending}>
              {send.isPending ? "Starting…" : "Send"}
            </Button>
            {draftStatus && <p role="status" className="text-xs text-muted-foreground">{draftStatus}</p>}
          </div>
          {(draftError || draft.error) && <p role="alert" className="text-sm text-red-300">{draftError || String(draft.error)}</p>}
          {send.error && <p role="alert" className="text-sm text-red-300">{String(send.error)}</p>}
        </div>
      </form>
    </section>
  );
}
