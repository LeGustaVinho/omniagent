import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState, type FormEvent } from "react";
import type { Capability } from "@/bindings/Capability";
import type { JobRule } from "@/bindings/JobRule";
import type { JobWorkspace } from "@/bindings/JobWorkspace";
import type { RequiredCustomization } from "@/bindings/RequiredCustomization";
import { Button } from "@/components/ui/button";
import { customizationLabel } from "@/queue-model";
import { InlineIssue } from "@/tasks-git-ui";

const field = "rounded border border-border bg-background px-2 py-1 text-sm";
const capabilityOptions: Capability[] = [
  "model_inventory", "fresh_session", "resume", "native_turn_fork", "reasoning", "tool_approvals",
  "native_plan_mode", "planning_questions", "planning_answers", "native_plan_output", "native_goal_command",
  "native_goal_events", "cancel_turn", "live_steering", "subagent_events", "per_child_events", "native_subagent_audit",
  "native_delegation_controls", "native_compaction", "manual_native_compaction", "native_suggestions",
  "side_request", "workspace_write", "os_command_sandbox", "run_scoped_mcp",
];
const customizationFields = [
  ["instruction", "Required instruction paths"],
  ["skill", "Required skill IDs"],
  ["plugin", "Required plugin IDs"],
  ["executable_entry", "Required executable customization IDs"],
] as const;

export function QueuePolicy({ projectId, checkoutId, capacity }: {
  projectId: string;
  checkoutId: string;
  capacity: number;
}) {
  const [open, setOpen] = useState(false);
  const rules = useQuery({
    queryKey: ["project-job-rules", projectId],
    queryFn: () => invoke<JobRule[]>("project_job_rules", { projectId }),
    enabled: open,
  });
  return (
    <details className="border-b border-border px-3 py-2 text-sm" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary className="w-fit cursor-pointer font-medium">Queue settings</summary>
      {open && <div className="grid gap-4 py-2">
        <CapacityEditor key={capacity} capacity={capacity} />
        {rules.isPending && <p role="status">Loading project rules…</p>}
        {rules.isError && <InlineIssue>{String(rules.error)}</InlineIssue>}
        {rules.data && <RuleEditor key={projectId} projectId={projectId} checkoutId={checkoutId} rules={rules.data} />}
      </div>}
    </details>
  );
}

function CapacityEditor({ capacity }: { capacity: number }) {
  const client = useQueryClient();
  const [value, setValue] = useState(String(capacity));
  const mutation = useMutation({
    mutationFn: (capacity: number) => invoke("set_job_queue_capacity", { capacity }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["project-job-queue"] }),
  });
  return (
    <form className="grid gap-1.5" onSubmit={(event) => {
      event.preventDefault();
      mutation.mutate(Number(value));
    }}>
      <label className="flex w-fit items-center gap-2">
        Managed execution slots across projects
        <input className={field + " w-24"} type="number" min={1} max={4_294_967_295} required value={value}
          onChange={(event) => setValue(event.target.value)} />
      </label>
      <Button type="submit" variant="outline" className="justify-self-start" disabled={mutation.isPending || Number(value) === capacity}>
        Save capacity
      </Button>
      {mutation.isError && <InlineIssue>{String(mutation.error)}</InlineIssue>}
    </form>
  );
}

function RuleEditor({ projectId, checkoutId, rules }: { projectId: string; checkoutId: string; rules: JobRule[] }) {
  const client = useQueryClient();
  const [scope, setScope] = useState<JobWorkspace["type"]>("project");
  const mutation = useMutation({
    mutationFn: ({ expected, updated }: { expected: JobRule[]; updated: JobRule[] }) =>
      invoke<JobRule[]>("set_project_job_rules", { projectId, expected, rules: updated }),
    onSuccess: (saved) => {
      client.setQueryData(["project-job-rules", projectId], saved);
      return client.invalidateQueries({ queryKey: ["project-job-queue"] });
    },
    onError: () => client.invalidateQueries({ queryKey: ["project-job-rules", projectId] }),
  });
  const save = (updated: JobRule[]) => mutation.mutate({ expected: rules, updated });

  function addRule(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const data = new FormData(event.currentTarget);
    let workspace: JobWorkspace = { type: "project" };
    if (scope === "checkout") workspace = { type: "checkout", checkoutId };
    if (scope === "task") workspace = { type: "task", taskId: String(data.get("taskId")).trim() };
    const customizations = customizationFields.flatMap(([type]) =>
      [...new Set(String(data.get(type) ?? "").split("\n").map((value) => value.trim()).filter(Boolean))]
        .map((value): RequiredCustomization => type === "instruction" ? { type, path: value } : { type, id: value }),
    );
    save([...rules, {
      role: data.get("role") as JobRule["role"],
      operation: data.get("operation") as JobRule["operation"],
      workspace,
      permission: data.get("permission") as JobRule["permission"],
      requirements: { capabilities: data.getAll("capability") as Capability[], customizations },
    }]);
  }

  function moveUp(index: number) {
    const reordered = [...rules];
    const [rule] = reordered.splice(index, 1);
    if (!rule) return;
    reordered.splice(index - 1, 0, rule);
    save(reordered);
  }

  return (
    <section className="grid gap-2" aria-label="Project job rules">
      <h3 className="font-semibold">Project job rules</h3>
      <p className="text-muted-foreground">The first matching rule applies. By default, agent requests and repository writers require approval.</p>
      {rules.length > 0 && <ol className="divide-y divide-border rounded border border-border">
        {rules.map((rule, index) => <li key={index} className="flex flex-wrap items-center gap-2 p-2">
          <span className="min-w-0 flex-1 break-words">
            {rule.role} · {rule.operation.replaceAll("_", " ")} · {workspaceLabel(rule.workspace, checkoutId)} · {rule.permission}
            {rule.requirements.capabilities.length + rule.requirements.customizations.length > 0 && <span className="block text-muted-foreground">
              {[
                ...rule.requirements.capabilities.map((capability) => capability.replaceAll("_", " ")),
                ...rule.requirements.customizations.map(customizationLabel),
              ].join(", ")}
            </span>}
          </span>
          <Button variant="outline" type="button" disabled={mutation.isPending || index === 0} onClick={() => moveUp(index)}>Move up</Button>
          <Button variant="outline" type="button" disabled={mutation.isPending} onClick={() => save(rules.filter((_, position) => position !== index))}>Remove</Button>
        </li>)}
      </ol>}
      <form className="flex flex-wrap items-end gap-2" onSubmit={addRule}>
        <label className="grid gap-1">Role
          <select name="role" className={field}><option value="agent">Agent</option><option value="operator">Operator</option><option value="pipeline">Pipeline</option></select>
        </label>
        <label className="grid gap-1">Operation
          <select name="operation" className={field}><option value="read_only">Read only</option><option value="plan">Plan</option><option value="approved_write">Repository write</option></select>
        </label>
        <label className="grid gap-1">Workspace
          <select value={scope} className={field} onChange={(event) => setScope(event.target.value as JobWorkspace["type"])}>
            <option value="project">Project</option><option value="checkout">Selected worktree</option><option value="task">Task</option>
          </select>
        </label>
        {scope === "task" && <label className="grid gap-1">Task ID<input name="taskId" className={field} required /></label>}
        <label className="grid gap-1">Permission
          <select name="permission" className={field}><option value="review">Require approval</option><option value="allow">Allow</option><option value="deny">Deny</option></select>
        </label>
        <details className="basis-full">
          <summary className="w-fit cursor-pointer">Required dispatch evidence</summary>
          <div className="grid gap-2 py-2 md:grid-cols-2">
            <label className="grid gap-1">Required verified capabilities
              <select className={field} multiple name="capability" size={5}>
                {capabilityOptions.map((capability) => <option key={capability} value={capability}>{capability === "os_command_sandbox" ? "OS command sandbox" : capability.replaceAll("_", " ")}</option>)}
              </select>
            </label>
            {customizationFields.map(([name, label]) => <label className="grid gap-1" key={name}>{label}
              <textarea className={field} name={name} rows={2} placeholder="One per line" />
            </label>)}
          </div>
        </details>
        <Button type="submit" disabled={mutation.isPending}>Add rule</Button>
      </form>
      {mutation.isError && <InlineIssue>{String(mutation.error)}</InlineIssue>}
    </section>
  );
}

function workspaceLabel(workspace: JobWorkspace, checkoutId: string) {
  switch (workspace.type) {
    case "project": return "project";
    case "checkout": return workspace.checkoutId === checkoutId ? "selected worktree" : `worktree ${workspace.checkoutId}`;
    case "task": return `task ${workspace.taskId}`;
  }
}
