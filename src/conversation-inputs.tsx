import { invoke } from "@tauri-apps/api/core";
import { useId, useRef, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";

type RequestId = string | number;
type FieldValue = string | number | boolean | string[];
type QuestionAction = "accept" | "decline" | "cancel";
type Input =
  | { kind: "text"; minLength: number | null; maxLength: number | null }
  | { kind: "boolean" }
  | { kind: "number"; integer: boolean; minimum: number | null; maximum: number | null }
  | { kind: "choice"; options: { id: string; label: string }[]; multiple: boolean; allowCustom: boolean };
type Field = {
  id: string;
  label: string;
  description: string | null;
  required: boolean;
  secret: boolean;
  default: FieldValue | null;
  input: Input;
};
type QuestionForm = { message: string | null; fields: Field[]; actions: QuestionAction[] };
type Approval = { type: "approval"; request_id: RequestId; operation: string; description: string | null };
type Question = { type: "question"; request_id: RequestId; normalized?: QuestionForm };
export type ConversationPendingInput = { data: Approval | Question };

const inputClass = "w-full rounded-md border border-input bg-background px-2 py-1.5 text-sm focus-visible:outline-ring";

function FieldControl({ field, value, update }: {
  field: Field;
  value: FieldValue | undefined;
  update: (value: FieldValue | undefined) => void;
}) {
  const id = useId();
  const input = field.input;
  let control;
  if (input.kind === "choice" && input.multiple) {
    const selected = Array.isArray(value) ? value : [];
    const optionIds = new Set(input.options.map((option) => option.id));
    const custom = selected.find((choice) => !optionIds.has(choice));
    return (
      <fieldset className="grid gap-1.5">
        <legend className="mb-1 text-xs font-medium">{field.label}</legend>
        {field.description && <p className="text-xs text-muted-foreground">{field.description}</p>}
        {input.options.map((option) => <label key={option.id} className="conv-checkbox text-sm">
          <input type="checkbox" checked={selected.includes(option.id)}
            onChange={(event) => update(event.currentTarget.checked ? [...selected, option.id] : selected.filter((choice) => choice !== option.id))} />
          {option.label}
        </label>)}
        {input.allowCustom && <input className={inputClass} aria-label={`Other ${field.label}`} value={custom ?? ""}
          onChange={(event) => update([...selected.filter((choice) => optionIds.has(choice)), ...(event.target.value ? [event.target.value] : [])])} />}
      </fieldset>
    );
  } else if (input.kind === "choice" && input.allowCustom) {
    control = (
      <>
        <input id={id} className={inputClass} list={`${id}-choices`} required={field.required}
          value={String(value ?? "")} onChange={(event) => update(event.target.value || undefined)} />
        <datalist id={`${id}-choices`}>
          {input.options.map((option) => <option key={option.id} value={option.id} label={option.label} />)}
        </datalist>
      </>
    );
  } else if (input.kind === "choice" || input.kind === "boolean") {
    control = (
      <select id={id} className={inputClass} required={field.required} value={String(value ?? "")}
        onChange={(event) => update(!event.target.value ? undefined : input.kind === "boolean" ? event.target.value === "true" : event.target.value)}>
        <option value="">Choose…</option>
        {input.kind === "boolean"
          ? <><option value="true">Yes</option><option value="false">No</option></>
          : input.options.map((option) => <option key={option.id} value={option.id}>{option.label}</option>)}
      </select>
    );
  } else {
    control = (
      <input id={id} className={inputClass} required={field.required} value={String(value ?? "")}
        type={input.kind === "number" ? "number" : field.secret ? "password" : "text"}
        min={input.kind === "number" ? input.minimum ?? undefined : undefined}
        max={input.kind === "number" ? input.maximum ?? undefined : undefined}
        step={input.kind === "number" ? (input.integer ? 1 : "any") : undefined}
        minLength={input.kind === "text" ? input.minLength ?? undefined : undefined}
        maxLength={input.kind === "text" ? input.maxLength ?? undefined : undefined}
        onChange={(event) => update(input.kind === "number" ? (event.target.value === "" ? undefined : event.target.valueAsNumber) : event.target.value)} />
    );
  }
  return (
    <div className="grid gap-1">
      <label className="text-xs font-medium" htmlFor={id}>{field.label}</label>
      {field.description && <p className="text-xs text-muted-foreground">{field.description}</p>}
      {control}
    </div>
  );
}

export function PendingInput({ input, conversationId, runId, onResolved }: {
  input: ConversationPendingInput;
  conversationId: string;
  runId: string;
  onResolved: () => void;
}) {
  const request = input.data;
  const form = request.type === "question" ? request.normalized : undefined;
  const [values, setValues] = useState<Record<string, FieldValue | undefined>>(() =>
    Object.fromEntries(form?.fields.map((field) => [field.id, field.default ?? undefined]) ?? []));
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [error, setError] = useState("");
  const target = { conversationId, runId, requestId: request.request_id };

  async function submit(command: string, data: object) {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError("");
    try {
      await invoke(command, { ...target, ...data });
      onResolved();
    } catch (error) {
      inFlight.current = false;
      setError(String(error));
      setBusy(false);
    }
  }

  const answer = (action: QuestionAction) => submit("answer_conversation_question", {
    answer: { action, values: action === "accept" ? values : {} },
  });
  function accept(event: FormEvent) {
    event.preventDefault();
    const missing = form?.fields.find((field) => {
      const value = values[field.id];
      return field.required && (value === undefined || value === "" || (Array.isArray(value) && value.length === 0));
    });
    if (missing) {
      setError(`Choose a value for ${missing.label}.`);
      return;
    }
    void answer("accept");
  }

  return (
    <div className="rounded-md border border-border bg-card p-3">
      {request.type === "approval" ? (
        <>
          <p className="text-sm font-medium">Approval requested for {request.operation}</p>
          {request.description && <p className="mt-1 whitespace-pre-wrap text-xs text-muted-foreground">{request.description}</p>}
          <div className="mt-2 flex flex-wrap gap-2">
            <Button disabled={busy} onClick={() => void submit("approve_conversation_input", { decision: "allow_once" })}>Allow once</Button>
            <Button variant="outline" disabled={busy} onClick={() => void submit("approve_conversation_input", { decision: "deny" })}>Deny</Button>
          </div>
        </>
      ) : form ? (
        <form onSubmit={accept} className="grid gap-2">
          <p className="text-sm font-medium">{form.message || "Input requested"}</p>
          {form.fields.map((field) => <FieldControl key={field.id} field={field} value={values[field.id]}
            update={(value) => setValues((current) => ({ ...current, [field.id]: value }))} />)}
          <div className="flex flex-wrap gap-2">
            {form.actions.includes("accept") && <Button type="submit" disabled={busy}>Reply</Button>}
            {form.actions.includes("decline") && <Button type="button" variant="outline" disabled={busy} onClick={() => void answer("decline")}>Decline</Button>}
            {form.actions.includes("cancel") && <Button type="button" variant="outline" disabled={busy} onClick={() => void answer("cancel")}>Cancel question</Button>}
          </div>
        </form>
      ) : <p className="text-sm text-muted-foreground">This question format is unavailable.</p>}
      {error && <p role="alert" className="mt-2 text-sm text-red-300">{error}</p>}
    </div>
  );
}
