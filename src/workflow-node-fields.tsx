import { Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { CONVERSATION_START_INPUT_TYPES, START_VALUE_TYPES,
  type ConversationStartSource, type WorkflowNodeConfiguration, type WorkflowPredicate, type WorkflowScalar, type WorkflowStartField,
} from "@/workflow-graph-model";
import { StructuredValueField, structuredObject } from "@/workflow-structured-fields";

type Fields = {
  configuration: WorkflowNodeConfiguration;
  disabled: boolean;
  onChange: (configuration: WorkflowNodeConfiguration) => void;
};

function text(value: unknown) {
  return typeof value === "string" ? value : "";
}

export function StartInputFields({ configuration, disabled, onChange }: Fields) {
  const fields: WorkflowStartField[] = Array.isArray(configuration.fields) ? configuration.fields : [];
  const fromRequest = configuration.promptOrMarkdown === true;
  const update = (fields: WorkflowStartField[], promptOrMarkdown = fromRequest) =>
    onChange({ kind: "typed_start", fields, ...(promptOrMarkdown ? { promptOrMarkdown } : {}) });
  function add() {
    let ordinal = fields.length + 1;
    while (fields.some(({ name }) => name === `input-${ordinal}`)) ordinal += 1;
    update([...fields, { name: `input-${ordinal}`, valueType: "text", required: true }]);
  }
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>Launch inputs</legend>
    <label className="workflow-check"><input type="checkbox" checked={fromRequest}
      onChange={(event) => update(fields, event.currentTarget.checked)} />Accept a prompt or saved Markdown</label>
    {fromRequest && <p className="pipeline-meta">The inputs port provides the request as <code>/request</code>.</p>}
    {fields.map((field, index) => {
      const replace = (next: WorkflowStartField) => update(fields.map((entry, position) => position === index ? next : entry));
      const edit = (patch: Partial<WorkflowStartField>) => replace({ ...field, ...patch });
      return <div className="workflow-inspector-item" key={index}>
        <label>Input {index + 1} name<input value={field.name} onChange={(event) => edit({ name: event.currentTarget.value })} /></label>
        <label>Input {index + 1} source<select value={field.conversationSource ?? ""} onChange={(event) => {
          const source = event.currentTarget.value as ConversationStartSource | "";
          if (source) edit({ conversationSource: source, valueType: CONVERSATION_START_INPUT_TYPES[source] });
          else replace({ name: field.name, valueType: field.valueType, required: field.required });
        }}>
          <option value="">At launch</option>
          <option value="message">Conversation message</option>
          <option value="history">Conversation history</option>
        </select></label>
        <label>Input {index + 1} type<select value={field.valueType} disabled={Boolean(field.conversationSource)}
          onChange={(event) => edit({ valueType: event.currentTarget.value as WorkflowStartField["valueType"] })}>
          {START_VALUE_TYPES.map((type) => <option value={type} key={type}>{type[0]!.toUpperCase()}{type.slice(1)}</option>)}
        </select></label>
        {field.conversationSource && <small className="pipeline-meta">{field.conversationSource === "message"
          ? "Uses the message that starts this run." : "Uses earlier messages in order, with each message’s role and text."} Manual runs supply this input at launch.</small>}
        <label className="workflow-check"><input type="checkbox" checked={field.required}
          onChange={(event) => edit({ required: event.currentTarget.checked })} />Required at launch</label>
        <Button type="button" variant="ghost" aria-label={`Remove input ${index + 1}`}
          onClick={() => update(fields.filter((_, position) => position !== index))}><Trash2 size={15} /> Remove input</Button>
      </div>;
    })}
    <Button type="button" variant="outline" onClick={add}><Plus size={15} /> Add input</Button>
  </fieldset>;
}

export function PredicateFields({ value, label, disabled, onChange }: {
  value: unknown; label: string; disabled: boolean; onChange: (value: WorkflowPredicate) => void;
}) {
  const { op, pointer, value: expected } = structuredObject(value);
  const operation = op === "equals" ? op : "exists";
  const path = text(pointer);
  const literal: WorkflowScalar = expected === null || typeof expected === "boolean" || typeof expected === "number" ? expected : text(expected);

  return <fieldset className="pipeline-stack" disabled={disabled}>
    <legend>{label}</legend>
    <label>Test<select value={operation} onChange={(event) => {
      const next = event.currentTarget.value;
      if (next === "equals") onChange({ op: next, pointer: path, value: literal });
      else onChange({ op: "exists", pointer: path });
    }}>
      <option value="exists">Field exists</option><option value="equals">Field equals</option>
    </select></label>
    <label>Field path<input value={path} placeholder="/result/status"
      onChange={(event) => onChange(operation === "equals" ? { op: operation, pointer: event.currentTarget.value, value: literal }
        : { op: operation, pointer: event.currentTarget.value })} /></label>
    {operation === "equals" && <StructuredValueField label="Expected value" value={literal} disabled={disabled} scalar
      onChange={(next) => onChange({ op: operation, pointer: path, value: next as WorkflowScalar })} />}
  </fieldset>;
}
