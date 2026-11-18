import { useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";

export function WorkflowDraftRow({ projectId, draftId, name, revision, kind, published, memberCount = 1, open, openLabel, openAriaLabel }: {
  projectId: string; draftId: string; name: string; revision: string | null; kind: "graph" | "package";
  published?: boolean; memberCount?: number; open: () => void; openLabel: string; openAriaLabel?: string;
}) {
  const client = useQueryClient();
  const [confirm, setConfirm] = useState(false);
  const row = useRef<HTMLLIElement>(null);
  const { copy, feedback } = useContextActionFeedback();
  const remove = useMutation({
    mutationFn: () => invoke("delete_workflow_draft", { projectId, draftId, kind, expectedRevision: revision }),
    onSuccess: async () => {
      const list = row.current?.closest("section");
      await Promise.all([
        client.invalidateQueries({ queryKey: ["workflow-packages", projectId], exact: true }),
        client.invalidateQueries({ queryKey: ["project-workflow-graph-drafts", projectId] }),
      ]);
      client.removeQueries({ queryKey: [kind === "package" ? "workflow-packages" : "workflow-authoring", projectId, draftId] });
      client.removeQueries({ queryKey: ["workflow-package-preview", projectId, draftId] });
      setConfirm(false);
      requestAnimationFrame(() => list?.querySelector<HTMLElement>("button, [tabindex='-1']")?.focus());
    },
  });
  return <li ref={row}>
    <ContextActions label={name} className="workflow-draft-menu" groups={[[
      { label: openLabel, run: open },
      { label: "Copy draft ID", run: () => copy(draftId) },
      ...(revision ? [{ label: "Copy draft revision", run: () => copy(revision) }] : []),
    ], published ? [] : [{ label: "Delete draft", destructive: true, run: () => { remove.reset(); setConfirm(true); } }]]}>
      <div><strong>{name}</strong>{kind === "graph" && revision && <p>Revision <code>{revision.slice(0, 12)}</code></p>}</div>
    </ContextActions>
    <Button type="button" variant="outline" aria-label={openAriaLabel} onClick={open}>{openLabel} <ArrowRight size={15} aria-hidden /></Button>
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    <WorkflowBuilderDialog title="Delete draft?" description={
      `Permanently delete “${name}” and its saved draft revisions.${memberCount > 1 ? ` This includes all ${memberCount} members of the draft package.` : ""}`
    } open={confirm} onOpenChange={(next) => { if (!remove.isPending) setConfirm(next); }}
      onCloseAutoFocus={(event) => { event.preventDefault(); row.current?.querySelector<HTMLElement>(".context-actions-more")?.focus(); }}>
      {remove.error && <p role="alert">{String(remove.error)}</p>}
      <div className="pipeline-actions justify-end">
        <Button type="button" variant="outline" disabled={remove.isPending} onClick={() => setConfirm(false)}>Cancel</Button>
        <Button type="button" className="workflow-delete-button" disabled={remove.isPending} onClick={() => remove.mutate()}>
          {remove.isPending ? "Deleting…" : "Delete draft"}
        </Button>
      </div>
    </WorkflowBuilderDialog>
  </li>;
}
