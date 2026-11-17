import { invoke } from "@tauri-apps/api/core";
import { queryOptions, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
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

const templateKey = (template: PublishedWorkflow) => JSON.stringify(template.version);

function templateQuery(projectId: string, version: ReusableVersion | undefined) {
  return queryOptions({ queryKey: ["saved-workflow-template", projectId, version], staleTime: Infinity,
    queryFn: () => invoke<SavedWorkflow | null>("read_published_workflow_package_artifact", { projectId, version }) });
}

export function WorkflowTemplateHistory({ checkout, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openWorkspace">) {
  const { projectId, checkoutId } = checkout;
  const client = useQueryClient();
  const [selected, setSelected] = useState<PublishedWorkflow>();
  const [initialLaunch, setInitialLaunch] = useState<number>();
  const launchRequest = useRef(0);
  const [message, setMessage] = useState<{ error: boolean; text: string }>();
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<PublishedWorkflow[]>("workflow_package_catalog", { projectId }),
  });
  const artifact = useQuery({ ...templateQuery(projectId, selected?.version), enabled: Boolean(selected) });
  const templates = catalog.data?.filter(({ version }) => version.kind === "pipeline") ?? [];
  const saved = artifact.data;
  async function loadTemplate(template: PublishedWorkflow) {
    const artifact = await client.fetchQuery(templateQuery(projectId, template.version));
    if (!artifact) throw new Error("This saved revision is unavailable.");
    return artifact;
  }

  const revise = useMutation({
    mutationFn: async ({ template, duplicate }: { template: PublishedWorkflow; duplicate?: boolean }) => {
      if (duplicate) return invoke<PackageDraft>("duplicate_workflow_template_draft", { projectId, version: template.version });
      const source = await loadTemplate(template);
      return invoke<PackageDraft>("create_workflow_package", {
        projectId, conversationId: null,
        package: { name: source.name, members: [{
          key: "pipeline", name: source.name, destination: "project",
          base: source.version, body: source.body, references: source.references,
        }] },
      });
    },
    onSuccess: (draft) => {
      client.setQueryData(["workflow-packages", projectId, draft.id], draft);
      void client.invalidateQueries({ queryKey: ["workflow-packages", projectId], exact: true });
      openWorkspace("pipelines", packageDraftTarget(draft.id));
    },
  });

  function inspect(template: PublishedWorkflow, prepareLaunch = false) {
    if (revise.isPending) return;
    revise.reset();
    setMessage(undefined);
    setSelected(template);
    setInitialLaunch(prepareLaunch ? ++launchRequest.current : undefined);
  }

  async function copy(read: () => string | Promise<string>) {
    setMessage(undefined);
    try {
      await navigator.clipboard.writeText(await read());
      setMessage({ error: false, text: "Copied to the clipboard." });
    } catch (failure) { setMessage({ error: true, text: String(failure) }); }
  }

  return <section className="pipeline-box pipeline-stack" aria-label="Saved workflow templates">
    <div className="pipeline-card-heading">
      <h3>Saved templates</h3>
      <Button type="button" variant="outline" disabled={revise.isPending}
        onClick={() => openWorkspace("pipelines", "drafts")}>Open Builder</Button>
    </div>
    {catalog.isPending && <p role="status">Loading saved workflow templates…</p>}
    {catalog.error && <p role="alert">{String(catalog.error)}</p>}
    {revise.error && <p role="alert">{String(revise.error)}</p>}
    {message && <p role={message.error ? "alert" : "status"}>{message.text}</p>}
    {catalog.data && <fieldset disabled={revise.isPending}><PipelineTemplateList entries={templates.map((template) => {
      const { version, name } = template;
      return { id: templateKey(template), name, detail: `Revision ${version.revision.slice(0, 12)} · ${version.destination}`, menu: [[
        { label: "Inspect pinned template", run: () => inspect(template) },
        ...(version.destination === "project" ? [
          { label: "Edit in Builder…", disabled: revise.isPending, run: () => revise.mutate({ template }) },
          { label: "Configure run of this revision…", disabled: revise.isPending, run: () => inspect(template, true) },
        ] : []),
        { label: "Duplicate as project draft…", disabled: revise.isPending, run: () => revise.mutate({ template, duplicate: true }) },
      ], [
        { label: "Copy workflow ID", run: () => void copy(() => version.id) },
        { label: "Copy revision ID", run: () => void copy(() => version.revision) },
        { label: "Copy graph JSON", run: () => void copy(async () => JSON.stringify((await loadTemplate(template)).body.graph, null, 2)) },
      ]] };
    })} onOpen={(id) => {
      const template = templates.find((entry) => templateKey(entry) === id);
      if (template) inspect(template);
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
          <Button type="button" disabled={revise.isPending} onClick={() => revise.mutate({ template: saved })}>
            {revise.isPending ? "Creating draft…" : "Edit in Builder"}
          </Button>
        </div>}
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
        {saved.version.destination === "project" && <WorkflowLaunchPicker key={`launch:${templateKey(saved)}:${initialLaunch ?? "default"}`} projectId={projectId}
          checkoutId={checkoutId} preferred={saved.version} openWorkspace={openWorkspace} initiallyExpanded={initialLaunch !== undefined} />}
      </>}
    </section>}
  </section>;
}
