import { invoke } from "@tauri-apps/api/core";
import { skipToken, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef } from "react";
import { Button } from "@/components/ui/button";
import { FileDialog, type FilePanelApi } from "@/ide-file-workspace";
import { RepositoryMarkdownPicker } from "@/repository-markdown-picker";
import type { NoteRevision } from "@/repository-note-buffers";
import type { WorkspaceProps } from "@/workbench-contract";

type LinkedTask = { id: string; title: string };
type NotePlan = { runId: string; task: LinkedTask | null };

export const repositoryNotePlanningKey = (projectId: string) => ["repository-note-planning", projectId] as const;

export function useRepositoryNotePlans(projectId: string, source?: NoteRevision) {
  return useQuery({
    queryKey: [...repositoryNotePlanningKey(projectId), source?.checkoutId, source?.id],
    queryFn: source ? () => invoke<NotePlan[]>("repository_note_planning", {
      projectId, checkoutId: source.checkoutId, revisionId: source.id,
    }) : skipToken,
  });
}

function PlanningRuns({ plans, openDelivery, openTask, busy }: {
  plans: readonly NotePlan[];
  openDelivery: (runId: string) => void;
  openTask?: (taskId: string) => void;
  busy?: boolean;
}) {
  if (!plans.length) return null;
  return <section className="pipeline-stack" aria-label="Planning runs for this revision">
    <h3>Planning runs</h3>
    <ul className="pipeline-stack">
      {plans.map(({ runId, task }) => <li key={runId} className="pipeline-actions">
        <Button type="button" disabled={busy} variant="outline" title={runId} onClick={() => openDelivery(runId)}>
          {task ? `Open planning for ${task.title}${openTask ? ` · ${runId.slice(0, 8)}` : ""}` : `Open planning · ${runId.slice(0, 8)}`}
        </Button>
        {task && openTask && <Button type="button" variant="outline" onClick={() => openTask(task.id)}>Open Task {task.title}</Button>}
      </li>)}
    </ul>
  </section>;
}

export function LinkedRepositoryNotePlanning({ projectId, source, openWorkspace, onClose, ownerPanel }: {
  projectId: string; source: NoteRevision; openWorkspace: WorkspaceProps["openWorkspace"]; onClose: () => void; ownerPanel?: FilePanelApi;
}) {
  const plans = useRepositoryNotePlans(projectId, source);
  const navigate: WorkspaceProps["openWorkspace"] = (workspace, target) => {
    onClose();
    openWorkspace(workspace, target);
  };
  return <FileDialog title="Linked planning and delivery" description={`${source.relativePath} · ${source.sha256.slice(0, 12)}`}
    ownerPanel={ownerPanel} onClose={onClose}>
    {plans.isPending && <p role="status">Loading planning runs for this saved revision…</p>}
    {plans.error && <p role="alert">{String(plans.error)}</p>}
    {plans.isSuccess && !plans.data.length && <p>No planning runs are linked to this saved revision.</p>}
    <PlanningRuns plans={plans.data ?? []} openDelivery={(runId) => navigate("pipelines", runId)} openTask={(taskId) => navigate("tasks", taskId)} />
  </FileDialog>;
}

export function RepositoryNotePlanning({ projectId, source, openWorkspace, onClose }: {
  projectId: string; source: NoteRevision; openWorkspace: WorkspaceProps["openWorkspace"]; onClose: () => void;
}) {
  const client = useQueryClient();
  const requestId = useRef<string | undefined>(undefined);
  const plans = useRepositoryNotePlans(projectId, source);

  function openDelivery(runId: string) {
    onClose();
    openWorkspace("pipelines", runId);
  }

  const launch = useMutation({
    mutationFn: () => invoke<{ id: string }>("start_repository_delivery", {
      projectId, checkoutId: source.checkoutId, revisionId: source.id, requestId: requestId.current,
    }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: repositoryNotePlanningKey(projectId) });
      void client.invalidateQueries({ queryKey: ["repository-delivery", projectId] });
    },
  });

  return <FileDialog title="Plan from note" description="Review the saved note before opening Repository delivery." onClose={onClose}>
    <RepositoryMarkdownPicker checkoutId={source.checkoutId} pinnedRevisionId={source.id} launchLabel="Start planning"
      delivering={launch.isPending} onDeliver={() => {
        requestId.current ??= crypto.randomUUID();
        launch.mutate(undefined, { onSuccess: ({ id }) => openDelivery(id) });
      }} />
    {(launch.error ?? plans.error) && <p role="alert">{String(launch.error ?? plans.error)}</p>}
    <PlanningRuns plans={plans.data ?? []} openDelivery={openDelivery} busy={launch.isPending} />
  </FileDialog>;
}
