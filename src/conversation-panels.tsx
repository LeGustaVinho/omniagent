import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { IDockviewPanelProps } from "dockview-react";
import { useRef, useState } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { MessageRevision } from "@/bindings/MessageRevision";
import { Button } from "@/components/ui/button";
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
  });
  const history = useQuery({
    queryKey: ["conversation", conversationId, "messages"],
    queryFn: () => invoke<MessageRevision[]>("conversation_messages", { conversationId }),
  });
  const activeEdit = useRef(0);
  const [draftError, setDraftError] = useState("");
  const [draftStatus, setDraftStatus] = useState("");
  const text = draft.data ?? "";

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
      </div>
      <div className="border-t border-border p-4">
        <div className="mx-auto grid max-w-3xl gap-2">
          <label htmlFor={`draft-${conversationId}`} className="text-sm font-medium">Message draft</label>
          <textarea id={`draft-${conversationId}`} className="min-h-24 rounded-md border border-input bg-background p-3 text-sm focus-visible:outline-ring"
            value={text} onChange={(event) => editDraft(event.target.value)} disabled={draft.isPending || Boolean(draft.error)} />
          {draftStatus && <p role="status" className="text-xs text-muted-foreground">{draftStatus}</p>}
          {(draftError || draft.error) && <p role="alert" className="text-sm text-red-300">{draftError || String(draft.error)}</p>}
        </div>
      </div>
    </section>
  );
}
