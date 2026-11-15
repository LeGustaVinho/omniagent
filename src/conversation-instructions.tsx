import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import type { InstructionDelivery } from "@/bindings/InstructionDelivery";
import type { PendingInstruction } from "@/bindings/PendingInstruction";
import { Button } from "@/components/ui/button";
import { queueBlockerReason } from "@/queue-model";

const deliveryLabels: Record<InstructionDelivery, string> = {
  queued: "Queued instruction",
  paused: "Paused instruction",
  delivering: "Delivering instruction",
  needs_review: "Delivery needs review",
};

export function useConversationInstructions(conversationId: string, visible: boolean) {
  const queryClient = useQueryClient();
  const key = ["conversation", conversationId, "instructions"];
  const query = useQuery({
    queryKey: key,
    queryFn: () => invoke<PendingInstruction[]>("pending_conversation_instructions", { conversationId }),
    enabled: visible,
    refetchInterval: visible ? 500 : false,
  });
  const refresh = () => Promise.all([
    queryClient.invalidateQueries({ queryKey: key }),
    queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-job"] }),
    queryClient.invalidateQueries({ queryKey: ["project-job-queue"] }),
  ]);
  return { data: query.data ?? [], error: query.error, refresh };
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
