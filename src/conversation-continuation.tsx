import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Dialog, Popover } from "radix-ui";
import { useState } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";

type Choice = "same_tab" | "fork_with_context" | "fresh";
type Settings = { projectDefault: Choice; conversationOverride: Choice | null; effective: Choice };
type ContinuationScope = {
  conversationId: string;
  selection: ModelSelection;
  effort: string | null;
  mode: "read_only" | "plan";
};

export type ConversationContinuationReview = {
  status: "review_required";
  previewId: string;
  contextId: string;
  reason: "profile_changed" | "session_unavailable" | "policy_changed";
  settings: Settings;
  seed: {
    revisionId: string;
    role: "user" | "assistant";
    text: string;
    attachments: {
      name: string;
      sha256: string;
      size: number;
      delivery: { kind: "native" | "reference" } | { kind: "excerpt"; start: number; end: number };
      excerpt: string | null;
      referenceTarget: string | null;
    }[];
  }[];
};

type Outcome = { conversation: Conversation; job: Job };

export function submitConversationContinuation(request: ContinuationScope & {
  expectedContextId: string; text: string; referenceIds?: string[]; attachmentPreviewId?: string; targetCheckoutId?: string;
}) {
  return invoke<ConversationContinuationReview | { status: "queued"; job: Job }>("submit_conversation_continuation", request);
}

export function confirmConversationContinuation(request: ContinuationScope & { previewId: string; choice: Choice; text: string }) {
  return invoke<Outcome>("confirm_conversation_continuation", request);
}

const choiceLabels: Record<Choice, string> = {
  same_tab: "Continue in this tab",
  fork_with_context: "Fork with context",
  fresh: "Start a fresh conversation",
};
const choices = Object.keys(choiceLabels) as Choice[];
const reasons: Record<ConversationContinuationReview["reason"], string> = {
  profile_changed: "The selected execution profile differs from the previous session.",
  session_unavailable: "There is no verified provider session for the saved history.",
  policy_changed: "The previous provider session uses a different subagent policy.",
};

export function ConversationContinuationDialog({ review, pending, error, onConfirm, onClose }: {
  review: ConversationContinuationReview;
  pending: boolean;
  error?: unknown;
  onConfirm: (choice: Choice) => void;
  onClose: () => void;
}) {
  const [choice, setChoice] = useState(review.settings.effective);
  return <Dialog.Root open onOpenChange={(open) => { if (!open && !pending) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog">
        <Dialog.Title>Choose how to continue</Dialog.Title>
        <Dialog.Description>{reasons[review.reason]}</Dialog.Description>
        <form className="conv-dialog-form" onSubmit={(event) => {
          event.preventDefault();
          if (!pending) onConfirm(choice);
        }}>
          <fieldset className="grid gap-1" disabled={pending}>
            <legend className="text-xs text-muted-foreground">Continuation method</legend>
            {choices.map((entry) => <label className="!flex items-center gap-2" key={entry}>
              <input type="radio" name="continuation-method" checked={choice === entry} value={entry} onChange={() => setChoice(entry)} />
              {choiceLabels[entry]}
            </label>)}
          </fieldset>
          {choice === "fresh"
            ? <p className="text-xs text-muted-foreground">The new conversation starts with this draft.</p>
            : <section aria-label="History seed" className="grid gap-2">
              <h3 className="text-sm font-medium">History sent to the new session</h3>
              {review.seed.length === 0 ? <p className="text-xs text-muted-foreground">This conversation has no saved messages.</p>
                : <ol className="grid max-h-64 gap-2 overflow-auto rounded-md border border-border p-2">
                  {review.seed.map(({ revisionId, role, text, attachments }) => <li className="min-w-0" key={revisionId}>
                    <strong className="text-xs">{role === "user" ? "You" : "Assistant"}</strong>
                    <pre className="whitespace-pre-wrap break-words text-xs">{text}</pre>
                    {attachments.length > 0 && <ul className="pl-3 text-xs text-muted-foreground">
                      {attachments.map((file, index) => <li key={`${file.sha256}:${index}`}>
                        <span>{file.name} · {file.delivery.kind} · {file.size.toLocaleString()} bytes</span>
                        {file.delivery.kind === "excerpt" && <div>
                          <span>Bytes {file.delivery.start}–{file.delivery.end}</span>
                          <pre className="whitespace-pre-wrap break-words">{file.excerpt}</pre>
                        </div>}
                        {file.referenceTarget && <code className="block break-all">{file.referenceTarget}</code>}
                      </li>)}
                    </ul>}
                  </li>)}
                </ol>}
            </section>}
          {error != null && <p className="conv-alert" role="alert">{String(error)}</p>}
          <footer>
            <Dialog.Close asChild><button type="button" className="conv-quiet-button" disabled={pending}>Cancel</button></Dialog.Close>
            <button type="submit" className="conv-primary-button" disabled={pending}>{pending ? "Queueing…" : "Continue and send"}</button>
          </footer>
        </form>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}

const settingsKey = ["conversation-continuation-settings"];

export function ConversationContinuationSettings({ conversationId }: { conversationId: string }) {
  const [open, setOpen] = useState(false);
  const client = useQueryClient();
  const key = [...settingsKey, conversationId];
  const settings = useQuery({
    queryKey: key,
    queryFn: () => invoke<Settings>("conversation_continuation_settings", { conversationId }),
    enabled: open,
  });
  const save = useMutation({
    mutationFn: ({ level, choice }: { level: "project" | "conversation"; choice: Choice | null }) =>
      invoke<Settings>("set_conversation_continuation_policy", { conversationId, level, choice }),
    onSuccess(current) {
      client.setQueryData(key, current);
      void client.invalidateQueries({ queryKey: settingsKey });
    },
  });
  const error = settings.error ?? save.error;
  return <Popover.Root open={open} onOpenChange={setOpen}>
    <Popover.Trigger asChild><button type="button" className="conv-quiet-button">Continuation</button></Popover.Trigger>
    <Popover.Portal>
      <Popover.Content align="end" sideOffset={6} className="work-project-popover conv-dialog-form" aria-label="Continuation settings">
        {settings.isPending && <p role="status">Loading continuation settings…</p>}
        {settings.data && <>
          <label>Project default
            <select className="conv-select" value={settings.data.projectDefault} disabled={save.isPending}
              onChange={(event) => save.mutate({ level: "project", choice: event.currentTarget.value as Choice })}>
              {choices.map((choice) => <option key={choice} value={choice}>{choiceLabels[choice]}</option>)}
            </select>
          </label>
          <label>Conversation override
            <select className="conv-select" value={settings.data.conversationOverride ?? ""} disabled={save.isPending}
              onChange={(event) => save.mutate({ level: "conversation", choice: (event.currentTarget.value as Choice) || null })}>
              <option value="">Use project default</option>
              {choices.map((choice) => <option key={choice} value={choice}>{choiceLabels[choice]}</option>)}
            </select>
          </label>
        </>}
        {error && <p className="conv-alert" role="alert">{String(error)}</p>}
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}
