import { useEffect, useId, useRef, useState, type FormEvent } from "react";
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

type ResourceFormProps = {
  id: string; disabled: boolean; onSave: (resource: Resource) => void; onValidity: (valid: boolean) => void;
};

function SchemaForm({ id, initial, disabled, onSave, onValidity }: ResourceFormProps & {
  initial: Extract<Resource, { kind: "schema" }>;
}) {
  const [draft, setDraft] = useState(initial);
  const valid = Boolean(draft.name.trim());
  useEffect(() => onValidity(valid), [valid, onValidity]);
  function submit(event: FormEvent) {
    event.preventDefault();
    if (valid && !disabled) onSave({ ...draft, name: draft.name.trim() });
  }
  return <form id={id} className="pipeline-stack" onSubmit={submit}>
    <WorkflowSchemaEditor name={draft.name} fields={draft.fields} disabled={disabled}
      onChange={(name, fields) => setDraft({ ...draft, name, fields })} />
  </form>;
}

function RoleForm({ id, projectId, checkoutId, initial, disabled, onSave, onValidity }: Scope & ResourceFormProps & {
  initial: TemplateDraft;
}) {
  const [draft, setDraft] = useState(initial);
  const validation = useRoleValidation(projectId, checkoutId, draft, true);
  useEffect(() => onValidity(validation.canSave), [validation.canSave, onValidity]);
  return <form id={id} className="pipeline-stack pipeline-form workflow-role-form" onSubmit={(event) => {
    event.preventDefault();
    if (validation.canSave && !disabled) onSave({ kind: "role", template: draft });
  }}>
    <WorkflowRoleFields projectId={projectId} checkoutId={checkoutId} draft={draft}
      disabled={disabled} validation={validation} onChange={setDraft} />
  </form>;
}

export function WorkflowResourceActions({ projectId, checkoutId, kind, label, selected, draftResource, onSaved }: Scope & {
  kind: Resource["kind"]; label: string; selected?: ReusableReference; draftResource?: DraftResource;
  onSaved: (reference: Extract<ReusableReference, { source: "published" }>) => void;
}) {
  const [mode, setMode] = useState<"create" | "edit">();
  const [valid, setValid] = useState(false);
  const id = useId();
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
    setValid(false);
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
      wide={kind === "role"} open={!!mode} closeDisabled={save.isPending}
      onOpenChange={(open) => { if (!open && !save.isPending) setMode(undefined); }}
      onCloseAutoFocus={(event) => { event.preventDefault(); origin.current?.focus({ preventScroll: true }); }}
      footer={<>
        {save.error && <p role="alert" className="workflow-dialog-error">{String(save.error)}</p>}
        <Button type="button" variant="outline" disabled={save.isPending} onClick={() => setMode(undefined)}>Cancel</Button>
        <Button type="submit" form={id} disabled={!initial || !valid || save.isPending}>
          {save.isPending ? "Saving…" : "Save and use"}
        </Button>
      </>}>
      {base && artifact.isPending ? <p role="status">Loading revision…</p>
        : initial?.kind === "schema" ? <SchemaForm id={id} initial={initial} disabled={save.isPending} onSave={save.mutate} onValidity={setValid} />
        : initial?.kind === "role" ? <RoleForm projectId={projectId} checkoutId={checkoutId} initial={initial.template}
          id={id} disabled={save.isPending} onSave={save.mutate} onValidity={setValid} />
        : <p role="alert">{String(artifact.error ?? "This revision is unavailable.")}</p>}
    </WorkflowBuilderDialog>
  </div>;
}
