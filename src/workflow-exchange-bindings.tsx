import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import { ModelPicker } from "@/components/model-picker";
import { liveModelsQuery } from "@/live-models";

type JsonObject = Record<string, unknown>;

export type ExchangeBindingKind =
  | { type: "command"; argumentCount: number; inputFields: string[] }
  | { type: "mcp_server"; tool: string; contractRevision: string; inputSchema: unknown;
      outputSchema: unknown | null; argumentFields: string[] }
  | { type: "model_profile"; provider: Provider; modelId: string };

export type ExchangeBindingRequirement = {
  member: string;
  node: string | null;
  name: string;
  requiredBeforeImport: boolean;
  kind: ExchangeBindingKind;
};

export type ExchangeBinding =
  | { type: "command"; registrationId: string; registrationRevision: string; arguments: string[]; input: JsonObject }
  | { type: "mcp_server"; serverId: string; serverRevision: string; arguments: JsonObject }
  | { type: "model_profile"; selection: ModelSelection };

type Registration = { id: string; revision: string; name: string };
type Server = Registration & { tools: { name: string; revision: string }[] };
type InputProps = { projectId: string; disabled: boolean; onChange: (binding: ExchangeBinding | undefined) => void };

function registryKey({ id, revision }: Registration) {
  return JSON.stringify([id, revision]);
}

function parseJson(text: string, maxBytes: number): unknown {
  try {
    if (new TextEncoder().encode(text).length > maxBytes) return undefined;
    const value: unknown = JSON.parse(text);
    JSON.stringify(value, (_key, entry: unknown) => {
      if (typeof entry === "number" && (!Number.isFinite(entry) || Number.isInteger(entry) && !Number.isSafeInteger(entry))) {
        throw new Error("The number cannot be passed without loss of precision");
      }
      return entry;
    });
    return value;
  } catch {
    return undefined;
  }
}

function exactObject(text: string, fields: readonly string[]): JsonObject | undefined {
  const value = parseJson(text, 256 * 1024);
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  if (Object.keys(value).length !== fields.length || fields.some((field) => !Object.hasOwn(value, field))) return undefined;
  return value as JsonObject;
}

function exactArguments(text: string, count: number): string[] | undefined {
  const value = parseJson(text, 256 * 1024);
  if (!Array.isArray(value) || value.length !== count || value.length > 128) return undefined;
  if (value.some((arg) => typeof arg !== "string" || arg.includes("\0"))) return undefined;
  if (value.reduce((length, arg: string) => length + arg.length + 1, 0) > 16 * 1024) return undefined;
  return value as string[];
}

function fieldsLabel(fields: readonly string[]) {
  return fields.length ? `Expected fields · ${fields.join(", ")}` : "Enter an empty object.";
}

function RegistrationSelect({ label, entries, selected, onSelect, disabled }: {
  label: string; entries: readonly Registration[]; selected: string; onSelect: (key: string) => void; disabled: boolean;
}) {
  const unavailable = selected !== "" && !entries.some((entry) => registryKey(entry) === selected);
  return <label>{label}
    <select value={selected} onChange={(event) => onSelect(event.currentTarget.value)} disabled={disabled}>
      <option value="">Choose a registered resource</option>
      {unavailable && <option value={selected} disabled>Selected revision unavailable</option>}
      {entries.map((entry) => <option key={registryKey(entry)} value={registryKey(entry)}>{entry.name}</option>)}
    </select>
  </label>;
}

type CommandDraft = { registration: string; argumentsText: string; inputText: string };

function CommandBinding({ projectId, disabled, kind, onChange }: InputProps & {
  kind: Extract<ExchangeBindingKind, { type: "command" }>;
}) {
  const [draft, setDraft] = useState<CommandDraft>({ registration: "", argumentsText: "[]", inputText: "{}" });
  const [edited, setEdited] = useState(false);
  const query = useQuery({
    queryKey: ["workflow-command-registrations", projectId],
    queryFn: () => invoke<Registration[]>("list_workflow_command_registrations", { projectId }),
  });
  const entries = query.data ?? [];
  function edit(patch: Partial<CommandDraft>) {
    const next = { ...draft, ...patch };
    setDraft(next);
    setEdited(true);
    const registration = entries.find((entry) => registryKey(entry) === next.registration);
    const args = exactArguments(next.argumentsText, kind.argumentCount);
    const input = exactObject(next.inputText, kind.inputFields);
    onChange(registration && args && input ? {
      type: "command", registrationId: registration.id, registrationRevision: registration.revision, arguments: args, input,
    } : undefined);
  }
  const invalidArguments = edited && !exactArguments(draft.argumentsText, kind.argumentCount);
  const invalidInput = edited && !exactObject(draft.inputText, kind.inputFields);
  return <>
    <RegistrationSelect label="Local command registration" entries={entries} selected={draft.registration}
      disabled={disabled || query.isPending} onSelect={(registration) => edit({ registration })} />
    {!query.isPending && !query.error && entries.length === 0 && <p>No command is registered in this project.</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    <label>Command arguments (JSON array)
      <textarea rows={3} spellCheck={false} disabled={disabled} value={draft.argumentsText} aria-invalid={invalidArguments}
        onChange={(event) => edit({ argumentsText: event.currentTarget.value })} />
    </label>
    <p className="pipeline-meta">Enter {kind.argumentCount} argument{kind.argumentCount === 1 ? "" : "s"} as JSON strings in order.</p>
    {invalidArguments && <p role="alert">Enter the required number of arguments as valid JSON strings.</p>}
    <label>Command input (JSON object)
      <textarea rows={3} spellCheck={false} disabled={disabled} value={draft.inputText} aria-invalid={invalidInput}
        onChange={(event) => edit({ inputText: event.currentTarget.value })} />
    </label>
    <p className="pipeline-meta">{fieldsLabel(kind.inputFields)}</p>
    {invalidInput && <p role="alert">Enter a valid JSON object with exactly the expected fields.</p>}
  </>;
}

function McpBinding({ projectId, disabled, kind, onChange }: InputProps & {
  kind: Extract<ExchangeBindingKind, { type: "mcp_server" }>;
}) {
  const [draft, setDraft] = useState({ server: "", argumentsText: "{}" });
  const [edited, setEdited] = useState(false);
  const query = useQuery({
    queryKey: ["workflow-mcp-servers", projectId],
    queryFn: () => invoke<Server[]>("workflow_mcp_servers", { projectId }),
  });
  const servers = (query.data ?? []).filter((server) =>
    server.tools.some((entry) => entry.name === kind.tool && entry.revision === kind.contractRevision));
  function edit(next: typeof draft) {
    setDraft(next);
    setEdited(true);
    const server = servers.find((entry) => registryKey(entry) === next.server);
    const args = exactObject(next.argumentsText, kind.argumentFields);
    onChange(server && args ? {
      type: "mcp_server", serverId: server.id, serverRevision: server.revision, arguments: args,
    } : undefined);
  }
  const invalid = edited && !exactObject(draft.argumentsText, kind.argumentFields);
  return <>
    <p>Tool <code>{kind.tool}</code> · Contract revision <code>{kind.contractRevision}</code></p>
    <RegistrationSelect label="Local MCP server" entries={servers} selected={draft.server}
      disabled={disabled || query.isPending} onSelect={(server) => edit({ ...draft, server })} />
    {!query.isPending && !query.error && servers.length === 0 && <p>No local MCP server has this tool contract.</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
    <label>MCP arguments (JSON object)
      <textarea rows={3} spellCheck={false} disabled={disabled} value={draft.argumentsText} aria-invalid={invalid}
        onChange={(event) => edit({ ...draft, argumentsText: event.currentTarget.value })} />
    </label>
    <p className="pipeline-meta">{fieldsLabel(kind.argumentFields)}</p>
    {invalid && <p role="alert">Enter a valid JSON object with exactly the expected fields.</p>}
    <details className="pipeline-evidence">
      <summary>Required MCP schemas</summary>
      <pre>{JSON.stringify(kind.inputSchema, null, 2)}</pre>
      {kind.outputSchema !== null && <pre>{JSON.stringify(kind.outputSchema, null, 2)}</pre>}
    </details>
  </>;
}

function ModelBinding({ kind, disabled, onChange }: {
  kind: Extract<ExchangeBindingKind, { type: "model_profile" }>;
  disabled: boolean; onChange: InputProps["onChange"];
}) {
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const query = useQuery(liveModelsQuery(kind.provider, kind.modelId));
  const options = (query.data ?? []).map(({ option }) => option).filter(({ selection }) =>
    selection.provider === kind.provider && selection.modelId === kind.modelId);
  return <>
    <ModelPicker label={`Local profile for ${kind.provider} · ${kind.modelId}`} options={options}
      loading={query.isPending} disabled={disabled} selection={{
        mode: "single", selected: selection,
        onChange: (next) => { setSelection(next); onChange({ type: "model_profile", selection: next }); },
        reset: { label: "Unbound", onSelect: () => { setSelection(null); onChange(undefined); } },
      }} />
    {!query.isPending && !query.error && !options.some((option) => option.selectable) &&
      <p>No selectable local profile has this provider and model.</p>}
    {query.error && <p role="alert">{String(query.error)}</p>}
  </>;
}

export function WorkflowExchangeBindings({ projectId, requirements, disabled, onChange }: {
  projectId: string;
  requirements: Record<string, ExchangeBindingRequirement>;
  disabled: boolean;
  onChange: (key: string, binding: ExchangeBinding | undefined) => void;
}) {
  return <div className="pipeline-stack" aria-label="Local workflow bindings">
    {Object.entries(requirements).map(([key, requirement]) => {
      const kind = requirement.kind;
      const props = { projectId, disabled, onChange: (binding: ExchangeBinding | undefined) => onChange(key, binding) };
      return <fieldset className="pipeline-stack workflow-record-card" disabled={disabled}
        key={JSON.stringify([projectId, key, requirement])}>
        <legend>{requirement.name}</legend>
        <p className="pipeline-meta">{requirement.member}{requirement.node && ` · ${requirement.node}`}</p>
        {!requirement.requiredBeforeImport ? <p>
          {kind.type === "mcp_server" && <>MCP tool <code>{kind.tool}</code> · </>}
          Bind in the consuming workflow.
        </p> : <>
          {kind.type === "command" && <CommandBinding kind={kind} {...props} />}
          {kind.type === "mcp_server" && <McpBinding kind={kind} {...props} />}
          {kind.type === "model_profile" && <ModelBinding kind={kind} disabled={disabled} onChange={props.onChange} />}
        </>}
      </fieldset>;
    })}
  </div>;
}
