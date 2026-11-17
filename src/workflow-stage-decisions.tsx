import { useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState, type FormEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { FieldControl, type FieldValue, type QuestionForm } from "@/conversation-inputs";

export type WorkflowPendingDecision = {
  requestId: string;
  nodeId: string;
  nodeRevision: string;
  planRevision: string | null;
  unitId: string | null;
  inputs: Record<string, string>;
  request: { kind: "request_input"; form: QuestionForm } | { kind: "approval"; message: string };
};

type StageDecision = { kind: "answer"; values: Record<string, FieldValue> } | { kind: "approval"; approved: boolean };
type FormProps = { projectId: string; runId: string; pending: WorkflowPendingDecision; evidence?: ReactNode };

export function WorkflowStageDecisionForm(props: FormProps) {
  return <DecisionForm key={props.pending.requestId} {...props} />;
}

function DecisionForm({ projectId, runId, pending, evidence }: FormProps) {
  const client = useQueryClient();
  const request = pending.request;
  const [values, setValues] = useState<Record<string, FieldValue | undefined>>(() => request.kind === "request_input"
    ? Object.fromEntries(request.form.fields.map((field) => [field.id, field.default ?? undefined])) : {});
  const mutation = useMutation({
    mutationFn: (decision: StageDecision) => invoke("resolve_workflow_stage_decision", {
      submission: { projectId, runId, requestId: pending.requestId, decision },
    }),
    onSuccess: () => Promise.all([
      client.invalidateQueries({ queryKey: ["repository-delivery", projectId] }),
      client.invalidateQueries({ queryKey: ["project-tasks", projectId] }),
    ]),
  });
  const error = mutation.error && <p role="alert">{String(mutation.error)}</p>;

  if (request.kind === "approval") {
    return <section className="workflow-record-card pipeline-stack" aria-label="Workflow approval">
      <p className="whitespace-pre-wrap">{request.message}</p>
      {evidence}
      <div className="pipeline-actions">
        <Button type="button" disabled={mutation.isPending} onClick={() => mutation.mutate({ kind: "approval", approved: true })}>Approve</Button>
        <Button type="button" variant="outline" disabled={mutation.isPending} onClick={() => mutation.mutate({ kind: "approval", approved: false })}>Reject</Button>
      </div>
      {error}
    </section>;
  }

  function reply(event: FormEvent) {
    event.preventDefault();
    if (mutation.isPending) return;
    const answers = Object.fromEntries(Object.entries(values).filter((entry): entry is [string, FieldValue] => entry[1] !== undefined));
    mutation.mutate({ kind: "answer", values: answers });
  }

  return <form className="workflow-record-card pipeline-stack" aria-label="Workflow input request" onSubmit={reply}>
    <p className="whitespace-pre-wrap">{request.form.message}</p>
    {evidence}
    <fieldset className="pipeline-stack" disabled={mutation.isPending}>
      {request.form.fields.map((field) => <FieldControl key={field.id} field={field} value={values[field.id]}
        update={(value) => setValues((current) => ({ ...current, [field.id]: value }))} />)}
    </fieldset>
    <Button type="submit" className="justify-self-start" disabled={mutation.isPending}>Reply</Button>
    {error}
  </form>;
}
