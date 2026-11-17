import { Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";

type ChoiceOption = { id: string; label: string };
type FieldInput = { kind: "text" } | { kind: "boolean" } | { kind: "number"; integer: boolean }
  | { kind: "choice"; multiple: boolean; options: ChoiceOption[] };
export type InputField = { id: string; label: string; required: boolean; input: FieldInput };
export type WorkflowDecisionConfiguration = { kind: "request_input"; message: string; fields: InputField[] }
  | { kind: "approval"; message: string };

export function defaultDecisionConfiguration(kind: WorkflowDecisionConfiguration["kind"]): WorkflowDecisionConfiguration {
  return kind === "approval" ? { kind, message: "Does this stage have your approval?" }
    : { kind, message: "Input requested", fields: [{ id: "answer", label: "Answer", required: true, input: { kind: "text" } }] };
}

function defaultInput(kind: FieldInput["kind"]): FieldInput {
  switch (kind) {
    case "number": return { kind, integer: false };
    case "choice": return { kind, multiple: false, options: ["option-1", "option-2"].map((id, index) => ({ id, label: `Option ${index + 1}` })) };
    default: return { kind };
  }
}

function nextId(entries: { id: string }[], prefix: string) {
  let ordinal = entries.length + 1;
  while (entries.some(({ id }) => id === `${prefix}-${ordinal}`)) ordinal += 1;
  return `${prefix}-${ordinal}`;
}

function ChoiceFields({ input, update }: { input: Extract<FieldInput, { kind: "choice" }>; update: (input: FieldInput) => void }) {
  const { options } = input;
  function setOption(index: number, patch: Partial<ChoiceOption>) {
    update({ ...input, options: options.map((option, position) => position === index ? { ...option, ...patch } : option) });
  }
  return <div className="pipeline-stack">
    <label className="pipeline-checkline"><input type="checkbox" checked={input.multiple}
      onChange={(event) => update({ ...input, multiple: event.currentTarget.checked })} /> Allow multiple choices</label>
    <fieldset className="pipeline-stack">
      <legend>Choices</legend>
      {options.map((option, index) => <div className="pipeline-stack" key={index}>
        <label>Option {index + 1} label<input value={option.label} onChange={(event) => setOption(index, { label: event.currentTarget.value })} /></label>
        <label>Option {index + 1} ID<input value={option.id} onChange={(event) => setOption(index, { id: event.currentTarget.value })} /></label>
        <Button type="button" variant="ghost" className="justify-self-start" disabled={options.length <= 1} aria-label={`Remove option ${index + 1}`}
          onClick={() => update({ ...input, options: options.filter((_, position) => position !== index) })}><Trash2 size={15} /></Button>
      </div>)}
      <Button type="button" variant="outline" className="justify-self-start" onClick={() => update({ ...input,
        options: [...options, { id: nextId(options, "option"), label: `Option ${options.length + 1}` }],
      })}><Plus size={15} /> Add option</Button>
    </fieldset>
  </div>;
}

export function DecisionNodeFields({ configuration, onChange, disabled }: {
  configuration: WorkflowDecisionConfiguration;
  onChange: (value: WorkflowDecisionConfiguration) => void;
  disabled?: boolean;
}) {
  const approval = configuration.kind === "approval";
  return <fieldset className="pipeline-stack" disabled={disabled}>
    <label>{approval ? "Approval message" : "Question message"}<textarea rows={3} value={configuration.message}
      onChange={(event) => onChange({ ...configuration, message: event.currentTarget.value })} /></label>
    {!approval && <DecisionAnswerFields fields={configuration.fields} onChange={(fields) => onChange({ ...configuration, fields })} />}
  </fieldset>;
}

export function DecisionAnswerFields({ fields, minimum = 1, onChange }: {
  fields: InputField[]; minimum?: number; onChange: (fields: InputField[]) => void;
}) {
  function updateField(index: number, patch: Partial<InputField>) {
    onChange(fields.map((field, position) => position === index ? { ...field, ...patch } : field));
  }
  return <fieldset className="pipeline-stack">
        <legend>Answer fields</legend>
        {fields.map((field, index) => {
          const { input } = field;
          const update = (input: FieldInput) => updateField(index, { input });
          return <fieldset key={index} className="pipeline-stack">
            <legend>Field {index + 1}</legend>
            <label>Field {index + 1} label<input value={field.label} onChange={(event) => updateField(index, { label: event.currentTarget.value })} /></label>
            <label>Field {index + 1} ID<input value={field.id} onChange={(event) => updateField(index, { id: event.currentTarget.value })} /></label>
            <label>Field {index + 1} type<select value={input.kind} onChange={(event) => update(defaultInput(event.currentTarget.value as FieldInput["kind"]))}>
              <option value="text">Text</option><option value="boolean">Boolean</option><option value="number">Number</option><option value="choice">Choice</option>
            </select></label>
            <label className="pipeline-checkline"><input type="checkbox" checked={field.required}
              onChange={(event) => updateField(index, { required: event.currentTarget.checked })} /> Required</label>
            {input.kind === "number" && <label className="pipeline-checkline"><input type="checkbox" checked={input.integer}
              onChange={(event) => update({ ...input, integer: event.currentTarget.checked })} /> Whole numbers only</label>}
            {input.kind === "choice" && <ChoiceFields input={input} update={update} />}
            <Button type="button" variant="ghost" className="justify-self-start" disabled={fields.length <= minimum}
              aria-label={`Remove field ${index + 1}`} onClick={() => onChange(fields.filter((_, position) => position !== index))}><Trash2 size={15} /></Button>
          </fieldset>;
        })}
        <Button type="button" variant="outline" className="justify-self-start" onClick={() => onChange([
          ...fields, { id: nextId(fields, "field"), label: `Answer ${fields.length + 1}`, required: true, input: { kind: "text" } },
        ])}><Plus size={15} /> Add field</Button>
      </fieldset>;
}
