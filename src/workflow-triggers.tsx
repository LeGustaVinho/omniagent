import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { RepositoryMarkdownPicker } from "@/repository-markdown-picker";
import type { WorkspaceProps } from "@/workbench-contract";

type Version = { id: string; revision: string };
type Source = { checkoutId: string; revisionId: string };
const events = {
  created: "Work created",
  state_changed: "Status changed",
  checklist_checked: "Checklist item checked",
  checklist_reopened: "Checklist item reopened",
};
const workKinds = { all: "Tasks and Sub-Tasks", task: "Tasks", subtask: "Sub-Tasks" };
const states = ["backlog", "ready", "in_progress", "in_review", "blocked", "completed", "failed", "cancelled"];
const outcomes = { launched: "Launched", skipped_cycle: "Skipped · pipeline already in chain", failed: "Failed" };
type Spec = {
  name: string; enabled: boolean; event: keyof typeof events; work: keyof typeof workKinds;
  targetState: string | null; pipeline: Version; source: Source;
};
type Rule = { id: string; revision: number; spec: Spec };
type Activation = {
  ruleId: string; ruleName: string; ruleRevision: number; eventId: string; workId: string;
  runId: string | null; outcome: keyof typeof outcomes; detail: string | null; recordedAt: string;
};
type Base = { version: Version; name: string };
const versionKey = ({ id, revision }: Version) => JSON.stringify([id, revision]);

export function WorkflowTriggers({ checkout, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openWorkspace">) {
  const { projectId, checkoutId } = checkout;
  const client = useQueryClient();
  const rulesKey = ["workflow-trigger-rules", projectId];
  const [editing, setEditing] = useState<Rule | "new" | null>(null);
  const rules = useQuery({ queryKey: rulesKey, queryFn: () => invoke<Rule[]>("workflow_trigger_rules", { projectId }) });
  const activations = useQuery({
    queryKey: ["workflow-trigger-activations", projectId],
    queryFn: () => invoke<Activation[]>("workflow_trigger_activations", { projectId }),
    refetchInterval: 3000,
  });
  return <section className="pipeline-view pipeline-view-content pipeline-stack" aria-label="Pipeline automation rules">
    <header className="pipeline-section-heading"><h2>Automation rules</h2>
      <Button onClick={() => setEditing("new")} disabled={editing === "new"}>New rule</Button>
    </header>
    {rules.error && <p role="alert">{String(rules.error)}</p>}
    {rules.data?.length === 0 && <p className="pipeline-note">No automation rules for this project.</p>}
    {rules.data?.map((rule) => <div className="pipeline-evidence pipeline-actions" key={rule.id}>
      <strong>{rule.spec.name}</strong><span>Revision {rule.revision} · {rule.spec.enabled ? "Enabled" : "Disabled"}</span>
      <span>{events[rule.spec.event]} · {workKinds[rule.spec.work]}</span>
      <Button variant="outline" onClick={() => setEditing(rule)}>Edit {rule.spec.name}</Button>
    </div>)}
    {editing && <RuleEditor key={editing === "new" ? "new" : editing.id + ":" + editing.revision}
      rule={editing === "new" ? null : editing} projectId={projectId} checkoutId={checkoutId}
      onClose={() => setEditing(null)} onSaved={() => {
        void client.invalidateQueries({ queryKey: rulesKey });
        setEditing(null);
      }} />}
    <h3>Recent activations</h3>
    {activations.error && <p role="alert">{String(activations.error)}</p>}
    {activations.data?.length === 0 && <p className="pipeline-note">No rule activations recorded.</p>}
    {activations.data?.map((activation) => <article className="pipeline-evidence pipeline-stack" key={activation.ruleId + ":" + activation.eventId}>
      <strong>{activation.ruleName} · revision {activation.ruleRevision}</strong>
      <span>{outcomes[activation.outcome]} · <time dateTime={activation.recordedAt}>{new Date(activation.recordedAt).toLocaleString()}</time></span>
      <span>Event <code>{activation.eventId}</code></span>
      {activation.detail && activation.outcome === "failed" && <p>{activation.detail}</p>}
      <div className="pipeline-actions">
        <Button variant="outline" onClick={() => openWorkspace("tasks", activation.workId)}>Open work item</Button>
        {activation.runId && <Button variant="outline" onClick={() => openWorkspace("pipelines", activation.runId ?? undefined)}>Open execution</Button>}
      </div>
    </article>)}
  </section>;
}

function RuleEditor({ rule, projectId, checkoutId, onClose, onSaved }: {
  rule: Rule | null; projectId: string; checkoutId: string; onClose: () => void; onSaved: () => void;
}) {
  const [name, setName] = useState(rule?.spec.name ?? "");
  const [enabled, setEnabled] = useState(rule?.spec.enabled ?? false);
  const [event, setEvent] = useState<Spec["event"]>(rule?.spec.event ?? "created");
  const [work, setWork] = useState<Spec["work"]>(rule?.spec.work ?? "all");
  const [targetState, setTargetState] = useState(rule?.spec.targetState ?? "");
  const [version, setVersion] = useState(rule ? versionKey(rule.spec.pipeline) : "");
  const [source, setSource] = useState<Source | null>(rule?.spec.source ?? null);
  const bases = useQuery({
    queryKey: ["workflow-authoring-bases", projectId],
    queryFn: () => invoke<Base[]>("workflow_authoring_bases", { projectId }),
  });
  const pipeline = bases.data?.find((base) => versionKey(base.version) === version)?.version ?? null;
  const save = useMutation({
    mutationFn: (spec: Spec) => invoke<Rule>("save_workflow_trigger_rule", {
      projectId, ruleId: rule?.id ?? null, expectedRevision: rule?.revision ?? null, spec,
    }),
    onSuccess: onSaved,
  });
  return <form className="pipeline-form pipeline-stack pipeline-evidence" onSubmit={(form) => {
    form.preventDefault();
    if (source && pipeline && name.trim()) save.mutate({
      name, enabled, event, work, targetState: event === "state_changed" ? targetState || null : null, pipeline, source,
    });
  }}>
    <h3>{rule ? "Edit " + rule.spec.name : "New automation rule"}</h3>
    <label>Rule name<input value={name} onChange={(input) => setName(input.currentTarget.value)} required /></label>
    <label>Event<select value={event} onChange={(input) => setEvent(input.currentTarget.value as Spec["event"])}>
      {Object.entries(events).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
    </select></label>
    <label>Work item type<select value={work} onChange={(input) => setWork(input.currentTarget.value as Spec["work"])}>
      {Object.entries(workKinds).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
    </select></label>
    {event === "state_changed" && <label>New status<select value={targetState} onChange={(input) => setTargetState(input.currentTarget.value)}>
      <option value="">Any status</option>
      {states.map((state) => <option key={state} value={state}>{state.replaceAll("_", " ")}</option>)}
    </select></label>}
    <label>Pipeline revision<select value={version} onChange={(input) => setVersion(input.currentTarget.value)}>
      <option value="">Choose a revision</option>
      {bases.data?.map((base) => <option key={versionKey(base.version)} value={versionKey(base.version)}>{base.name} · {base.version.revision.slice(0, 12)}</option>)}
    </select></label>
    {bases.error && <p role="alert">{String(bases.error)}</p>}
    <RepositoryMarkdownPicker checkoutId={checkoutId} launchLabel="Use this Markdown revision" onDeliver={(revisionId) => setSource({ checkoutId, revisionId })} />
    {source && <p>Selected Markdown revision <code>{source.revisionId}</code>{source.checkoutId !== checkoutId && " · another worktree in this project"}</p>}
    <label className="pipeline-checkline"><input type="checkbox" checked={enabled} onChange={(input) => setEnabled(input.currentTarget.checked)} />Enable rule</label>
    {save.error && <p role="alert">{String(save.error)}</p>}
    <div className="pipeline-actions"><Button type="submit" disabled={!name.trim() || !pipeline || !source || save.isPending}>Save rule</Button>
      <Button type="button" variant="outline" onClick={onClose}>Cancel</Button></div>
  </form>;
}
