import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { Job } from "@/bindings/Job";
import { Button } from "@/components/ui/button";
import type { WorkflowNodeConfiguration } from "@/workflow-graph-model";
import { StructuredListField, StructuredObjectField, StructuredValueField } from "@/workflow-structured-fields";

type Tool = { name: string; revision: string; inputSchema: Record<string, unknown>; outputSchema: Record<string, unknown> | null };
type Server = { id: string; revision: string; name: string; executable: string; tools: Tool[] };
type Review = { serverName: string; toolName: string; arguments: Record<string, unknown>; destination: string | null; decision: string; outboundNetwork: boolean };
type Evidence = { state: string; artifact: { id: string; value: { evidence: unknown } } | null };

const savedJson = (argumentsValue: unknown) => JSON.stringify(argumentsValue ?? {}, null, 2);

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function argumentPointer(value: unknown): value is string {
  return typeof value === "string" && value.startsWith("/") && value.length <= 2048 && !/~(?![01])/.test(value);
}

function FixedArgument({ name, schema, value, required, disabled, onChange }: {
  name: string; schema: Record<string, unknown>; value: unknown; required: boolean; disabled: boolean; onChange: (value: unknown) => void;
}) {
  const choices = Array.isArray(schema.enum) ? schema.enum : schema.type === "boolean" ? [true, false] : schema.type === "null" ? [null] : undefined;
  if (choices) {
    const selected = choices.findIndex((choice) => JSON.stringify(choice) === JSON.stringify(value));
    return <label>{name}<select value={selected === -1 ? "" : selected} required={required} disabled={disabled}
      onChange={(event) => onChange(event.currentTarget.value === "" ? undefined : choices[Number(event.currentTarget.value)])}>
      <option value="">{required ? "Select a value" : "Omit argument"}</option>
      {choices.map((choice, index) => <option key={index} value={index}>{typeof choice === "string" ? choice : JSON.stringify(choice)}</option>)}
    </select></label>;
  }
  if (schema.type === "string") {
    return <label>{name}<input value={typeof value === "string" ? value : ""} required={required} disabled={disabled}
      onChange={(event) => onChange(event.currentTarget.value)} /></label>;
  }
  if (schema.type === "number" || schema.type === "integer") {
    return <label>{name}<input type="number" value={typeof value === "number" ? value : ""} required={required} disabled={disabled}
      step={schema.type === "integer" ? 1 : "any"} min={typeof schema.minimum === "number" ? schema.minimum : undefined}
      max={typeof schema.maximum === "number" ? schema.maximum : undefined}
      onChange={(event) => onChange(Number.isFinite(event.currentTarget.valueAsNumber) ? event.currentTarget.valueAsNumber : undefined)} /></label>;
  }
  if (schema.type === "object" || schema.type === "array") {
    if (value === undefined) return <Button type="button" variant="outline" disabled={disabled}
      onClick={() => onChange(schema.type === "object" ? {} : [])}>Set {name}</Button>;
    return schema.type === "object"
      ? <StructuredObjectField label={name} value={value} disabled={disabled} onChange={onChange} />
      : <StructuredListField label={name} value={value} disabled={disabled} onChange={onChange} />;
  }
  return <StructuredValueField label={name} value={value} disabled={disabled} onChange={onChange} />;
}

function ArgumentFields({ schema, configuration, disabled, onChange }: {
  schema: Tool["inputSchema"]; configuration: WorkflowNodeConfiguration; disabled: boolean;
  onChange: (configuration: WorkflowNodeConfiguration) => void;
}) {
  const properties = Object.entries(asRecord(schema.properties));
  const knownNames = new Set(properties.map(([name]) => name));
  const required = new Set(Array.isArray(schema.required) ? schema.required : []);
  const fixed = asRecord(configuration.arguments);
  const mappings = asRecord(configuration.argumentMappings);
  const additional = Object.fromEntries(Object.entries(fixed).filter(([name]) => !knownNames.has(name)));
  const locked = disabled || configuration.arguments === null || configuration.argumentMappings === null;
  const update = (name: string, source: "fixed" | "artifact", value: unknown) => {
    const otherFixed = Object.fromEntries(Object.entries(fixed).filter(([key]) => key !== name));
    const otherMappings = Object.fromEntries(Object.entries(mappings).filter(([key]) => key !== name));
    onChange({ ...configuration,
      arguments: source === "fixed" && value !== undefined ? { ...otherFixed, [name]: value } : otherFixed,
      argumentMappings: source === "artifact" ? { ...otherMappings, [name]: value } : otherMappings,
    });
  };
  return <fieldset className="pipeline-stack">
    <legend>Tool arguments</legend>
    {properties.map(([name, property]) => {
      const mapped = Object.hasOwn(mappings, name);
      const pointer = mappings[name];
      const isRequired = required.has(name);
      const invalidPointer = mapped && !argumentPointer(pointer);
      return <div key={name} role="group" aria-label={name} className="pipeline-stack">
        <label>{name} source{isRequired && <span aria-hidden="true"> (required)</span>}
          <select value={mapped ? "artifact" : "fixed"} disabled={locked} onChange={(event) => {
            if (event.currentTarget.value === "artifact") update(name, "artifact", "");
            else update(name, "fixed", undefined);
          }}><option value="fixed">Fixed value</option><option value="artifact">Map from artifact</option></select>
        </label>
        {mapped ? <label>{name} JSON Pointer<input value={typeof pointer === "string" ? pointer : ""} required
          aria-invalid={invalidPointer} placeholder="/evidence/field" disabled={locked}
          onChange={(event) => update(name, "artifact", event.currentTarget.value)} /></label>
          : <FixedArgument name={name} schema={asRecord(property)} value={Object.hasOwn(fixed, name) ? fixed[name] : undefined} required={isRequired} disabled={locked}
            onChange={(value) => update(name, "fixed", value)} />}
        {invalidPointer && <p role="alert">Enter a JSON Pointer for {name}.</p>}
        {!isRequired && !mapped && Object.hasOwn(fixed, name) && <Button type="button" variant="outline" disabled={locked}
          onClick={() => update(name, "fixed", undefined)}>Omit {name}</Button>}
      </div>;
    })}
    {(schema.additionalProperties !== false || Object.keys(additional).length > 0) &&
      <StructuredObjectField label="Additional fixed arguments" value={additional} disabled={locked} reservedNames={[...knownNames, ...Object.keys(mappings)]}
        onChange={(fields) => onChange({ ...configuration, arguments: {
          ...Object.fromEntries(Object.entries(fixed).filter(([name]) => knownNames.has(name))), ...fields,
        } })} />}
  </fieldset>;
}

export function WorkflowMcpNodeConfiguration({ projectId, configuration, onChange, disabled }: {
  projectId: string; configuration: WorkflowNodeConfiguration; onChange: (configuration: WorkflowNodeConfiguration) => void; disabled: boolean;
}) {
  const client = useQueryClient();
  const key = ["workflow-mcp-servers", projectId];
  const servers = useQuery({ queryKey: key, queryFn: () => invoke<Server[]>("workflow_mcp_servers", { projectId }) });
  const [name, setName] = useState("");
  const [executable, setExecutable] = useState("");
  const [launchArguments, setLaunchArguments] = useState("");
  const [outboundNetwork, setOutboundNetwork] = useState(false);
  const [argumentText, setArgumentText] = useState(() => savedJson(configuration.arguments));
  const [mappingText, setMappingText] = useState(() => savedJson(configuration.argumentMappings));
  const [inputError, setInputError] = useState(false);
  const server = servers.data?.find((item) => item.id === configuration.serverId && item.revision === configuration.serverRevision);
  const tool = server?.tools.find((item) => item.name === configuration.toolName && item.revision === configuration.contractRevision);
  const signature = savedJson(configuration.arguments);
  const mappingSignature = savedJson(configuration.argumentMappings);
  useEffect(() => { if (configuration.arguments !== null) { setArgumentText(signature); setInputError(false); } }, [signature, configuration.arguments]);
  useEffect(() => { if (configuration.argumentMappings !== null) setMappingText(mappingSignature); }, [mappingSignature, configuration.argumentMappings]);
  const register = useMutation({ mutationFn: () => invoke<Server>("register_workflow_mcp_server", {
    projectId, request: { name: name.trim(), executable: executable.trim(), arguments: launchArguments.split(/\r?\n/).filter(Boolean), outboundNetwork },
  }), onSuccess: (saved) => {
    client.setQueryData<Server[]>(key, (current) => [saved, ...(current ?? [])]);
    onChange({ kind: "mcp_tool", serverId: saved.id, serverRevision: saved.revision, toolName: "", contractRevision: "", arguments: {}, argumentMappings: {}, destinationPointer: null });
  } });
  return <div className="pipeline-stack">
    <label>Local MCP server<select value={server?.id ?? ""} disabled={disabled} onChange={(event) => {
      const selected = servers.data?.find((item) => item.id === event.currentTarget.value);
      onChange({ kind: "mcp_tool", serverId: selected?.id ?? "", serverRevision: selected?.revision ?? "", toolName: "", contractRevision: "", arguments: {}, argumentMappings: {}, destinationPointer: null });
    }}><option value="">Select a registered server</option>
      {servers.data?.map((row) => <option key={row.id} value={row.id}>{row.name}</option>)}
    </select></label>
    <details className="pipeline-evidence"><summary>Register stdio server</summary>
      <div className="pipeline-stack">
        <label>Name<input value={name} disabled={disabled} onChange={(event) => setName(event.currentTarget.value)} /></label>
        <label>Self-contained Windows executable<input value={executable} disabled={disabled} onChange={(event) => setExecutable(event.currentTarget.value)} /></label>
        <label>Launch arguments, one per line<textarea rows={3} value={launchArguments} disabled={disabled} onChange={(event) => setLaunchArguments(event.currentTarget.value)} /></label>
        <label className="flex items-center gap-2"><input type="checkbox" checked={outboundNetwork} disabled={disabled} onChange={(event) => setOutboundNetwork(event.currentTarget.checked)} />Allow outbound Internet during approved tool calls</label>
        {outboundNetwork && <p className="pipeline-meta">This grants general outbound Internet access to the server. Destination approvals match the mapped tool argument.</p>}
        <Button type="button" disabled={disabled || register.isPending || !name.trim() || !executable.trim()} onClick={() => register.mutate()}>Discover and register</Button>
      </div>
    </details>
    {server && <label>Tool<select value={tool?.name ?? ""} disabled={disabled} onChange={(event) => {
      const selected = server.tools.find((entry) => entry.name === event.currentTarget.value);
      onChange({ ...configuration, toolName: selected?.name ?? "", contractRevision: selected?.revision ?? "", arguments: {}, argumentMappings: {}, destinationPointer: null });
    }}><option value="">Select a discovered tool</option>{server.tools.map((entry) => <option key={entry.name}>{entry.name}</option>)}</select></label>}
    {tool && <>
      <ArgumentFields schema={tool.inputSchema} configuration={configuration} onChange={onChange} disabled={disabled} />
      <details className="pipeline-evidence"><summary>Argument JSON</summary><div className="pipeline-stack">
      <label>Fixed arguments JSON<textarea value={argumentText} rows={4} disabled={disabled} onChange={(event) => {
        const draft = event.currentTarget.value;
        setArgumentText(draft);
        try {
          const parsed: unknown = JSON.parse(draft);
          if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw Error("object required");
          setInputError(false);
          onChange({ ...configuration, arguments: parsed });
        } catch {
          setInputError(true);
          onChange({ ...configuration, arguments: null });
        }
      }} /></label>
      {inputError && <p role="alert">Enter a JSON object for fixed arguments.</p>}
      <label>Argument mappings<textarea value={mappingText} rows={4} disabled={disabled} onChange={(event) => {
        const draft = event.currentTarget.value;
        setMappingText(draft);
        try {
          const parsed: unknown = JSON.parse(draft);
          if (!parsed || typeof parsed !== "object" || Array.isArray(parsed) || Object.values(parsed).some((pointer) => !argumentPointer(pointer))) throw Error("pointer map required");
          onChange({ ...configuration, argumentMappings: parsed });
        } catch {
          onChange({ ...configuration, argumentMappings: null });
        }
      }} /></label>
      <p className="pipeline-meta">Map argument names to JSON Pointers in the artifact wired to Arguments.</p>
      {configuration.argumentMappings === null && <p role="alert">Enter a JSON object of argument names and JSON Pointers.</p>}
      </div></details>
      <label>Destination argument pointer<input value={typeof configuration.destinationPointer === "string" ? configuration.destinationPointer : ""}
        placeholder="/destination" disabled={disabled} onChange={(event) => onChange({ ...configuration, destinationPointer: event.currentTarget.value || null })} /></label>
      <details className="pipeline-evidence"><summary>Pinned schemas</summary><pre className="workflow-evidence-json">{JSON.stringify({ input: tool.inputSchema, output: tool.outputSchema }, null, 2)}</pre></details>
    </>}
    {(servers.error || register.error) && <p role="alert">{String(servers.error || register.error)}</p>}
  </div>;
}

export function WorkflowMcpToolRun({ projectId, runId, checkoutId, nodeId, argumentsArtifact, waitsForArguments, job }: {
  projectId: string; runId: string; checkoutId: string; nodeId: string; argumentsArtifact?: string; waitsForArguments?: boolean; job?: Job;
}) {
  const client = useQueryClient();
  const refresh = () => client.invalidateQueries({ queryKey: ["repository-delivery", projectId, "execution", runId] });
  const latest = job?.latestAttempt;
  const review = useQuery({ queryKey: ["workflow-mcp-job", projectId, job?.id], enabled: Boolean(job),
    queryFn: () => invoke<Review>("workflow_mcp_job_review", { projectId, jobId: job?.id }) });
  const evidence = useQuery({ queryKey: ["workflow-mcp-attempt", projectId, latest?.runId, latest?.status], enabled: Boolean(latest),
    queryFn: () => invoke<Evidence | null>("workflow_mcp_attempt_evidence", { projectId, runId: latest?.runId }) });
  const queue = useMutation({ mutationFn: () => invoke<Job>("submit_workflow_mcp_tool", {
    submission: { projectId, runId, nodeId, checkoutId, inputs: argumentsArtifact ? { arguments: argumentsArtifact } : {} },
  }), onSuccess: refresh });
  const decide = useMutation({ mutationFn: (approve: boolean) => invoke("decide_workflow_mcp_job", { projectId, jobId: job?.id, approve }),
    onSuccess: () => Promise.all([review.refetch(), refresh()]) });
  const grant = useMutation({ mutationFn: (allowed: boolean) => invoke("set_workflow_mcp_grant", {
    projectId, jobId: job?.id, allowed,
  }) });
  const reconcile = useMutation({ mutationFn: () => invoke("reconcile_workflow_mcp_attempt", { projectId, runId: latest?.runId }),
    onSuccess: () => evidence.refetch() });
  const error = queue.error || review.error || decide.error || grant.error || evidence.error || reconcile.error;
  return <section aria-label="Local MCP tool" className="pipeline-stack">
    {!job && <Button type="button" disabled={queue.isPending || waitsForArguments} onClick={() => queue.mutate()}>Prepare tool call</Button>}
    {!job && waitsForArguments && <p role="status">Waiting for the mapped input artifact.</p>}
    {review.data && <>
      <p>{review.data.serverName} · {review.data.toolName} · {review.data.decision}</p>
      {review.data.outboundNetwork && <p>General outbound Internet access is enabled for this server.</p>}
      {review.data.destination && <p className="break-all">Destination <code>{review.data.destination}</code></p>}
      <details><summary>Mapped arguments</summary><pre className="workflow-evidence-json">{savedJson(review.data.arguments)}</pre></details>
      {!latest && ["pending", "preauthorized"].includes(review.data.decision) && <div className="pipeline-actions">
        <Button type="button" disabled={decide.isPending} onClick={() => decide.mutate(true)}>Approve tool call</Button>
        <Button type="button" variant="outline" disabled={decide.isPending} onClick={() => decide.mutate(false)}>Deny tool call</Button>
      </div>}
      {review.data.destination && <details><summary>Destination pre-authorization</summary>
        <div className="pipeline-actions">
          <Button type="button" variant="outline" disabled={grant.isPending} onClick={() => grant.mutate(true)}>Authorize future calls to this destination</Button>
          <Button type="button" variant="outline" disabled={grant.isPending} onClick={() => grant.mutate(false)}>Revoke destination authorization</Button>
        </div>
        {grant.isSuccess && <p role="status">Destination authorization updated.</p>}
      </details>}
    </>}
    {evidence.data && <>
      <p role="status">{evidence.data.state === "uncertain" ? "External effect needs review" : evidence.data.state.replaceAll("_", " ")}</p>
      {evidence.data.state === "uncertain" && <Button type="button" variant="outline" disabled={reconcile.isPending}
        onClick={() => reconcile.mutate()}>Record external effect reviewed</Button>}
      {evidence.data.artifact && <details><summary>Tool response</summary>
        <pre className="workflow-evidence-json">{JSON.stringify(evidence.data.artifact.value.evidence, null, 2)}</pre>
      </details>}
    </>}
    {error && <p role="alert">{String(error)}</p>}
  </section>;
}
