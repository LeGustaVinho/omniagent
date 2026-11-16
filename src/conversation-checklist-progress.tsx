import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

type Availability = "ready" | "pending_dispatch" | "inactive" | "bridge_unavailable" | "bridge_unconfirmed";
type ChecklistItem = { id: string; revision: number; body: string; checked: boolean };
type Progress = {
  access: { taskId: string; mode: "track_progress" | "run_by_item"; availability: Availability; selectedItemId: string | null };
  checklist: { id: string; items: ChecklistItem[] };
};

const accessLabels: Record<Availability, string> = {
  ready: "Agent updates available",
  pending_dispatch: "Agent updates wait for dispatch",
  inactive: "Run ended. Manual updates are available.",
  bridge_unavailable: "Agent updates are unavailable for this profile",
  bridge_unconfirmed: "Agent update access is unconfirmed",
};

export function ConversationChecklistProgress({ projectId, runId, live }: {
  projectId: string;
  runId: string;
  live: boolean;
}) {
  const client = useQueryClient();
  const key = ["conversation-run", runId, "checklist-progress", projectId];
  const progress = useQuery({
    queryKey: key,
    queryFn: () => invoke<Progress | null>("conversation_checklist_progress", { projectId, runId }),
    refetchInterval: live ? 800 : false,
  });
  const change = useMutation({
    mutationFn: ({ taskId, item, checked }: { taskId: string; item: ChecklistItem; checked: boolean }) =>
      invoke("set_conversation_checklist_item_checked", {
        projectId, taskId, itemId: item.id, expectedRevision: item.revision, checked,
      }),
    onSettled: () => client.invalidateQueries({ queryKey: key }),
  });

  if (progress.error) return <p role="alert" className="text-xs">{String(progress.error)}</p>;
  if (!progress.data) return null;
  const { access, checklist } = progress.data;
  const items = access.mode === "run_by_item" ? checklist.items.filter((item) => item.id === access.selectedItemId) : checklist.items;
  return <details>
    <summary>Checklist progress</summary>
    <p className="mt-2 text-xs text-muted-foreground">{access.mode === "run_by_item" ? "Run by item" : "Track progress"} · {accessLabels[access.availability]}</p>
    <ul aria-label="Checklist progress" className="mt-2 grid gap-2 text-xs">
      {items.map((item) => <li key={item.id}>
        <label className="flex items-start gap-2">
          <input type="checkbox" checked={item.checked} disabled={change.isPending}
            onChange={(event) => change.mutate({ taskId: access.taskId, item, checked: event.currentTarget.checked })} />
          <span className="whitespace-pre-wrap break-words">{item.body}</span>
        </label>
      </li>)}
    </ul>
    {change.error && <p role="alert" className="text-xs">{String(change.error)}</p>}
  </details>;
}
