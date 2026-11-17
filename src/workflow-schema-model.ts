export const schemaTypes = ["string", "integer", "number", "boolean", "object", "array"] as const;
export type SchemaType = typeof schemaTypes[number];

export type SchemaValue = {
  type: SchemaType;
  fields?: SchemaField[];
  items?: SchemaValue;
};
export type SchemaField = SchemaValue & { name: string; description?: string; required: boolean };
export type JsonSchema = {
  type: SchemaType;
  description?: string;
  properties?: Record<string, JsonSchema>;
  required?: string[];
  additionalProperties?: false;
  items?: JsonSchema;
};

export function objectSchema(fields: readonly SchemaField[]): JsonSchema {
  return {
    type: "object",
    properties: Object.fromEntries(fields.map((field) => [field.name, {
      ...valueSchema(field),
      ...(field.description && { description: field.description }),
    }])),
    required: fields.filter((field) => field.required).map((field) => field.name),
    additionalProperties: false,
  };
}

function valueSchema(value: SchemaValue): JsonSchema {
  if (value.type === "object") return objectSchema(value.fields ?? []);
  if (value.type === "array") return { type: "array", items: valueSchema(value.items ?? { type: "string" }) };
  return { type: value.type };
}

export function schemaMismatch(source: JsonSchema, target: JsonSchema, path = ""): string | undefined {
  if (source.type !== target.type && !(source.type === "integer" && target.type === "number")) {
    return `${path || "Value"} is ${source.type} and needs to be ${target.type}.`;
  }
  if (source.type === "array" && target.items) {
    return source.items ? schemaMismatch(source.items, target.items, `${path || "Value"}[]`)
      : `${path || "Value"} needs a declared item type.`;
  }
  if (source.type !== "object") return;
  if (target.additionalProperties === false && source.additionalProperties !== false) {
    return `${path || "Value"} needs a source that declares every field.`;
  }
  const required = new Set(source.required);
  for (const field of target.required ?? []) {
    if (!required.has(field)) return `${path ? path + "." : ""}${field} is required by the destination.`;
  }
  const targetFields = target.properties ?? {};
  for (const [field, value] of Object.entries(source.properties ?? {})) {
    const fieldPath = path ? `${path}.${field}` : field;
    const targetField = Object.hasOwn(targetFields, field) ? targetFields[field] : undefined;
    if (!targetField) {
      if (target.additionalProperties === false) return `${fieldPath} is outside the destination schema.`;
      continue;
    }
    const mismatch = schemaMismatch(value, targetField, fieldPath);
    if (mismatch) return mismatch;
  }
}

export function templateVariables(schema: JsonSchema | undefined, prefix = "input"): string[] {
  if (!schema) return [];
  return [prefix, ...Object.entries(schema.properties ?? {}).flatMap(([name, value]) =>
    templateVariables(value, `${prefix}.${name}`))];
}

export function schemaValueProblem(schema: JsonSchema, value: unknown, name = "Value"): string | undefined {
  switch (schema.type) {
    case "string": return typeof value === "string" ? undefined : `${name} needs text.`;
    case "boolean": return typeof value === "boolean" ? undefined : `${name} needs a Boolean.`;
    case "number": return typeof value === "number" && Number.isFinite(value) ? undefined : `${name} needs a number.`;
    case "integer": return typeof value === "number" && Number.isInteger(value) ? undefined : `${name} needs an integer.`;
    case "array": {
      if (!Array.isArray(value)) return `${name} needs a list.`;
      if (schema.items) for (const [index, item] of value.entries()) {
        const issue = schemaValueProblem(schema.items, item, `${name} item ${index + 1}`);
        if (issue) return issue;
      }
      return;
    }
    case "object": {
      if (!value || typeof value !== "object" || Array.isArray(value)) return `${name} needs an object.`;
      const fields = value as Record<string, unknown>;
      const missing = schema.required?.find((field) => !Object.hasOwn(fields, field) || fields[field] === undefined);
      if (missing) return `${name} needs ${missing}.`;
      for (const [field, content] of Object.entries(fields)) {
        const property = schema.properties && Object.hasOwn(schema.properties, field) ? schema.properties[field] : undefined;
        if (!property) {
          if (schema.additionalProperties === false) return `${name} does not declare ${field}.`;
          continue;
        }
        const issue = schemaValueProblem(property, content, `${name}.${field}`);
        if (issue) return issue;
      }
    }
  }
}
