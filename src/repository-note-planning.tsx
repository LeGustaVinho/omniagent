import { invoke } from "@tauri-apps/api/core";
import { skipToken, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef } from "react";
import { Button } from "@/components/ui/button";
import { FileDialog } from "@/ide-file-workspace";
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
    {Boolean(plans.data?.length) && <section className="pipeline-stack" aria-label="Planning runs for this revision">
      <h3>Planning runs</h3>
      <ul className="pipeline-stack">
        {plans.data?.map(({ runId, task }) => <li key={runId} className="pipeline-actions">
          <Button type="button" disabled={launch.isPending} variant="outline" onClick={() => openDelivery(runId)}>
            Open planning{task ? ` for ${task.title}` : ` · ${runId.slice(0, 8)}`}
          </Button>
        </li>)}
      </ul>
    </section>}
  </FileDialog>;
}
