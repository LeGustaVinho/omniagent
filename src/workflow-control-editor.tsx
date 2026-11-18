import { Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DecisionAnswerFields, type InputField } from "@/workflow-decision-editor";
import { configuredBranches, type WorkflowDefinition, type WorkflowGraphNode, type WorkflowNodeConfiguration, type WorkflowPort } from "@/workflow-graph-model";
import { PredicateFields } from "@/workflow-node-fields";

type Props = {
  configuration: WorkflowNodeConfiguration;
  disabled: boolean;
  nodes: WorkflowGraphNode[];
  subflow?: WorkflowDefinition;
  onChange: (configuration: WorkflowNodeConfiguration) => void;
};

const text = (value: unknown) => typeof value === "string" ? value : "";
const changeNumber = (input: HTMLInputElement) => input.value ? input.valueAsNumber : null;

export function WorkflowControlFields({ configuration, nodes, subflow, disabled, onChange }: Props) {
  function set(field: string, value: unknown) {
    onChange({ ...configuration, [field]: value });
  }
  if (configuration.kind === "fork" || configuration.kind === "join_results") {
    const branches = configuredBranches(configuration);
    const fork = nodes.find(({ id, kind }) => kind === "fork" && id === configuration.fork);
    const locked = !!fork && configuration.kind === "join_results";
    const join = configuration.kind === "join_results";
    return <fieldset className="pipeline-stack" disabled={disabled}>
      {join && <label>Associated Fork<select value={text(configuration.fork)} onChange={(event) => {
        const next = nodes.find(({ id }) => id === event.currentTarget.value);
        onChange({ ...configuration, fork: next?.id ?? null,
          branches: next ? configuredBranches(next.configuration) : branches,
          ...(next ? {} : { cancelRemaining: false }),
        });
      }}>
        <option value="">No associated Fork</option>
        {nodes.filter(({ kind }) => kind === "fork").map(({ id }) => <option value={id} key={id}>{id}</option>)}
      </select></label>}
      <fieldset className="pipeline-stack" disabled={locked}>
        <legend>Branches</legend>
        {branches.map((branch, index) => <div className="workflow-schema-field-head" key={index}>
          <label>Branch {index + 1}<input value={branch}
            onChange={(event) => set("branches", branches.map((item, position) => position === index ? event.currentTarget.value : item))} /></label>
          <Button type="button" variant="ghost" className="ui-icon-button" aria-label={`Remove branch ${index + 1}`}
            disabled={branches.length <= 2} onClick={() => set("branches", branches.filter((_, position) => position !== index))}>
            <Trash2 size={15} aria-hidden />
          </Button>
        </div>)}
        <Button type="button" variant="outline" className="w-fit" disabled={branches.length >= 32} onClick={() => {
          let index = branches.length + 1;
          while (branches.includes(`branch-${index}`)) index += 1;
          set("branches", [...branches, `branch-${index}`]);
        }}><Plus size={14} aria-hidden /> Add branch</Button>
      </fieldset>
      {!join ? <label>Concurrency limit<input type="number" min={1} max={branches.length} step={1}
        placeholder="All branches" value={typeof configuration.concurrency === "number" ? configuration.concurrency : ""}
        onChange={(event) => set("concurrency", changeNumber(event.currentTarget))} /></label> : <>
        <label>Join strategy<select value={text(configuration.strategy)} onChange={(event) => {
          const strategy = event.currentTarget.value;
          onChange({ ...configuration, strategy, quorum: strategy === "quorum" ? configuration.quorum ?? 1 : null });
        }}>
          <option value="all">All</option><option value="any">Any</option><option value="quorum">Quorum</option>
        </select></label>
        {configuration.strategy === "quorum" && <label>Quorum<input type="number" min={1} max={branches.length} step={1}
          value={typeof configuration.quorum === "number" ? configuration.quorum : ""}
          onChange={(event) => set("quorum", changeNumber(event.currentTarget))} /></label>}
        <label className="pipeline-checkline"><input type="checkbox" checked={configuration.countFailures === true}
          onChange={(event) => set("countFailures", event.currentTarget.checked)} /> Count failed branches</label>
        {configuration.strategy !== "all" && <label className="pipeline-checkline"><input type="checkbox" checked={configuration.cancelRemaining === true}
          disabled={!fork} onChange={(event) => set("cancelRemaining", event.currentTarget.checked)} /> Cancel remaining branches</label>}
      </>}
    </fieldset>;
  }
  if (configuration.kind === "wait") {
    const until = configuration.until as { kind: "delay"; seconds: number } | { kind: "scheduled"; at: string } | undefined;
    const scheduled = until?.kind === "scheduled" && Number.isFinite(Date.parse(until.at))
      ? new Date(until.at).toISOString().slice(0, 19) : "";
    return <fieldset className="pipeline-stack" disabled={disabled}>
      <label>Wait until<select value={until?.kind ?? "delay"} onChange={(event) => set("until", event.currentTarget.value === "scheduled"
        ? { kind: "scheduled", at: "" } : { kind: "delay", seconds: 60 })}>
        <option value="delay">After a delay</option><option value="scheduled">Scheduled time</option>
      </select></label>
      {until?.kind === "scheduled" ? <label>Scheduled time (UTC)<input type="datetime-local" step={1}
        value={scheduled} onChange={(event) => set("until", { kind: "scheduled",
          at: event.currentTarget.value ? new Date(event.currentTarget.value + "Z").toISOString() : "" })} /></label>
        : <label>Delay (seconds)<input type="number" min={1} step={1} value={until?.seconds ?? ""}
          onChange={(event) => set("until", { kind: "delay", seconds: changeNumber(event.currentTarget) })} /></label>}
    </fieldset>;
  }
  if (configuration.kind === "human") {
    const interaction = text(configuration.interaction);
    const fields = Array.isArray(configuration.fields) ? configuration.fields as InputField[] : [];
    return <fieldset className="pipeline-stack" disabled={disabled}>
      <label>Interaction<select value={interaction} onChange={(event) => {
        const next = event.currentTarget.value;
        onChange({ ...configuration, interaction: next, onExpiration: next !== "approval" && configuration.onExpiration === "reject" ? "failed" : configuration.onExpiration });
      }}>
        <option value="approval">Approval</option><option value="review">Review</option>
        <option value="input">Input</option><option value="feedback">Feedback</option>
      </select></label>
      <label>Message<textarea rows={3} value={text(configuration.message)}
        onChange={(event) => set("message", event.currentTarget.value)} /></label>
      <DecisionAnswerFields fields={fields} minimum={["input", "feedback"].includes(interaction) ? 1 : 0}
        onChange={(values) => set("fields", values)} />
      <label>Timeout (seconds)<input type="number" min={1} step={1} placeholder="No timeout"
        value={typeof configuration.timeoutSeconds === "number" ? configuration.timeoutSeconds : ""}
        onChange={(event) => set("timeoutSeconds", changeNumber(event.currentTarget))} /></label>
      {configuration.timeoutSeconds != null && <label>On expiration<select value={text(configuration.onExpiration) || "failed"}
        onChange={(event) => set("onExpiration", event.currentTarget.value)}>
        <option value="failed">Fail</option><option value="expired">Take expired route</option>
        {interaction === "approval" && <option value="reject">Reject</option>}
      </select></label>}
    </fieldset>;
  }
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <label>Maximum iterations<input type="number" min={1} max={32} step={1}
      value={typeof configuration.maxIterations === "number" ? configuration.maxIterations : ""}
      onChange={(event) => set("maxIterations", changeNumber(event.currentTarget))} /></label>
    <label>At the iteration limit<select value={text(configuration.limit)} onChange={(event) => set("limit", event.currentTarget.value)}>
      <option value="complete">Complete</option><option value="limit_reached">Take limit reached route</option><option value="fail">Fail</option>
    </select></label>
    {subflow && <fieldset className="pipeline-stack">
      <legend>Function ports</legend>
      <Port label="Ready input" ports={subflow.inputs.filter(control)} value={configuration.readyInput} change={(value) => set("readyInput", value)} />
      <Port label="Context input" ports={subflow.inputs.filter(data)} value={configuration.contextInput} change={(value) => set("contextInput", value)} />
      <Port label="Completion output" ports={subflow.outputs.filter(control)} value={configuration.completionOutput} change={(value) => set("completionOutput", value)} />
      <Port label="Result output" ports={subflow.outputs.filter(data)} value={configuration.resultOutput} change={(value) => set("resultOutput", value)} />
      <Port label="Failure output" ports={subflow.outputs.filter(control)} value={configuration.failureOutput} change={(value) => set("failureOutput", value || null)} optional />
    </fieldset>}
    {(["breakWhen", "exitWhen"] as const).map((field) => {
      const label = field === "breakWhen" ? "Break before an iteration" : "Exit after an iteration";
      return <fieldset className="pipeline-stack" key={field}>
        <label className="pipeline-checkline"><input type="checkbox" checked={configuration[field] != null}
          onChange={(event) => set(field, event.currentTarget.checked ? { op: "exists", pointer: field === "breakWhen" ? "/previous" : "/result" } : null)} /> {label}</label>
        {configuration[field] != null && <PredicateFields label={label} value={configuration[field]} disabled={disabled}
          onChange={(value) => set(field, value)} />}
      </fieldset>;
    })}
  </fieldset>;
}

const control = ({ value }: WorkflowPort) => value.kind === "control";
const data = ({ value }: WorkflowPort) => value.kind === "data" && value.artifact === "json_object";

function Port({ label, value, ports, change, optional }: {
  label: string; value: unknown; ports: WorkflowPort[]; change: (value: string) => void; optional?: boolean;
}) {
  const current = text(value);
  return <label>{label}<select value={current} onChange={(event) => change(event.currentTarget.value)}>
    <option value="">{optional ? "None" : "Choose a port"}</option>
    {current && !ports.some(({ name }) => name === current) && <option value={current} disabled>{current} (unavailable)</option>}
    {ports.map(({ name }) => <option key={name} value={name}>{name}</option>)}
  </select></label>;
}
