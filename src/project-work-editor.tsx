import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { AgileWorkDialog } from "@/agile-layout";
import { AgileNotePreview, AgileWorkEditor, workFormLines, type AgileFormValues } from "@/agile-work-editor";
import { Button } from "@/components/ui/button";
import { ConversationMarkdown } from "@/conversation-markdown";
import { resolveWiki } from "@/knowledge-document";
import { LIVE_MODEL_PROVIDERS, sameModelSelection, useLiveModelOptions } from "@/live-models";
import { canMove, statusLabel, workLabel, workflowStates, type AgileRow, type WorkKind, type WorkflowStatus } from "@/project-agile-model";
import type { DocumentOpen, NoteCatalogEntry } from "@/repository-note-buffers";
import type { WorkspaceProps } from "@/workbench-contract";

type Placement = { kind: "epic" } | { kind: "task"; epicId: string | null } | { kind: "subtask"; parentTaskId: string };
type Assignment = Omit<ModelSelection, "modelId"> & { modelId: string | null };
type Settings = {
  priority: number; acceptanceCriteria: string[]; prerequisites: string[]; blockers: string[];
  checklistMode: "track_progress" | "run_by_item" | null; branchStrategy: "independent" | "shared_branch_per_task" | null;
  assignment: Assignment | null;
};
type Draft = { description: string; settings: Settings | null; state: WorkflowStatus | null };
type Editor = { title: string; placement: Placement; draft: Draft; branchLocked: boolean; version: string };
export type ProjectEditorSelection = { kind: WorkKind; work?: AgileRow; parentId?: string; state?: WorkflowStatus; copyOf?: AgileRow };
export type WorkspaceWorkInput = { title: string; placement: Placement; draft?: Draft } & ({ workId: string; expected: string; draft: Draft } | { workId?: undefined });

function placementFor(kind: WorkKind, parentId: string): Placement {
  if (kind === "epic") return { kind };
  return kind === "task" ? { kind, epicId: parentId || null } : { kind, parentTaskId: parentId };
}

export function ProjectWorkEditor({ checkout, selection, rows, busy, error, close, save, resetError }:
  Pick<WorkspaceProps, "checkout"> & {
    selection: ProjectEditorSelection; rows: AgileRow[]; busy: boolean; error: Error | null; close: () => void;
    save: (input: WorkspaceWorkInput) => void; resetError: () => void;
  }) {
  const { projectId } = checkout;
  const workId = selection.work?.id;
  const sourceId = workId ?? selection.copyOf?.id;
  const activator = useState(() => document.activeElement)[0];
  const [reloadCount, setReloadCount] = useState(0);
  const info = useQuery({
    queryKey: ["project-tasks", projectId, sourceId, "editor"],
    queryFn: () => invoke<Editor>("workspace_work_editor", { projectId, workId: sourceId }),
    refetchOnWindowFocus: false, refetchOnMount: "always", enabled: Boolean(sourceId),
  });
  const loading = Boolean(sourceId && !info.data && info.isPending);
  async function reload() {
    resetError();
    const result = await info.refetch();
    if (result.isSuccess) setReloadCount((count) => count + 1);
  }
  return <AgileWorkDialog title={workId ? `Edit ${workLabel(selection.kind).toLowerCase()}` : selection.copyOf ? "Duplicate work" : "New work"}
    description="Edit the work details, planned assignment, dependencies and criteria." closeLabel="Close work editor" busy={busy}
    onCloseAutoFocus={(event) => {
      if (activator instanceof HTMLElement && activator.getClientRects().length) { event.preventDefault(); activator.focus(); }
    }}>
    {loading && <p role="status">Loading work fields…</p>}
    {sourceId && !info.data && info.error && <><p role="alert">{String(info.error)}</p><Button type="button" onClick={reload}>Reload saved work</Button></>}
    {(!sourceId || info.data) && <ProjectWorkForm key={`${info.data?.version ?? "new"}:${reloadCount}`} checkout={checkout}
      selection={selection} rows={rows} current={info.data} busy={busy || Boolean(sourceId && info.isFetching)}
      error={error ?? info.error} close={close} save={save}
      reload={reload} />}
  </AgileWorkDialog>;
}

function ProjectWorkForm({ checkout, selection, rows, current, busy, error, close, save, reload }:
  Pick<WorkspaceProps, "checkout"> & {
    selection: ProjectEditorSelection; rows: AgileRow[]; current?: Editor; busy: boolean; error: Error | null;
    close: () => void; save: (input: WorkspaceWorkInput) => void; reload: () => void;
  }) {
  const { checkoutId } = checkout;
  const [kind, setKind] = useState(selection.kind);
  const [assignment, setAssignment] = useState<Assignment | null>(current?.draft.settings?.assignment ?? null);
  const [linkError, setLinkError] = useState("");
  const [linkedNote, setLinkedNote] = useState<string>();
  const [values, setValues] = useState<AgileFormValues>(() => {
    const settings = current?.draft.settings;
    const saved = current?.placement;
    return {
      title: selection.copyOf ? `${current?.title ?? selection.copyOf.title} copy` : current?.title ?? "", description: current?.draft.description ?? "",
      parentId: saved?.kind === "subtask" ? saved.parentTaskId : saved?.kind === "task" ? saved.epicId ?? "" : selection.parentId ?? "",
      state: selection.copyOf ? "backlog" : current?.draft.state ?? selection.work?.status ?? selection.state ?? "backlog",
      priority: String(settings?.priority ?? 0),
      checklistMode: settings?.checklistMode ?? "", branchStrategy: settings?.branchStrategy ?? "",
      acceptanceCriteria: settings?.acceptanceCriteria.join("\n") ?? "", dependencies: settings?.prerequisites ?? [],
      blockers: settings?.blockers.join("\n") ?? "",
    };
  });
  const catalog = useQuery({ queryKey: ["project-description-note-catalog", checkoutId],
    queryFn: () => invoke<NoteCatalogEntry[]>("repository_note_catalog", { checkoutId }) });
  const note = useQuery({ queryKey: ["project-description-note-preview", checkoutId, linkedNote], enabled: Boolean(linkedNote),
    queryFn: () => invoke<DocumentOpen>("open_repository_document", { checkoutId, path: linkedNote }) });
  const notes = (catalog.data ?? []).map(({ path, frontmatter }) => ({ path, content: frontmatter ?? "" }));
  const { options, loading } = useLiveModelOptions(LIVE_MODEL_PROVIDERS, kind !== "epic");
  const workId = selection.work?.id;
  const path = `${workId ?? "new-work"}.md`;
  const parents = rows.filter((row) => row.kind === (kind === "task" ? "epic" : "task"))
    .map(({ id, title }) => ({ value: id, label: title }));
  const dependencies = rows.filter(({ kind, id }) => kind !== "epic" && id !== workId)
    .map(({ id, title }) => ({ value: id, label: title }));
  const states = kind === "epic" ? [{ value: values.state, label: statusLabel(values.state as WorkflowStatus) }]
    : workflowStates.filter(({ value }) => !selection.work || value === current?.draft.state || canMove(selection.work, value));
  const model = assignment?.modelId ? { provider: assignment.provider, profileId: assignment.profileId, modelId: assignment.modelId } : null;
  const retained = assignment && (!model || !options.some(({ selection }) => sameModelSelection(selection, model)))
    ? `Saved assignment · ${assignment.provider} · ${assignment.modelId ?? "Profile default"} · ${assignment.profileId}` : undefined;

  function submit() {
    const core = { title: values.title.trim(), placement: placementFor(kind, values.parentId) };
    const settings: Settings | null = kind === "epic" ? null : {
      priority: Number(values.priority), acceptanceCriteria: workFormLines(values.acceptanceCriteria),
      prerequisites: values.dependencies, blockers: workFormLines(values.blockers),
      checklistMode: (values.checklistMode || null) as Settings["checklistMode"],
      branchStrategy: kind === "task" ? (values.branchStrategy || null) as Settings["branchStrategy"] : null,
      assignment,
    };
    const draft: Draft = { description: values.description, settings, state: kind === "epic" ? null : values.state as WorkflowStatus };
    if (workId && current) {
      save({ ...core, workId, expected: current.version, draft });
    } else {
      const configured = draft.description || draft.state && draft.state !== "backlog" || settings && (
        settings.priority !== 0 || settings.acceptanceCriteria.length || settings.prerequisites.length || settings.blockers.length ||
        settings.checklistMode || settings.branchStrategy || settings.assignment
      );
      save(configured ? { ...core, draft } : core);
    }
  }

  return <AgileWorkEditor kind={kind} setKind={!workId ? (next) => {
    setKind(next); setValues((values) => ({ ...values, parentId: "", state: "backlog" }));
  } : undefined} values={values} change={(patch) => setValues((values) => ({ ...values, ...patch }))}
    parents={parents} states={states} dependencies={dependencies} notes={notes} path={path}
    branchLocked={Boolean(workId && current?.branchLocked)} parentLocked={kind === "subtask" && Boolean(workId && current?.branchLocked)}
    assignment={{ selected: model, options, loading, change: setAssignment, retained }}
    openNote={(link) => {
      const targets = resolveWiki(link, link.sourcePath ?? path, notes);
      if (targets.length === 1 && targets[0]) { setLinkError(""); setLinkedNote(targets[0].path); }
      else { setLinkedNote(undefined); setLinkError(targets.length ? "The note reference matches multiple files." : "The referenced note could not be found."); }
    }} descriptionPreview={linkedNote && <AgileNotePreview path={linkedNote}>
      {note.isPending ? <p role="status">Loading note…</p> : note.data?.status === "document"
        ? <ConversationMarkdown>{note.data.document.markdown}</ConversationMarkdown>
        : <p role="alert">{note.error ? String(note.error) : "This note is unavailable in the checkout."}</p>}
    </AgileNotePreview>} busy={busy} error={error ? String(error) : linkError} notices={error && workId &&
      <Button type="button" variant="outline" className="justify-self-start" onClick={reload}>Reload saved work</Button>}
    saveLabel={workId ? "Save work" : "Create work"} cancel={close} save={submit} />;
}
