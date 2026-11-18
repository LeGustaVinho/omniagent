import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, Braces, Layers3, Plus, Waypoints } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PipelineSectionHeading } from "@/pipeline-layout";
import { packageDraftTarget, schemaMember, WorkflowTemplateChooser, type PackageDraft, type PackageSummary } from "@/workflow-authoring-package";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import type { WorkflowCatalogItem } from "@/workflow-node-picker";
import { defaultTypedPrompt } from "@/workflow-prompt-model";
import type { ReusableBody, ReusableReference } from "@/workflow-reusable";
import type { WorkspaceProps } from "@/workbench-contract";
import { workflowTemplates } from "@/workflow-templates";

const sections = [
  { key: "schema", label: "Schemas", title: "Schema", icon: Braces, description: "Define the data nodes can pass to each other.",
    includes: ({ version }: WorkflowCatalogItem) => version.kind === "schema" },
  { key: "prefab", label: "Node Prefabs", title: "Node Prefab", icon: Layers3, description: "Save a reusable AI Node with a Role, prompt, and Schemas.",
    includes: ({ nodePrefab }: WorkflowCatalogItem) => Boolean(nodePrefab) },
  { key: "function", label: "Functions", title: "Function", icon: Waypoints, description: "Build a reusable graph between Start and Return.",
    includes: ({ version, prompt, typedPrompt }: WorkflowCatalogItem) => version.kind === "subflow" && !prompt && !typedPrompt },
] as const;

const nativeCategories = [
  ["subflow", "Functions"], ["schema", "Schemas"], ["prefab", "Node Prefabs"], ["role", "Roles"], ["prompt", "Prompts"],
] as const;

type DraftKind = typeof sections[number]["key"];
type DraftRequest = { kind: DraftKind; name: string } | { kind: "published"; item: WorkflowCatalogItem };
type DraftMember = { key: string; name: string; body: ReusableBody; references?: Record<string, ReusableReference>;
  base?: WorkflowCatalogItem["version"]; destination?: string };

function newMember(kind: DraftKind, name: string): DraftMember {
  switch (kind) {
    case "schema": return schemaMember(kind, name);
    case "prefab": return { key: kind, name, body: { kind: "prompt", prompt: { ...defaultTypedPrompt(), nodePrefab: true } } };
    case "function": return { key: kind, name, body: {
      kind: "subflow", graph: { name, nodes: [
        { id: "start", kind: "start", configuration: { kind: "subflow_entry", outputs: [
          { name: "ready", value: { kind: "control" } },
          { name: "inputs", value: { kind: "data", artifact: "json_object" } },
        ] } },
        { id: "return", kind: "end", configuration: { kind: "end" } },
      ], wires: [
        { from: { node: "start", port: "ready" }, to: { node: "return", port: "ready" } },
        { from: { node: "start", port: "inputs" }, to: { node: "return", port: "result" } },
      ] },
      inputs: { ready: { node: "start", port: "ready" }, input: { node: "start", port: "inputs" } },
      outputs: { completed: { node: "return", port: "completed" }, result: { node: "return", port: "result" } },
    } };
  }
}

export function WorkflowLibrary({ projectId, openWorkspace }: { projectId: string; openWorkspace: WorkspaceProps["openWorkspace"] }) {
  const client = useQueryClient();
  const [open, setOpen] = useState<DraftKind>();
  const [name, setName] = useState("");
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<WorkflowCatalogItem[]>("workflow_package_catalog", { projectId }),
  });
  const drafts = useQuery({
    queryKey: ["workflow-packages", projectId],
    queryFn: () => invoke<PackageSummary[]>("list_workflow_packages", { projectId }),
    select: (items) => items.filter(({ standalonePipeline, published }) => !standalonePipeline && !published),
  });
  function openDraft(result: PackageDraft) {
    client.setQueryData(["workflow-packages", projectId, result.id], result);
    void client.invalidateQueries({ queryKey: ["workflow-packages", projectId], exact: true });
    openWorkspace("pipelines", packageDraftTarget(result.id));
  }
  const draft = useMutation({
    mutationFn: async (request: DraftRequest) => {
      let member: DraftMember;
      if (request.kind === "published") {
        const { item } = request;
        const artifact = await invoke<{ body: ReusableBody; references: Record<string, ReusableReference> } | null>(
          "read_published_workflow_package_artifact", { projectId, version: item.version });
        if (!artifact) throw new Error("The selected artifact revision is unavailable.");
        member = { key: "artifact", name: item.name, body: artifact.body, references: artifact.references,
          base: item.version, destination: item.version.destination };
      } else {
        member = newMember(request.kind, request.name);
      }
      return invoke<PackageDraft>("create_workflow_package", { projectId, conversationId: null, package: { name: member.name, members: [member] } });
    },
    onSuccess: (result) => { setOpen(undefined); openDraft(result); },
  });
  const items = catalog.data ?? [];
  return <section className="pipeline-view-content pipeline-stack" aria-label="Workflow library">
    <PipelineSectionHeading title="Library" icon={<Layers3 size={20} aria-hidden />} />
    <section className="workflow-library-section" aria-label="Native templates">
      <h3>Native templates</h3>
      {workflowTemplates.map((template) => <article className="pipeline-stack" key={template.key} aria-label={template.name}>
        <header className="pipeline-actions justify-between">
          <h4>{template.name}</h4>
          <WorkflowTemplateChooser projectId={projectId} initialTemplateKey={template.key} onCreated={openDraft} />
        </header>
        {nativeCategories.map(([kind, label]) => {
          const members = template.members.filter((member) => member.kind === kind);
          return !!members.length && <details key={kind}>
            <summary>{label} ({members.length})</summary>
            <ul aria-label={label}>{members.map(({ key, name }) => <li key={key}><strong>{name}</strong></li>)}</ul>
          </details>;
        })}
      </article>)}
    </section>
    {(catalog.error || drafts.error || !open && draft.error) && <p role="alert">{String(catalog.error ?? drafts.error ?? draft.error)}</p>}
    {catalog.isPending && <p role="status">Loading published artifacts…</p>}
    {sections.map(({ key, label, title, description, icon: Icon, includes }) => {
      const list = items.filter(includes);
      return <section className="workflow-library-section" key={key} aria-label={label}>
        <header className="pipeline-actions justify-between">
          <h3><Icon size={17} aria-hidden /> {label}</h3>
          <WorkflowBuilderDialog title={`New ${title}`} description={description} open={open === key}
            onOpenChange={(next) => {
              if (draft.isPending) return;
              if (next) { draft.reset(); setName(""); }
              setOpen(next ? key : undefined);
            }} trigger={<Button type="button" variant="outline" disabled={draft.isPending}>
              <Plus size={15} aria-hidden /> New {title}
            </Button>}>
            <form className="pipeline-stack pipeline-form" onSubmit={(event) => {
              event.preventDefault();
              if (name.trim() && !draft.isPending) draft.mutate({ kind: key, name: name.trim() });
            }}>
              <label>Name<input value={name} disabled={draft.isPending} onChange={(event) => setName(event.currentTarget.value)}
                required maxLength={160} autoComplete="off" /></label>
              <Button type="submit" className="w-fit" disabled={!name.trim() || draft.isPending}>Create {title}</Button>
              {draft.error && <p role="alert">{String(draft.error)}</p>}
            </form>
          </WorkflowBuilderDialog>
        </header>
        {list.length ? <ul>{list.map((item) => <li key={JSON.stringify(item.version)}>
          <div><strong>{item.name}</strong><code>{item.version.revision.slice(0, 10)}</code></div>
          <Button type="button" variant="ghost" disabled={draft.isPending} aria-label={`Open revision of ${item.name}`}
            onClick={() => draft.mutate({ kind: "published", item })}>Open <ArrowUpRight size={15} aria-hidden /></Button>
          {!!item.blockers.length && <small>{item.blockers.join(" ")}</small>}
        </li>)}</ul> : !catalog.isPending && <p className="text-sm text-muted-foreground">No published {label.toLowerCase()}.</p>}
      </section>;
    })}
    {!!drafts.data?.length && <section className="workflow-library-section" aria-label="Reusable drafts">
      <h3>Reusable drafts</h3>
      <ul>{drafts.data.map(({ id, name }) => <li key={id}>
        <strong>{name}</strong>
        <Button type="button" variant="ghost" aria-label={`Open draft of ${name}`}
          onClick={() => openWorkspace("pipelines", packageDraftTarget(id))}>Open draft <ArrowUpRight size={15} aria-hidden /></Button>
      </li>)}</ul>
    </section>}
  </section>;
}
