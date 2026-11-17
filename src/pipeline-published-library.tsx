import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { nodePurposes, PipelineLibraryCatalog, workflowRegistryGroups } from "@/pipeline-catalog";
import { PipelineNodeDocumentation } from "@/pipeline-node-details";
import type { WorkflowDefinition } from "@/workflow-graph-model";
import { reusableNodeQuery, type ReusableVersion } from "@/workflow-reusable";

type PublishedSummary = { name: string; version: ReusableVersion; blockers: string[] };
const reusableKinds = [{ kind: "preset", title: "Published presets" }, { kind: "subflow", title: "Published subflows" }] as const;

export function ProjectNodeLibrary({ projectId, definitions }: { projectId: string; definitions: WorkflowDefinition[] }) {
  const published = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<PublishedSummary[]>("workflow_package_catalog", { projectId }),
  });
  const reusable = reusableKinds.map(({ kind, title }) => ({
    title,
    entries: (published.data ?? []).filter(({ version }) => version.kind === kind)
      .map(({ name, version, blockers }) => ({
        id: JSON.stringify(version),
        label: name,
        purpose: (version.destination === "project" ? "This project" : "Reusable library") + " · " + version.revision.slice(0, 10),
        input: "",
        output: "",
        details: <PublishedNodeDetails projectId={projectId} version={version} kind={kind} blockers={blockers ?? []} />,
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

function PublishedNodeDetails({ projectId, version, kind, blockers }: {
  projectId: string;
  version: ReusableVersion;
  kind: "preset" | "subflow";
  blockers: string[];
}) {
  const detail = useQuery(reusableNodeQuery(projectId, version));
  if (detail.isError) return <div className="pipeline-library-notice">
    <p role="alert">This published revision could not be read.</p>
    <Button type="button" variant="outline" onClick={() => void detail.refetch()}>Try again</Button>
  </div>;
  if (!detail.data) return <p role="status">Loading published node…</p>;
  const { artifact, definition, editableOverrides } = detail.data;
  return <PipelineNodeDocumentation definition={definition}
    purpose={kind === "subflow" ? "Run the saved child workflow through its public inputs and outputs."
      : nodePurposes[definition.kind] ?? "Use the saved node configuration in another workflow."}
    published={{ kind, revision: artifact.version.revision, editable: editableOverrides, blockers: artifact.blockers ?? blockers }} />;
}
