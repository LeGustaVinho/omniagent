import { useState } from "react";
import { Bot, Braces, CirclePause, GitFork, GitMerge, Layers3, Plus, Repeat2, UserRoundCheck, Waypoints } from "lucide-react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import { reusableNodeQuery, type ReusableDescription, type ReusableVersion } from "@/workflow-reusable";
import { invoke } from "@tauri-apps/api/core";

const nodeTypes = [
  { kind: "subflow", label: "AI Node Agent", icon: Bot },
  { kind: "validate_output", label: "Validate Output", icon: Braces },
  { kind: "fork", label: "Parallel / Fork", icon: GitFork },
  { kind: "join_results", label: "Join", icon: GitMerge },
  { kind: "wait", label: "Wait", icon: CirclePause },
  { kind: "loop", label: "Loop", icon: Repeat2 },
  { kind: "human", label: "Human in the Loop", icon: UserRoundCheck },
] as const;

export const builderNodeKinds = nodeTypes.map(({ kind }) => kind);
export type WorkflowCatalogItem = { name: string; version: ReusableVersion; blockers: string[]; typedPrompt?: boolean; nodePrefab?: boolean; prompt?: boolean };

export function WorkflowNodePicker({ projectId, disabled, onAdd, onReuse }: {
  projectId: string; disabled: boolean; onAdd: (kind: string) => void; onReuse: (node: ReusableDescription) => void;
}) {
  const [open, setOpen] = useState(false);
  const catalog = useQuery({
    queryKey: ["workflow-package-catalog", projectId],
    queryFn: () => invoke<WorkflowCatalogItem[]>("workflow_package_catalog", { projectId }),
    enabled: open,
  });
  const load = useMutation({
    mutationFn: (version: ReusableVersion) => reusableNodeQuery(projectId, version).queryFn(),
    onSuccess: (node) => { onReuse(node); setOpen(false); },
  });
  const reusable = (catalog.data ?? []).filter(({ version, nodePrefab, prompt }) =>
    version.kind === "subflow" && (nodePrefab || !prompt));
  return <WorkflowBuilderDialog title="Add node" description="Choose a node or reuse a published configuration."
    open={open} onOpenChange={setOpen} trigger={<Button type="button" variant="outline" disabled={disabled}>
      <Plus size={15} aria-hidden /> Add node
    </Button>}>
    <div className="workflow-node-picker-list">
      {nodeTypes.map(({ kind, label, icon: Icon }) => <Button type="button" key={kind} variant="outline" onClick={() => {
        onAdd(kind);
        setOpen(false);
      }}><Icon size={16} aria-hidden /> {label}</Button>)}
    </div>
    {catalog.isLoading && <p role="status">Loading reusable nodes…</p>}
    {reusable.length > 0 && <fieldset className="pipeline-stack">
      <legend>Reusable nodes</legend>
      {reusable.map(({ name, version, nodePrefab, blockers }) => <Button key={JSON.stringify(version)} type="button"
        variant="outline" className="justify-start" disabled={load.isPending || !!blockers.length}
        title={blockers.join(" ") || undefined} onClick={() => load.mutate(version)}>
        {nodePrefab ? <Layers3 size={16} aria-hidden /> : <Waypoints size={16} aria-hidden />}
        {name}<span className="text-muted-foreground">{nodePrefab ? "Node Prefab" : "Function"}</span>
      </Button>)}
    </fieldset>}
    {(catalog.error ?? load.error) && <p role="alert">{String(catalog.error ?? load.error)}</p>}
  </WorkflowBuilderDialog>;
}
