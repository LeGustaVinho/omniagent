import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { GitPullRequest, MessageSquare, Workflow, X } from "lucide-react";
import { Dialog } from "radix-ui";
import { useId, useRef, useState } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { ConversationPipeline as ChatPipeline } from "@/bindings/ConversationPipeline";
import type { ConversationPipelineChoice } from "@/bindings/ConversationPipelineChoice";
import type { ConversationPipelineOption as PipelineOption } from "@/bindings/ConversationPipelineOption";
import { Button } from "@/components/ui/button";
import { conversationLibraryKey } from "@/conversation-library";
import "./conversation-pipeline.css";

export const directConversationPipeline = { kind: "direct" } as const;
export const repositoryDeliveryChoice = { name: "Repository delivery", pipeline: { kind: "repository_delivery" } } as const;
export const directConversationChoice = { name: "Direct conversation", pipeline: directConversationPipeline } as const;

export function conversationPipelineKey(pipeline: ConversationPipelineChoice) {
  return pipeline.kind === "published" ? JSON.stringify([pipeline.workflow.id, pipeline.workflow.revision]) : pipeline.kind;
}

export function ConversationPipelineOptions({ options, selected, onSelect, disabled, directOnly }: {
  options: readonly PipelineOption[];
  selected?: string;
  onSelect: (key: string) => void;
  disabled?: boolean;
  directOnly?: boolean;
}) {
  const name = useId();
  return <fieldset className="conversation-pipeline-options" disabled={disabled}>
    <legend className="sr-only">Pipeline</legend>
    {options.map(({ pipeline, name: label }) => {
      const key = conversationPipelineKey(pipeline);
      const Icon = pipelineIcons[pipeline.kind];
      return <label className="conversation-pipeline-option" key={key}>
        <input type="radio" name={name} value={key} checked={selected === key}
          disabled={directOnly && pipeline.kind !== "direct"} onChange={() => onSelect(key)} />
        <Icon size={18} aria-hidden />
        <span><strong>{label}</strong><small>{pipelineDescriptions[pipeline.kind]}</small></span>
        {pipeline.kind === "published" && <code title={pipeline.workflow.revision}>{pipeline.workflow.revision.slice(0, 10)}</code>}
      </label>;
    })}
  </fieldset>;
}

const pipelineIcons = { direct: MessageSquare, repository_delivery: GitPullRequest, published: Workflow };
const pipelineDescriptions = {
  direct: "Talk directly with an agent in this worktree.",
  repository_delivery: "Start the repository delivery workflow with a saved Markdown revision.",
  published: "Run this published revision for each message.",
};

export function ConversationPipelineLabel({ pipeline }: { pipeline: ChatPipeline }) {
  const Icon = pipelineIcons[pipeline.kind];
  const name = pipeline.kind === "published" ? pipeline.name
    : pipeline.kind === "repository_delivery" ? repositoryDeliveryChoice.name : directConversationChoice.name;
  const revision = pipeline.kind === "direct" ? undefined : pipeline.workflow.revision;
  return <span className="conversation-pipeline-label" title={revision ? `${name} · ${revision}` : name} aria-label={revision ? `${name}, revision ${revision}` : name}>
    <Icon size={14} aria-hidden /> <span>{name}</span>{revision && <code>{revision.slice(0, 10)}</code>}
  </span>;
}

export function ConversationPipelinePicker({ options, loading, error, onRetry, onCreate, pending, onClose, opener }: {
  options: readonly PipelineOption[];
  loading?: boolean;
  error?: unknown;
  onRetry?: () => void;
  onCreate: (pipeline: ConversationPipelineChoice) => void;
  pending?: boolean;
  onClose: () => void;
  opener?: HTMLElement | null;
}) {
  const [selected, setSelected] = useState("");
  const choice = options.find(({ pipeline }) => conversationPipelineKey(pipeline) === selected);
  const formId = useId();
  return <Dialog.Root open onOpenChange={(open) => { if (!open && !pending) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog conv-dialog-layout conversation-pipeline-dialog"
        onCloseAutoFocus={(event) => {
          if (opener?.isConnected) { event.preventDefault(); opener.focus({ preventScroll: true }); }
        }}>
        <header className="conv-dialog-header">
          <span className="conv-dialog-mark"><Workflow size={20} aria-hidden /></span>
          <div>
            <Dialog.Title>Choose a conversation pipeline</Dialog.Title>
            <Dialog.Description>The pipeline stays fixed for this conversation.</Dialog.Description>
          </div>
          <Dialog.Close asChild><Button type="button" variant="ghost" className="ui-icon-button" aria-label="Close pipeline picker" disabled={pending}><X size={17} aria-hidden /></Button></Dialog.Close>
        </header>
        <div className="conv-dialog-body">
          <form id={formId} onSubmit={(event) => { event.preventDefault(); if (choice && !pending) onCreate(choice.pipeline); }}>
            <ConversationPipelineOptions options={options} selected={selected} onSelect={setSelected} disabled={pending} />
          </form>
          {loading && <p className="conversation-pipeline-note" role="status">Loading published pipelines…</p>}
          {Boolean(error) && <div className="conversation-pipeline-feedback">
            <p role="alert">{String(error)}</p>
            {onRetry && <Button type="button" variant="outline" onClick={onRetry}>Refresh pipelines</Button>}
          </div>}
        </div>
        <footer className="conv-dialog-actions">
          <Dialog.Close asChild><Button type="button" variant="outline" disabled={pending}>Cancel</Button></Dialog.Close>
          <Button type="submit" form={formId} disabled={!choice || pending}>{pending ? "Creating…" : "Create conversation"}</Button>
        </footer>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}

export function useNewConversation(checkoutId: string, onOpen: (conversation: Conversation) => void) {
  const client = useQueryClient();
  const key = ["conversations", checkoutId];
  const [open, setOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const options = useQuery({
    queryKey: ["conversation-pipeline-options", checkoutId],
    queryFn: () => invoke<PipelineOption[]>("conversation_pipeline_options", { checkoutId }),
    enabled: open,
  });
  const start = useMutation({
    mutationKey: ["new-conversation", checkoutId],
    scope: { id: "new-conversation:" + checkoutId },
    mutationFn: async (pipeline: ConversationPipelineChoice) => {
      const list = await client.fetchQuery({ queryKey: key, queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }) });
      return invoke<Conversation>("create_conversation", { checkoutId, title: `Conversation ${list.length + 1}`, pipeline });
    },
    onSuccess: (conversation) => {
      setOpen(false);
      void client.invalidateQueries({ queryKey: key });
      void client.invalidateQueries({ queryKey: conversationLibraryKey });
      onOpen(conversation);
    },
  });
  return {
    isPending: start.isPending,
    choose: () => {
      opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      start.reset();
      setOpen(true);
    },
    dialog: open && <ConversationPipelinePicker options={options.data ?? [directConversationChoice]}
      loading={options.isPending} error={start.error ?? options.error} onRetry={options.error ? () => void options.refetch() : undefined}
      pending={start.isPending} onCreate={(pipeline) => start.mutate(pipeline)} onClose={() => setOpen(false)} opener={opener.current} />,
  };
}
