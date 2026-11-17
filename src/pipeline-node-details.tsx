import { ArrowDownToLine, ArrowUpFromLine, Lightbulb, SlidersHorizontal } from "lucide-react";
import type { StageKind } from "@/pipelines-model";
import { nodeExamples, nodeParameters, nodePortNote, simulationExamples, simulationKinds } from "@/pipeline-node-guide";
import { portType, type WorkflowDefinition, type WorkflowPort } from "@/workflow-graph-model";

function words(value: string) {
  return value.replaceAll("_", " ");
}

type SimulationNode = { label: string; stage?: StageKind; input: string; output: string };
type PublishedNode = {
  kind: "preset" | "subflow";
  revision: string;
  editable: readonly string[];
  blockers: readonly string[];
};

const simulationParameters = [
  ["Role", "Select whose instructions the stage follows."],
  ["Permission profile", "Choose read-only, plan, or write mode for the simulated stage."],
  ["Model override", "Choose a model for this stage or keep the role default."],
  ["Timeout and retry limit", "Set how long an attempt can run and how many retries are allowed."],
  ["Typed output", "Choose the artifact this stage produces."],
  ["Requirements and goal", "Set required capabilities, subagent limits, or an optional objective and completion evidence."],
  ["Dependencies", "Choose upstream stages and, where available, the required branch."],
] as const;

const loopOutputs: WorkflowPort[] = [
  { name: "completed", value: { kind: "control" } },
  { name: "failed", value: { kind: "control" } },
];

function SimulationParameters({ stage }: { stage?: StageKind }) {
  if (!stage) return <p className="pipeline-node-muted">This entry describes a catalog example. It has no editable settings in the simulated Builder.</p>;
  return <dl className="pipeline-node-parameters">
    {simulationParameters.map(([name, detail]) => <div key={name}><dt>{name}</dt><dd>{detail}</dd></div>)}
    {stage === "approval" && <div><dt>Decision source</dt><dd>Wait for a human decision or apply the selected rule predicate.</dd></div>}
    {stage === "condition" && <div><dt>Branch condition</dt><dd>An edge marked If migration reads the migration answer from the plan.</dd></div>}
  </dl>;
}

export function PipelineNodeDocumentation({ purpose, definition, effect, simulation, published }: {
  purpose?: string;
  definition?: WorkflowDefinition;
  effect?: string;
  simulation?: SimulationNode;
  published?: PublishedNode;
}) {
  const kind = definition?.kind ?? (simulation && simulationKinds[simulation.label]);
  const normalExample = kind ? nodeExamples[kind] : undefined;
  const example = simulation ? simulationExamples[simulation.label] ?? normalExample
    : published?.kind === "subflow" ? ["Use the same workflow in another pipeline",
      "Add the pinned subflow in Builder and connect matching artifacts to its public inputs. Use its public outputs to continue the parent workflow."] as const
      : normalExample;
  const exampleHeading = published?.kind === "preset" && example ? "Example for this node type" : "Example";
  const parameters = definition ? nodeParameters[definition.configuration] ?? [] : [];
  const editable = published?.kind === "preset" ? new Set(published.editable) : undefined;
  const visibleParameters = editable ? parameters.filter(([field]) => editable.has(field)) : parameters;
  const referenceNote = definition && !published ? configuredPortNotes[definition.kind] : undefined;

  return <div className="pipeline-node-documentation">
    {purpose && <section className="pipeline-node-purpose">
      <h3>How it works</h3>
      <p>{purpose}</p>
    </section>}
    {example && <section className="pipeline-node-example">
      <Lightbulb size={17} aria-hidden />
      <div><h3>{exampleHeading}</h3><strong>{example[0]}</strong><p>{example[1]}</p></div>
    </section>}
    {definition && <div className="pipeline-node-connections">
      <PortList direction="Inputs" ports={definition.inputs}
        suppliedBySubflow={Boolean(referenceNote && definition.inputs.length === 0)} />
      <PortList direction="Outputs" ports={definition.kind === "repeat_until" ? loopOutputs : definition.outputs}
        suppliedBySubflow={definition.kind === "subflow" && !published} />
      {referenceNote && <p className="pipeline-node-port-note">{referenceNote}</p>}
    </div>}
    {simulation && <div className="pipeline-node-connections">
      <section className="pipeline-node-port-list"><h3><ArrowDownToLine size={15} aria-hidden /> Inputs</h3><p>{simulation.input}</p></section>
      <section className="pipeline-node-port-list"><h3><ArrowUpFromLine size={15} aria-hidden /> Outputs</h3><p>{simulation.output}</p></section>
    </div>}
    <section className="pipeline-node-settings">
      <h3><SlidersHorizontal size={16} aria-hidden /> Parameters</h3>
      {simulation ? <SimulationParameters stage={simulation.stage} />
        : published?.kind === "subflow" ? <p className="pipeline-node-muted">The graph and its public ports are pinned to this revision.</p>
          : <>
            {visibleParameters.length > 0 ? <dl className="pipeline-node-parameters">
              {visibleParameters.map(([field, name, detail]) => <div key={field}><dt>{name}</dt><dd>{detail}</dd></div>)}
            </dl> : <p className="pipeline-node-muted">{editable?.size
              ? "See the editable fields recorded in the published revision."
              : "No editable parameters for this definition."}</p>}
            {editable && <p className="pipeline-node-muted">Editable fields: {published?.editable.join(", ") || "None"}. Other values come from the saved preset.</p>}
          </>}
    </section>
    {published && published.blockers.length > 0 && <section className="pipeline-node-blockers">
      <h3>Requirements to resolve</h3>
      <ul>{published.blockers.map((blocker) => <li key={blocker}>{blocker}</li>)}</ul>
    </section>}
    {definition && <details className="pipeline-node-technical">
      <summary>Definition details</summary>
      <dl>
        <dt>Effect</dt><dd>{effect ?? words(definition.effect)}</dd>
        <dt>Node revision</dt><dd>{definition.revision}</dd>
        {published && <><dt>Published revision</dt><dd><code>{published.revision}</code></dd></>}
        <dt>Configuration</dt><dd>{words(definition.configuration)}</dd>
        <dt>Executor</dt><dd>{words(definition.executor)}</dd>
      </dl>
    </details>}
  </div>;
}

const configuredPortNotes: Partial<Record<string, string>> = {
  subflow: "Select a pinned subflow in Builder to use its public ports.",
  repeat_until: "The pinned subflow provides the inputs. The loop exposes its control outputs once the subflow is selected.",
  for_each: "Extra inputs can be inherited from the pinned subflow.",
  find_work: "The task and target outputs are available when Results is set to Exactly one match.",
};

function PortList({ direction, ports, suppliedBySubflow = false }: {
  direction: "Inputs" | "Outputs"; ports: WorkflowPort[]; suppliedBySubflow?: boolean;
}) {
  const Icon = direction === "Inputs" ? ArrowDownToLine : ArrowUpFromLine;
  return <section className="pipeline-node-port-list" aria-label={direction}>
    <h3><Icon size={15} aria-hidden /> {direction} {!suppliedBySubflow && <small>{ports.length}</small>}</h3>
    {ports.length > 0 ? <ul>{ports.map(({ name, value }) => {
      const note = nodePortNote(name, value.kind);
      return <li key={name}>
        <div><code>{name}</code><span className="pipeline-node-port-type" data-kind={value.kind}>{portType(value)}</span></div>
        {note && <p>{note}</p>}
      </li>;
    })}</ul> : <p className="pipeline-node-muted">{suppliedBySubflow ? "Defined by the selected subflow." : "No " + direction.toLowerCase() + " on this definition."}</p>}
  </section>;
}
