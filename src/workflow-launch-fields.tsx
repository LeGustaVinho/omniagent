import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";
import type { Hierarchy } from "@/project-agile-model";
import { RepositoryMarkdownPicker } from "@/repository-markdown-picker";
import type { WorkflowStartField } from "@/workflow-graph-model";
import { StructuredListField, StructuredObjectField } from "@/workflow-structured-fields";
import { WorkflowSchemaInput } from "@/workflow-schema-editor";
import { schemaValueProblem, type JsonSchema } from "@/workflow-schema-model";

export type { WorkflowStartField } from "@/workflow-graph-model";
type ValueType = WorkflowStartField["valueType"];

export type WorkflowStartSchema = {
  fields: WorkflowStartField[];
  requiresDocument: boolean;
  requiresWork: boolean;
  requiresBacklogPlan?: boolean;
  requiresEvent: boolean;
  promptOrMarkdown?: boolean;
  jsonSchema?: JsonSchema;
};

export type TypedLaunchFields = {
  checkoutId: string;
  documentRevisionId: string | null;
  workId: string | null;
  inputs: Record<string, unknown>;
  automatic?: boolean;
};

export function initialWorkflowStartValue(type: ValueType) {
  switch (type) {
    case "text": return "";
    case "number": return 0;
    case "boolean": return false;
    case "null": return null;
    case "object": return {};
    case "array": return [];
  }
}

export function initialWorkflowStartFields({ fields }: WorkflowStartSchema) {
  return Object.fromEntries(fields.filter(({ required, valueType }) =>
    required && (valueType === "null" || valueType === "object" || valueType === "array"))
    .map(({ name, valueType }) => [name, initialWorkflowStartValue(valueType)]));
}

export function WorkflowStartValueField({ field: { name, valueType, required }, schema, value, disabled, onChange }: {
  field: WorkflowStartField;
  schema?: JsonSchema;
  value: unknown;
  disabled: boolean;
  onChange: (value: unknown) => void;
}) {
  if (schema) return <WorkflowSchemaInput label={name} schema={schema} value={value} disabled={disabled} onChange={onChange} />;
  switch (valueType) {
    case "object": return <StructuredObjectField label={name} value={value} disabled={disabled} onChange={onChange} />;
    case "array": return <StructuredListField label={name} value={value} disabled={disabled} onChange={onChange} />;
    case "null": return <p className="pipeline-meta">{name} · Null</p>;
    case "number": return <label>{name}<input type="number" step="any" value={typeof value === "number" ? value : ""}
      required={required} disabled={disabled} onChange={(event) => {
        const number = event.currentTarget.valueAsNumber;
        onChange(Number.isFinite(number) ? number : undefined);
      }} /></label>;
    case "boolean": return <label>{name}<select value={typeof value === "boolean" ? String(value) : ""} disabled={disabled}
      required={required} onChange={(event) => {
        const selected = event.currentTarget.value;
        onChange(selected === "" ? undefined : selected === "true");
      }}>
      <option value="">Select a Boolean</option>
      <option value="true">True</option>
      <option value="false">False</option>
    </select></label>;
    case "text": return <label>{name}<input value={typeof value === "string" ? value : ""}
      aria-required={required} disabled={disabled}
      onChange={(event) => onChange(event.currentTarget.value)} /></label>;
  }
}

export function TypedWorkflowLaunchForm({ projectId, initialCheckoutId, pinnedDocumentRevisionId, workId, schema, pending, onLaunch }: {
  projectId: string;
  initialCheckoutId: string;
  pinnedDocumentRevisionId?: string;
  workId?: string;
  schema: WorkflowStartSchema;
  pending: boolean;
  onLaunch: (fields: TypedLaunchFields) => void;
}) {
  const [requestedCheckout, chooseCheckout] = useState(initialCheckoutId);
  const [selectedWork, chooseWork] = useState("");
  const [includeDocument, setIncludeDocument] = useState(false);
  const [automatic, setAutomatic] = useState(false);
  const [requestSource, setRequestSource] = useState<"prompt" | "markdown">("prompt");
  const [prompt, setPrompt] = useState("");
  const [values, setValues] = useState(() => initialWorkflowStartFields(schema));
  const [includedFields, setIncludedFields] = useState<string[]>([]);
  const checkouts = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
  });
  const hierarchy = useQuery({
    queryKey: ["project-tasks", projectId, "hierarchy"],
    queryFn: () => invoke<Hierarchy>("project_work_hierarchy", { projectId }),
  });
  const available = (checkouts.data ?? []).filter((entry) => entry.projectId === projectId);
  const pinned = pinnedDocumentRevisionId !== undefined;
  const checkout = available.find((entry) => entry.checkoutId === (pinned ? initialCheckoutId : requestedCheckout));
  const requestedWork = workId ?? selectedWork;
  const work = (hierarchy.data?.work ?? []).find(({ task }) => task.id === requestedWork);
  const workAvailable = requestedWork ? Boolean(work) : !schema.requiresWork;
  const supplied = schema.fields.every(({ name, required }) => (!required && !includedFields.includes(name)) || Object.hasOwn(values, name));
  const invalidValues = schema.jsonSchema && schemaValueProblem(schema.jsonSchema, values, "Workflow inputs");
  const needsDocument = pinned || schema.requiresDocument ||
    (schema.promptOrMarkdown ? requestSource === "markdown" : includeDocument);
  const ready = Boolean(checkout && workAvailable && supplied && !invalidValues && !pending
    && (!schema.promptOrMarkdown || needsDocument || prompt.trim()));
  const error = checkouts.error ?? hierarchy.error;

  function setValue(name: string, value: unknown) {
    setValues((current) => value === undefined
      ? Object.fromEntries(Object.entries(current).filter(([field]) => field !== name))
      : { ...current, [name]: value });
  }

  function toggleField(field: WorkflowStartField, included: boolean) {
    setIncludedFields((current) => included ? [...current, field.name] : current.filter((name) => name !== field.name));
    setValue(field.name, included ? initialWorkflowStartValue(field.valueType) : undefined);
  }

  function start(documentRevisionId: string | null) {
    if (ready && checkout) onLaunch({
      checkoutId: checkout.checkoutId,
      documentRevisionId,
      workId: work?.task.id ?? null,
      inputs: {
        ...Object.fromEntries(schema.fields.filter(({ name }) => Object.hasOwn(values, name))
          .map(({ name }) => [name, values[name]])),
        ...(schema.promptOrMarkdown && !needsDocument ? { prompt } : {}),
      },
      ...(automatic ? { automatic } : {}),
    });
  }

  return <section className="pipeline-stack" aria-label="Start inputs">
    <label>Checkout<select value={checkout?.checkoutId ?? ""} onChange={(event) => chooseCheckout(event.currentTarget.value)}
      disabled={pinned || pending || available.length === 0}>
      {!checkout && <option value="">{checkouts.isPending ? "Loading project checkouts…" : "Select a project checkout"}</option>}
      {available.map(({ checkoutId, checkoutRoot }) => <option key={checkoutId} value={checkoutId}>{checkoutRoot}</option>)}
    </select></label>
    {!workId ? <label>Task or Sub-Task<select value={work?.task.id ?? ""} onChange={(event) => chooseWork(event.currentTarget.value)}
      disabled={pending || hierarchy.isPending}>
      <option value="">{schema.requiresWork ? "Select work" : "No linked work"}</option>
      {hierarchy.data?.work.map(({ task, placement }) => <option key={task.id} value={task.id}>
        {placement.kind === "subtask" ? "Sub-Task · " : "Task · "}{task.title}
      </option>)}
    </select></label> : <p className="pipeline-meta">{work?.task.title ?? (hierarchy.isPending ? "Loading selected work…" : "Selected work is unavailable")}</p>}
    {schema.fields.length > 0 && <fieldset className="pipeline-stack">
      <legend>Workflow inputs</legend>
      {schema.fields.map((field) => {
        const included = field.required || includedFields.includes(field.name);
        return <div className="pipeline-stack" key={field.name}>
          {!field.required && <label className="pipeline-checkline"><input type="checkbox" disabled={pending}
            checked={included} onChange={(event) => toggleField(field, event.currentTarget.checked)} />
            Provide {field.name}
          </label>}
          {included && <WorkflowStartValueField field={field} schema={schema.jsonSchema?.properties?.[field.name]} value={values[field.name]} disabled={pending}
            onChange={(value) => setValue(field.name, value)} />}
        </div>;
      })}
    </fieldset>}
    {supplied && invalidValues && <p role="alert">{invalidValues}</p>}
    {schema.promptOrMarkdown && !pinned && <label>Request source
      <select value={requestSource} disabled={pending}
        onChange={(event) => setRequestSource(event.currentTarget.value === "markdown" ? "markdown" : "prompt")}>
        <option value="prompt">Prompt</option>
        <option value="markdown">Saved Markdown revision</option>
      </select>
    </label>}
    {schema.promptOrMarkdown && !needsDocument && <label>Prompt
      <textarea rows={5} value={prompt} disabled={pending} onChange={(event) => setPrompt(event.currentTarget.value)} />
    </label>}
    <label className="pipeline-checkline"><input type="checkbox" checked={automatic} disabled={pending}
      onChange={(event) => setAutomatic(event.currentTarget.checked)} />
      Dispatch ready workflow stages automatically
    </label>
    {!pinned && !schema.requiresDocument && !schema.promptOrMarkdown && <label className="pipeline-checkline"><input type="checkbox" checked={includeDocument}
      disabled={pending} onChange={(event) => setIncludeDocument(event.currentTarget.checked)} />
      Include a Markdown revision
    </label>}
    {needsDocument ? checkout && <RepositoryMarkdownPicker key={checkout.checkoutId} checkoutId={checkout.checkoutId}
      pinnedRevisionId={pinnedDocumentRevisionId} onDeliver={start} delivering={pending} launchDisabled={!ready} launchLabel="Start workflow" />
      : <Button type="button" className="justify-self-start" onClick={() => start(null)} disabled={!ready}>
        {pending ? "Starting…" : "Start workflow"}
      </Button>}
    {error && <p role="alert">{String(error)}</p>}
  </section>;
}
