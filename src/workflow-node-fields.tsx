import { ArrowDown, ArrowUp, Plus, Trash2 } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { LIVE_MODEL_PROVIDERS, sameModelSelection, useLiveModelOptions } from "@/live-models";
import { CONVERSATION_START_INPUT_TYPES, defaultConfiguration, MAX_ITERATION_ITEMS, portType, START_VALUE_TYPES,
  type ConversationStartSource, type SetFieldOperation, type WorkflowDefinition, type WorkflowNodeConfiguration, type WorkflowPort,
  type WorkflowPredicate, type WorkflowScalar, type WorkflowStartField } from "@/workflow-graph-model";
import { StructuredListField, StructuredValueField, structuredObject } from "@/workflow-structured-fields";

type Fields = {
  configuration: WorkflowNodeConfiguration;
  disabled: boolean;
  onChange: (configuration: WorkflowNodeConfiguration) => void;
};

function text(value: unknown) {
  return typeof value === "string" ? value : "";
}

export function StartInputFields({ configuration, disabled, onChange }: Fields) {
  const fields: WorkflowStartField[] = Array.isArray(configuration.fields) ? configuration.fields : [];
  const fromRequest = configuration.promptOrMarkdown === true;
  const update = (fields: WorkflowStartField[], promptOrMarkdown = fromRequest) =>
    onChange({ kind: "typed_start", fields, ...(promptOrMarkdown ? { promptOrMarkdown } : {}) });
  function add() {
    let ordinal = fields.length + 1;
    while (fields.some(({ name }) => name === `input-${ordinal}`)) ordinal += 1;
    update([...fields, { name: `input-${ordinal}`, valueType: "text", required: true }]);
  }
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>Launch inputs</legend>
    <label className="workflow-check"><input type="checkbox" checked={fromRequest}
      onChange={(event) => update(fields, event.currentTarget.checked)} />Accept a prompt or saved Markdown</label>
    {fromRequest && <p className="pipeline-meta">The inputs port provides the request as <code>/request</code>.</p>}
    {fields.map((field, index) => {
      const replace = (next: WorkflowStartField) => update(fields.map((entry, position) => position === index ? next : entry));
      const edit = (patch: Partial<WorkflowStartField>) => replace({ ...field, ...patch });
      return <div className="workflow-inspector-item" key={index}>
        <label>Input {index + 1} name<input value={field.name} onChange={(event) => edit({ name: event.currentTarget.value })} /></label>
        <label>Input {index + 1} source<select value={field.conversationSource ?? ""} onChange={(event) => {
          const source = event.currentTarget.value as ConversationStartSource | "";
          if (source) edit({ conversationSource: source, valueType: CONVERSATION_START_INPUT_TYPES[source] });
          else replace({ name: field.name, valueType: field.valueType, required: field.required });
        }}>
          <option value="">At launch</option>
          <option value="message">Conversation message</option>
          <option value="history">Conversation history</option>
        </select></label>
        <label>Input {index + 1} type<select value={field.valueType} disabled={Boolean(field.conversationSource)}
          onChange={(event) => edit({ valueType: event.currentTarget.value as WorkflowStartField["valueType"] })}>
          {START_VALUE_TYPES.map((type) => <option value={type} key={type}>{type[0]!.toUpperCase()}{type.slice(1)}</option>)}
        </select></label>
        {field.conversationSource && <small className="pipeline-meta">{field.conversationSource === "message"
          ? "Uses the message that starts this run." : "Uses earlier messages in order, with each message’s role and text."} Manual runs supply this input at launch.</small>}
        <label className="workflow-check"><input type="checkbox" checked={field.required}
          onChange={(event) => edit({ required: event.currentTarget.checked })} />Required at launch</label>
        <Button type="button" variant="ghost" aria-label={`Remove input ${index + 1}`}
          onClick={() => update(fields.filter((_, position) => position !== index))}><Trash2 size={15} /> Remove input</Button>
      </div>;
    })}
    <Button type="button" variant="outline" onClick={add}><Plus size={15} /> Add input</Button>
  </fieldset>;
}

function PredicateFields({ value, label, disabled, onChange, allowChecks = true }: {
  value: unknown; label: string; disabled: boolean; onChange: (value: WorkflowPredicate) => void; allowChecks?: boolean;
}) {
  const { op, pointer, value: expected } = structuredObject(value);
  const operation = op === "equals" || (allowChecks && op === "candidate_checks_passed") ? op : "exists";
  const path = text(pointer);
  const literal: WorkflowScalar = expected === null || typeof expected === "boolean" || typeof expected === "number" ? expected : text(expected);

  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>{label}</legend>
    <label>Test<select value={operation} onChange={(event) => {
      const next = event.currentTarget.value;
      if (next === "candidate_checks_passed") onChange({ op: next });
      else if (next === "equals") onChange({ op: next, pointer: path, value: literal });
      else onChange({ op: "exists", pointer: path });
    }}>
      <option value="exists">Field exists</option><option value="equals">Field equals</option>
      {allowChecks && <option value="candidate_checks_passed">Candidate checks passed</option>}
    </select></label>
    {operation !== "candidate_checks_passed" && <label>Field path<input value={path} placeholder="/evidence/status"
      onChange={(event) => onChange(operation === "equals" ? { op: operation, pointer: event.currentTarget.value, value: literal }
        : { op: operation, pointer: event.currentTarget.value })} /></label>}
    {operation === "equals" && <StructuredValueField label="Expected value" value={literal} disabled={disabled} scalar
      onChange={(next) => onChange({ op: operation, pointer: path, value: next as WorkflowScalar })} />}
  </fieldset>;
}

export function ConditionFields({ configuration, disabled, onChange }: Fields) {
  return <PredicateFields label="Branch condition" value={configuration.predicate} disabled={disabled}
    onChange={(predicate) => onChange({ kind: "condition", predicate })} />;
}

export function BuildListFields({ configuration, disabled, fixedSource, onChange }: Fields & { fixedSource: boolean }) {
  const external = configuration.source === "evidence";
  return <>
    <label>List source<select disabled={disabled || fixedSource} value={external ? "evidence" : "values"}
      onChange={(event) => onChange(event.currentTarget.value === "evidence"
        ? { kind: "build_list", source: "evidence", pointer: "/evidence" }
        : defaultConfiguration("build_list"))}>
      <option value="values">Configured items</option><option value="evidence">Source artifact</option>
    </select></label>
    {external ? <label>Array path<input disabled={disabled} value={text(configuration.pointer)} placeholder="/evidence/items"
      onChange={(event) => onChange({ ...configuration, pointer: event.currentTarget.value })} />
      <small>Connect the Value input to the source artifact.</small>
    </label> : <StructuredListField label="List items" value={configuration.values} disabled={disabled} limit={MAX_ITERATION_ITEMS}
      onChange={(values) => onChange({ kind: "build_list", source: "values", values })} />}
  </>;
}

function SubflowPortField({ label, value, ports, disabled, onChange }: {
  label: string; value: string; ports?: WorkflowPort[]; disabled: boolean; onChange: (value: string) => void;
}) {
  return ports ? <label>{label}<select disabled={disabled} value={value} onChange={(event) => onChange(event.currentTarget.value)}>
    <option value="">Choose a port</option>
    {value && !ports.some(({ name }) => name === value) && <option value={value} disabled>{value} (unavailable)</option>}
    {ports.map(({ name, value }) => <option key={name} value={name}>{name} · {portType(value)}</option>)}
  </select></label> : <label>{label}<input disabled={disabled} value={value} onChange={(event) => onChange(event.currentTarget.value)} /></label>;
}

const control = ({ value }: WorkflowPort) => value.kind === "control";
const data = ({ value }: WorkflowPort) => value.kind === "data";
const jsonItem = ({ value }: WorkflowPort) => value.kind === "data" && value.artifact === "json_item";

export function LoopFields({ configuration, disabled, subflow, onChange }: Fields & { subflow?: WorkflowDefinition }) {
  const repeat = configuration.kind === "repeat_until";
  const limit = repeat ? "maxIterations" : "maxItems";
  const feedback = structuredObject(configuration.feedback);
  const feedbackInput = subflow?.inputs.find(({ name }) => name === feedback.input)?.value;
  const exports = Array.isArray(configuration.exports) ? configuration.exports.filter((name): name is string => typeof name === "string") : [];
  const exportOptions = subflow?.outputs.filter(({ name, value }) => value.kind === "data" && !["completed", "failed"].includes(name))
    .map(({ name }) => name) ?? [];
  return <>
    <label>{repeat ? "Maximum iterations" : "Maximum items"}<input type="number" min={1} max={MAX_ITERATION_ITEMS} step={1} disabled={disabled}
      value={typeof configuration[limit] === "number" ? configuration[limit] as number : ""}
      onChange={(event) => onChange({ ...configuration, [limit]: event.currentTarget.value === "" ? null : event.currentTarget.valueAsNumber })} /></label>
    <SubflowPortField label="Subflow ready input" value={text(configuration.readyInput)} ports={subflow?.inputs.filter(control)}
      disabled={disabled} onChange={(readyInput) => onChange({ ...configuration, readyInput })} />
    {!repeat && <SubflowPortField label="Subflow item input" value={text(configuration.itemInput)} ports={subflow?.inputs.filter(jsonItem)}
      disabled={disabled} onChange={(itemInput) => onChange({ ...configuration, itemInput })} />}
    <SubflowPortField label="Subflow completion output" value={text(configuration.completionOutput)} ports={subflow?.outputs.filter(control)}
      disabled={disabled} onChange={(completionOutput) => onChange({ ...configuration, completionOutput })} />
    {repeat ? <>
      <SubflowPortField label="Subflow evidence output" value={text(configuration.evidenceOutput)} ports={subflow?.outputs.filter(data)}
        disabled={disabled} onChange={(evidenceOutput) => onChange({ ...configuration, evidenceOutput })} />
      <PredicateFields label="Stopping condition" value={configuration.until} disabled={disabled}
        onChange={(until) => onChange({ ...configuration, until })} />
      {!!(exportOptions.length || exports.length) && <fieldset disabled={disabled}>
        <legend>Export from the approved iteration</legend>
        {[...new Set([...exportOptions, ...exports])].map((name) => <label className="workflow-check" key={name}>
          <input type="checkbox" checked={exports.includes(name)}
            onChange={(event) => {
              const next = event.currentTarget.checked ? [...exports, name] : exports.filter((entry) => entry !== name);
              const { exports: _previous, ...rest } = configuration;
              onChange(next.length ? { ...rest, exports: next } : rest);
            }} />{name}
        </label>)}
      </fieldset>}
      <label className="workflow-check"><input type="checkbox" checked={Boolean(configuration.feedback)} disabled={disabled}
        onChange={(event) => {
          const { feedback: _previous, ...rest } = configuration;
          onChange(event.currentTarget.checked ? { ...rest, feedback: { input: "", output: "" } } : rest);
        }} />Pass a result to the next iteration</label>
      {!!configuration.feedback && <>
        <SubflowPortField label="Feedback input" value={text(feedback.input)} ports={subflow?.inputs.filter(data)} disabled={disabled}
          onChange={(input) => onChange({ ...configuration, feedback: { input, output: "" } })} />
        <SubflowPortField label="Feedback output" value={text(feedback.output)} ports={subflow?.outputs.filter(({ value }) =>
          feedbackInput?.kind === "data" && value.kind === "data" && value.artifact === feedbackInput.artifact)} disabled={disabled}
          onChange={(output) => onChange({ ...configuration, feedback: { ...feedback, output } })} />
      </>}
    </> : <SubflowPortField label="Subflow result output" value={text(configuration.resultOutput)} ports={subflow?.outputs.filter(data)}
      disabled={disabled} onChange={(resultOutput) => onChange({ ...configuration, resultOutput })} />}
  </>;
}

export function RunBacklogFields({ projectId, configuration, backlogBound, disabled, onChange }: Fields & {
  projectId: string;
  backlogBound?: boolean;
}) {
  type Child = { name: string; version: { id: string; revision: string };
    start?: { fields: WorkflowStartField[]; requiresBacklogPlan?: boolean } };
  const query = useQuery({ queryKey: ["workflow-authoring-bases", projectId], enabled: !backlogBound,
    queryFn: () => invoke<Child[]>("workflow_authoring_bases", { projectId }) });
  const choices = (query.data ?? []).filter(({ start }) =>
    start?.requiresBacklogPlan && start.fields.some(({ name, valueType }) => name === "backlog" && valueType === "object"));
  const child = structuredObject(configuration.child);
  const failed = structuredObject(configuration.failed);
  const key = (version: { id?: unknown; revision?: unknown }) => JSON.stringify([version.id, version.revision]);
  const selected = text(child.id) ? key(child) : "";
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>Child workflow dispatch</legend>
    {!backlogBound && <label>Published child revision<select value={selected} onChange={(event) => {
      const revision = choices.find(({ version }) => key(version) === event.currentTarget.value)?.version;
      onChange({ ...configuration, child: revision ?? { id: "", revision: "" } });
    }}>
      <option value="">Choose a workflow with a backlog input</option>
      {selected && !choices.some(({ version }) => key(version) === selected) &&
        <option value={selected}>{text(child.id)} · {text(child.revision).slice(0, 12)}</option>}
      {choices.map(({ name, version }) => <option key={key(version)} value={key(version)}>
        {name} · {version.revision.slice(0, 12)}
      </option>)}
    </select></label>}
    <label>Maximum parallel tasks<input type="number" min={1} max={MAX_ITERATION_ITEMS} step={1}
      value={typeof configuration.maxParallel === "number" ? configuration.maxParallel : ""}
      onChange={(event) => onChange({ ...configuration, maxParallel: event.currentTarget.value === "" ? null : event.currentTarget.valueAsNumber })} /></label>
    <label>Child failure node<input value={text(failed.node)}
      onChange={(event) => onChange({ ...configuration, failed: { ...failed, node: event.currentTarget.value } })} /></label>
    <label>Child failure control port<input value={text(failed.port)}
      onChange={(event) => onChange({ ...configuration, failed: { ...failed, port: event.currentTarget.value } })} /></label>
  </fieldset>;
}

export function TestPhaseFields({ projectId, configuration, disabled, onChange }: Fields & { projectId: string }) {
  const policy = useQuery({ queryKey: ["candidate-gate-policy", projectId],
    queryFn: () => invoke<{ gates: { name: string; action: { type: string } }[] }>("candidate_gate_policy", { projectId }) });
  const check = text(configuration.check);
  const options = (policy.data?.gates ?? []).filter(({ action }) => action.type === "command").map(({ name }) => name);
  const paths = Array.isArray(configuration.allowedTestPaths) ? configuration.allowedTestPaths.join("\n") : "";
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>Test verification</legend>
    <label>Test phase<select value={text(configuration.phase)} onChange={(event) => {
      const phase = event.currentTarget.value;
      const { allowedTestPaths, ...current } = configuration;
      onChange({ ...current, phase, ...(phase === "red" ? { allowedTestPaths: allowedTestPaths ?? [] } : {}) });
    }}>
      <option value="baseline">Baseline</option>
      <option value="red">Red</option>
      <option value="green">Green</option>
    </select></label>
    <label>Registered test check<select value={check} onChange={(event) =>
      onChange({ ...configuration, check: event.currentTarget.value })}>
      <option value="">Choose a project check</option>
      {check && !options.includes(check) && <option value={check}>{check}</option>}
      {options.map((name) => <option key={name} value={name}>{name}</option>)}
    </select></label>
    {configuration.phase === "red" && <label>Allowed test paths<textarea rows={3} value={paths}
      onChange={(event) => onChange({ ...configuration, allowedTestPaths: event.currentTarget.value.split("\n") })} />
      <small>One file, directory prefix such as tests/, or suffix such as *.test.ts per line.</small>
    </label>}
    <p>The check must write one JSON report to stdout with the suite status and test IDs, statuses and failure reasons.</p>
    <details>
      <summary>Test report format</summary>
      <pre className="workflow-evidence-json">{JSON.stringify({
        status: "completed",
        tests: [{ id: "BDD-1", status: "failed", reason: "feature missing" }],
      }, null, 2)}</pre>
      <p>Use passed or skipped for other test outcomes. Use build_error, environment_error or tool_error for a suite that could not run.</p>
    </details>
  </fieldset>;
}

export function IntegratedChecksFields({ projectId, configuration, disabled, onChange }: Fields & { projectId: string }) {
  const policy = useQuery({ queryKey: ["candidate-gate-policy", projectId],
    queryFn: () => invoke<{ gates: { name: string; action: { type: string } }[] }>("candidate_gate_policy", { projectId }) });
  const checks = Array.isArray(configuration.checks) ? configuration.checks.filter((value) => typeof value === "string") : [];
  const registered = (policy.data?.gates ?? []).filter(({ action }) => action.type === "command").map(({ name }) => name);
  const options = [...new Set([...checks, ...registered])];
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>Integrated revision checks</legend>
    {options.map((name) => <label key={name} className="workflow-check">
      <input type="checkbox" checked={checks.includes(name)} onChange={(event) =>
        onChange({ ...configuration, checks: event.currentTarget.checked ? [...checks, name] : checks.filter((check) => check !== name) })} />
      {name}
    </label>)}
    {!policy.isPending && !policy.isError && options.length === 0 && <p>Register command checks in the project settings.</p>}
    {policy.isError && <p role="alert">The project checks could not be loaded.</p>}
  </fieldset>;
}

export function CandidateAcceptanceFields({ configuration, disabled, onChange }: Fields) {
  const aggregated = configuration.kind === "candidate_acceptance";
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>Acceptance evidence</legend>
    <label className="workflow-check"><input type="checkbox" checked={aggregated}
      onChange={(event) => onChange(event.currentTarget.checked
        ? { kind: "candidate_acceptance", buildCheck: "", minimumReviews: 1, requireGreen: false } : { kind: "none" })} />
      Require aggregate quality evidence and a build
    </label>
    {aggregated && <>
      <label>Required build check<input value={text(configuration.buildCheck)}
        onChange={(event) => onChange({ ...configuration, buildCheck: event.currentTarget.value })} /></label>
      <label>Minimum independent reviews<input type="number" min={1} max={32} step={1}
        value={typeof configuration.minimumReviews === "number" ? configuration.minimumReviews : ""}
        onChange={(event) => onChange({ ...configuration, minimumReviews: event.currentTarget.value === "" ? null : event.currentTarget.valueAsNumber })} /></label>
      <label className="workflow-check"><input type="checkbox" checked={configuration.requireGreen === true}
        onChange={(event) => onChange({ ...configuration, requireGreen: event.currentTarget.checked })} />Require verified green tests</label>
    </>}
  </fieldset>;
}

export function RequiredChecksFields({ configuration, disabled, fixedNames, fixedGroups, stageOptions, strategyOptions, onChange }: Fields & {
  fixedNames: boolean;
  fixedGroups: boolean;
  stageOptions: boolean;
  strategyOptions: boolean;
}) {
  const groups: string[][] = Array.isArray(configuration.parallelGroups) ? configuration.parallelGroups : [];
  const update = (parallelGroups: string[][]) => {
    const { parallelGroups: _previous, ...rest } = configuration;
    onChange(parallelGroups.length ? { ...rest, parallelGroups } : rest);
  };
  return <>
    <label>Required checks<textarea rows={3} disabled={disabled || fixedNames}
      value={Array.isArray(configuration.names) ? configuration.names.join("\n") : ""}
      onChange={(event) => onChange({ ...configuration, names: event.currentTarget.value ? event.currentTarget.value.split("\n") : [] })} />
      <small>Enter one check name per line.</small>
    </label>
    <fieldset className="pipeline-stack" disabled={disabled || fixedGroups}>
      <legend>Parallel check groups</legend>
      {groups.map((group, index) => <div key={index} className="workflow-inspector-item">
        <label>Group {index + 1}<textarea rows={2} value={group.join("\n")}
          onChange={(event) => update(groups.map((current, position) => position === index
            ? event.currentTarget.value.split("\n") : current))} /></label>
        <Button type="button" variant="ghost" aria-label={"Remove group " + (index + 1)}
          onClick={() => update(groups.filter((_, position) => position !== index))}><Trash2 size={15} /> Remove group</Button>
      </div>)}
      <Button type="button" variant="outline" onClick={() => update([...groups, [""]])}><Plus size={15} /> Add check group</Button>
      {!!groups.length && <small>Checks in each group run in order. Groups run independently.</small>}
    </fieldset>
    {stageOptions && <label>Checks deferred to a later stage<textarea rows={2} disabled={disabled}
      value={Array.isArray(configuration.deferredNames) ? configuration.deferredNames.join("\n") : ""}
      onChange={(event) => {
        const { deferredNames: _previous, strategyChecks, ...current } = configuration;
        const value = event.currentTarget.value;
        onChange(value ? { ...current, kind: "staged_checks", deferredNames: value.split("\n"),
          ...(Array.isArray(strategyChecks) ? { strategyChecks } : {}) }
          : { ...current, kind: "required_checks" });
      }} />
      <small>The acceptance step still requires the complete project policy.</small>
    </label>}
    {strategyOptions && configuration.kind === "staged_checks" && <label>Checks selectable by approved test strategy
      <textarea rows={2} disabled={disabled}
        value={Array.isArray(configuration.strategyChecks) ? configuration.strategyChecks.join("\n") : ""}
        onChange={(event) => {
          const { strategyChecks: _previous, ...current } = configuration;
          const value = event.currentTarget.value;
          onChange(value ? { ...current, strategyChecks: value.split("\n") } : current);
        }} />
      <small>Connect a strategy object with a requiredChecks array to select any of these registered checks.</small>
    </label>}
  </>;
}

const emptySetOperation = (): SetFieldOperation => ({ op: "set", path: "", value: { source: "literal", value: "" } });

export function SetFieldsFields({ configuration, disabled, onChange }: Fields) {
  const operations: SetFieldOperation[] = Array.isArray(configuration.operations) ? configuration.operations : [];
  const update = (next: SetFieldOperation[]) => onChange({ ...configuration, operations: next });
  return <>
    <label>Object source<select disabled={disabled} value={configuration.mode === "update" ? "update" : "build"}
      onChange={(event) => onChange({ ...configuration, mode: event.currentTarget.value })}>
      <option value="build">Build an object</option><option value="update">Update the source object</option>
    </select></label>
    <fieldset className="pipeline-stack" disabled={disabled}>
      <legend>Field operations</legend>
      {operations.map((operation, index) => {
        const edit = (value: SetFieldOperation) => update(operations.map((item, i) => i === index ? value : item));
        return <div className="workflow-inspector-item" key={index}>
          <label>Operation {index + 1}<select value={operation.op} onChange={(event) => edit(event.currentTarget.value === "rename"
            ? { op: "rename", from: "", path: operation.path }
            : { op: "set", path: operation.path, value: { source: "literal", value: "" } })}>
            <option value="set">Set field</option><option value="rename">Rename field</option>
          </select></label>
          <label>Destination path<input value={operation.path} placeholder="/status" onChange={(event) => edit({ ...operation, path: event.currentTarget.value })} /></label>
          {operation.op === "rename" ? <label>Original path<input value={operation.from} placeholder="/original"
            onChange={(event) => edit({ ...operation, from: event.currentTarget.value })} /></label> : <>
            <label>Value source<select value={operation.value.source} onChange={(event) => edit({ ...operation, value: event.currentTarget.value === "input"
              ? { source: "input", pointer: "" } : { source: "literal", value: "" } })}>
              <option value="literal">Fixed value</option><option value="input">Field from source</option>
            </select></label>
            {operation.value.source === "input" ? <label>Source field path<input value={operation.value.pointer} placeholder="/original"
              onChange={(event) => edit({ ...operation, value: { source: "input", pointer: event.currentTarget.value } })} /></label>
              : <StructuredValueField label="Value" value={operation.value.value} disabled={disabled}
                onChange={(value) => edit({ ...operation, value: { source: "literal", value } })} />}
          </>}
          <Button type="button" variant="ghost" aria-label={`Remove operation ${index + 1}`}
            onClick={() => update(operations.filter((_, i) => i !== index))}><Trash2 size={15} /> Remove operation</Button>
        </div>;
      })}
      <Button type="button" variant="outline" onClick={() => update([...operations, emptySetOperation()])}><Plus size={15} /> Add operation</Button>
    </fieldset>
  </>;
}

export function FilterMapListFields({ configuration, disabled, onChange }: Fields) {
  const filter = configuration.predicate != null;
  const mappings: { from: string; path: string }[] = Array.isArray(configuration.mappings) ? configuration.mappings : [];
  const update = (next: typeof mappings) => onChange({ ...configuration, mappings: next });
  return <>
    <label className="workflow-check"><input type="checkbox" checked={filter} disabled={disabled}
      onChange={(event) => onChange({ ...configuration, predicate: event.currentTarget.checked ? { op: "exists", pointer: "" } : null })} />Filter items</label>
    {filter && <PredicateFields label="Item condition" value={configuration.predicate} disabled={disabled} allowChecks={false}
      onChange={(predicate) => onChange({ ...configuration, predicate })} />}
    <fieldset className="pipeline-stack" disabled={disabled}>
      <legend>Field mappings</legend>
      {mappings.length === 0 && <p className="pipeline-meta">Keep the original fields or add a mapping.</p>}
      {mappings.map((mapping, index) => {
        const edit = (next: typeof mapping) => update(mappings.map((item, i) => i === index ? next : item));
        return <div className="workflow-inspector-item" key={index}>
          <label>Source path<input value={mapping.from} placeholder="/name" onChange={(event) => edit({ ...mapping, from: event.currentTarget.value })} /></label>
          <label>Destination path<input value={mapping.path} placeholder="/label" onChange={(event) => edit({ ...mapping, path: event.currentTarget.value })} /></label>
          <Button type="button" variant="ghost" aria-label={`Remove mapping ${index + 1}`}
            onClick={() => update(mappings.filter((_, i) => i !== index))}><Trash2 size={15} /> Remove mapping</Button>
        </div>;
      })}
      <Button type="button" variant="outline" onClick={() => update([...mappings, { from: "", path: "" }])}><Plus size={15} /> Add mapping</Button>
    </fieldset>
  </>;
}

export function BuildTextFields({ configuration, disabled, onChange }: Fields) {
  return <label>Markdown template<textarea rows={6} value={text(configuration.template)} disabled={disabled}
    onChange={(event) => onChange({ kind: "build_text", template: event.currentTarget.value })} />
    <small>Use {"{{field_name}}"} to insert a field from the Values input.</small></label>;
}

export function CompareModelsFields({ configuration, disabled, onChange }: Fields) {
  const { options, loading } = useLiveModelOptions(LIVE_MODEL_PROVIDERS, !disabled);
  const selected: ModelSelection[] = Array.isArray(configuration.selections) ? configuration.selections : [];
  const update = (selections: ModelSelection[]) => onChange({ kind: "compare_models", selections });
  const move = (from: number, to: number) => {
    const candidate = selected[from];
    if (!candidate || to < 0 || to >= selected.length) return;
    const next = selected.filter((_, index) => index !== from);
    next.splice(to, 0, candidate);
    update(next);
  };
  return <>
    <ModelPicker label="Comparison models" options={options} loading={loading} disabled={disabled}
      selection={{ mode: "multiple", selected, onChange: update }} />
    {selected.length > 0 && <ol aria-label="Candidate order" className="workflow-model-order">{selected.map((selection, index) => {
      const name = options.find((option) => sameModelSelection(option.selection, selection))?.name ?? selection.modelId;
      return <li key={JSON.stringify([selection.provider, selection.profileId, selection.modelId])}>
        <span>{name}</span>
        <Button type="button" variant="ghost" aria-label={`Move ${name} earlier`} disabled={disabled || index === 0} onClick={() => move(index, index - 1)}><ArrowUp size={15} /></Button>
        <Button type="button" variant="ghost" aria-label={`Move ${name} later`} disabled={disabled || index === selected.length - 1} onClick={() => move(index, index + 1)}><ArrowDown size={15} /></Button>
        <Button type="button" variant="ghost" aria-label={`Remove ${name}`} disabled={disabled}
          onClick={() => update(selected.filter((_, position) => position !== index))}><Trash2 size={15} /></Button>
      </li>;
    })}</ol>}
  </>;
}
