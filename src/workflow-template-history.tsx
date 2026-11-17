import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { PipelineTemplateList } from "@/pipeline-layout";
import type { WorkspaceProps } from "@/workbench-contract";
import { packageDraftTarget, type PackageDraft } from "@/workflow-authoring-package";
import { WorkflowEditorAssistance } from "@/workflow-editor-assistance";
import { WorkflowGraphEditor } from "@/workflow-graph-editor";
import type { WorkflowDefinitions, WorkflowGraph, WorkflowLayout } from "@/workflow-graph-model";
import { WorkflowLaunchPicker } from "@/workflow-launch";
import { PublishedSubflow, type ReusableReference, type ReusableVersion } from "@/workflow-reusable";

type PublishedWorkflow = { name: string; version: ReusableVersion };
type SavedWorkflow = PublishedWorkflow & {
  body: { graph: WorkflowGraph };
  definitions: WorkflowDefinitions;
  layout?: WorkflowLayout;
  references: Record<string, ReusableReference>;
};

export function WorkflowTemplateHistory({ checkout, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openWorkspace">) {
  const { projectId, checkoutId } = checkout;
  const client = useQueryClient();
  const [selected, setSelected] = useState<PublishedWorkflow>();
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<PublishedWorkflow[]>("workflow_package_catalog", { projectId }),
  });
  const artifact = useQuery({
    queryKey: ["saved-workflow-template", projectId, selected?.version],
    queryFn: () => invoke<SavedWorkflow | null>("read_published_workflow_package_artifact", { projectId, version: selected?.version }),
    enabled: Boolean(selected),
  });
  const templates = catalog.data?.filter(({ version }) => version.kind === "pipeline") ?? [];
  const saved = artifact.data;
  const revise = useMutation({
    mutationFn: (source: SavedWorkflow) => invoke<PackageDraft>("create_workflow_package", {
      projectId, conversationId: null,
      package: { name: source.name, members: [{
        key: "pipeline", name: source.name, destination: "project",
        base: source.version, body: source.body, references: source.references,
      }] },
    }),
    onSuccess: (draft) => {
      client.setQueryData(["workflow-packages", projectId, draft.id], draft);
      void client.invalidateQueries({ queryKey: ["workflow-packages", projectId], exact: true });
      openWorkspace("pipelines", packageDraftTarget(draft.id));
    },
  });

  return <section className="pipeline-box pipeline-stack" aria-label="Saved workflow templates">
    <div className="pipeline-card-heading">
      <h3>Saved templates</h3>
      <Button type="button" variant="outline" disabled={revise.isPending}
        onClick={() => openWorkspace("pipelines", "drafts")}>Open Builder</Button>
    </div>
    {catalog.isPending && <p role="status">Loading saved workflow templates…</p>}
    {catalog.error && <p role="alert">{String(catalog.error)}</p>}
    {catalog.data && <fieldset disabled={revise.isPending}><PipelineTemplateList entries={templates.map(({ name, version }) => ({
      id: JSON.stringify(version), name, detail: `Revision ${version.revision.slice(0, 12)} · ${version.destination}`,
    }))} onOpen={(id) => {
      revise.reset();
      setSelected(templates.find(({ version }) => JSON.stringify(version) === id));
    }} /></fieldset>}
    {selected && <section className="pipeline-stack" aria-label="Selected workflow template">
      <div className="pipeline-card-heading">
        <h4>{selected.name}</h4>
        <Button type="button" variant="outline" disabled={revise.isPending} onClick={() => setSelected(undefined)}>Close template</Button>
      </div>
      {artifact.isPending && <p role="status">Loading pinned revision…</p>}
      {artifact.error && <p role="alert">{String(artifact.error)}</p>}
      {saved === null && <p>This saved revision is unavailable.</p>}
      {saved && <>
        <p className="pipeline-meta">Published revision <code className="break-all">{saved.version.revision}</code></p>
        {saved.version.destination === "project" && <div className="pipeline-actions">
          <Button type="button" disabled={revise.isPending} onClick={() => revise.mutate(saved)}>
            {revise.isPending ? "Creating draft…" : "Edit in Builder"}
          </Button>
        </div>}
        {revise.error && <p role="alert">{String(revise.error)}</p>}
        <WorkflowGraphEditor key={`graph:${saved.version.revision}`} projectId={projectId} checkoutId={checkoutId}
          state={{ graph: saved.body.graph, layout: saved.layout ?? {}, updateDefinitions: {} }}
          setState={() => undefined} definitions={saved.definitions} catalog={[]} disabled problems={[]} allowDefinitionUpdates={false}
          inspectReference={(id) => {
            const reference = saved.references[id];
            return reference?.source === "published" && reference.version.kind === "subflow"
              ? <PublishedSubflow projectId={projectId} checkoutId={checkoutId} version={reference.version} /> : null;
          }} />
        <WorkflowEditorAssistance key={`assistant:${saved.version.revision}`} projectId={projectId} checkoutId={checkoutId}
          artifact={saved} openWorkspace={openWorkspace} />
        {saved.version.destination === "project" && <WorkflowLaunchPicker key={`launch:${saved.version.revision}`} projectId={projectId}
          checkoutId={checkoutId} preferred={saved.version} openWorkspace={openWorkspace} />}
      </>}
    </section>}
  </section>;
}
