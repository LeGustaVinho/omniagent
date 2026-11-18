import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowUpRight, CircleAlert, CircleCheck, Clock3, FileCheck, ListChecks, Play, RotateCcw, SkipForward, X, XCircle, Zap } from "lucide-react";
import { Dialog } from "radix-ui";
import { useRef, useState } from "react";
import type { InstructionDelivery } from "@/bindings/InstructionDelivery";
import type { MessageRevision } from "@/bindings/MessageRevision";
import type { PendingInstruction } from "@/bindings/PendingInstruction";
import { Button } from "@/components/ui/button";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { conversationSkillMessage } from "@/conversation-customization-picker";
import { queueBlockerReason } from "@/queue-model";
import "./conversation-instructions.css";

const deliveryLabels: Record<InstructionDelivery, string> = {
  queued: "Queued instruction",
  paused: "Paused instruction",
  delivering: "Delivering instruction",
  needs_review: "Delivery needs review",
};

export type InstructionDeliveryEvidence = {
  instructionId: string;
  revisionId: string;
  runId: string;
  route: "steering" | "continuation" | "standalone";
  state: "claimed" | "prepared" | "accepted" | "rejected" | "unconfirmed" | "undelivered" | "discarded";
  canResolve: boolean;
};

const deliveryState = {
  claimed: { label: "Awaiting dispatch", icon: Clock3, tone: "neutral" },
  prepared: { label: "Dispatch prepared", icon: FileCheck, tone: "neutral" },
  accepted: { label: "Delivered", icon: CircleCheck, tone: "success" },
  rejected: { label: "Returned to queue", icon: RotateCcw, tone: "neutral" },
  unconfirmed: { label: "Delivery needs review", icon: CircleAlert, tone: "warning" },
  undelivered: { label: "Confirmed undelivered", icon: RotateCcw, tone: "neutral" },
  discarded: { label: "Discarded after review", icon: XCircle, tone: "neutral" },
} satisfies Record<InstructionDeliveryEvidence["state"], { label: string; icon: typeof Clock3; tone: string }>;

const deliveryRoute = {
  steering: { label: "Active turn", icon: Zap },
  continuation: { label: "Next turn", icon: SkipForward },
  standalone: { label: "Separate run", icon: Play },
} satisfies Record<InstructionDeliveryEvidence["route"], { label: string; icon: typeof Play }>;

export function useConversationInstructions(conversationId: string, visible: boolean) {
  const queryClient = useQueryClient();
  const key = ["conversation", conversationId, "instructions"];
  const deliveryKey = ["conversation", conversationId, "instruction-deliveries"];
  const query = useQuery({
    queryKey: key,
    queryFn: () => invoke<PendingInstruction[]>("pending_conversation_instructions", { conversationId }),
    enabled: visible,
    refetchInterval: visible ? 500 : false,
  });
  const deliveries = useQuery({ queryKey: deliveryKey,
    queryFn: () => invoke<InstructionDeliveryEvidence[]>("conversation_instruction_deliveries", { conversationId }),
    enabled: visible, refetchInterval: visible ? 900 : false,
  });
  const refresh = () => Promise.all([
    queryClient.invalidateQueries({ queryKey: key }),
    queryClient.invalidateQueries({ queryKey: deliveryKey }),
    queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-job"] }),
    queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "pipeline-turns"] }),
    queryClient.invalidateQueries({ queryKey: ["project-job-queue"] }),
  ]);
  return { data: query.data ?? [], deliveries: deliveries.data ?? [], error: query.error, refresh,
    deliveryPending: deliveries.isPending, deliveryError: deliveries.error, reloadDeliveries: deliveries.refetch };
}

function InstructionRow({ item, conversationId, refresh }: {
  item: PendingInstruction;
  conversationId: string;
  refresh: () => Promise<unknown>;
}) {
  const [edit, setEdit] = useState<{ text: string; revisionId: string }>();
  const change = useMutation({
    mutationFn: ({ kind, revisionId, text }: { kind: "edit" | "remove"; revisionId: string; text?: string }) =>
      invoke(kind === "edit" ? "revise_pending_instruction" : "remove_pending_instruction", {
        conversationId, instructionId: item.id, revisionId, text,
      }),
    onSuccess: () => setEdit(undefined),
    onSettled: refresh,
  });
  const canEdit = item.editable && !change.isPending;
  const reason = item.blockers[0] ? queueBlockerReason(item.blockers[0]) : undefined;
  return <li className="conv-queued conv-pending-instruction">
    <strong>{deliveryLabels[item.delivery]}</strong>
    <p className="text-xs text-muted-foreground">{[item.model ?? item.provider, item.mode.replaceAll("_", " "),
      item.recordedContext ? "Recorded context" : null].filter(Boolean).join(" · ")}</p>
    {edit && item.editable ? <form className="grid gap-1.5" onSubmit={(event) => {
      event.preventDefault();
      if (edit.text.trim() && canEdit) change.mutate({ kind: "edit", ...edit });
    }}>
      <textarea aria-label="Pending instruction text" className="min-h-14 rounded-md border border-input bg-background p-2"
        disabled={!canEdit} value={edit.text} onChange={(event) => setEdit({ ...edit, text: event.target.value })} />
      <div className="flex gap-2">
        <Button type="submit" disabled={!canEdit || !edit.text.trim()}>Save instruction</Button>
        <Button type="button" variant="outline" disabled={change.isPending} onClick={() => { change.reset(); setEdit(undefined); }}>Cancel edit</Button>
      </div>
    </form> : <p className="whitespace-pre-wrap break-words">{item.text}</p>}
    {item.attachments.length > 0 && <ul className="list-disc pl-5 text-xs" aria-label="Instruction attachments">
      {item.attachments.map((name, index) => <li key={index}>{name}</li>)}
    </ul>}
    {reason && <p role="status" className="text-xs text-muted-foreground">{reason}</p>}
    {item.editable && !edit && <div className="flex flex-wrap gap-2">
      <button type="button" className="conv-quiet-button" disabled={!canEdit}
        onClick={() => { change.reset(); setEdit({ text: item.text, revisionId: item.revisionId }); }}>Edit instruction</button>
      <button type="button" className="conv-quiet-button" disabled={!canEdit}
        onClick={() => change.mutate({ kind: "remove", revisionId: item.revisionId })}>Remove instruction</button>
    </div>}
    {change.error && <p role="alert" className="conv-alert">{String(change.error)}</p>}
  </li>;
}

function DeliveryEvidence({ item, instruction, conversationId, openRun, refresh }: {
  item: InstructionDeliveryEvidence;
  instruction?: string;
  conversationId: string;
  openRun: (runId: string) => void;
  refresh: () => Promise<unknown>;
}) {
  const { instructionId, revisionId, runId, state } = item;
  const runLink = useRef<HTMLButtonElement>(null);
  const resolve = useMutation({
    mutationFn: (confirmedUndelivered: boolean) => invoke("resolve_uncertain_steering", {
      conversationId, instructionId, revisionId, runId, confirmedUndelivered,
    }),
    onSuccess: () => runLink.current?.focus({ preventScroll: true }),
    onSettled: refresh,
  });
  const { icon: StatusIcon, label: status, tone } = deliveryState[state];
  const { icon: RouteIcon, label: route } = deliveryRoute[item.route];
  return <li className="instruction-delivery-row">
    <div className="instruction-delivery-details">
      {instruction ? <p className="instruction-delivery-prompt" title={instruction}>{instruction}</p>
        : <p className="instruction-delivery-reference" title={revisionId}>Instruction <code>{revisionId.slice(0, 8)}</code></p>}
      <div className="instruction-delivery-facts">
        <span className="instruction-delivery-status" data-tone={tone}><StatusIcon size={14} aria-hidden /> {status}</span>
        <span><RouteIcon size={14} aria-hidden /> {route}</span>
      </div>
    </div>
    <Button ref={runLink} type="button" variant="ghost" className="instruction-delivery-link" onClick={() => openRun(runId)}>
      {state === "accepted" ? "Open receiving turn" : "Open delivery attempt"} <ArrowUpRight size={15} aria-hidden />
    </Button>
    {item.canResolve && <div className="instruction-delivery-review">
      <Button type="button" variant="outline" disabled={resolve.isPending} onClick={() => resolve.mutate(true)}>Confirm undelivered and queue</Button>
      <Button type="button" variant="outline" disabled={resolve.isPending} onClick={() => resolve.mutate(false)}>Discard instruction</Button>
    </div>}
    {resolve.error && <p role="alert" className="conv-alert">{String(resolve.error)}</p>}
  </li>;
}

export function ConversationInstructions({ conversationId, instructions }: {
  conversationId: string;
  instructions: ReturnType<typeof useConversationInstructions>;
}) {
  return <>
    {instructions.data.length > 0 && <ol aria-label="Pending instructions" className="grid max-h-44 gap-1.5 overflow-y-auto">
      {instructions.data.map((item) => <InstructionRow key={item.id} item={item}
        conversationId={conversationId} refresh={instructions.refresh} />)}
    </ol>}
    {instructions.error && <p role="alert" className="conv-alert">{String(instructions.error)}</p>}
  </>;
}

export function ConversationInstructionHistory({ conversationId, instructions, messages, openRun }: {
  conversationId: string;
  instructions: ReturnType<typeof useConversationInstructions>;
  messages: MessageRevision[];
  openRun: (runId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const nextRun = useRef<string>(undefined);
  const reviewCount = instructions.deliveries.filter(({ canResolve }) => canResolve).length;
  const references = open ? new Map<string, string>() : undefined;
  if (references) {
    for (const { revisionId, text } of messages) references.set(revisionId, text);
    for (const { revisionId, text } of instructions.data) references.set(revisionId, text);
  }

  function openDeliveryRun(runId: string) {
    nextRun.current = runId;
    setOpen(false);
  }

  return <Dialog.Root open={open} onOpenChange={setOpen}>
    <ControlTooltip label="Instruction delivery history">
      <Dialog.Trigger asChild>
        <button ref={trigger} type="button" className="conv-quiet-button instruction-history-trigger"
          aria-label="Instruction delivery history" aria-description={reviewCount ? `${reviewCount} awaiting review` : undefined}>
          <ListChecks size={16} aria-hidden /> Deliveries
          {reviewCount > 0 && <span className="instruction-history-count" aria-hidden>{reviewCount}</span>}
        </button>
      </Dialog.Trigger>
    </ControlTooltip>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog conv-dialog-layout instruction-history-dialog" onCloseAutoFocus={(event) => {
        const runId = nextRun.current;
        if (!runId) return;
        nextRun.current = undefined;
        event.preventDefault();
        trigger.current?.focus({ preventScroll: true });
        openRun(runId);
      }}>
        <div className="conv-dialog-header">
          <span className="conv-dialog-mark"><ListChecks size={21} aria-hidden /></span>
          <div>
            <Dialog.Title>Instruction delivery history</Dialog.Title>
            <Dialog.Description>See where each instruction went and review uncertain deliveries.</Dialog.Description>
          </div>
          <Dialog.Close asChild><Button type="button" variant="ghost" className="ui-icon-button" aria-label="Close instruction history">
            <X size={17} aria-hidden />
          </Button></Dialog.Close>
        </div>
        <div className="conv-dialog-body instruction-history-body">
          {instructions.deliveryError && <div className="instruction-history-error">
            <p role="alert">Could not load instruction deliveries. {String(instructions.deliveryError)}</p>
            <Button type="button" variant="outline" onClick={() => void instructions.reloadDeliveries()}>Retry</Button>
          </div>}
          {instructions.deliveryPending && <p role="status">Loading instruction deliveries…</p>}
          {!instructions.deliveryPending && !instructions.deliveryError && instructions.deliveries.length === 0 && <div className="instruction-history-empty">
            <ListChecks size={22} aria-hidden />
            <strong>No deliveries yet</strong>
            <p>Instructions sent to an active turn or dispatched from the queue appear here.</p>
          </div>}
          {instructions.deliveries.length > 0 && <ol className="instruction-delivery-list" aria-label="Recorded instruction deliveries">
            {instructions.deliveries.map((item) => {
              const text = references?.get(item.revisionId);
              return <DeliveryEvidence key={`${item.instructionId}:${item.revisionId}:${item.runId}`} item={item}
                instruction={text ? conversationSkillMessage(text) : undefined}
                conversationId={conversationId} openRun={openDeliveryRun} refresh={instructions.refresh} />;
            })}
          </ol>}
        </div>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
