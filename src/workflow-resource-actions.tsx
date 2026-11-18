import { useRef, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Pencil, Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import { emptyRoleDraft, useRoleValidation, WorkflowRoleFields, type TemplateDraft } from "@/conversation-template-editor";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import { publishedWorkflowArtifactQuery, type ReusableReference, type ReusableVersion } from "@/workflow-reusable";
import { WorkflowSchemaEditor } from "@/workflow-schema-editor";
import type { SchemaField } from "@/workflow-schema-model";

export type Resource = { kind: "schema"; name: string; fields: SchemaField[] }
  | { kind: "role"; template: TemplateDraft };
type Scope = { projectId: string; checkoutId: string };
export type DraftResource = (key: string) => Resource | undefined;
type ResourceSave = { status: "saved"; detail: { revision: { templateId: string; version: { revision: string } } } }
  | { status: "conflict" };

function SchemaForm({ initial, disabled, onSave }: {
  initial: Extract<Resource, { kind: "schema" }>; disabled: boolean; onSave: (resource: Resource) => void;
}) {
  const [draft, setDraft] = useState(initial);
  function submit(event: FormEvent) {
    event.preventDefault();
    if (draft.name.trim() && !disabled) onSave({ ...draft, name: draft.name.trim() });
  }
  return <form className="pipeline-stack" onSubmit={submit}>
    <WorkflowSchemaEditor name={draft.name} fields={draft.fields} disabled={disabled}
      onChange={(name, fields) => setDraft({ ...draft, name, fields })} />
    <Button type="submit" className="w-fit" disabled={disabled || !draft.name.trim()}>Save and use</Button>
  </form>;
}

function RoleForm({ projectId, checkoutId, initial, disabled, onSave }: Scope & {
  initial: TemplateDraft; disabled: boolean; onSave: (resource: Resource) => void;
}) {
  const [draft, setDraft] = useState(initial);
  const validation = useRoleValidation(projectId, checkoutId, draft, true);
  return <form className="pipeline-stack pipeline-form workflow-role-form" onSubmit={(event) => {
    event.preventDefault();
    if (validation.canSave && !disabled) onSave({ kind: "role", template: draft });
  }}>
    <WorkflowRoleFields projectId={projectId} checkoutId={checkoutId} draft={draft}
      disabled={disabled} validation={validation} onChange={setDraft} />
    <Button type="submit" className="w-fit" disabled={disabled || !validation.canSave}>Save and use</Button>
  </form>;
}

export function WorkflowResourceActions({ projectId, checkoutId, kind, label, selected, draftResource, onSaved }: Scope & {
  kind: Resource["kind"]; label: string; selected?: ReusableReference; draftResource?: DraftResource;
  onSaved: (reference: Extract<ReusableReference, { source: "published" }>) => void;
}) {
  const [mode, setMode] = useState<"create" | "edit">();
  const origin = useRef<HTMLButtonElement>(null);
  const client = useQueryClient();
  const target = mode === "edit" ? selected : undefined;
  const base = target?.source === "published" && target.version.kind === kind ? target.version : undefined;
  const local = selected?.source === "draft" ? draftResource?.(selected.key) : undefined;
  const canEdit = selected?.source === "published" && selected.version.kind === kind || local?.kind === kind;
  const artifact = useQuery({
    ...publishedWorkflowArtifactQuery(projectId, base ?? { kind, destination: "project", id: "", revision: "" }),
    enabled: Boolean(base),
  });
  const published = artifact.data;
  let initial: Resource | undefined;
  if (mode === "create") initial = kind === "role" ? { kind, template: emptyRoleDraft() } : { kind, name: "", fields: [] };
  else if (target?.source === "draft") initial = local;
  else if (published?.body.template && kind === "role") initial = { kind, template: published.body.template };
  else if (published?.body.fields && kind === "schema") initial = { kind, name: published.name, fields: published.body.fields };
  const save = useMutation({
    mutationFn: async (resource: Resource): Promise<ReusableVersion> => {
      if (resource.kind === "role" && base?.destination !== "library") {
        const result = await invoke<ResourceSave>("save_conversation_template", {
          projectId, checkoutId, template: resource.template,
          base: base ? { templateId: base.id, expectedRevision: base.revision } : null,
        });
        if (result.status === "conflict") throw new Error("The published Role changed. Select its current revision before editing.");
        const { templateId, version } = result.detail.revision;
        return { id: templateId, revision: version.revision, kind, destination: "project" };
      }
      const name = resource.kind === "role" ? resource.template.name.trim() : resource.name;
      const body = resource.kind === "role" ? { kind, template: resource.template } : { kind, fields: resource.fields };
      return invoke<ReusableVersion>("publish_workflow_resource", {
        projectId, member: { key: kind, name, body, base: base ?? null, destination: base?.destination ?? "project" },
      });
    },
    onSuccess: (version) => {
      onSaved({ source: "published", version });
      setMode(undefined);
      void client.invalidateQueries({ queryKey: ["workflow-package-catalog", projectId] });
      if (kind === "role") void client.invalidateQueries({ queryKey: ["conversation-templates", projectId], exact: true });
    },
  });
  function open(mode: "create" | "edit", button: HTMLButtonElement) {
    origin.current = button;
    save.reset();
    setMode(mode);
  }
  return <div className="workflow-reference-actions">
    <Button type="button" variant="ghost" aria-label={`Create ${label}`} onClick={(event) => open("create", event.currentTarget)}>
      <Plus size={14} aria-hidden /> Create
    </Button>
    {canEdit && <Button type="button" variant="ghost" aria-label={`Edit ${label}`} onClick={(event) => open("edit", event.currentTarget)}>
      <Pencil size={14} aria-hidden /> Edit
    </Button>}
    <WorkflowBuilderDialog title={mode === "edit" ? `Edit ${label}` : `Create ${label}`}
      description={`Save a reusable revision in the ${base?.destination === "library" ? "shared library" : "project"} and select it here.`}
      wide={kind === "role"} open={!!mode} onOpenChange={(open) => { if (!open && !save.isPending) setMode(undefined); }}
      onCloseAutoFocus={(event) => { event.preventDefault(); origin.current?.focus({ preventScroll: true }); }}>
      {base && artifact.isPending ? <p role="status">Loading revision…</p>
        : initial?.kind === "schema" ? <SchemaForm initial={initial} disabled={save.isPending} onSave={save.mutate} />
        : initial?.kind === "role" ? <RoleForm projectId={projectId} checkoutId={checkoutId} initial={initial.template}
          disabled={save.isPending} onSave={save.mutate} />
        : <p role="alert">{String(artifact.error ?? "This revision is unavailable.")}</p>}
      {save.error && <p role="alert">{String(save.error)}</p>}
      {save.isPending && <p role="status">Saving revision…</p>}
      <Button type="button" variant="outline" className="w-fit" disabled={save.isPending} onClick={() => setMode(undefined)}>Cancel</Button>
    </WorkflowBuilderDialog>
  </div>;
}
