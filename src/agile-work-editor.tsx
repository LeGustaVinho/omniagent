import { lazy, Suspense, useState, type FormEvent, type ReactNode } from "react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker } from "@/components/model-picker";
import { Button } from "@/components/ui/button";
import { ConversationMarkdown } from "@/conversation-markdown";
import type { NoteTarget } from "@/knowledge-document";
import type { WorkKind } from "@/project-agile-model";
import { workLevels } from "@/project-agile-model";

const MarkdownEditor = lazy(() => import("@/knowledge-editor").then(({ MarkdownEditor }) => ({ default: MarkdownEditor })));
type Choice = { value: string; label: string };
type Note = { path: string; content: string };

export const priorityChoices = [
  { value: "2", label: "Urgent" }, { value: "1", label: "High" },
  { value: "0", label: "Normal" }, { value: "-1", label: "Low" },
] as const;

export const checklistChoices = [
  { value: "", label: "Inherit default" }, { value: "track_progress", label: "Track progress" }, { value: "run_by_item", label: "Run by item" },
] as const;

export const branchChoices = [
  { value: "", label: "Inherit project strategy" }, { value: "independent", label: "Branch per work item" },
  { value: "shared_branch_per_task", label: "Shared branch per Task" },
] as const;

export type AgileFormValues = {
  title: string;
  description: string;
  parentId: string;
  state: string;
  priority: string;
  checklistMode: string;
  branchStrategy: string;
  acceptanceCriteria: string;
  dependencies: string[];
  blockers: string;
};

function ChoiceField({ label, value, options, onChange, disabled, required, children }: {
  label: string; value: string; options: readonly Choice[]; onChange: (value: string) => void;
  disabled?: boolean; required?: boolean; children?: ReactNode;
}) {
  return <label className="delivery-field">{label}<select className="delivery-filter" value={value} disabled={disabled} required={required}
    onChange={(event) => onChange(event.currentTarget.value)}>
    {options.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
  </select>{children}</label>;
}

function DescriptionField({ value, path, files, onChange, onLink, children, busy }: {
  value: string; path: string; files: Note[]; onChange: (value: string) => void; onLink?: (link: NoteTarget) => void;
  children?: ReactNode; busy?: boolean;
}) {
  const modes = [["source", "Source"], ["live", "Live Preview"], ["reading", "Reading"]] as const;
  const [mode, setMode] = useState<typeof modes[number][0]>("live");
  return <section aria-label="Description" className="grid gap-1.5">
    <h4 className="text-sm font-medium">Description</h4>
    <div className="agile-description">
      <div className="flex flex-wrap gap-1 border-b border-border px-1.5 py-1" aria-label="Description views">
        {modes.map(([id, label]) => <button key={id} type="button" className="delivery-tab" aria-pressed={mode === id}
          onClick={() => setMode(id)}>{label}</button>)}
      </div>
      {mode === "reading" ? <div className="agile-description-reading">
        {value ? <ConversationMarkdown>{value}</ConversationMarkdown> : <p className="text-muted-foreground">No description.</p>}
      </div> : <div className="agile-description-editor" inert={busy || undefined}>
        <Suspense fallback={<p className="p-3 text-xs text-muted-foreground" role="status">Loading editor…</p>}>
          <MarkdownEditor mode={mode} source={value} path={path} files={files} onChange={onChange} onLink={onLink ?? (() => {})} />
        </Suspense>
      </div>}
    </div>
    {children}
  </section>;
}

export function AgileNotePreview({ path, children }: { path: string; children: ReactNode }) {
  return <details open className="agile-description-reading agile-description">
    <summary className="mb-1 cursor-pointer font-semibold text-primary">{path}</summary>
    {children}
  </details>;
}

export function AgileWorkEditor({ kind, setKind, values, change, parents, states, dependencies, notes, path, openNote,
  descriptionPreview, assignment, branchLocked, parentLocked, busy, error, notices, saveLabel, cancel, save }: {
  kind: WorkKind; setKind?: (kind: WorkKind) => void; values: AgileFormValues; change: (patch: Partial<AgileFormValues>) => void;
  parents: readonly Choice[]; states: readonly Choice[]; dependencies: readonly Choice[]; notes: Note[]; path: string;
  openNote?: (link: NoteTarget) => void; descriptionPreview?: ReactNode;
  assignment?: { selected: ModelSelection | null; options: ModelOption[]; loading?: boolean; change: (selection: ModelSelection | null) => void; retained?: string };
  branchLocked?: boolean; parentLocked?: boolean; busy?: boolean; error?: ReactNode; notices?: ReactNode;
  saveLabel: string; cancel: () => void; save: () => void;
}) {
  const epic = kind === "epic";
  const child = kind === "subtask";
  const valid = Boolean(values.title.trim() && (!child || values.parentId));
  const priorities = priorityChoices.some(({ value }) => value === values.priority)
    ? priorityChoices : [...priorityChoices, { value: values.priority, label: `Priority ${values.priority}` }];

  function submit(event: FormEvent) {
    event.preventDefault();
    if (valid && !busy) save();
  }

  return <form className="agile-create-form" onSubmit={submit}>
    {setKind && <ChoiceField label="Work type" disabled={busy} value={kind} options={workLevels} onChange={(kind) => setKind(kind as WorkKind)} />}
    <label className="delivery-field">Title<input className="delivery-filter" required disabled={busy} value={values.title}
      onChange={(event) => change({ title: event.currentTarget.value })} /></label>
    <DescriptionField value={values.description} path={path} files={notes} busy={busy} onChange={(description) => change({ description })} onLink={openNote}>
      {descriptionPreview}
    </DescriptionField>
    {!epic && <ChoiceField label={child ? "Parent Task" : "Epic"} value={values.parentId} required={child} disabled={busy || parentLocked}
      options={[{ value: "", label: child ? "Select a Task" : "Standalone Task" }, ...parents]} onChange={(parentId) => change({ parentId })}>
      {parentLocked && <span className="text-xs text-muted-foreground">Parent is locked after worktree provisioning.</span>}
    </ChoiceField>}
    <div className="agile-editor-fields">
      {!epic && <ChoiceField label="Priority" value={values.priority} disabled={busy} options={priorities} onChange={(priority) => change({ priority })} />}
      {states.length > 0 && <ChoiceField label="State" value={values.state} disabled={busy || states.length === 1} options={states} onChange={(state) => change({ state })} />}
    </div>
    {!epic && <>
      <div className="agile-editor-fields">
        <ChoiceField label="Checklist execution" value={values.checklistMode} disabled={busy} options={checklistChoices}
          onChange={(checklistMode) => change({ checklistMode })} />
        {!child && <ChoiceField label="Branch strategy" value={values.branchStrategy} disabled={busy || branchLocked} options={branchChoices}
          onChange={(branchStrategy) => change({ branchStrategy })}>
          {branchLocked && <span className="text-xs text-muted-foreground">Branch strategy is locked after worktree provisioning.</span>}
        </ChoiceField>}
      </div>
      {assignment && <section aria-label="Planned assignment" className="grid gap-1.5">
        <ModelPicker label="Planned assignment" options={assignment.options} loading={assignment.loading} disabled={busy}
          selection={{ mode: "single", selected: assignment.selected, onChange: assignment.change }} />
        {assignment.retained && <p className="text-xs text-muted-foreground">{assignment.retained}</p>}
        {(assignment.selected || assignment.retained) && <Button type="button" variant="outline" className="justify-self-start" disabled={busy}
          onClick={() => assignment.change(null)}>Clear assignment</Button>}
      </section>}
      <label className="delivery-field">Acceptance criteria<textarea className="delivery-filter" rows={3} disabled={busy}
        value={values.acceptanceCriteria} onChange={(event) => change({ acceptanceCriteria: event.currentTarget.value })} /></label>
      <label className="delivery-field">Dependencies<select className="delivery-filter" multiple size={Math.min(5, Math.max(2, dependencies.length))}
        disabled={busy || !dependencies.length} value={values.dependencies} onChange={(event) => change({
          dependencies: Array.from(event.currentTarget.selectedOptions, ({ value }) => value),
        })}>
        {dependencies.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
      </select></label>
      <label className="delivery-field">Blockers<textarea className="delivery-filter" rows={2} disabled={busy}
        value={values.blockers} onChange={(event) => change({ blockers: event.currentTarget.value })} /></label>
    </>}
    {notices}
    {error && <p role="alert">{error}</p>}
    <div className="agile-dialog-actions">
      <Button type="button" variant="outline" disabled={busy} onClick={cancel}>Cancel</Button>
      <Button type="submit" disabled={busy || !valid}>{saveLabel}</Button>
    </div>
  </form>;
}

export function workFormLines(value: string) {
  return value.split("\n").map((line) => line.trim()).filter(Boolean);
}
