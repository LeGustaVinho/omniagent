import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useRef, useState } from "react";
import { Button } from "@/components/ui/button";

type GateAction =
  | { type: "command"; executable: string; arguments: string[]; timeoutSeconds: number | null }
  | { type: "unavailable"; reason: string };
type Gate = { name: string; required: boolean; action: GateAction };
type GatePolicy = { revision: string; gates: Gate[] };
type Row = Gate & { id: number };

const fieldClass = "mt-1 block w-full rounded-md border border-input bg-background px-2 py-1.5 text-sm";

function newAction(type: GateAction["type"]): GateAction {
  return type === "command"
    ? { type, executable: "", arguments: [], timeoutSeconds: null }
    : { type, reason: "" };
}

export function CandidateGateSettings({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const queryKey = ["candidate-gate-policy", projectId];
  const policy = useQuery({
    queryKey,
    queryFn: () => invoke<GatePolicy>("candidate_gate_policy", { projectId }),
  });
  const save = useMutation({
    mutationFn: (gates: Gate[]) => invoke<string>("set_candidate_gate_policy", { projectId, gates }),
    onSuccess: () => client.invalidateQueries({ queryKey }),
  });

  return <section data-setting="candidate-gates" className="grid gap-3 pb-5">
    <h2 className="text-base font-semibold">Candidate verification checks</h2>
    <p className="text-sm text-muted-foreground">
      Commands run in a clean checkout of the selected changes. Required checks must pass before the diff can be accepted.
    </p>
    {policy.isPending && <p role="status" className="text-sm">Loading checks…</p>}
    {policy.isError && <p role="alert" className="settings-result settings-result-error">{String(policy.error)}</p>}
    {policy.data && <GateForm key={policy.data.revision} gates={policy.data.gates} pending={save.isPending}
      submit={(gates) => save.mutate(gates)} onEdit={save.reset} />}
    {save.isSuccess && <p role="status" className="text-sm">Checks saved</p>}
    {save.isError && <p role="alert" className="settings-result settings-result-error">{String(save.error)}</p>}
  </section>;
}

function GateForm({ gates, pending, submit, onEdit }: {
  gates: Gate[];
  pending: boolean;
  submit: (gates: Gate[]) => void;
  onEdit: () => void;
}) {
  const [rows, setRows] = useState<Row[]>(() => gates.map((gate, id) => ({ ...gate, id })));
  const nextId = useRef(rows.length);

  function edit(rows: Row[]) {
    setRows(rows);
    onEdit();
  }

  return <form className="grid gap-3" onSubmit={(event) => {
    event.preventDefault();
    submit(rows.map(({ name, required, action }) => ({ name, required, action })));
  }}>
    <fieldset disabled={pending} className="grid gap-3">
      {rows.map((row, index) => <GateFields key={row.id} gate={row}
        change={(change) => edit(rows.map((gate) => gate.id === row.id ? { ...gate, ...change } : gate))}
        remove={() => edit(rows.filter((gate) => gate.id !== row.id))}
        label={row.name || `Check ${index + 1}`} />)}
      <div className="flex flex-wrap gap-2">
        <Button type="button" variant="outline" onClick={() => {
          const row = { id: nextId.current++, name: "", required: false, action: newAction("command") };
          edit([...rows, row]);
        }}>Add check</Button>
        <Button type="submit">{pending ? "Saving…" : "Save checks"}</Button>
      </div>
    </fieldset>
  </form>;
}

function GateFields({ gate, label, change, remove }: {
  gate: Gate;
  label: string;
  change: (value: Partial<Gate>) => void;
  remove: () => void;
}) {
  const action = gate.action;
  return <fieldset className="grid gap-3 rounded-md border border-border p-3 text-sm">
    <legend className="px-1 font-medium">{label}</legend>
    <div className="grid gap-3 sm:grid-cols-2">
      <label>Check name
        <input className={fieldClass} required value={gate.name} onChange={(event) => change({ name: event.target.value })} />
      </label>
      <label>How to check
        <select className={fieldClass} value={action.type}
          onChange={(event) => change({ action: newAction(event.target.value as GateAction["type"]) })}>
          <option value="command">Run a command</option>
          <option value="unavailable">Record as unavailable</option>
        </select>
      </label>
    </div>
    {action.type === "command" ? <>
      <label>Executable path
        <input className={fieldClass} required value={action.executable}
          onChange={(event) => change({ action: { ...action, executable: event.target.value } })} />
      </label>
      <label>Arguments, one per line
        <textarea className={fieldClass} rows={3} defaultValue={action.arguments.join("\n")}
          onChange={(event) => {
            const value = event.target.value;
            change({ action: { ...action, arguments: value ? value.split(/\r?\n/) : [] } });
          }} />
      </label>
      <label>Timeout in seconds
        <input className={`${fieldClass} max-w-32`} type="number" min={1} value={action.timeoutSeconds ?? ""}
          onChange={(event) => change({ action: { ...action, timeoutSeconds: event.target.value ? event.target.valueAsNumber : null } })} />
      </label>
    </> : <label>Reason the check is unavailable
      <input className={fieldClass} required value={action.reason}
        onChange={(event) => change({ action: { ...action, reason: event.target.value } })} />
    </label>}
    <div className="flex flex-wrap items-center justify-between gap-2">
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={gate.required} onChange={(event) => change({ required: event.target.checked })} />
        Required for acceptance
      </label>
      <Button type="button" variant="ghost" aria-label={`Remove ${label}`} onClick={remove}>Remove</Button>
    </div>
  </fieldset>;
}
