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
  deadlineAt?: number;
  request: { kind: "human"; interaction: "approval" | "review" | "input" | "feedback"; form: QuestionForm; original?: Record<string, unknown> };
};

type StageDecision = { kind: "answer"; values: Record<string, FieldValue> }
  | { kind: "human_approval"; approved: boolean; values: Record<string, FieldValue> };
type FormProps = { projectId: string; runId: string; pending: WorkflowPendingDecision; evidence?: ReactNode };

export function WorkflowStageDecisionForm(props: FormProps) {
  return <DecisionForm key={props.pending.requestId} {...props} />;
}

function DecisionForm({ projectId, runId, pending, evidence }: FormProps) {
  const client = useQueryClient();
  const request = pending.request;
  const [values, setValues] = useState<Record<string, FieldValue | undefined>>(() =>
    Object.fromEntries(request.form.fields.map((field) => [field.id, field.default ?? undefined])));
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

  function reply(event: FormEvent) {
    event.preventDefault();
    if (mutation.isPending) return;
    const answers = Object.fromEntries(Object.entries(values).filter((entry): entry is [string, FieldValue] => entry[1] !== undefined));
    if (request.interaction === "approval") {
      const source = (event.nativeEvent as SubmitEvent).submitter;
      mutation.mutate({ kind: "human_approval", approved: source?.getAttribute("value") !== "reject", values: answers });
    } else mutation.mutate({ kind: "answer", values: answers });
  }

  const approval = request.interaction === "approval";
  const title = {
    approval: "Workflow approval", review: "Workflow review", input: "Workflow input request", feedback: "Workflow feedback",
  }[request.interaction];
  return <form className="workflow-record-card pipeline-stack" aria-label={title} onSubmit={reply}>
    <h3>{title}</h3>
    <p className="whitespace-pre-wrap">{request.form.message}</p>
    {pending.deadlineAt !== undefined && <p className="pipeline-meta">
      Expires <time dateTime={new Date(pending.deadlineAt).toISOString()}>{new Date(pending.deadlineAt).toLocaleString()}</time>
    </p>}
    {request.original && !evidence && <details>
      <summary>Original input</summary>
      <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(request.original, null, 2)}</pre>
    </details>}
    {evidence}
    <fieldset className="pipeline-stack" disabled={mutation.isPending}>
      {request.form.fields.map((field) => <FieldControl key={field.id} field={field} value={values[field.id]}
        update={(value) => setValues((current) => ({ ...current, [field.id]: value }))} />)}
    </fieldset>
    {approval ? <div className="pipeline-actions">
      <Button type="submit" value="approve" disabled={mutation.isPending}>Approve</Button>
      <Button type="submit" value="reject" variant="outline" disabled={mutation.isPending}>Reject</Button>
    </div> : <Button type="submit" className="justify-self-start" disabled={mutation.isPending}>
      {request.interaction === "review" ? "Submit review" : request.interaction === "feedback" ? "Send feedback" : "Reply"}
    </Button>}
    {error}
  </form>;
}
