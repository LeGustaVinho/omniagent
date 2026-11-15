import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { History } from "lucide-react";
import { Dialog } from "radix-ui";
import { useState } from "react";
import type { ConversationHistory } from "@/bindings/ConversationHistory";
import type { QueueHistoryPolicy } from "@/bindings/QueueHistoryPolicy";
import type { QueueHistorySettings } from "@/bindings/QueueHistorySettings";
import { Button } from "@/components/ui/button";
import { SettingRow } from "@/settings-controls";

const policies = {
  review: "Pause for review",
  recorded: "Use recorded context",
  cancel: "Cancel pending jobs",
} satisfies Record<QueueHistoryPolicy, string>;

function policyOptions() {
  return Object.entries(policies).map(([value, label]) => <option key={value} value={value}>{label}</option>);
}

export function ProjectQueueHistoryPolicy({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const key = ["project-queue-history-policy", projectId];
  const saved = useQuery({
    queryKey: key,
    queryFn: () => invoke<QueueHistoryPolicy>("project_queue_history_policy", { projectId }),
  });
  const save = useMutation({
    mutationFn: (policy: QueueHistoryPolicy) => invoke<void>("set_project_queue_history_policy", { projectId, policy }),
    onSuccess: (_, policy) => {
      client.setQueryData(key, policy);
      return client.invalidateQueries({ queryKey: ["conversation-queue-history-policy"] });
    },
  });

  return <>
    <SettingRow id="queued-context" label="Queued work after context changes"
      description="Applies when you rewind or clear a conversation that uses the project default.">
      <select id="queued-context" aria-describedby="queued-context-description"
        value={save.isPending ? save.variables : saved.data ?? ""}
        disabled={!saved.data || save.isPending}
        onChange={(event) => save.mutate(event.currentTarget.value as QueueHistoryPolicy)}>
        {!saved.data && <option value="">Loading policy…</option>}
        {policyOptions()}
      </select>
    </SettingRow>
    {(saved.error || save.error) && <p role="alert" className="settings-result settings-result-error">{String(saved.error || save.error)}</p>}
  </>;
}

export function ConversationHistoryControls({ conversationId, history, disabled }: {
  conversationId: string;
  history?: ConversationHistory;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState("");
  const client = useQueryClient();
  const key = ["conversation-queue-history-policy", conversationId];
  const policy = useQuery({
    queryKey: key,
    queryFn: () => invoke<QueueHistorySettings>("conversation_queue_history_policy", { conversationId }),
    enabled: open,
  });
  const save = useMutation({
    mutationFn: (policy: QueueHistoryPolicy | null) =>
      invoke<QueueHistorySettings>("set_conversation_queue_history_policy", { conversationId, policy }),
    onSuccess: (settings) => client.setQueryData(key, settings),
  });
  const change = useMutation({
    mutationFn: ({ baseId, throughRevision }: { baseId: string; throughRevision: string | null }) =>
      invoke<string>("rewind_conversation_history", { conversationId, baseId, throughRevision }),
    onSuccess: async () => {
      await Promise.all([
        client.invalidateQueries({ queryKey: ["conversation", conversationId] }),
        client.invalidateQueries({ queryKey: ["project-job-queue"] }),
      ]);
      setOpen(false);
    },
    onError: () => client.invalidateQueries({ queryKey: ["conversation", conversationId, "messages"] }),
  });
  const lock = save.isPending || change.isPending;
  const selectedMessage = history?.messages.find((message) => message.revisionId === selected);

  return <Dialog.Root open={open} onOpenChange={(next) => {
    if (lock) return;
    if (next) {
      setSelected("");
      save.reset();
      change.reset();
    }
    setOpen(next);
  }}>
    <Dialog.Trigger type="button" className="conv-quiet-button" title="Conversation history controls">
      <History aria-hidden /> History
    </Dialog.Trigger>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog">
        <Dialog.Title>Conversation history</Dialog.Title>
        <Dialog.Description>Clear the transcript or keep history through a selected message.</Dialog.Description>
        <div className="conv-dialog-form">
          <label>
            Queued work after context changes
            <select value={(save.isPending ? save.variables : policy.data?.conversationOverride) ?? ""}
              disabled={!policy.data || lock}
              onChange={(event) => save.mutate(event.currentTarget.value ? event.currentTarget.value as QueueHistoryPolicy : null)}>
              <option value="">{policy.data ? `Use project default · ${policies[policy.data.projectDefault]}` : "Loading policy…"}</option>
              {policyOptions()}
            </select>
          </label>
          <label>
            Keep messages through
            <select value={selected} disabled={disabled || lock || !history?.messages.length}
              onChange={(event) => setSelected(event.currentTarget.value)}>
              <option value="">Clear all messages</option>
              {selected && !selectedMessage && <option value={selected} disabled>Message no longer available</option>}
              {history?.messages.map((message) => <option value={message.revisionId} key={message.revisionId}>
                {message.role === "user" ? "You" : "Assistant"} · {message.text.trim().replace(/\s+/g, " ").slice(0, 100)}
              </option>)}
            </select>
          </label>
          {disabled && <p role="status">Wait for the active turn to stop before changing history.</p>}
          {(policy.error || save.error || change.error) && <p className="conv-alert" role="alert">{String(policy.error || save.error || change.error)}</p>}
          <footer>
            <Dialog.Close asChild><Button type="button" variant="outline" disabled={lock}>Close</Button></Dialog.Close>
            <Button type="button"
              disabled={!history || !policy.data || disabled || lock || Boolean(selected && !selectedMessage)}
              onClick={() => {
                if (history) change.mutate({ baseId: history.contextId, throughRevision: selectedMessage?.revisionId ?? null });
              }}>
              {selectedMessage ? "Rewind to message" : "Clear conversation"}
            </Button>
          </footer>
        </div>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
