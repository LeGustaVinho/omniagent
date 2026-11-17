import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { nodePurposes, PipelineLibraryCatalog, workflowRegistryGroups } from "@/pipeline-catalog";
import { PipelineNodeDocumentation } from "@/pipeline-node-details";
import type { WorkflowDefinition } from "@/workflow-graph-model";
import { PromptInspector } from "@/workflow-prompt";
import { reusableNodeQuery, type ReusableVersion } from "@/workflow-reusable";

type PublishedSummary = { name: string; version: ReusableVersion; blockers: string[]; prompt?: boolean };
const reusableKinds = [
  { kind: "prompt", title: "Published Prompts" },
  { kind: "preset", title: "Published presets" },
  { kind: "subflow", title: "Published subflows" },
] as const;

export function ProjectNodeLibrary({ projectId, definitions }: { projectId: string; definitions: WorkflowDefinition[] }) {
  const published = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<PublishedSummary[]>("workflow_package_catalog", { projectId }),
  });
  const reusable = reusableKinds.map(({ kind, title }) => ({
    title,
    entries: (published.data ?? []).filter(({ version, prompt }) => (prompt ? "prompt" : version.kind) === kind)
      .map(({ name, version, blockers }) => ({
        id: JSON.stringify(version),
        label: name,
        purpose: (version.destination === "project" ? "This project" : "Reusable library") + " · " + version.revision.slice(0, 10),
        input: "",
        output: "",
        details: <PublishedNodeDetails projectId={projectId} version={version} kind={kind} blockers={blockers ?? []} catalog={definitions} />,
      })),
  }));

  return <>
    {published.isError && <div className="pipeline-library-notice">
      <p role="alert">Published nodes could not be loaded.</p>
      <Button type="button" variant="ghost" onClick={() => void published.refetch()}>Retry</Button>
    </div>}
    <PipelineLibraryCatalog groups={[...workflowRegistryGroups(definitions), ...reusable]} />
  </>;
}

function PublishedNodeDetails({ projectId, version, kind, blockers, catalog }: {
  projectId: string;
  version: ReusableVersion;
  kind: "preset" | "subflow" | "prompt";
  blockers: string[];
  catalog: WorkflowDefinition[];
}) {
  const detail = useQuery(reusableNodeQuery(projectId, version));
  if (detail.isError) return <div className="pipeline-library-notice">
    <p role="alert">This published revision could not be read.</p>
    <Button type="button" variant="outline" onClick={() => void detail.refetch()}>Try again</Button>
  </div>;
  if (!detail.data) return <p role="status">Loading published node…</p>;
  const { artifact, definition, editableOverrides, graphDefinitions } = detail.data;
  if (artifact.body.kind === "prompt" && artifact.body.prompt) {
    const member = { key: artifact.version.id, name: artifact.name, body: { kind: "prompt" as const, prompt: artifact.body.prompt },
      references: artifact.references };
    return <div className="pipeline-stack">
      <p>Reuse this revision from the Builder's Reuse menu.</p>
      <PromptInspector member={member} choices={[]} catalog={catalog} disabled onChange={() => undefined} technical={graphDefinitions} />
    </div>;
  }
  return <PipelineNodeDocumentation definition={definition}
    purpose={kind === "subflow" ? "Run the saved child workflow through its public inputs and outputs."
      : nodePurposes[definition.kind] ?? "Use the saved node configuration in another workflow."}
    published={{ kind: kind === "preset" ? "preset" : "subflow", revision: artifact.version.revision, editable: editableOverrides, blockers: artifact.blockers ?? blockers }} />;
}
