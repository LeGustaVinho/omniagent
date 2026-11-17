import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, Braces, Layers3, Plus, Waypoints } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PipelineSectionHeading } from "@/pipeline-layout";
import { packageDraftTarget, schemaMember, type PackageDraft } from "@/workflow-authoring-package";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import type { WorkflowCatalogItem } from "@/workflow-node-picker";
import type { ReusableBody, ReusableReference } from "@/workflow-reusable";
import type { WorkspaceProps } from "@/workbench-contract";

export function WorkflowLibrary({ projectId, openWorkspace }: { projectId: string; openWorkspace: WorkspaceProps["openWorkspace"] }) {
  const client = useQueryClient();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<WorkflowCatalogItem[]>("workflow_package_catalog", { projectId }),
  });
  const draft = useMutation({
    mutationFn: async (item: WorkflowCatalogItem | null) => {
      const label = item?.name ?? name.trim();
      let member: { key: string; name: string; body: ReusableBody; references?: Record<string, ReusableReference>;
        base?: WorkflowCatalogItem["version"]; destination?: string } = schemaMember("schema", label);
      if (item) {
        const artifact = await invoke<{ body: ReusableBody; references: Record<string, ReusableReference> } | null>(
          "read_published_workflow_package_artifact", { projectId, version: item.version });
        if (!artifact) throw new Error("The selected artifact revision is unavailable.");
        member = { ...member, body: artifact.body, references: artifact.references, base: item.version, destination: item.version.destination };
      }
      return invoke<PackageDraft>("create_workflow_package", { projectId, conversationId: null, package: { name: label, members: [member] } });
    },
    onSuccess: (result) => {
      client.setQueryData(["workflow-packages", projectId, result.id], result);
      void client.invalidateQueries({ queryKey: ["workflow-packages", projectId], exact: true });
      setOpen(false);
      openWorkspace("pipelines", packageDraftTarget(result.id));
    },
  });
  const items = catalog.data ?? [];
  return <section className="pipeline-view-content pipeline-stack" aria-label="Workflow library">
    <div className="pipeline-actions justify-between">
      <PipelineSectionHeading title="Library" icon={<Layers3 size={20} aria-hidden />} />
      <WorkflowBuilderDialog title="New Schema" description="Define data your pipeline can pass between nodes."
        open={open} onOpenChange={(next) => { if (next) { draft.reset(); setName(""); } setOpen(next); }}
        trigger={<Button type="button"><Plus size={15} aria-hidden /> New Schema</Button>}>
        <form className="pipeline-stack pipeline-form" onSubmit={(event) => {
          event.preventDefault();
          if (name.trim()) draft.mutate(null);
        }}>
          <label>Name<input value={name} onChange={(event) => setName(event.currentTarget.value)} required autoComplete="off" /></label>
          <Button type="submit" className="w-fit" disabled={!name.trim() || draft.isPending}>Create Schema</Button>
          {draft.error && <p role="alert">{String(draft.error)}</p>}
        </form>
      </WorkflowBuilderDialog>
    </div>
    {(catalog.error || !open && draft.error) && <p role="alert">{String(catalog.error ?? draft.error)}</p>}
    {catalog.isPending && <p role="status">Loading published artifacts…</p>}
    {[
      { label: "Schemas", icon: Braces, list: items.filter(({ version }) => version.kind === "schema") },
      { label: "Node Prefabs", icon: Layers3, list: items.filter(({ nodePrefab }) => nodePrefab) },
      { label: "Functions", icon: Waypoints, list: items.filter(({ version, prompt, typedPrompt }) => version.kind === "subflow" && !prompt && !typedPrompt) },
    ].map(({ label, icon: Icon, list }) => <section className="workflow-library-section" key={label} aria-label={label}>
      <h3><Icon size={17} aria-hidden /> {label}</h3>
      {list.length ? <ul>{list.map((item) => <li key={JSON.stringify(item.version)}>
        <div><strong>{item.name}</strong><code>{item.version.revision.slice(0, 10)}</code></div>
        <Button type="button" variant="ghost" disabled={draft.isPending} aria-label={`Open revision of ${item.name}`}
          onClick={() => draft.mutate(item)}>Open <ArrowUpRight size={15} aria-hidden /></Button>
        {!!item.blockers.length && <small>{item.blockers.join(" ")}</small>}
      </li>)}</ul> : <p className="text-sm text-muted-foreground">No published {label.toLowerCase()}.</p>}
    </section>)}
  </section>;
}
