import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Dialog, Popover } from "radix-ui";
import { Layers3 } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { PackageDraft } from "@/workflow-authoring-package";
import { WorkflowGraphEditor } from "@/workflow-graph-editor";
import type { PromptContract, PromptExpansion } from "@/workflow-prompt-model";
import {
  isPresetEligible, isStartNode, portType, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowEndpoint,
  type WorkflowGraph, type WorkflowLayout, type WorkflowNodeConfiguration, type WorkflowPortType, type WorkflowReuseSelection, type WorkflowWire,
} from "@/workflow-graph-model";

export type ReusableVersion = { id: string; revision: string; kind: string; destination: "project" | "library" };
export type ReusableReference = { source: "published"; version: ReusableVersion } | { source: "draft"; key: string }
  | { source: "parameter"; name: string; kind: string };
export type ReusableBody = { kind: string; graph?: WorkflowGraph; configuration?: WorkflowNodeConfiguration; editableFields?: string[];
  inputs?: Record<string, WorkflowEndpoint>; outputs?: Record<string, WorkflowEndpoint>; prompt?: PromptContract };
type MatchSource = { type: "package_revision"; draftId: string; revision: string; memberKey: string }
  | { type: "published"; version: ReusableVersion };
type Boundary = {
  inputs: { wire: WorkflowWire; value: WorkflowPortType }[];
  outputs: { endpoint: WorkflowEndpoint; value: WorkflowPortType; consumedOutsideSelection: boolean }[];
};
type InputGroup = { key: string; source: WorkflowEndpoint; wires: WorkflowWire[]; value: WorkflowPortType };
type Selection = { type: "preset"; nodeId: string; editableFields: string[] }
  | { type: "subflow"; nodeIds: string[] | null; inputs: Record<string, WorkflowWire[]>; outputs: Record<string, WorkflowEndpoint> };
type ArtifactSummary = { version: ReusableVersion; name: string; blockers: string[]; prompt?: boolean };
export type ReusableDescription = {
  artifact: ArtifactSummary & { body: ReusableBody; references: Record<string, ReusableReference>; layout?: WorkflowLayout;
    promptExpansion?: PromptExpansion };
  definition: WorkflowDefinition;
  graphDefinitions: WorkflowDefinitions;
  editableOverrides: string[];
};

const endpointKey = ({ node, port }: WorkflowEndpoint) => JSON.stringify([node, port]);
const endpointLabel = ({ node, port }: WorkflowEndpoint) => `${node}.${port}`;
const slug = (value: string) => value.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");

export function reusableEditableFields(configuration: WorkflowNodeConfiguration) {
  switch (configuration.kind) {
    case "agent_role": return ["role"];
    case "required_checks": return ["names"];
    case "condition": return ["predicate"];
    case "command": return ["output"];
    case "build_list": return configuration.source === "values" ? ["values"] : ["pointer"];
    default: return [];
  }
}

export function reusableNodeQuery(projectId: string, version: ReusableVersion) {
  return {
    queryKey: ["workflow-reusable-node", projectId, version],
    queryFn: () => invoke<ReusableDescription>("describe_workflow_reusable_node", { projectId, version }),
  };
}

type DerivationProps = {
  projectId: string; source: MatchSource; graph: WorkflowGraph; disabled: boolean; onSaved: (draft: PackageDraft) => void;
};

export function WorkflowReusableDerivation(props: DerivationProps) {
  const [open, setOpen] = useState(false);
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Save a reusable node</summary>
    <ReusableDerivationForm {...props} active={open} />
  </details>;
}

export function WorkflowReusableDerivationDialog({ selection, onClose, onSaved, ...props }: DerivationProps & {
  selection: WorkflowReuseSelection; onClose: () => void;
}) {
  const [saving, setSaving] = useState(false);
  return <Dialog.Root open onOpenChange={(open) => { if (!open && !saving) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog">
        <Dialog.Title>{selection.type === "preset" ? "Save node as preset" : "Extract subflow"}</Dialog.Title>
        <Dialog.Description>Review the selection from the saved graph revision.</Dialog.Description>
        <ReusableDerivationForm {...props} active initial={selection} onSaving={setSaving}
          onSaved={(draft) => { onClose(); onSaved(draft); }} />
        <Dialog.Close asChild><Button type="button" variant="outline" disabled={saving}>Close</Button></Dialog.Close>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}

function ReusableDerivationForm({ projectId, source, graph, disabled, onSaved, active, initial, onSaving }: DerivationProps & {
  active: boolean; initial?: WorkflowReuseSelection; onSaving?: (saving: boolean) => void;
}) {
  const [mode, setMode] = useState<Selection["type"]>(initial?.type ?? "preset");
  const [name, setName] = useState("");
  const [key, setKey] = useState("");
  const [destination, setDestination] = useState<ReusableVersion["destination"]>("project");
  const [preset, setPreset] = useState<{ nodeId: string; editableFields: string[] }>({
    nodeId: initial?.type === "preset" ? initial.nodeId : "", editableFields: [],
  });
  const [subset, setSubset] = useState<string[] | null>(initial?.type === "subflow" ? initial.nodeIds : null);
  const nodes = graph.nodes.filter(({ kind }) => !isStartNode(kind));
  const selectedNode = nodes.find((node) => node.id === preset.nodeId && isPresetEligible(node));
  const boundary = useQuery({
    queryKey: ["workflow-reusable-boundary", projectId, source, subset],
    queryFn: () => invoke<Boundary>("describe_workflow_reusable_boundary", { projectId, source, nodeIds: subset }),
    enabled: active && !disabled && mode === "subflow" && (subset === null || subset.length > 0),
    retry: false,
  });
  const save = useMutation({
    mutationFn: (selection: Selection) => invoke<PackageDraft>("derive_workflow_reusable", {
      projectId, source, request: { key: key.trim() || slug(name), name: name.trim(), destination, selection },
    }),
    onMutate: () => onSaving?.(true),
    onSuccess: (draft) => onSaved(draft),
    onSettled: () => onSaving?.(false),
  });
  const locked = disabled || save.isPending;
  const ready = !locked && Boolean(name.trim() && (key.trim() || slug(name)));
  return active && <fieldset className="pipeline-stack" disabled={locked}>
    {disabled && <p>Save the current revision before deriving a reusable node.</p>}
    <label>Reusable type<select value={mode} onChange={(event) => setMode(event.currentTarget.value as Selection["type"])}>
      <option value="preset">Configured node preset</option><option value="subflow">Subflow</option>
    </select></label>
    <label>Name<input value={name} onChange={(event) => setName(event.currentTarget.value)} /></label>
    <label>Artifact key<input value={key} placeholder={slug(name)} onChange={(event) => setKey(event.currentTarget.value)} /></label>
    <label>Destination<select value={destination} onChange={(event) => setDestination(event.currentTarget.value as ReusableVersion["destination"])}>
      <option value="project">This project</option><option value="library">Reusable library</option>
    </select></label>
    {mode === "preset" ? <>
      <label>Configured node<select value={preset.nodeId} onChange={(event) =>
        setPreset({ nodeId: event.currentTarget.value, editableFields: [] })}>
        <option value="">Choose a saved node</option>
          {nodes.filter(isPresetEligible).map(({ id }) => <option key={id} value={id}>{id}</option>)}
      </select></label>
      {selectedNode && <fieldset className="pipeline-stack">
        <legend>Editable fields in consuming graphs</legend>
        {reusableEditableFields(selectedNode.configuration).map((field) => <label key={field}>
          <input type="checkbox" checked={preset.editableFields.includes(field)} onChange={(event) => {
            const checked = event.currentTarget.checked;
            setPreset((previous) => ({ ...previous, editableFields: checked ? [...previous.editableFields, field] :
              previous.editableFields.filter((item) => item !== field) }));
          }} /> {field}
        </label>)}
      </fieldset>}
      <Button type="button" variant="outline" disabled={!ready || !selectedNode}
        onClick={() => save.mutate({ type: "preset", ...preset })}>Save as preset</Button>
    </> : <>
      <label>Subflow source<select value={subset === null ? "whole" : "subset"}
        onChange={(event) => setSubset(event.currentTarget.value === "whole" ? null : [])}>
        <option value="whole">Entire saved graph</option><option value="subset">Selected nodes</option>
      </select></label>
      {subset !== null && <fieldset className="pipeline-stack"><legend>Internal nodes</legend>{nodes.map(({ id }) => <label key={id}>
        <input type="checkbox" checked={subset.includes(id)} onChange={(event) =>
          setSubset(event.currentTarget.checked ? [...subset, id] : subset.filter((item) => item !== id))} /> {id}
      </label>)}</fieldset>}
      {boundary.error && <p role="alert">{String(boundary.error)}</p>}
      {boundary.isLoading && <p role="status">Resolving saved ports…</p>}
      {boundary.data && <BoundaryMappings key={JSON.stringify([source, subset])} boundary={boundary.data}
        graph={graph} disabled={!ready} save={(inputs, outputs) => save.mutate({ type: "subflow", nodeIds: subset, inputs, outputs })} />}
    </>}
    {save.error && <p role="alert">{String(save.error)}</p>}
  </fieldset>;
}

function inputGroups(items: Boundary["inputs"]) {
  const groups = new Map<string, InputGroup>();
  for (const { wire, value } of items) {
    const key = endpointKey(wire.from);
    const existing = groups.get(key);
    if (existing) existing.wires.push(wire);
    else groups.set(key, { key, source: wire.from, wires: [wire], value });
  }
  return [...groups.values()];
}

function namesFor(entries: { key: string; label: string }[]) {
  const allocated = new Set<string>();
  return Object.fromEntries(entries.map(({ key, label }) => {
    const base = slug(label) || "port";
    let name = base;
    for (let number = 2; allocated.has(name); number += 1) name = `${base}-${number}`;
    allocated.add(name);
    return [key, name];
  }));
}

function BoundaryMappings({ boundary, graph, disabled, save }: {
  boundary: Boundary; graph: WorkflowGraph; disabled: boolean;
  save: (inputs: Record<string, WorkflowWire[]>, outputs: Record<string, WorkflowEndpoint>) => void;
}) {
  const groups = inputGroups(boundary.inputs);
  const [inputs, setInputs] = useState(() => namesFor(groups.map((entry) => ({ key: entry.key, label: entry.source.port }))));
  const [outputs, setOutputs] = useState(() => namesFor(boundary.outputs
    .filter(({ endpoint, consumedOutsideSelection }) => consumedOutsideSelection || !graph.wires.some(({ from }) => endpointKey(from) === endpointKey(endpoint)))
    .map(({ endpoint }) => ({ key: endpointKey(endpoint), label: endpoint.port }))));
  const mappedInputs = groups.map(({ key, wires }) => [inputs[key]?.trim() ?? "", wires] as const);
  const mappedOutputs = boundary.outputs.filter(({ endpoint }) => Object.hasOwn(outputs, endpointKey(endpoint)))
    .map(({ endpoint }) => [outputs[endpointKey(endpoint)]?.trim() ?? "", endpoint] as const);
  const unique = (names: string[]) => names.length > 0 && names.every(Boolean) && new Set(names).size === names.length;
  const valid = unique(mappedInputs.map(([name]) => name)) && unique(mappedOutputs.map(([name]) => name));
  return <div className="pipeline-stack">
    <fieldset className="pipeline-stack">
      <legend>Public inputs</legend>
      {groups.map(({ key, source, value, wires }) => <label key={key}>
        {endpointLabel(source)} → {wires.map(({ to }) => endpointLabel(to)).join(", ")} · {portType(value)}
        <input aria-label={`Public input name for ${endpointLabel(source)}`} value={inputs[key]}
          onChange={(event) => setInputs({ ...inputs, [key]: event.currentTarget.value })} />
      </label>)}
    </fieldset>
    <fieldset className="pipeline-stack">
      <legend>Public outputs</legend>
      {boundary.outputs.map(({ endpoint, value }) => {
        const key = endpointKey(endpoint);
        const included = Object.hasOwn(outputs, key);
        return <div key={key}>
          <label><input type="checkbox" checked={included} onChange={(event) => {
            if (event.currentTarget.checked) setOutputs({ ...outputs, [key]: slug(endpointLabel(endpoint)) || "output" });
            else { const next = { ...outputs }; delete next[key]; setOutputs(next); }
          }} /> {endpointLabel(endpoint)} · {portType(value)}</label>
          {included && <label>Public output name for {endpointLabel(endpoint)}
            <input value={outputs[key]} onChange={(event) => setOutputs({ ...outputs, [key]: event.currentTarget.value })} />
          </label>}
        </div>;
      })}
    </fieldset>
    {!valid && <p>Choose at least one input and output with distinct names in each list.</p>}
    <Button type="button" variant="outline" disabled={disabled || !valid}
      onClick={() => save(Object.fromEntries(mappedInputs), Object.fromEntries(mappedOutputs))}>Save as subflow</Button>
  </div>;
}

export function ReusableCatalog({ projectId, disabled, onAdd }: {
  projectId: string; disabled: boolean; onAdd: (reusable: ReusableDescription) => void;
}) {
  const [open, setOpen] = useState(false);
  const [choice, select] = useState("");
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId], enabled: open,
    queryFn: () => invoke<ArtifactSummary[]>("workflow_package_catalog", { projectId }),
  });
  const options = (catalog.data ?? []).filter(({ version }) => version.kind === "preset" || version.kind === "subflow");
  const selected = options.find(({ version }) => JSON.stringify(version) === choice);
  const load = useMutation({
    mutationFn: (version: ReusableVersion) => reusableNodeQuery(projectId, version).queryFn(),
    onSuccess: (description) => { onAdd(description); setOpen(false); select(""); },
  });
  return <Popover.Root open={open} onOpenChange={setOpen}>
    <Popover.Trigger asChild><Button type="button" variant="outline" disabled={disabled} aria-label="Add a reusable node">
      <Layers3 size={15} aria-hidden /> Reuse
    </Button></Popover.Trigger>
    <Popover.Portal>
      <Popover.Content align="end" sideOffset={7} className="workflow-reuse-popover pipeline-form pipeline-stack" aria-label="Add a reusable node">
      <label>Published reusable node<select value={choice} disabled={disabled || load.isPending}
        onChange={(event) => select(event.currentTarget.value)}>
        <option value="">Choose a Prompt, preset, or subflow</option>
        {options.map(({ name, version, prompt }) => <option key={JSON.stringify(version)} value={JSON.stringify(version)}>
          {name} · {prompt ? "Prompt" : version.kind} · {version.destination} · {version.revision.slice(0, 10)}
        </option>)}
      </select></label>
      {(catalog.error ?? load.error) && <p role="alert">{String(catalog.error ?? load.error)}</p>}
      <Button type="button" variant="outline" disabled={disabled || !selected || load.isPending}
        onClick={() => { if (selected) load.mutate(selected.version); }}>Add pinned node</Button>
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}

export function PublishedSubflow({ projectId, checkoutId, version }: {
  projectId: string; checkoutId?: string; version: ReusableVersion;
}) {
  return <Dialog.Root>
    <Dialog.Trigger asChild><Button type="button" variant="outline">Open pinned subflow</Button></Dialog.Trigger>
    <Dialog.Portal>
      <Dialog.Overlay className="fixed inset-0 z-50 bg-background/75 backdrop-blur-sm" />
      <Dialog.Content className="fixed left-1/2 top-1/2 z-50 max-h-[calc(100%-2rem)] w-[min(80rem,calc(100%-2rem))] -translate-x-1/2 -translate-y-1/2 overflow-auto rounded-lg border border-border bg-card p-5 shadow-lg">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <Dialog.Title className="text-lg font-semibold">Pinned subflow</Dialog.Title>
          <Dialog.Close asChild><Button type="button" variant="outline">Close pinned subflow</Button></Dialog.Close>
        </div>
        <Dialog.Description>Inspect the saved child graph and its typed connections.</Dialog.Description>
        <SubflowGraph projectId={projectId} checkoutId={checkoutId} version={version} />
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}

function SubflowGraph({ projectId, checkoutId, version }: {
  projectId: string; checkoutId?: string; version: ReusableVersion;
}) {
  const child = useQuery(reusableNodeQuery(projectId, version));
  const graph = child.data?.artifact.body.graph;
  if (child.error) return <p role="alert">{String(child.error)}</p>;
  if (!child.data) return <p role="status">Loading pinned subflow…</p>;
  if (!graph) return <p role="alert">The pinned subflow graph is unavailable.</p>;
  const { artifact, graphDefinitions } = child.data;
  return <section className="pipeline-stack" aria-label={`Pinned subflow ${artifact.name}`}>
    <p>{artifact.name} · <code className="break-all">{artifact.version.revision}</code></p>
    <WorkflowGraphEditor projectId={projectId} checkoutId={checkoutId}
      state={{ graph, layout: artifact.layout ?? {}, updateDefinitions: {} }} setState={() => undefined}
      definitions={graphDefinitions} catalog={[]} disabled problems={[]} allowDefinitionUpdates={false}
      inspectReference={(id) => {
        const reference = artifact.references[id];
        return reference?.source === "published" && reference.version.kind === "subflow" ?
          <PublishedSubflow key={JSON.stringify(reference.version)} projectId={projectId} checkoutId={checkoutId} version={reference.version} /> : null;
      }} />
  </section>;
}
