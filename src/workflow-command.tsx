import { useId, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import type { Job } from "@/bindings/Job";
import type { CommandJobOutput } from "@/bindings/CommandJobOutput";

export type CommandNodeConfigurationValue = { kind: string; [field: string]: unknown };

type Target = { kind: "executable"; path: string }
  | { kind: "script"; path: string; executable: string; interpreterArguments: string[] };
type Registration = { id: string; revision: string; name: string; target: Target };
type ResultArtifact = {
  value: { kind: "command_result"; evidence: { runId: string; output:
    { format: "json"; value: unknown } | { format: "text"; value: string } } };
};

const field = "w-full rounded-md border border-border bg-background px-2 py-1 text-sm";
const splitArguments = (text: string) => text === "" ? [] : text.split(/\r?\n/);
const bindingKey = (registration: Registration) => `${registration.id}:${registration.revision}`;

function parseInput(text: string): Record<string, unknown> | null {
  try {
    const value = JSON.parse(text);
    return value && typeof value === "object" && !Array.isArray(value) ? value : null;
  } catch {
    return null;
  }
}

export function CommandNodeConfiguration({ projectId, configuration, onChange, disabled, fixedInvocation = false }: {
  projectId: string;
  configuration: CommandNodeConfigurationValue;
  onChange: (next: CommandNodeConfigurationValue) => void;
  disabled: boolean;
  fixedInvocation?: boolean;
}) {
  const producer = useId();
  const client = useQueryClient();
  const queryKey = ["workflow-command-registrations", projectId];
  const [registering, setRegistering] = useState(false);
  const [name, setName] = useState("");
  const [targetKind, setTargetKind] = useState<Target["kind"]>("executable");
  const [file, setFile] = useState("");
  const [interpreter, setInterpreter] = useState("");
  const [prefix, setPrefix] = useState("");
  const [inputText, setInputText] = useState(() => JSON.stringify(configuration.input === undefined ? {} : configuration.input, null, 2));
  const update = (fields: Partial<CommandNodeConfigurationValue>) => onChange({ ...configuration, ...fields });
  const registrations = useQuery({
    queryKey,
    queryFn: () => invoke<Registration[]>("list_workflow_command_registrations", { projectId }),
  });
  const register = useMutation({
    mutationFn: () => invoke<Registration>("register_workflow_command", {
      projectId,
      request: {
        name,
        target: targetKind === "executable" ? { kind: targetKind, path: file }
          : { kind: targetKind, path: file, executable: interpreter, interpreterArguments: splitArguments(prefix) },
      },
    }),
    onSuccess: (saved) => {
      client.setQueryData<Registration[]>(queryKey, (entries) =>
        [...(entries ?? []), saved].sort((first, second) => first.name.localeCompare(second.name)));
      update({ registrationId: saved.id, registrationRevision: saved.revision });
      setRegistering(false);
      setName("");
      setFile("");
    },
  });
  const selected = `${configuration.registrationId ?? ""}:${configuration.registrationRevision ?? ""}`;
  const available = registrations.data ?? [];
  const hasRegistration = available.some((entry) => bindingKey(entry) === selected);
  const missing = selected !== ":" && !hasRegistration;
  const validInput = parseInput(inputText) !== null;

  return <div className="grid gap-3">
    <label className="grid gap-1 text-sm">
      Local command registration
      <select className={field} value={hasRegistration || missing ? selected : ""} disabled={disabled || fixedInvocation}
        onChange={(event) => {
          const entry = available.find((item) => bindingKey(item) === event.currentTarget.value);
          update({ registrationId: entry?.id ?? "", registrationRevision: entry?.revision ?? "" });
        }}>
        <option value="">Select registered command</option>
        {missing && <option value={selected} disabled>Selected revision unavailable</option>}
        {available.map((entry) => <option value={bindingKey(entry)} key={bindingKey(entry)}>{entry.name}</option>)}
      </select>
    </label>
    {registrations.isError && <p role="alert">{String(registrations.error)}</p>}
    {!fixedInvocation && <Button type="button" variant="outline" disabled={disabled} aria-expanded={registering}
      onClick={() => setRegistering(!registering)}>Register local command</Button>}
    {registering && <fieldset className="grid gap-2 rounded-md border border-border p-3 text-sm" disabled={disabled || register.isPending}>
      <label>Name<input className={field} value={name} onChange={(event) => setName(event.target.value)} /></label>
      <label>Command type
        <select className={field} value={targetKind} onChange={(event) => setTargetKind(event.target.value as Target["kind"])}>
          <option value="executable">Executable</option>
          <option value="script">Script with interpreter</option>
        </select>
      </label>
      <label>{targetKind === "script" ? "Absolute script path" : "Absolute executable path"}
        <input className={field} value={file} onChange={(event) => setFile(event.target.value)} />
      </label>
      {targetKind === "script" && <>
        <label>Absolute interpreter path<input className={field} value={interpreter} onChange={(event) => setInterpreter(event.target.value)} /></label>
        <label>Interpreter arguments before script, one per line
          <textarea className={field} rows={3} value={prefix} onChange={(event) => setPrefix(event.target.value)} />
        </label>
      </>}
      {register.isError && <p role="alert">{String(register.error)}</p>}
      <Button type="button" disabled={!name.trim() || !file.trim() || (targetKind === "script" && !interpreter.trim())}
        onClick={() => register.mutate()}>Save command registration</Button>
    </fieldset>}
    <label className="grid gap-1 text-sm">Arguments, one per line
      <textarea className={field} disabled={disabled || fixedInvocation} rows={3} value={Array.isArray(configuration.arguments) ? configuration.arguments.join("\n") : ""}
        onChange={(event) => update({ arguments: splitArguments(event.target.value) })} />
    </label>
    <label className="grid gap-1 text-sm">Standard input JSON
      <textarea className={`${field} font-mono`} disabled={disabled || fixedInvocation} rows={5} value={inputText}
        aria-invalid={!validInput} aria-describedby={validInput ? undefined : producer}
        onChange={(event) => {
          const value = event.target.value;
          setInputText(value);
          update({ input: parseInput(value) });
        }} />
    </label>
    {!validInput && <p role="alert" id={producer}>Enter a JSON object.</p>}
    <label className="grid gap-1 text-sm">Standard output
      <select className={field} disabled={disabled} value={configuration.output === "json" ? "json" : "text"}
        onChange={(event) => update({ output: event.target.value })}>
        <option value="text">Text</option>
        <option value="json">JSON</option>
      </select>
    </label>
  </div>;
}

type RunProps = {
  projectId: string;
  runId: string;
  nodeId: string;
  releaseId?: string;
  worktreeId?: string;
  job?: { id: string; latestAttempt?: { runId: string; status: string } | null } | null;
  onQueued?: () => void;
};

export function WorkflowCommandRun({ projectId, runId, nodeId, releaseId, worktreeId, job, onQueued }: RunProps) {
  const queued = useMutation({
    mutationFn: () => invoke<Job>("submit_workflow_command", {
      submission: { projectId, runId, nodeId, inputs: { release: releaseId, worktree: worktreeId } },
    }),
    onSuccess: onQueued,
  });
  const selected = job ?? queued.data;
  const attempt = selected?.latestAttempt;
  const attemptId = attempt?.runId;
  const ended = ["succeeded", "failed", "cancelled", "interrupted"].includes(attempt?.status ?? "");
  const args = { projectId, runId: attemptId };
  const result = useQuery({
    queryKey: ["workflow-command-result", projectId, attemptId],
    queryFn: () => invoke<ResultArtifact | null>("workflow_command_result", args),
    enabled: ended && !!attemptId,
  });
  const logs = useQuery({
    queryKey: ["workflow-command-logs", projectId, attemptId],
    queryFn: () => invoke<CommandJobOutput | null>("command_job_output", args),
    enabled: ended && !!attemptId,
  });
  const output = result.data?.value.evidence.output;
  return <section className="grid gap-2 rounded-md border border-border p-3 text-sm" aria-label={`Command ${nodeId}`}>
    <h3 className="font-medium">{nodeId}</h3>
    {selected ? <p role="status">{attempt?.status ?? "Queued"}</p> : <>
      <Button type="button" variant="outline" disabled={!releaseId || !worktreeId || queued.isPending}
        onClick={() => queued.mutate()}>Run command</Button>
      {(!releaseId || !worktreeId) && <p>Approve implementation and prepare the worktree to run this command.</p>}
    </>}
    {(queued.isError || result.isError || logs.isError) && <p role="alert">{String(queued.error ?? result.error ?? logs.error)}</p>}
    {output && <pre className="overflow-auto whitespace-pre-wrap" aria-label="Command result">
      {output.format === "json" ? JSON.stringify(output.value, null, 2) : output.value}
    </pre>}
    {logs.data && <dl className="grid gap-1">
      {logs.data.process && <><dt>Verified exit code</dt><dd>{logs.data.process.code}</dd></>}
      {logs.data.truncated && <><dt>Capture</dt><dd>Command output was truncated.</dd></>}
      {logs.data.stderr && <><dt>Standard error</dt><dd className="whitespace-pre-wrap font-mono">{logs.data.stderr}</dd></>}
      {!output && logs.data.stdout && <><dt>Standard output</dt><dd className="whitespace-pre-wrap font-mono">{logs.data.stdout}</dd></>}
    </dl>}
  </section>;
}
