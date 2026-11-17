import { useState, type KeyboardEvent, type ReactNode } from "react";
import { Braces, ChevronRight, CornerDownRight, GitBranch, MessageSquareText, Plus, Trash2, UsersRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { portType, type WorkflowDefinition, type WorkflowDefinitions } from "@/workflow-graph-model";
import {
  availablePromptArtifacts, PROMPT_EXECUTORS, PROMPT_OUTPUTS, promptDataOptions, promptNodeDefinition, promptReferenceValue,
  type PromptContract, type PromptInput, type PromptMember, type PromptOutput, type PromptPortRename, type PromptRoleChoice,
  type PromptProposal,
} from "@/workflow-prompt-model";
import type { ReusableReference } from "@/workflow-reusable";
import "./workflow-prompt.css";

const artifactLabel = (kind: string) => portType({ kind: "data", artifact: kind });
const reservedPortNames = ["ready", "falhou", "decision", "outcome", "evidence"];
const validName = (name: string) => /^[a-z0-9_-]{1,64}$/.test(name);
const displayRoute = (name: string) => name === "concluido" ? "Completed" : name === "falhou" ? "Failed" : name.replace(/[_-]/g, " ");

function nextName(stem: string, occupied: readonly string[]) {
  let next = stem;
  for (let number = 2; occupied.includes(next); number += 1) next = stem + "-" + number;
  return next;
}

function mappedName<T>(values: Record<string, T>, key: string, next: string): Record<string, T> {
  return Object.fromEntries(Object.entries(values).map(([name, value]) => [name === key ? next : name, value]));
}

function without<T>(values: Record<string, T>, key: string): Record<string, T> {
  return Object.fromEntries(Object.entries(values).filter(([name]) => name !== key));
}

function RenameField({ label, value, occupied, disabled, onChange }: {
  label: string; value: string; occupied: readonly string[]; disabled: boolean; onChange: (name: string) => void;
}) {
  const [pending, setPending] = useState<string>();
  const nextName = (pending ?? value).trim();
  const invalid = !validName(nextName) || (nextName !== value && occupied.includes(nextName));
  const commit = () => {
    if (invalid) return;
    setPending(undefined);
    if (nextName !== value) onChange(nextName);
  };
  return <label>{label}
    <input value={pending ?? value} spellCheck={false} disabled={disabled} aria-invalid={invalid || undefined}
      onChange={(event) => setPending(event.currentTarget.value)} onBlur={commit}
      onKeyDown={(event: KeyboardEvent<HTMLInputElement>) => {
        if (event.key === "Enter") { event.preventDefault(); commit(); }
        if (event.key === "Escape") setPending(undefined);
      }} />
    {invalid && <small role="status">Use a unique name with letters, digits, hyphens or underscores.</small>}
  </label>;
}

function RoleField({ label, reference, choices, disabled, onChange }: {
  label: string; reference?: ReusableReference; choices: PromptRoleChoice[]; disabled: boolean;
  onChange: (reference?: ReusableReference) => void;
}) {
  const selected = promptReferenceValue(reference);
  const hasChoice = choices.some((item) => promptReferenceValue(item.reference) === selected);
  return <label>{label}
    <select disabled={disabled} value={selected} onChange={(event) =>
      onChange(choices.find((item) => promptReferenceValue(item.reference) === event.currentTarget.value)?.reference)}>
      <option value="">Choose a Role</option>
      {reference && !hasChoice && <option value={selected}>
        {reference.source === "published" ? "Pinned revision " + reference.version.revision.slice(0, 10)
          : reference.source === "parameter" ? "Binding " + reference.name : reference.key}
      </option>}
      {choices.map(({ name, reference }) => <option key={promptReferenceValue(reference)} value={promptReferenceValue(reference)}>{name}</option>)}
    </select>
  </label>;
}

type RolePortOption = { name: string; kind?: string };
function PortRows({ direction, ports, options, native, inputs, promptValueOutputs, disabled, occupied, update, rename }: {
  direction: "inputs" | "outputs"; ports: Record<string, PromptInput & PromptOutput>; options: string[];
  native: RolePortOption[]; inputs: Record<string, PromptInput>; promptValueOutputs: boolean; disabled: boolean; occupied: string[];
  update: (ports: Record<string, PromptInput & PromptOutput>) => void; rename: (from: string, to: string) => void;
}) {
  const input = direction === "inputs";
  const label = input ? "input" : "output";
  const nativeField = input ? "target" : "source";
  const mappings = input ? native : [
    ...native,
    ...Object.entries(inputs).map(([name, { kind }]) => ({ name: "input." + name, kind })),
  ];
  const available = Object.entries(ports);
  const nativeDefault = mappings.find(({ name }) => !input || !available.some(([, port]) => port.target === name));
  const add = () => {
    const name = nextName(label, occupied);
    const port = promptValueOutputs ? { kind: "json_object" }
      : { kind: nativeDefault?.kind ?? "json_object", [nativeField]: nativeDefault?.name };
    update({ ...ports, [name]: port });
  };
  return <div className="pipeline-stack">
    <h4>{input ? "Data inputs" : "Data outputs"}</h4>
    {available.map(([name, port], index) => {
      const mapping = port[nativeField];
      const boundKind = mappings.find((candidate) => candidate.name === mapping)?.kind;
      const kinds = boundKind ? [boundKind] : mapping || input ? options : [...PROMPT_OUTPUTS];
      const mappingChoices = mappings.filter((candidate) => !input || candidate.name === mapping ||
        !available.some(([other, port]) => other !== name && port.target === candidate.name));
      return <div key={name} className="workflow-prompt-settings-row">
        <RenameField label={`${input ? "Input" : "Output"} ${index + 1} name`} value={name} occupied={occupied} disabled={disabled}
          onChange={(next) => rename(name, next)} />
        <Button type="button" variant="ghost" className="ui-icon-button" disabled={disabled}
          aria-label={"Remove " + label + " " + name} onClick={() => update(without(ports, name))}><Trash2 size={15} aria-hidden /></Button>
        <label>Type for {name}<select value={port.kind} disabled={disabled || !!boundKind} onChange={(event) => {
          update({ ...ports, [name]: { ...port, kind: event.currentTarget.value } });
        }}>
          {!kinds.includes(port.kind) && <option value={port.kind}>{artifactLabel(port.kind)}</option>}
          {kinds.map((kind) => <option key={kind} value={kind}>{artifactLabel(kind)}</option>)}
        </select></label>
        <label>{input ? "Pass to " : "Read from "}{name}<select value={mapping ?? ""} disabled={disabled} onChange={(event) => {
          const target = event.currentTarget.value;
          const chosen = mappingChoices.find((candidate) => candidate.name === target);
          const kind = chosen?.kind ?? (!input && !target && !PROMPT_OUTPUTS.some((kind) => kind === port.kind) ? "json_object" : port.kind);
          update({ ...ports, [name]: { kind, ...(target && { [nativeField]: target }) } });
        }}>
          {promptValueOutputs && <option value="">{input ? "Prompt context" : "Structured response"}</option>}
          {mapping && !mappingChoices.some((candidate) => candidate.name === mapping) &&
            <option value={mapping}>{mapping} (unavailable)</option>}
          {!promptValueOutputs && !mapping && <option value="">Choose a native port</option>}
          {mappingChoices.map(({ name }) => <option value={name} key={name}>{name.replace("input.", "Input · ")}</option>)}
        </select></label>
      </div>;
    })}
    {!disabled && <Button type="button" variant="outline" className="w-fit" onClick={add} disabled={available.length >= 32 || (!promptValueOutputs && !nativeDefault)}>
      <Plus size={14} aria-hidden /> Add {label}
    </Button>}
  </div>;
}

function PromptSection({ title, count, icon: Icon, children }: {
  title: string; count?: number; icon: typeof Braces; children: ReactNode;
}) {
  return <details className="workflow-inspector-group">
    <summary><Icon size={15} aria-hidden /> {title}{count !== undefined && <span>{count}</span>}<ChevronRight size={14} aria-hidden /></summary>
    <div className="pipeline-stack">{children}</div>
  </details>;
}

function PromptSchemaField({ schema, disabled, onChange }: {
  schema?: PromptContract["outputSchema"]; disabled: boolean; onChange: (schema?: PromptContract["outputSchema"]) => void;
}) {
  const [editing, edit] = useState<string>();
  const [error, setError] = useState(false);
  const saved = schema ? JSON.stringify(schema, null, 2) : "";
  function apply() {
    try {
      const value: unknown = editing?.trim() ? JSON.parse(editing) : undefined;
      if (value !== undefined && (!value || typeof value !== "object" || Array.isArray(value))) throw new Error();
      onChange(value as PromptContract["outputSchema"]);
      edit(undefined);
      setError(false);
    } catch {
      setError(true);
    }
  }
  return <details className="pipeline-evidence">
    <summary>Structured response schema</summary>
    <div className="pipeline-stack pt-2">
      <label>Output JSON schema
        <textarea className="font-mono text-xs" disabled={disabled} value={editing ?? saved}
          onChange={(event) => { edit(event.currentTarget.value); setError(false); }} />
      </label>
      {error && <p role="alert">Enter a JSON object describing the structured output.</p>}
      {!disabled && <Button type="button" variant="outline" className="w-fit" disabled={editing === undefined || editing === saved}
        onClick={apply}>Apply schema</Button>}
    </div>
  </details>;
}

export function PromptInspector({ member, choices, catalog, disabled, onChange, technical, actions, suggestion }: {
  member: PromptMember; choices: PromptRoleChoice[]; catalog: WorkflowDefinition[]; disabled: boolean;
  onChange: (member: PromptMember, rename?: PromptPortRename) => void;
  technical?: WorkflowDefinitions; actions?: ReactNode;
  suggestion?: (onApply: (proposal: PromptProposal) => void) => ReactNode;
}) {
  const { prompt } = member.body;
  const refs = member.references ?? {};
  const executor = catalog.find(({ kind }) => kind === prompt.executor);
  const availableTypes = availablePromptArtifacts(catalog);
  const nativeInputs = promptDataOptions(executor?.inputs).filter(({ name }) => prompt.executor !== "agent" || name !== "prompt");
  const subjects = Object.values(prompt.inputs).map(({ kind }) => kind);
  const reviewer = subjects.includes("candidate") ? "review_candidate" : subjects.includes("git_inspection") ? "review_diff_candidate"
    : subjects.includes("plan") ? "review_plan" : "agent";
  const reviewDefinition = catalog.find(({ kind }) => kind === reviewer);
  const nativeOutputs = [...promptDataOptions(executor?.outputs),
    ...prompt.delegates.flatMap(({ alias }) => promptDataOptions(reviewDefinition?.outputs).map((port) => ({ ...port, name: alias + "." + port.name }))),
  ];
  const standard = prompt.executor === "agent";
  const routeChoice = standard || prompt.routes.length !== 1 || prompt.routes[0] !== "concluido";
  const occupiedInputs = [...Object.keys(prompt.inputs), "ready"];
  const occupiedOutputs = [...Object.keys(prompt.outputs), ...prompt.routes, ...reservedPortNames.slice(1)];
  const missingRole = !refs.role;
  const missingSpecialist = prompt.delegates.some(({ alias }) => !refs["delegate-" + alias]);
  function update(next: PromptContract, references = refs, rename?: PromptPortRename) {
    onChange({ ...member, body: { ...member.body, prompt: next }, references }, rename);
  }
  function updateRole(slot: string, reference?: ReusableReference) {
    update(prompt, reference ? { ...refs, [slot]: reference } : without(refs, slot));
  }
  function renamePort(direction: "inputs" | "outputs", from: string, to: string) {
    const next = { ...prompt, [direction]: mappedName(prompt[direction] as Record<string, PromptInput & PromptOutput>, from, to) };
    if (direction === "inputs") {
      next.outputs = Object.fromEntries(Object.entries(prompt.outputs).map(([name, output]) =>
        [name, output.source === "input." + from ? { ...output, source: "input." + to } : output]));
    }
    update(next, refs, { direction, from, to });
  }
  const base = catalog.find(({ kind }) => kind === "subflow");
  const definition = base && promptNodeDefinition(member.name, prompt, base);
  return <div aria-label="Prompt settings" className="workflow-prompt-inspector pipeline-form pipeline-stack">
    <label>Card title<input value={member.name} disabled={disabled}
      onChange={(event) => onChange({ ...member, name: event.currentTarget.value })} /></label>
    <RoleField label="Role revision" reference={refs.role} choices={choices} disabled={disabled}
      onChange={(reference) => updateRole("role", reference)} />
    <label>Prompt<textarea value={prompt.text} disabled={disabled} placeholder="Describe the work and expected result."
      onChange={(event) => update({ ...prompt, text: event.currentTarget.value })} /></label>
    <label>Work type<select value={prompt.executor} disabled={disabled} onChange={(event) =>
      update({ ...prompt, executor: event.currentTarget.value as PromptContract["executor"] })}>
      {Object.entries(PROMPT_EXECUTORS).map(([kind, label]) => <option key={kind} value={kind}>{label}</option>)}
    </select></label>
    {!disabled && missingRole && <p role="status">{choices.length
      ? "Choose the Role that runs this prompt." : "Create a Role in Roles & templates, then select its revision here."}</p>}

    <PromptSection title="Inputs and outputs" icon={Braces} count={Object.keys(prompt.inputs).length + Object.keys(prompt.outputs).length}>
      {!disabled && suggestion?.((proposal) => update({ ...prompt, ...proposal }))}
      <PortRows direction="inputs" ports={prompt.inputs} options={availableTypes} native={nativeInputs} inputs={prompt.inputs}
        promptValueOutputs={standard} disabled={disabled} occupied={occupiedInputs} update={(inputs) => update({ ...prompt, inputs })}
        rename={(from, to) => renamePort("inputs", from, to)} />
      <PortRows direction="outputs" ports={prompt.outputs} options={availableTypes} native={nativeOutputs} inputs={prompt.inputs}
        promptValueOutputs={standard} disabled={disabled} occupied={occupiedOutputs} update={(outputs) => update({ ...prompt, outputs })}
        rename={(from, to) => renamePort("outputs", from, to)} />
      {standard && <PromptSchemaField schema={prompt.outputSchema} disabled={disabled}
        onChange={(outputSchema) => update({ ...prompt, outputSchema })} />}
    </PromptSection>

    <PromptSection title="Routes" icon={GitBranch} count={prompt.routes.length + 1}>
      {routeChoice ? prompt.routes.map((name, index) => <div key={name} className="workflow-prompt-settings-row">
        <RenameField label={"Route " + (index + 1)} value={name} occupied={occupiedOutputs} disabled={disabled}
          onChange={(to) => update({ ...prompt, routes: prompt.routes.map((route) => route === name ? to : route) },
            refs, { direction: "outputs", from: name, to })} />
        <Button type="button" variant="ghost" className="ui-icon-button" disabled={disabled || prompt.routes.length <= 1}
          aria-label={"Remove route " + name} onClick={() => update({ ...prompt, routes: prompt.routes.filter((route) => route !== name) })}>
          <Trash2 size={15} aria-hidden />
        </Button>
      </div>) : <p>Completed</p>}
      <p className="workflow-prompt-help"><strong>Failed</strong> · Runtime errors and invalid responses</p>
      {!disabled && standard && <Button type="button" variant="outline" className="w-fit"
        disabled={prompt.routes.length >= 12} onClick={() => update({ ...prompt, routes: [...prompt.routes, nextName("route", occupiedOutputs)] })}>
        <Plus size={14} aria-hidden /> Add route
      </Button>}
    </PromptSection>

    <PromptSection title="Invited Roles" icon={UsersRound} count={prompt.delegates.length}>
      {!standard && <p className="workflow-prompt-help">Use General instructions to dispatch read-only specialists.</p>}
      {prompt.delegates.map(({ alias, required }, index) => <div key={alias} className="workflow-prompt-settings-row">
        <RenameField label={"Alias " + (index + 1)} value={alias} disabled={disabled} occupied={["input", ...prompt.delegates.map(({ alias }) => alias)]}
          onChange={(next) => update({
            ...prompt,
            delegates: prompt.delegates.map((delegate) => delegate.alias === alias ? { ...delegate, alias: next } : delegate),
            outputs: Object.fromEntries(Object.entries(prompt.outputs).map(([name, output]) =>
              [name, output.source?.startsWith(alias + ".") ? { ...output, source: next + output.source.slice(alias.length) } : output])),
          }, mappedName(refs, "delegate-" + alias, "delegate-" + next))} />
        <Button type="button" variant="ghost" className="ui-icon-button" disabled={disabled}
          aria-label={"Remove invited Role " + alias} onClick={() =>
            update({ ...prompt, delegates: prompt.delegates.filter((delegate) => delegate.alias !== alias) }, without(refs, "delegate-" + alias))}>
          <Trash2 size={15} aria-hidden />
        </Button>
        <RoleField label={"Role for " + alias} reference={refs["delegate-" + alias]} choices={choices} disabled={disabled}
          onChange={(reference) => updateRole("delegate-" + alias, reference)} />
        <label className="workflow-check"><input type="checkbox" checked={required} disabled={disabled} onChange={(event) =>
          update({ ...prompt, delegates: prompt.delegates.map((delegate) =>
            delegate.alias === alias ? { ...delegate, required: event.currentTarget.checked } : delegate) })} /> Always call</label>
      </div>)}
      {!disabled && missingSpecialist && <p role="status">Choose a Role revision for each invited Role.</p>}
      {!disabled && standard && <Button type="button" variant="outline" className="w-fit" disabled={prompt.delegates.length >= 16}
        onClick={() => update({ ...prompt, delegates: [...prompt.delegates, { alias: nextName("specialist", prompt.delegates.map(({ alias }) => alias)), required: false }],
      })}><Plus size={14} aria-hidden /> Add invited Role</Button>}
    </PromptSection>

    <PromptSection title="Technical details" icon={CornerDownRight}>
      <div><strong>Native executor</strong> · {executor?.title ?? prompt.executor}</div>
      {definition && <ul className="workflow-prompt-technical-list">{definition.outputs.map(({ name, value }) =>
        <li key={name}>{value.kind === "control" ? displayRoute(name) : name}<code>{name} · {portType(value)}</code></li>)}</ul>}
      {technical && Object.keys(technical).length > 0 && <div><strong>Expanded stages</strong>
        <ul className="workflow-prompt-technical-list">{Object.entries(technical).map(([id, definition]) => <li key={id}>
          {definition.title}<code>{id}</code>
        </li>)}</ul>
      </div>}
    </PromptSection>
    {actions}
  </div>;
}

export function PromptRevisionView({ children }: { children: ReactNode }) {
  return <section className="workflow-prompt-revision-view" aria-label="Reusable Prompt">
    <div className="workflow-inspector-heading"><span className="workflow-inspector-icon"><MessageSquareText size={17} aria-hidden /></span>
      <div><h2>Reusable Prompt</h2></div>
    </div>
    {children}
  </section>;
}
