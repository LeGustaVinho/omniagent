import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Route } from "lucide-react";
import { Dialog, Popover } from "radix-ui";
import { useState } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { ConversationPipelineChoice } from "@/bindings/ConversationPipelineChoice";
import type { ConversationPipelineOption } from "@/bindings/ConversationPipelineOption";
import type { Job } from "@/bindings/Job";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { GoalCheckpointSummary, GoalExecutionScope, type GoalTransitionReview, type GoalTransitionScope } from "@/conversation-goal";
import { ConversationPipelineOptions, conversationPipelineKey, directConversationChoice } from "@/conversation-pipeline-picker";

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
  reason: "profile_changed" | "checkout_changed" | "session_unavailable" | "policy_changed";
  targetCheckout?: ProjectCheckout;
  goalTransition?: {
    recordedGoal: GoalTransitionReview;
    nextTurnScope: GoalTransitionScope;
    sameTabAvailable: boolean;
  } | null;
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

type Outcome = { conversation: Conversation; job?: Job };

export function submitConversationContinuation(request: ContinuationScope & {
  expectedContextId: string; text: string; referenceIds?: string[]; attachmentPreviewId?: string; targetCheckoutId?: string;
}) {
  return invoke<ConversationContinuationReview | { status: "queued"; job: Job }>("submit_conversation_continuation", request);
}

export function confirmConversationContinuation(request: ContinuationScope & {
  previewId: string; choice: Choice; text: string; pipeline?: ConversationPipelineChoice;
}) {
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
  checkout_changed: "The next turn will use a separate checkout and a fresh provider session.",
  session_unavailable: "There is no verified provider session for the saved history.",
  policy_changed: "The previous provider session uses a different subagent policy.",
};

export function ConversationContinuationDialog({ review, pipelines, pipelineRestriction, pending, error, onConfirm, onClose }: {
  review: ConversationContinuationReview;
  pipelines?: { data?: ConversationPipelineOption[]; isPending: boolean; error: unknown; refetch: () => unknown };
  pipelineRestriction?: string;
  pending: boolean;
  error?: unknown;
  onConfirm: (choice: Choice, pipeline?: ConversationPipelineChoice) => void;
  onClose: () => void;
}) {
  const goal = review.goalTransition;
  const sameTabAvailable = goal?.sameTabAvailable !== false;
  const [choice, setChoice] = useState(() => !sameTabAvailable && review.settings.effective === "same_tab"
    ? "fork_with_context" : review.settings.effective);
  const [selected, setSelected] = useState<string>();
  const options = pipelines?.data ?? [directConversationChoice];
  const fresh = choice === "fresh";
  const pipeline = fresh ? options.find((option) => conversationPipelineKey(option.pipeline) === selected)?.pipeline : undefined;
  const draftOnly = pipeline !== undefined && pipeline.kind !== "direct";
  const canConfirm = !pending && (sameTabAvailable || choice !== "same_tab") &&
    (!fresh || (pipeline !== undefined && (!pipelineRestriction || !draftOnly)));
  return <Dialog.Root open onOpenChange={(open) => { if (!open && !pending) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog max-h-[90vh] overflow-auto">
        <Dialog.Title>Choose how to continue</Dialog.Title>
        <Dialog.Description>{reasons[review.reason]}</Dialog.Description>
        <form className="conv-dialog-form" onSubmit={(event) => {
          event.preventDefault();
          if (!canConfirm) return;
          if (fresh) onConfirm(choice, pipeline);
          else onConfirm(choice);
        }}>
          {!goal && review.targetCheckout && <p className="break-all">Continue in <code>{review.targetCheckout.checkoutRoot}</code></p>}
          {goal && <>
            <section aria-label="Recorded goal" className="grid gap-1">
              <h3 className="text-sm font-medium">Goal in this conversation</h3>
              <GoalCheckpointSummary review={goal.recordedGoal} />
            </section>
            <section aria-label="Next turn scope" className="grid gap-1">
              <h3 className="text-sm font-medium">Next turn scope</h3>
              <GoalExecutionScope scope={goal.nextTurnScope} />
            </section>
            {!sameTabAvailable && <p className="text-xs">The recorded goal owns this conversation's checkout. Choose a new conversation to send to another checkout.</p>}
            <p className="text-xs">{choice === "same_tab" ? "The goal keeps its recorded checkpoint and scope." : "Start a separate goal in the new conversation."}</p>
          </>}
          <fieldset className="grid gap-1" disabled={pending}>
            <legend className="text-xs text-muted-foreground">Continuation method</legend>
            {choices.map((entry) => <label className="!flex items-center gap-2" key={entry}>
              <input type="radio" name="continuation-method" checked={choice === entry} value={entry}
                disabled={entry === "same_tab" && !sameTabAvailable} onChange={() => setChoice(entry)} />
              {choiceLabels[entry]}
            </label>)}
          </fieldset>
          {fresh
            ? <section aria-label="Choose a conversation pipeline" className="grid gap-2">
              <h3 className="text-sm font-medium">Conversation pipeline</h3>
              <ConversationPipelineOptions options={options} selected={selected} onSelect={setSelected} disabled={pending}
                directOnly={Boolean(pipelineRestriction)} />
              {pipelines?.isPending && <p role="status" className="text-xs text-muted-foreground">Loading published pipelines…</p>}
              {Boolean(pipelines?.error) && <div className="conversation-pipeline-feedback">
                <p role="alert">{String(pipelines?.error)}</p>
                <button type="button" className="conv-quiet-button" onClick={() => void pipelines?.refetch()}>Refresh pipelines</button>
              </div>}
              {pipelineRestriction && <p className="text-xs text-muted-foreground">{pipelineRestriction}</p>}
              {draftOnly && <p className="text-xs text-muted-foreground">Review pipeline inputs in the new conversation before sending.</p>}
            </section>
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
            <button type="submit" className="conv-primary-button" disabled={!canConfirm}>
              {draftOnly ? pending ? "Opening…" : "Open draft" : pending ? "Queueing…" : "Continue and send"}
            </button>
          </footer>
        </form>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}

const settingsKey = ["conversation-continuation-settings"];

export function ConversationContinuationSettings({ conversationId, targetCheckoutId, onCheckout }: {
  conversationId: string; targetCheckoutId?: string; onCheckout?: (checkoutId: string | undefined) => void;
}) {
  const [open, setOpen] = useState(false);
  const client = useQueryClient();
  const key = [...settingsKey, conversationId];
  const checkouts = useQuery({ queryKey: ["conversation-continuation-checkouts", conversationId],
    enabled: open && Boolean(onCheckout),
    queryFn: () => invoke<ProjectCheckout[]>("conversation_continuation_checkouts", { conversationId }),
  });
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
  const error = settings.error ?? save.error ?? checkouts.error;
  return <Popover.Root open={open} onOpenChange={setOpen}>
    <Popover.Trigger asChild>
      <button type="button" className="conv-quiet-button"><Route size={15} aria-hidden /> Continuation</button>
    </Popover.Trigger>
    <Popover.Portal>
      <Popover.Content align="end" sideOffset={6} className="work-project-popover conv-dialog-form" aria-label="Continuation settings">
        {onCheckout && <label>Checkout for next send
          <select value={targetCheckoutId ?? ""} disabled={checkouts.isPending} onChange={(event) => onCheckout(event.currentTarget.value || undefined)}>
            <option value="">Current checkout</option>
            {checkouts.data?.slice(1).map((checkout) => <option key={checkout.checkoutId} value={checkout.checkoutId}>{checkout.checkoutRoot}</option>)}
          </select>
        </label>}
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
