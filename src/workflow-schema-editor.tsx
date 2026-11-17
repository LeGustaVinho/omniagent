import { Braces, Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  objectSchema, schemaTypes, type JsonSchema, type SchemaField, type SchemaType, type SchemaValue,
} from "@/workflow-schema-model";

const labels: Record<SchemaType, string> = {
  string: "Text", integer: "Integer", number: "Number", boolean: "Boolean", object: "Object", array: "List",
};

function newValue(type: SchemaType): SchemaValue {
  if (type === "object") return { type, fields: [] };
  if (type === "array") return { type, items: { type: "string" } };
  return { type };
}

export function SchemaFieldEditor({ fields, disabled, onChange }: {
  fields: SchemaField[]; disabled?: boolean; onChange: (fields: SchemaField[]) => void;
}) {
  return <div className="pipeline-stack">
    <Fields fields={fields} disabled={disabled} onChange={onChange} />
    <details className="pipeline-evidence">
      <summary>JSON Schema preview</summary>
      <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(objectSchema(fields), null, 2)}</pre>
    </details>
  </div>;
}

export function WorkflowSchemaEditor({ name, fields, disabled, onChange }: {
  name: string; fields: SchemaField[]; disabled: boolean; onChange: (name: string, fields: SchemaField[]) => void;
}) {
  return <section aria-label="Schema editor" className="workflow-prompt-revision-view pipeline-form pipeline-stack">
    <div className="workflow-inspector-heading">
      <span className="workflow-inspector-icon"><Braces size={17} aria-hidden /></span>
      <div><h2>Schema</h2></div>
    </div>
    <label>Schema name<input value={name} disabled={disabled} onChange={(event) => onChange(event.currentTarget.value, fields)} /></label>
    <SchemaFieldEditor fields={fields} disabled={disabled} onChange={(next) => onChange(name, next)} />
  </section>;
}

function Fields({ fields, disabled, onChange }: {
  fields: SchemaField[]; disabled?: boolean; onChange: (fields: SchemaField[]) => void;
}) {
  function add() {
    let index = fields.length + 1;
    while (fields.some(({ name }) => name === "field_" + index)) index += 1;
    onChange([...fields, { name: "field_" + index, description: "", type: "string", required: true }]);
  }
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>Fields</legend>
    {fields.map((field, index) => {
      const update = (next: SchemaField) => onChange(fields.map((item, position) => position === index ? next : item));
      const label = `Field ${index + 1}`;
      const invalid = !/^[a-z0-9_-]{1,64}$/.test(field.name) ||
        fields.some(({ name }, position) => position !== index && name === field.name);
      return <fieldset key={index} className="workflow-schema-field pipeline-stack">
        <legend>{field.name || label}</legend>
        <div className="workflow-schema-field-head">
          <label>{label} name<input value={field.name} aria-invalid={invalid || undefined}
            onChange={(event) => update({ ...field, name: event.currentTarget.value })} /></label>
          <Button type="button" variant="ghost" className="ui-icon-button" aria-label={`Remove ${label.toLowerCase()}`}
            onClick={() => onChange(fields.filter((_, position) => position !== index))}><Trash2 size={15} aria-hidden /></Button>
        </div>
        {invalid && <p role="alert">Use up to 64 lowercase letters, digits, underscores, or hyphens in a unique name.</p>}
        <label>Description<textarea rows={2} maxLength={2048} value={field.description ?? ""} onChange={(event) =>
          update({ ...field, description: event.currentTarget.value })} /></label>
        <label className="pipeline-checkline"><input type="checkbox" checked={field.required} onChange={(event) =>
          update({ ...field, required: event.currentTarget.checked })} /> Required</label>
        <ValueEditor label="Type" value={field} onChange={(value) =>
          update({ ...value, name: field.name, description: field.description, required: field.required })} />
      </fieldset>;
    })}
    <Button type="button" variant="outline" className="w-fit" onClick={add}><Plus size={14} aria-hidden /> Add field</Button>
  </fieldset>;
}

function ValueEditor({ label, value, onChange }: {
  label: string; value: SchemaValue; onChange: (value: SchemaValue) => void;
}) {
  return <div className="pipeline-stack">
    <label>{label}<select value={value.type} onChange={(event) => onChange(newValue(event.currentTarget.value as SchemaType))}>
      {schemaTypes.map((type) => <option key={type} value={type}>{labels[type]}</option>)}
    </select></label>
    {value.type === "object" && <Fields fields={value.fields ?? []} onChange={(fields) => onChange({ type: "object", fields })} />}
    {value.type === "array" && <ValueEditor label="List item type" value={value.items ?? { type: "string" }}
      onChange={(items) => onChange({ type: "array", items })} />}
  </div>;
}

type SchemaInputProps = { label: string; schema: JsonSchema; value: unknown; disabled?: boolean; onChange: (value: unknown) => void };

function initialSchemaValue({ type }: JsonSchema) {
  switch (type) {
    case "string": return "";
    case "number":
    case "integer": return 0;
    case "boolean": return false;
    case "object": return {};
    case "array": return [];
  }
}

export function WorkflowSchemaInput({ label, schema, value, disabled, onChange }: SchemaInputProps) {
  if (schema.type === "object") {
    const fields = value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
    const required = new Set(schema.required);
    function update(name: string, value?: unknown) {
      if (value === undefined) onChange(Object.fromEntries(Object.entries(fields).filter(([key]) => key !== name)));
      else onChange({ ...fields, [name]: value });
    }
    return <fieldset className="workflow-schema-field pipeline-stack" disabled={disabled}>
      <legend>{label}</legend>
      {schema.description && <p className="pipeline-meta">{schema.description}</p>}
      {Object.entries(schema.properties ?? {}).map(([name, property]) => {
        const included = required.has(name) || Object.hasOwn(fields, name);
        return <div className="pipeline-stack" key={name}>
          {!required.has(name) && <label className="pipeline-checkline"><input type="checkbox" checked={included}
            onChange={(event) => update(name, event.currentTarget.checked ? initialSchemaValue(property) : undefined)} /> Provide {name}</label>}
          {included && <WorkflowSchemaInput label={name} schema={property} value={fields[name]}
            onChange={(value) => update(name, value)} />}
        </div>;
      })}
    </fieldset>;
  }
  if (schema.type === "array") {
    const values: unknown[] = Array.isArray(value) ? value : [];
    const item = schema.items;
    return <fieldset className="workflow-schema-field pipeline-stack" disabled={disabled}>
      <legend>{label}</legend>
      {schema.description && <p className="pipeline-meta">{schema.description}</p>}
      {item && <>
        {values.map((value, index) => <div className="workflow-schema-field-head" key={index}>
          <WorkflowSchemaInput label={`Item ${index + 1}`} schema={item} value={value}
            onChange={(next) => onChange(values.map((entry, position) => position === index ? next : entry))} />
          <Button type="button" variant="ghost" className="ui-icon-button" aria-label={`Remove item ${index + 1}`}
            onClick={() => onChange(values.filter((_, position) => position !== index))}><Trash2 size={14} aria-hidden /></Button>
        </div>)}
        <Button type="button" variant="outline" className="w-fit" onClick={() => onChange([...values, initialSchemaValue(item)])}>
          <Plus size={14} aria-hidden /> Add item
        </Button>
      </>}
    </fieldset>;
  }
  return <label>{label}
    {schema.type === "boolean" ? <select value={typeof value === "boolean" ? String(value) : ""} disabled={disabled}
      onChange={(event) => onChange(event.currentTarget.value === "" ? undefined : event.currentTarget.value === "true")}>
      <option value="">Choose a Boolean</option><option value="true">True</option><option value="false">False</option>
    </select> : schema.type === "string" ? <input value={typeof value === "string" ? value : ""} disabled={disabled}
      onChange={(event) => onChange(event.currentTarget.value)} /> : <input type="number"
      step={schema.type === "integer" ? 1 : "any"} value={typeof value === "number" ? value : ""} disabled={disabled}
      onChange={(event) => onChange(Number.isFinite(event.currentTarget.valueAsNumber) ? event.currentTarget.valueAsNumber : undefined)} />}
    {schema.description && <small className="pipeline-meta">{schema.description}</small>}
  </label>;
}
