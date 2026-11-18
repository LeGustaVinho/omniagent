import { useState } from "react";
import { ArrowUpRight, Workflow } from "lucide-react";
import { Button } from "@/components/ui/button";
import { WorkflowTemplateChooser, type PackageDraft } from "@/workflow-authoring-package";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import { objectSchema } from "@/workflow-schema-model";
import { workflowTemplates } from "@/workflow-templates";

const categories = [
  ["pipeline", "Pipeline"], ["subflow", "Functions"], ["schema", "Schemas"],
  ["prefab", "Node Prefabs"], ["role", "Roles"], ["prompt", "Prompts"],
] as const;

type NativeTemplate = typeof workflowTemplates[number];
type NativeArtifact = NativeTemplate["members"][number];

export function NativeWorkflowTemplates({ projectId, onCreated }: {
  projectId: string; onCreated: (draft: PackageDraft) => void;
}) {
  return <section className="workflow-library-section" aria-label="Native templates">
    <h3>Native templates</h3>
    <div className="workflow-native-template-list">
      {workflowTemplates.map((template) => <TemplateArtifacts key={template.key} template={template} projectId={projectId} onCreated={onCreated} />)}
    </div>
  </section>;
}

function TemplateArtifacts({ template, projectId, onCreated }: {
  template: NativeTemplate; projectId: string; onCreated: (draft: PackageDraft) => void;
}) {
  const groups = categories.flatMap(([kind, label]) => {
    const members = template.members.filter((member) => member.kind === kind);
    return members.length ? [{ kind, label, members }] : [];
  });
  const [category, setCategory] = useState<string>(groups[0]?.kind ?? "");
  const group = groups.find(({ kind }) => kind === category);
  return <article className="workflow-native-template" aria-label={template.name}>
    <header className="workflow-native-template-heading">
      <Workflow size={20} aria-hidden />
      <h4>{template.name}</h4>
      <WorkflowTemplateChooser projectId={projectId} initialTemplateKey={template.key} onCreated={onCreated} />
    </header>
    <div className="workflow-native-template-categories" role="group" aria-label="Artifact categories">
      {groups.map(({ kind, label, members }) => <Button key={kind} type="button"
        variant={kind === category ? "outline" : "ghost"} aria-pressed={kind === category} onClick={() => setCategory(kind)}>
        {label} <span>{members.length}</span>
      </Button>)}
    </div>
    {group && <ul className="workflow-native-template-artifacts" aria-label={group.label}>
      {group.members.map((member) => <li key={member.key}>
        <strong>{member.name}</strong>
        <WorkflowBuilderDialog title={member.name} description={`Read-only artifact from ${template.name}.`} wide
          trigger={<Button type="button" variant="outline" aria-label={`View ${member.name}`}>
            View <ArrowUpRight size={14} aria-hidden />
          </Button>}>
          <NativeArtifactDetails member={member} template={template} />
        </WorkflowBuilderDialog>
      </li>)}
    </ul>}
  </article>;
}

function NativeArtifactDetails({ member, template }: { member: NativeArtifact; template: NativeTemplate }) {
  const { body } = member;
  const graph = "graph" in body ? body.graph : undefined;
  const fields = "fields" in body ? body.fields : undefined;
  const prompt = "prompt" in body ? body.prompt : undefined;
  const role = body.template;
  const references = "references" in member ? member.references : undefined;
  const nameOf = (key: string) => template.members.find((candidate) => candidate.key === key)?.name ?? key;
  return <>
    {graph && <div className="workflow-native-graph-details">
      <section aria-label="Nodes"><h3>Nodes ({graph.nodes.length})</h3>
        <ul>{graph.nodes.map(({ id, kind }) => {
          const reference = references?.[id];
          return <li key={id}><code>{id}</code><span>{reference?.source === "draft" ? nameOf(reference.key) : kind.replaceAll("_", " ")}</span></li>;
        })}</ul>
      </section>
      <section aria-label="Connections"><h3>Connections ({graph.wires.length})</h3>
        <ul>{graph.wires.map(({ from, to }) => {
          const source = from.node + "." + from.port;
          const target = to.node + "." + to.port;
          return <li key={source + "→" + target}><code>{source} → {target}</code></li>;
        })}</ul>
      </section>
    </div>}
    {fields && <section aria-label="JSON Schema"><h3>JSON Schema</h3><pre className="workflow-native-source">{JSON.stringify(objectSchema(fields), null, 2)}</pre></section>}
    {prompt && <section aria-label="Prompt"><h3>Prompt</h3><pre className="workflow-native-source">{prompt.text}</pre></section>}
    {role && <section className="pipeline-stack" aria-label="Role">
      <div><h3>Instructions</h3><p>{role.instructions}</p></div>
      <div><h3>Completion contract</h3><p>{role.completionContract}</p></div>
      <dl><dt>Tool scope</dt><dd>{role.toolScope.replaceAll("_", " ")}</dd>
        <dt>Review policy</dt><dd>{role.reviewPolicy.replaceAll("_", " ")}</dd></dl>
    </section>}
    {references && Object.keys(references).length > 0 && <details className="pipeline-evidence">
      <summary>References ({Object.keys(references).length})</summary>
      <dl className="workflow-native-references">{Object.entries(references).map(([slot, reference]) =>
        <div key={slot}><dt>{slot}</dt><dd>{reference.source === "draft" ? nameOf(reference.key) : JSON.stringify(reference)}</dd></div>)}</dl>
    </details>}
    <details className="pipeline-evidence">
      <summary>Artifact source</summary>
      <pre className="workflow-native-source">{JSON.stringify({ ...body, ...(references && { references }) }, null, 2)}</pre>
    </details>
  </>;
}
