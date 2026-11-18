import { useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { SavedAttachments, type AttachmentDraftDestination, type AttachmentEvidence } from "@/conversation-attachment-evidence";
import { ReferencePreview, type ConversationReferencePreview } from "@/conversation-reference";

type InstructionDispatch = {
  instructionId: string;
  revisionId: string;
  text: string;
  mode: "standalone" | "continuation" | "steering";
  status: "prepared" | "submitted" | "accepted" | "rejected" | "unconfirmed";
  providerSessionId: string | null;
  providerTurnId: string | null;
  receipt: { kind: "turn_started"; eventSequence: string }
    | { kind: "steering_response"; eventSequence: string; outcome: string } | null;
  steeringFrame: string | null;
  review: "undelivered" | "discarded" | null;
};

export type ConversationInput = {
  delivery: "prepared" | "submitted" | "unconfirmed";
  turnId: string;
  frames: string[];
  attachments: AttachmentEvidence[];
  removedAttachmentCount: number;
  historyPayloadDeleted?: boolean;
  instructions: InstructionDispatch[];
  conversationReferences?: { messageRevisionId: string; reference: ConversationReferencePreview }[];
};

const modes: Record<InstructionDispatch["mode"], string> = {
  standalone: "Queued turn",
  continuation: "Queued continuation",
  steering: "Live instruction",
};

const statuses: Record<InstructionDispatch["status"], string> = {
  prepared: "Prepared",
  submitted: "Submitted",
  accepted: "Accepted",
  rejected: "Rejected",
  unconfirmed: "Delivery unconfirmed",
};

function DeliveryRow({ item, conversationId, runId, live }: {
  item: InstructionDispatch;
  conversationId: string;
  runId: string;
  live: boolean;
}) {
  const client = useQueryClient();
  const review = useMutation({
    mutationFn: (confirmedUndelivered: boolean) => invoke("resolve_uncertain_steering", {
      conversationId, runId, instructionId: item.instructionId, revisionId: item.revisionId, confirmedUndelivered,
    }),
    onSuccess: () => Promise.all([
      client.invalidateQueries({ queryKey: ["conversation-run", runId, "input"] }),
      client.invalidateQueries({ queryKey: ["conversation", conversationId, "instructions"] }),
      client.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-job"] }),
      client.invalidateQueries({ queryKey: ["conversation", conversationId, "pipeline-turns"] }),
      client.invalidateQueries({ queryKey: ["project-job-queue"] }),
    ]),
  });
  const needsReview = item.mode === "steering" && item.status === "unconfirmed" && !item.review && !live;
  return <li className="grid gap-1 border-t border-border pt-2 text-xs">
    <p className="font-medium">{modes[item.mode]} · {statuses[item.status]}</p>
    <p className="break-all text-muted-foreground">Job {item.instructionId} · Revision {item.revisionId}</p>
    <p className="whitespace-pre-wrap break-words">{item.text}</p>
    {(item.providerSessionId || item.providerTurnId) && <p className="break-all text-muted-foreground">
      {[item.providerSessionId && `Native session ${item.providerSessionId}`, item.providerTurnId && `Native turn ${item.providerTurnId}`].filter(Boolean).join(" · ")}
    </p>}
    {item.receipt && <p className="text-muted-foreground">
      {item.receipt.kind === "turn_started" ? "Provider confirmed turn start" : `Provider response ${item.receipt.outcome.replaceAll("_", " ")}`}
      {` · Event ${item.receipt.eventSequence}`}
    </p>}
    {item.steeringFrame !== null && <details>
      <summary>Exact steering frame</summary>
      <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-all">{item.steeringFrame}</pre>
    </details>}
    {item.review && <p>{item.review === "undelivered" ? "Confirmed undelivered" : "Instruction discarded"}</p>}
    {needsReview && <div className="flex flex-wrap gap-2">
      <Button type="button" variant="outline" disabled={review.isPending} onClick={() => review.mutate(true)}>Confirm undelivered</Button>
      <Button type="button" variant="outline" disabled={review.isPending} onClick={() => review.mutate(false)}>Discard instruction</Button>
    </div>}
    {review.error && <p role="alert">{String(review.error)}</p>}
  </li>;
}

export function ConversationDeliveries({ input, conversationId, runId, live, checkoutId, draftDestination }: {
  input: ConversationInput;
  conversationId: string;
  runId: string;
  live: boolean;
  checkoutId?: string;
  draftDestination?: AttachmentDraftDestination;
}) {
  return <>
    <p className="break-all text-xs text-muted-foreground">Turn {input.turnId}</p>
    <p className="text-xs text-muted-foreground">Delivery {input.delivery}</p>
    {input.historyPayloadDeleted ? <p className="mt-2 text-xs text-muted-foreground">
      Exact input is unavailable after a history deletion.
    </p> : input.removedAttachmentCount > 0 ? <p className="mt-2 text-xs text-muted-foreground">
      Exact input is unavailable after attachment history was deleted.
    </p> : <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">{input.frames.join("")}</pre>}
    <SavedAttachments attachments={input.attachments} conversationId={conversationId} runId={runId} checkoutId={checkoutId} draftDestination={draftDestination} />
    {Boolean(input.conversationReferences?.length) && <details className="mt-2">
      <summary>Conversation references</summary>
      {input.conversationReferences?.map(({ messageRevisionId, reference }) =>
        <ReferencePreview key={`${messageRevisionId}:${reference.id}`} reference={reference} />)}
    </details>}
    {input.instructions.length > 0 && <ol aria-label="Instruction deliveries" className="mt-2 grid gap-2">
      {input.instructions.map((item) => <DeliveryRow key={item.instructionId} item={item}
        conversationId={conversationId} runId={runId} live={live} />)}
    </ol>}
  </>;
}
