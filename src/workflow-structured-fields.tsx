import { useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";

const valueTypes = ["text", "number", "boolean", "null", "object", "array"] as const;
type ValueType = typeof valueTypes[number];

const captions: Record<ValueType, string> = {
  text: "Text", number: "Number", boolean: "Boolean", null: "Null", object: "Object", array: "List",
};

function typeOfValue(value: unknown): ValueType {
  if (Array.isArray(value)) return "array";
  if (value === null) return "null";
  const type = typeof value;
  if (type === "object" || type === "number" || type === "boolean") return type;
  return "text";
}

function emptyValue(type: ValueType) {
  switch (type) {
    case "number": return 0;
    case "boolean": return false;
    case "null": return null;
    case "object": return {};
    case "array": return [];
    case "text": return "";
  }
}

export function structuredObject(value: unknown): Record<string, unknown> {
  if (value && typeof value === "object" && !Array.isArray(value)) return value as Record<string, unknown>;
  return {};
}

function StructuredNumberField({ label, value, disabled, onChange }: {
  label: string; value: unknown; disabled: boolean; onChange: (value: number) => void;
}) {
  const [draft, setDraft] = useState<string>();
  return <label>{label}<input type="number" step="any" value={draft ?? (typeof value === "number" ? value : "")} disabled={disabled}
    onChange={(event) => {
      setDraft(event.currentTarget.value);
      if (Number.isFinite(event.currentTarget.valueAsNumber)) onChange(event.currentTarget.valueAsNumber);
    }} onBlur={() => setDraft(undefined)} /></label>;
}

export function StructuredValueField({ label, value, disabled, onChange, scalar = false }: {
  label: string;
  value: unknown;
  disabled: boolean;
  onChange: (value: unknown) => void;
  scalar?: boolean;
}) {
  const type = typeOfValue(value);
  const types = scalar ? valueTypes.slice(0, 4) : valueTypes;
  return <div className="workflow-structured-value">
    <label>{label} type<select value={type} disabled={disabled}
      onChange={(event) => onChange(emptyValue(event.currentTarget.value as ValueType))}>
      {types.map((choice) => <option value={choice} key={choice}>{captions[choice]}</option>)}
    </select></label>
    {type === "text" && <label>{label}<input value={typeof value === "string" ? value : ""} disabled={disabled}
      onChange={(event) => onChange(event.currentTarget.value)} /></label>}
    {type === "number" && <StructuredNumberField label={label} value={value} disabled={disabled} onChange={onChange} />}
    {type === "boolean" && <label>{label}<select value={String(value)} disabled={disabled}
      onChange={(event) => onChange(event.currentTarget.value === "true")}>
      <option value="true">True</option><option value="false">False</option>
    </select></label>}
    {!scalar && type === "array" && <StructuredListField label={label} value={value} disabled={disabled} onChange={onChange} />}
    {!scalar && type === "object" && <StructuredObjectField label={label} value={value} disabled={disabled} onChange={onChange} />}
  </div>;
}

export function StructuredListField({ label, value, disabled, onChange, limit }: {
  label: string;
  value: unknown;
  disabled: boolean;
  onChange: (value: unknown[]) => void;
  limit?: number;
}) {
  const values: unknown[] = Array.isArray(value) ? value : [];
  return <fieldset className="workflow-structured-fields" disabled={disabled}>
    <legend>{label}</legend>
    {values.map((item, index) => <div className="workflow-structured-entry" key={index}>
      <StructuredValueField label={`Item ${index + 1}`} value={item} disabled={disabled}
        onChange={(next) => onChange(values.map((entry, position) => position === index ? next : entry))} />
      <Button type="button" variant="ghost" aria-label={`Remove item ${index + 1}`} onClick={() => onChange(values.filter((_, position) => position !== index))}>
        <Trash2 size={15} />
      </Button>
    </div>)}
    <Button type="button" variant="outline" disabled={limit !== undefined && values.length >= limit} onClick={() => onChange([...values, ""])}>
      <Plus size={15} /> Add item
    </Button>
  </fieldset>;
}

export function StructuredObjectField({ label, value, disabled, onChange, reservedNames = [] }: {
  label: string;
  value: unknown;
  disabled: boolean;
  onChange: (value: Record<string, unknown>) => void;
  reservedNames?: readonly string[];
}) {
  const fields = structuredObject(value);
  const [name, setName] = useState("");
  const key = name.trim();
  const duplicate = Object.hasOwn(fields, key) || reservedNames.includes(key);
  function addField() {
    if (!key || duplicate) return;
    onChange({ ...fields, [key]: "" });
    setName("");
  }
  return <fieldset className="workflow-structured-fields" disabled={disabled}>
    <legend>{label}</legend>
    {Object.entries(fields).map(([field, item]) => <div className="workflow-structured-entry" key={field}>
      <StructuredValueField label={field} value={item} disabled={disabled} onChange={(next) => onChange({ ...fields, [field]: next })} />
      <Button type="button" variant="ghost" aria-label={`Remove field ${field}`}
        onClick={() => onChange(Object.fromEntries(Object.entries(fields).filter(([name]) => name !== field)))}>
        <Trash2 size={15} />
      </Button>
    </div>)}
    <div className="workflow-structured-entry">
      <label>Field name<input value={name} aria-invalid={duplicate || undefined} onChange={(event) => setName(event.currentTarget.value)}
        onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); addField(); } }} /></label>
      <Button type="button" variant="outline" disabled={!key || duplicate} onClick={addField}><Plus size={15} /> Add field</Button>
    </div>
    {duplicate && <p role="alert">This field name is already in use.</p>}
  </fieldset>;
}
