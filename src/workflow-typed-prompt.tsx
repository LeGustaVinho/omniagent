import { useRef, useState, type ReactNode } from "react";
import { CornerDownLeft, RotateCcw, Save } from "lucide-react";
import { Button } from "@/components/ui/button";
import { EffortSelect } from "@/components/effort-select";
import { ModelPicker } from "@/components/model-picker";
import type { JobRole } from "@/bindings/JobRole";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { SessionMode } from "@/bindings/SessionMode";
import { LIVE_MODEL_PROVIDERS, sameModelSelection, useLiveModelOptions } from "@/live-models";
import { WorkflowRequirementsEditor, type StageRequirements } from "@/workflow-stage-requirements";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";
import { promptReferenceValue, type PromptRoleChoice } from "@/workflow-prompt-model";
import type { ReusableReference } from "@/workflow-reusable";
import { templateVariables, type JsonSchema } from "@/workflow-schema-model";

export type RoleSnapshot = {
  name: string;
  role: JobRole;
  model: ModelSelection;
  effort: string | null;
  instructions: string;
  toolScope: SessionMode;
  completionContract: string;
  reviewPolicy: "independent" | "cross_review" | "human_approval";
  requirements?: StageRequirements;
};
export type PromptSlot = "role" | "input_schema" | "output_schema" | "reviewer";
type SelectionProps = {
  label: string; selected?: ReusableReference; choices: PromptRoleChoice[]; disabled?: boolean;
  onChange: (reference?: ReusableReference) => void;
};

export function WorkflowReferenceField({ label, selected, choices, disabled, onChange }: SelectionProps) {
  const value = promptReferenceValue(selected);
  return <label>{label}<select value={value} disabled={disabled} onChange={(event) =>
    onChange(choices.find(({ reference }) => promptReferenceValue(reference) === event.currentTarget.value)?.reference)}>
    <option value="">Choose {label.toLowerCase()}</option>
    {selected && !choices.some(({ reference }) => promptReferenceValue(reference) === value) && <option value={value}>
      {selected.source === "published" ? `Pinned revision ${selected.version.revision.slice(0, 10)}`
        : selected.source === "draft" ? selected.key : selected.name}
    </option>}
    {choices.map(({ name, reference }) => <option key={promptReferenceValue(reference)} value={promptReferenceValue(reference)}>{name}</option>)}
  </select></label>;
}

function Resettable({ changed, source, reset, children }: {
  changed: boolean; source: string; reset: () => void; children: ReactNode;
}) {
  return <div className="workflow-override-field">
    {children}
    <div className="workflow-override-origin">
      <span>{changed ? "Local override" : source}</span>
      {changed && <Button variant="ghost" className="text-xs" type="button" onClick={reset}><RotateCcw size={13} aria-hidden /> Restore</Button>}
    </div>
  </div>;
}

export function TypedPromptInspector({ projectId, checkoutId, prompt, defaultPrompt, references, defaults, role, defaultRoleOverrides,
  roleOverrides, roleChoices, schemaChoices, inputSchema, disabled, onPrompt, onReference, onRoleOverrides, onSavePrefab, title, onTitle }: {
  projectId: string; checkoutId: string; prompt?: string; defaultPrompt?: string;
  references: Partial<Record<PromptSlot, ReusableReference>>;
  defaults: Partial<Record<PromptSlot, ReusableReference>>;
  role?: RoleSnapshot; defaultRoleOverrides: Record<string, unknown>; roleOverrides: Record<string, unknown>;
  roleChoices: PromptRoleChoice[]; schemaChoices: PromptRoleChoice[]; inputSchema?: JsonSchema;
  disabled: boolean; onPrompt: (prompt?: string) => void; onReference: (slot: PromptSlot, reference?: ReusableReference) => void;
  onRoleOverrides: (overrides: Record<string, unknown>) => void; onSavePrefab?: (name: string) => void;
  title?: string; onTitle?: (title: string) => void;
}) {
  const [variable, selectVariable] = useState("");
  const [name, setName] = useState("");
  const [showSave, setShowSave] = useState(false);
  const input = useRef<HTMLTextAreaElement>(null);
  const variables = templateVariables(inputSchema);
  const body = prompt ?? defaultPrompt ?? "";
  const field = (slot: PromptSlot, label: string, choices: PromptRoleChoice[]) => {
    const selected = references[slot] ?? defaults[slot];
    const picker = <WorkflowReferenceField label={label} selected={selected} choices={choices} disabled={disabled}
      onChange={(reference) => onReference(slot, reference)} />;
    return disabled || !defaults[slot] ? picker : <Resettable changed={!!references[slot]}
      source="Inherited" reset={() => onReference(slot)}>{picker}</Resettable>;
  };
  const effectiveRole = role && { ...role, ...defaultRoleOverrides, ...roleOverrides } as RoleSnapshot;
  const requiresWrite = effectiveRole?.toolScope === "approved_write";
  const unresolved = ["role", "input_schema", "output_schema"].filter((slot) =>
    !((references as Record<string, unknown>)[slot] ?? (defaults as Record<string, unknown>)[slot]));
  return <div className="workflow-prompt-inspector pipeline-form pipeline-stack" aria-label="AI Node Agent settings">
    {onTitle && <label>Card title<input value={title ?? ""} disabled={disabled}
      onChange={(event) => onTitle(event.currentTarget.value)} /></label>}
    {field("role", "Role", roleChoices)}
    {requiresWrite && <p role="status">This Role requests worktree write access. Choose a read-only Role or set Tool scope to Repository read.</p>}
    {field("input_schema", "Input Schema", schemaChoices)}
    {field("output_schema", "Output Schema", schemaChoices)}
    {effectiveRole?.reviewPolicy === "cross_review" && field("reviewer", "Review Role", roleChoices)}
    <Resettable changed={defaultPrompt !== undefined && prompt !== undefined && !disabled}
      source={defaultPrompt !== undefined ? "Inherited prompt" : ""} reset={() => onPrompt()}>
      <label>Prompt<textarea ref={input} rows={7} value={body} disabled={disabled}
        onChange={(event) => onPrompt(event.currentTarget.value)} /></label>
    </Resettable>
    {variables.length > 0 && <div className="workflow-schema-field-head">
      <label>Input variable<select value={variable} disabled={disabled} onChange={(event) => selectVariable(event.currentTarget.value)}>
        <option value="">Choose a variable</option>
        {variables.map((path) => <option key={path} value={path}>{path}</option>)}
      </select></label>
      <Button variant="outline" type="button" disabled={disabled || !variables.includes(variable)} onClick={() => {
        const start = input.current?.selectionStart ?? body.length;
        const end = input.current?.selectionEnd ?? start;
        const insert = "{{" + variable + "}}";
        onPrompt(body.slice(0, start) + insert + body.slice(end));
        requestAnimationFrame(() => { input.current?.focus(); input.current?.setSelectionRange(start + insert.length, start + insert.length); });
      }}><CornerDownLeft size={14} aria-hidden /> Insert</Button>
    </div>}
    {effectiveRole && <RoleOverrides projectId={projectId} checkoutId={checkoutId} role={effectiveRole}
      inherited={defaultRoleOverrides} local={roleOverrides} disabled={disabled} onChange={onRoleOverrides} />}
    {onSavePrefab && <WorkflowBuilderDialog title="Save Node Prefab" description="Reuse this configuration in other pipelines."
      open={showSave} onOpenChange={setShowSave} trigger={<Button type="button" variant="outline" className="w-fit"
        disabled={disabled || requiresWrite || !!unresolved.length || !body.trim()}><Save size={15} aria-hidden /> Save as Node Prefab</Button>}>
      <form className="pipeline-form pipeline-stack" onSubmit={(event) => {
        event.preventDefault();
        if (!name.trim()) return;
        onSavePrefab(name.trim());
        setShowSave(false);
        setName("");
      }}>
        <label>Prefab name<input autoComplete="off" value={name} onChange={(event) => setName(event.currentTarget.value)} required /></label>
        <Button type="submit" className="w-fit" disabled={!name.trim()}>Add to draft artifacts</Button>
      </form>
    </WorkflowBuilderDialog>}
  </div>;
}

function RoleOverrides({ projectId, checkoutId, role, inherited, local, disabled, onChange }: {
  projectId: string; checkoutId: string; role: RoleSnapshot; inherited: Record<string, unknown>; local: Record<string, unknown>;
  disabled: boolean; onChange: (overrides: Record<string, unknown>) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const live = useLiveModelOptions(LIVE_MODEL_PROVIDERS, expanded && !disabled);
  const profile = live.models.find(({ option }) => sameModelSelection(option.selection, role.model));
  function write(key: keyof RoleSnapshot, value: unknown) {
    onChange({ ...local, [key]: value });
  }
  function field(key: keyof RoleSnapshot, content: ReactNode) {
    return <Resettable changed={Object.hasOwn(local, key) && !disabled}
      source={Object.hasOwn(inherited, key) ? "Inherited from Prefab" : "Inherited from Role"}
      reset={() => onChange(Object.fromEntries(Object.entries(local).filter(([field]) => field !== key)))}>
      {content}
    </Resettable>;
  }
  return <details className="workflow-inspector-group" onToggle={(event) => setExpanded(event.currentTarget.open)}>
    <summary>Role overrides</summary>
    {expanded && <fieldset className="pipeline-stack" disabled={disabled}>
      {field("name", <label>Role name<input value={role.name} onChange={(event) => write("name", event.currentTarget.value)} /></label>)}
      {field("role", <label>Job role<select value={role.role} onChange={(event) => write("role", event.currentTarget.value)}>
        <option value="agent">Agent</option><option value="operator">Operator</option><option value="pipeline">Pipeline</option>
      </select></label>)}
      {field("model", <ModelPicker label="Model" options={live.options} loading={live.loading} disabled={disabled}
        selection={{ mode: "single", selected: role.model, onChange: (model) => { if (model) onChange({ ...local, model, effort: null }); } }} />)}
      {!!profile?.efforts.length
        ? field("effort", <EffortSelect capabilities={profile} value={role.effort ?? ""} onChange={(effort) => write("effort", effort || null)} />)
        : (role.effort || Object.hasOwn(local, "effort")) && field("effort", <p>Effort · {role.effort ?? "CLI default"}</p>)}
      {field("instructions", <label>Role instructions<textarea rows={4} value={role.instructions}
        onChange={(event) => write("instructions", event.currentTarget.value)} /></label>)}
      {field("completionContract", <label>Completion contract<textarea rows={3} value={role.completionContract}
        onChange={(event) => write("completionContract", event.currentTarget.value)} /></label>)}
      {field("toolScope", <label>Tool scope<select value={role.toolScope} onChange={(event) => write("toolScope", event.currentTarget.value)}>
        <option value="read_only">Repository read</option>
        {role.toolScope === "approved_write" && <option value="approved_write" disabled>Approved worktree write</option>}
      </select></label>)}
      {field("reviewPolicy", <label>Review policy<select value={role.reviewPolicy} onChange={(event) => write("reviewPolicy", event.currentTarget.value)}>
        <option value="independent">Independent review</option><option value="cross_review">Cross-review</option>
        <option value="human_approval">Human approval</option>
      </select></label>)}
      {field("requirements", <WorkflowRequirementsEditor projectId={projectId} checkoutId={checkoutId}
        mode={role.toolScope} model={role.model} effort={role.effort} options={live.options} disabled={disabled}
        configuration={{ stage: role.requirements }} onChange={({ stage }) => write("requirements", stage ?? {})} />)}
    </fieldset>}
  </details>;
}
