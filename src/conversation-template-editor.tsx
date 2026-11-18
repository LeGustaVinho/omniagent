import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ChevronRight, Plus, Search, UserRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { EffortSelect } from "@/components/effort-select";
import { ModelPicker } from "@/components/model-picker";
import { sameModelSelection, useLiveModelOptions } from "@/live-models";
import { PipelineRolesHeading } from "@/pipeline-layout";
import type { Capability } from "@/bindings/Capability";
import type { JobRole } from "@/bindings/JobRole";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { SessionMode } from "@/bindings/SessionMode";
import { WorkflowRequirementsEditor, type StageRequirements } from "@/workflow-stage-requirements";
import { WorkflowEditorAssistance } from "@/workflow-editor-assistance";
import type { WorkspaceProps } from "@/workbench-contract";
import "./workflow-roles.css";

type ToolScope = Exclude<SessionMode, "plan">;
type ReviewPolicy = "independent" | "cross_review" | "human_approval";
type TemplateDraft = {
  name: string;
  role: JobRole;
  model: ModelSelection | null;
  effort: string | null;
  instructions: string;
  toolScope: ToolScope;
  completionContract: string;
  reviewPolicy: ReviewPolicy;
  requirements?: StageRequirements;
};
type Version = { revision: string; ordinal: number; baseRevision: string | null };
type Head = { templateId: string; version: Version; name: string; displayLabel?: string };
type Revision = { templateId: string; version: Version; template: TemplateDraft };
type Detail = { revision: Revision; history: Version[] };
type Selection = "new" | { templateId: string; revision: string };
type Save = { status: "saved"; detail: Detail } | { status: "conflict"; current: Head };
type Matrix = {
  cliVersion: string;
  transport: string;
  toolScopes: { mode: ToolScope; blocker: string | null }[];
  capabilities: { capability: Capability; status: string }[];
};

const scopeNames: Record<ToolScope, string> = {
  read_only: "Repository read",
  approved_write: "Approved worktree write",
};
const reviewNames: Record<ReviewPolicy, string> = {
  independent: "Independent review",
  cross_review: "Cross-review",
  human_approval: "Human approval",
};

function emptyDraft(): TemplateDraft {
  return {
    name: "", role: "agent", model: null, effort: null, instructions: "", toolScope: "read_only",
    completionContract: "", reviewPolicy: "human_approval",
  };
}

export function ConversationTemplateEditor({ checkout, openWorkspace }: {
  checkout: ProjectCheckout; openWorkspace?: WorkspaceProps["openWorkspace"];
}) {
  const { projectId, checkoutId } = checkout;
  const client = useQueryClient();
  const [selected, setSelected] = useState<Selection>();
  const [search, setSearch] = useState("");
  const headsKey = ["conversation-templates", projectId];
  const heads = useQuery({
    queryKey: headsKey,
    queryFn: () => invoke<Head[]>("list_conversation_templates", { projectId }),
  });
  const first = heads.data?.[0];
  const firstId = first?.templateId, firstRevision = first?.version.revision;
  useEffect(() => {
    if (firstId && firstRevision) setSelected((current) => current ?? { templateId: firstId, revision: firstRevision });
  }, [firstId, firstRevision]);
  const reference = selected === "new" ? undefined : selected;
  const revisionKey = (version: { templateId: string; revision: string } | undefined) =>
    [...headsKey, version?.templateId, version?.revision];
  const detail = useQuery({
    queryKey: revisionKey(reference),
    queryFn: () => invoke<Detail>("conversation_template_revision", { projectId, ...reference }),
    enabled: Boolean(reference),
  });
  const activeHead = heads.data?.find(({ templateId }) => templateId === reference?.templateId);
  const loadedHead = detail.data?.history[0];
  const knownHead = activeHead && (!loadedHead || activeHead.version.ordinal >= loadedHead.ordinal)
    ? activeHead.version : loadedHead;
  const query = search.trim().toLowerCase();
  const roles = heads.data ?? [];
  const visibleRoles = roles.filter(({ name, displayLabel }) => [name, displayLabel].some((label) => label?.toLowerCase().includes(query)));

  function openHead(head: Head) {
    const { templateId, version } = head;
    client.setQueryData<Head[]>(headsKey, (previous = []) => [
      ...previous.filter((entry) => entry.templateId !== templateId),
      previous.find((entry) => entry.templateId === templateId && entry.version.ordinal > version.ordinal) ?? head,
    ].sort((a, b) => a.name.localeCompare(b.name)));
    setSelected({ templateId, revision: version.revision });
  }

  async function accept(saved: Detail) {
    const { templateId, version } = saved.revision;
    await client.cancelQueries({ queryKey: headsKey, exact: true });
    client.setQueryData(revisionKey({ templateId, revision: version.revision }), saved);
    openHead({ templateId, version, name: saved.revision.template.name });
    await Promise.all([
      client.invalidateQueries({ queryKey: headsKey, exact: true }),
      client.invalidateQueries({ queryKey: ["workflow-package-catalog", projectId], exact: true }),
    ]);
  }

  return <section className="pipeline-view-content pipeline-stack workflow-roles" aria-label="Role library">
    <div className="workflow-roles-toolbar">
      <PipelineRolesHeading />
      <Button type="button" onClick={() => { setSelected("new"); setSearch(""); }}><Plus size={15} aria-hidden /> New Role</Button>
    </div>
    <div className="workflow-roles-layout">
      <section className="pipeline-box pipeline-stack workflow-roles-list" aria-label="Saved Roles">
        <h3>Saved Roles</h3>
        {roles.length > 1 && <label className="pipeline-form workflow-roles-search">
          <span className="sr-only">Search Roles</span><Search size={15} aria-hidden />
          <input type="search" placeholder="Search Roles" value={search} onChange={(event) => setSearch(event.currentTarget.value)} />
        </label>}
        {heads.isPending && <p role="status">Loading Roles…</p>}
        {heads.error && <p role="alert">{String(heads.error)}</p>}
        {visibleRoles.length > 0 && <ul className="pipeline-select-list">
          {visibleRoles.map(({ templateId, version, name, displayLabel }) => <li key={templateId}>
            <button className="pipeline-select-item" type="button" aria-pressed={reference?.templateId === templateId}
              onClick={() => setSelected({ templateId, revision: version.revision })}>
              <UserRound className="workflow-role-icon" size={17} aria-hidden />
              <span>{displayLabel ?? name}<small>{displayLabel && displayLabel !== name && `${name} · `}Revision {version.ordinal}</small></span>
              <ChevronRight size={14} className="pipeline-end" aria-hidden />
            </button>
          </li>)}
        </ul>}
        {heads.data && visibleRoles.length === 0 && <p className="pipeline-meta">{roles.length ? "No matching Roles." : "No saved Roles."}</p>}
      </section>
      {selected && <section className="pipeline-stack workflow-role-details" aria-label="Role details">
        {selected === "new" && <>
          {openWorkspace && <div className="workflow-role-tools"><WorkflowEditorAssistance projectId={projectId} checkoutId={checkoutId} openWorkspace={openWorkspace} /></div>}
          <TemplateForm key="new" checkoutId={checkoutId} projectId={projectId} onSaved={accept} onConflict={openHead} />
        </>}
        {reference && detail.isPending && <p role="status">Loading Role revision…</p>}
        {reference && detail.error && <p role="alert">{String(detail.error)}</p>}
        {reference && detail.data && <>
          <div className="workflow-role-tools">
            <label className="pipeline-form pipeline-stack">Saved revision
              <select value={reference.revision} onChange={(event) =>
                setSelected({ templateId: reference.templateId, revision: event.currentTarget.value })}>
                {detail.data.history.map((version) => <option key={version.revision} value={version.revision}>Revision {version.ordinal}</option>)}
              </select>
            </label>
            {openWorkspace && <WorkflowEditorAssistance key={reference.revision} projectId={projectId} checkoutId={checkoutId} openWorkspace={openWorkspace}
              artifact={{ name: detail.data.revision.template.name, version: { id: reference.templateId, revision: reference.revision, kind: "role", destination: "project" } }} />}
          </div>
          {knownHead?.revision === reference.revision && <TemplateDisplayLabelEditor
            key={`${projectId}:${reference.templateId}:${reference.revision}`} projectId={projectId}
            templateId={reference.templateId} templateRevision={reference.revision} />}
          <TemplateForm key={reference.revision} checkoutId={checkoutId} projectId={projectId} saved={detail.data.revision}
            historical={Boolean(knownHead && reference.revision !== knownHead.revision)}
            onSaved={accept} onConflict={openHead} />
        </>}
      </section>}
    </div>
  </section>;
}

type LabelSnapshot = { templateRevision: string; displayLabel: { revision: number; value: string | null } };

function TemplateDisplayLabelEditor({ projectId, templateId, templateRevision }: {
  projectId: string; templateId: string; templateRevision: string;
}) {
  const client = useQueryClient();
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<{ revision: number; value: string }>();
  const queryKey = ["template-display-label", projectId, templateId];
  const templatesKey = ["conversation-templates", projectId];
  const setting = useQuery({
    queryKey,
    queryFn: () => invoke<LabelSnapshot>("template_display_label", { projectId, templateId }),
    enabled: open,
  });
  const save = useMutation({
    mutationFn: (edit: { revision: number; value: string }) => invoke<LabelSnapshot>("set_template_display_label", {
      projectId, templateId, templateRevision, expectedRevision: edit.revision, label: edit.value.trim() || null,
    }),
    onSuccess: (result) => {
      client.setQueryData(queryKey, result);
      setDraft(undefined);
      void client.invalidateQueries({ queryKey: templatesKey, exact: true });
    },
    onError: () => Promise.all([
      client.invalidateQueries({ queryKey }),
      client.invalidateQueries({ queryKey: templatesKey, exact: true }),
    ]),
  });
  const current = setting.data?.displayLabel;
  const stale = draft && draft.revision !== current?.revision;
  const value = draft?.value ?? current?.value ?? "";
  const dirty = !!draft && value.trim() !== (current?.value ?? "");
  const valid = new TextEncoder().encode(value.trim()).length <= 120 && !/[\p{Cc}]/u.test(value);
  return <details className="pipeline-evidence" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Role display label</summary>
    {open && <div className="pipeline-stack pipeline-form">
      <p>The label appears in the Library list.</p>
      {current && setting.data?.templateRevision === templateRevision && <>
        <label>Display label<input value={value} disabled={save.isPending}
          onChange={(event) => {
            const text = event.currentTarget.value;
            setDraft({ revision: draft?.revision ?? current.revision, value: text });
            save.reset();
          }} /></label>
        <div className="flex flex-wrap gap-2">
          <Button type="button" variant="outline" disabled={!draft || !dirty || !valid || Boolean(stale) || save.isPending}
            onClick={() => { if (draft) save.mutate(draft); }}>Save display label</Button>
          {stale && <Button type="button" variant="outline" onClick={() => { setDraft(undefined); save.reset(); }}>Use current label</Button>}
        </div>
        {!valid && <p role="alert">Use at most 120 UTF-8 bytes and omit control characters.</p>}
        {stale && <p role="status">The display label changed while you were editing.</p>}
      </>}
      {(setting.error || save.error) && <p role="alert">{String(setting.error || save.error)}</p>}
    </div>}
  </details>;
}

function TemplateForm({ checkoutId, projectId, saved, historical = false, onSaved, onConflict }: {
  checkoutId: string;
  projectId: string;
  saved?: Revision;
  historical?: boolean;
  onSaved: (detail: Detail) => Promise<void>;
  onConflict: (head: Head) => void;
}) {
  const [draft, setDraft] = useState<TemplateDraft>(() => saved?.template ?? emptyDraft());
  const live = useLiveModelOptions(undefined, !historical);
  const model = live.models.find(({ option }) => draft.model && sameModelSelection(option.selection, draft.model));
  const liveModel = model?.option.selectable ? draft.model : null;
  const effortUnavailable = Boolean(liveModel && draft.effort && !model?.efforts.includes(draft.effort));
  const matrix = useQuery({
    queryKey: ["conversation-template-profile", projectId, checkoutId, liveModel, draft.effort, draft.toolScope],
    queryFn: () => invoke<Matrix>("conversation_template_profile", {
      projectId, checkoutId, model: liveModel, effort: draft.effort, toolScope: draft.toolScope,
    }),
    enabled: !historical && Boolean(liveModel) && !effortUnavailable,
    retry: false,
  });
  const save = useMutation({
    mutationFn: () => invoke<Save>("save_conversation_template", {
      projectId, checkoutId,
      base: saved ? { templateId: saved.templateId, expectedRevision: saved.version.revision } : null,
      template: draft,
    }),
    onSuccess: async (result) => { if (result.status === "saved") await onSaved(result.detail); },
  });
  const conflict = save.data?.status === "conflict" ? save.data.current : undefined;
  const option = matrix.data?.toolScopes.find(({ mode }) => mode === draft.toolScope);
  const canSave = !historical && !save.isPending && Boolean(draft.name.trim() && liveModel && !effortUnavailable
    && option && !option.blocker && !matrix.error && !conflict);

  function update<K extends keyof TemplateDraft>(key: K, value: TemplateDraft[K]) {
    setDraft((previous) => ({ ...previous, [key]: value }));
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (canSave) save.mutate();
  }

  return <form className="pipeline-stack pipeline-form workflow-role-form" onSubmit={submit}>
    <header>
      <h3>{historical ? `Revision ${saved?.version.ordinal}` : saved ? "Edit Role" : "New Role"}</h3>
      {historical && <p className="pipeline-meta">Read-only revision</p>}
    </header>
    <fieldset className="pipeline-stack workflow-role-fields" disabled={historical || save.isPending}>
      <section className="pipeline-box pipeline-stack workflow-role-group">
        <h4>Identity</h4>
        <div className="workflow-role-pair">
          <label>Name<input value={draft.name} required onChange={(event) => update("name", event.currentTarget.value)} /></label>
          <label>Job role<select value={draft.role} onChange={(event) => update("role", event.currentTarget.value as JobRole)}>
            <option value="agent">Agent</option><option value="operator">Operator</option><option value="pipeline">Pipeline</option>
          </select></label>
        </div>
      </section>
      <section className="pipeline-box pipeline-stack workflow-role-group">
        <h4>Execution</h4>
        <div className="workflow-role-pair">
          {historical ? <p className="break-all">CLI model · {draft.model?.provider} · {draft.model?.profileId} · {draft.model?.modelId}</p> :
            <ModelPicker label="Default CLI and model" options={live.options} loading={live.loading}
              disabled={save.isPending} selection={{ mode: "single", selected: draft.model, onChange: (selection) =>
                setDraft((previous) => ({ ...previous, model: selection, effort: null })) }} />}
          {historical ? <p>Effort · {draft.effort ?? "CLI default"}</p> :
            <label>Effort<EffortSelect capabilities={model} value={draft.effort ?? ""} onChange={(value) => update("effort", value || null)} /></label>}
        </div>
        {!historical && draft.model && !liveModel && !live.loading && <p role="alert">The saved CLI, profile, and model are unavailable. Choose an available model to save a revision.</p>}
        {!historical && live.errors.length > 0 && <details><summary>CLI availability</summary>
          {live.errors.map((error, index) => <p key={index}>{String(error)}</p>)}
        </details>}
        {!historical && effortUnavailable && <p role="alert">The saved effort is unavailable. <Button type="button" variant="outline" onClick={() => update("effort", null)}>Use CLI default effort</Button></p>}
        <label>Tool scope<select value={draft.toolScope} disabled={historical || !matrix.data || Boolean(matrix.error)}
          onChange={(event) => update("toolScope", event.currentTarget.value as ToolScope)}>
          {(matrix.data?.toolScopes ?? [{ mode: draft.toolScope, blocker: null }]).map(({ mode, blocker }) =>
            <option key={mode} value={mode} disabled={Boolean(blocker)}>{scopeNames[mode]}{blocker ? " (unavailable)" : ""}</option>)}
        </select></label>
        {option?.blocker && <p role="alert">{option.blocker}</p>}
        {draft.toolScope === "approved_write" && <p className="pipeline-meta">Writing stages require project permission and a workflow release.</p>}
        {!historical && <ProfileMatrix matrix={matrix.data} loading={Boolean(liveModel && !effortUnavailable && matrix.isPending)} error={matrix.error} />}
        <WorkflowRequirementsEditor projectId={projectId} checkoutId={checkoutId} mode={draft.toolScope} model={draft.model}
          options={live.options} effort={draft.effort} disabled={historical || save.isPending} configuration={{ stage: draft.requirements }}
          onChange={({ stage }) => update("requirements", stage)} />
      </section>
      <section className="pipeline-box pipeline-stack workflow-role-group">
        <h4>Behavior and review</h4>
        <label>Instructions<textarea rows={5} value={draft.instructions} onChange={(event) => update("instructions", event.currentTarget.value)} /></label>
        <label>Completion contract<textarea rows={3} value={draft.completionContract}
          onChange={(event) => update("completionContract", event.currentTarget.value)} /></label>
        <label>Review policy<select value={draft.reviewPolicy} onChange={(event) => update("reviewPolicy", event.currentTarget.value as ReviewPolicy)}>
          {Object.entries(reviewNames).map(([key, name]) => <option key={key} value={key}>{name}</option>)}
        </select></label>
      </section>
    </fieldset>
    {conflict && <div role="alert" className="pipeline-stack">
      <p>Revision {conflict.version.ordinal} was saved elsewhere. Your edits are still here.</p>
      <Button type="button" variant="outline" onClick={() => onConflict(conflict)}>Discard edits and open latest revision</Button>
    </div>}
    {save.error && <p role="alert">{String(save.error)}</p>}
    {!historical && <div className="workflow-role-footer">
      <Button type="submit" disabled={!canSave}>{save.isPending ? "Saving revision…" : "Save Role revision"}</Button>
    </div>}
  </form>;
}

function ProfileMatrix({ matrix, loading, error }: { matrix?: Matrix; loading: boolean; error: Error | null }) {
  if (loading) return <p role="status">Checking the CLI profile and checkout…</p>;
  if (error) return <p role="alert">{String(error)}</p>;
  if (!matrix) return null;
  return <details className="pipeline-evidence">
    <summary>Capability matrix</summary>
    <p>CLI {matrix.cliVersion} · {matrix.transport.replaceAll("_", " ")}</p>
    {matrix.toolScopes.filter(({ blocker }) => blocker).map(({ mode, blocker }) =>
      <p key={mode}>{scopeNames[mode]} · {blocker}</p>)}
    <ul className="pipeline-stack">
      {matrix.capabilities.map(({ capability, status }) => <li key={capability}>{capability.replaceAll("_", " ")} · {status}</li>)}
    </ul>
  </details>;
}
