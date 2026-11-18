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
  const [confirmation, setConfirmation] = useState<{ revision: string | null; name: string; memberCount: number }>();
  const row = useRef<HTMLLIElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const { copy, feedback } = useContextActionFeedback();
  function refreshDrafts() {
    return Promise.all([
      client.invalidateQueries({ queryKey: ["workflow-packages", projectId], exact: true }),
      client.invalidateQueries({ queryKey: ["project-workflow-graph-drafts", projectId] }),
    ]);
  }
  const remove = useMutation({
    mutationFn: (expectedRevision: string | null) => invoke("delete_workflow_draft", { projectId, draftId, kind, expectedRevision }),
    onSuccess: async () => {
      const list = row.current?.closest("section");
      const nearby = (row.current?.nextElementSibling ?? row.current?.previousElementSibling)?.querySelector<HTMLElement>("button");
      await refreshDrafts();
      client.removeQueries({ queryKey: [kind === "package" ? "workflow-packages" : "workflow-authoring", projectId, draftId] });
      client.removeQueries({ queryKey: ["workflow-package-preview", projectId, draftId] });
      setConfirmation(undefined);
      requestAnimationFrame(() => {
        const target = nearby?.isConnected ? nearby : list?.querySelector<HTMLElement>("button") ?? list;
        target?.focus();
      });
    },
  });
  function close() {
    setConfirmation(undefined);
    if (remove.error) void refreshDrafts();
  }
  const confirmedName = confirmation?.name ?? name;
  const confirmedCount = confirmation?.memberCount ?? memberCount;
  return <li className="workflow-draft-row" ref={row}>
    <ContextActions label={name} className="workflow-draft-menu" groups={[[
      { label: openLabel, run: open },
      { label: "Copy draft ID", run: () => copy(draftId) },
      ...(revision ? [{ label: "Copy draft revision", run: () => copy(revision) }] : []),
    ], published ? [] : [{ label: "Delete draft", destructive: true, run: () => {
      remove.reset();
      setConfirmation({ revision, name, memberCount });
    } }]]}>
      <div><strong>{name}</strong>{kind === "graph" && revision && <p>Revision <code>{revision.slice(0, 12)}</code></p>}</div>
    </ContextActions>
    <Button type="button" variant="outline" aria-label={openAriaLabel} onClick={open}>{openLabel} <ArrowRight size={15} aria-hidden /></Button>
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    <WorkflowBuilderDialog title="Delete draft?" description={
      `Permanently delete “${confirmedName}” and its saved draft revisions.${confirmedCount > 1 ? ` This includes all ${confirmedCount} members of the draft package.` : ""}`
    } open={!!confirmation} closeDisabled={remove.isPending} onOpenChange={(next) => { if (!next && !remove.isPending) close(); }}
      onOpenAutoFocus={(event) => { event.preventDefault(); cancel.current?.focus(); }}
      onCloseAutoFocus={(event) => { event.preventDefault(); row.current?.querySelector<HTMLElement>(".context-actions-more")?.focus(); }}
      footer={<>
        {remove.error && <p role="alert" className="workflow-dialog-error">{String(remove.error)}</p>}
        <Button ref={cancel} type="button" variant="outline" disabled={remove.isPending} onClick={close}>Cancel</Button>
        <Button type="button" className="workflow-delete-button" disabled={remove.isPending}
          onClick={() => { if (confirmation) remove.mutate(confirmation.revision); }}>
          {remove.isPending ? "Deleting…" : "Delete draft"}
        </Button>
      </>} />
  </li>;
}
