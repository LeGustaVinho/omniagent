import { invoke } from "@tauri-apps/api/core";
import { useId, useRef, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";

export type RequestId = string | number;
export type FieldValue = string | number | boolean | string[];
export type QuestionAction = "accept" | "decline" | "cancel";
type Input =
  | { kind: "text"; minLength: number | null; maxLength: number | null }
  | { kind: "boolean" }
  | { kind: "number"; integer: boolean; minimum: number | null; maximum: number | null }
  | { kind: "choice"; options: { id: string; label: string; description?: string | null }[];
      multiple: boolean; allowCustom: boolean; minSelections?: number; maxSelections?: number };
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
  if (input.kind === "choice") {
    const custom = typeof value === "string" && (input.multiple || !input.options.some((option) => option.id === value))
      ? value : undefined;
    const selected = Array.isArray(value) ? value : typeof value === "string" && custom === undefined ? [value] : [];
    return (
      <fieldset className="grid gap-1.5">
        <legend className="mb-1 text-xs font-medium">{field.label}</legend>
        {field.description && <p className="text-xs text-muted-foreground">{field.description}</p>}
        {input.options.map((option) => <label key={option.id} className="conv-checkbox text-sm">
          <input type={input.multiple ? "checkbox" : "radio"} name={id} checked={selected.includes(option.id)}
            onChange={(event) => {
              if (!input.multiple) update(option.id);
              else if (event.currentTarget.checked) update([...selected, option.id]);
              else {
                const remaining = selected.filter((choice) => choice !== option.id);
                update(remaining.length || field.required ? remaining : undefined);
              }
            }} />
          {option.label}
          {option.description && <span className="text-xs text-muted-foreground">{option.description}</span>}
        </label>)}
        {input.allowCustom && <input className={inputClass} aria-label={`Other ${field.label}`} value={custom ?? ""}
          onChange={(event) => update(event.currentTarget.value || undefined)} />}
        {!input.multiple && selected.length > 0 && !field.required && <Button type="button" variant="ghost" className="justify-self-start text-xs"
          onClick={() => update(undefined)}>Clear {field.label}</Button>}
      </fieldset>
    );
  }
  if (input.kind === "boolean") {
    return <div className="grid gap-1">
      <label className="conv-checkbox text-sm">
        <input type="checkbox" role="switch" checked={value === true}
          aria-describedby={field.description ? id : undefined}
          onChange={(event) => update(event.currentTarget.checked)} />
        {field.label}
      </label>
      {field.description && <p id={id} className="text-xs text-muted-foreground">{field.description}</p>}
      {value === undefined && <Button type="button" variant="ghost" className="justify-self-start text-xs"
        onClick={() => update(false)}>Set {field.label} to No</Button>}
      {!field.required && value !== undefined && <Button type="button" variant="ghost" className="justify-self-start text-xs"
        onClick={() => update(undefined)}>Clear {field.label}</Button>}
    </div>;
  }
  return (
    <div className="grid gap-1">
      <label className="text-xs font-medium" htmlFor={id}>{field.label}</label>
      {field.description && <p className="text-xs text-muted-foreground">{field.description}</p>}
      <input id={id} className={inputClass} required={field.required} value={String(value ?? "")}
        type={input.kind === "number" ? "number" : field.secret ? "password" : "text"}
        min={input.kind === "number" ? input.minimum ?? undefined : undefined}
        max={input.kind === "number" ? input.maximum ?? undefined : undefined}
        step={input.kind === "number" ? (input.integer ? 1 : "any") : undefined}
        minLength={input.kind === "text" ? input.minLength ?? undefined : undefined}
        maxLength={input.kind === "text" ? input.maxLength ?? undefined : undefined}
        onChange={(event) => update(input.kind === "number"
          ? (event.target.value === "" ? undefined : event.target.valueAsNumber) : event.target.value)} />
    </div>
  );
}

export function PendingInput({ input, conversationId, runId, onResolved, savedReason }: {
  input: ConversationPendingInput;
  conversationId: string;
  runId: string;
  onResolved: () => void;
  savedReason?: string;
}) {
  const request = input.data;
  const form = request.type === "question" ? request.normalized : undefined;
  const [values, setValues] = useState<Record<string, FieldValue | undefined>>(() =>
    Object.fromEntries(form?.fields.map((field) => {
      const input = field.input;
      const permitsEmpty = field.required && input.kind === "choice" && input.multiple && input.minSelections === 0;
      return [field.id, field.default ?? (permitsEmpty ? [] : undefined)];
    }) ?? []));
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

  const answer = (action: QuestionAction) => submit(savedReason ? "answer_saved_conversation_question" : "answer_conversation_question", {
    answer: { action, values: action === "accept"
      ? Object.fromEntries(Object.entries(values).filter(([, value]) => value !== undefined)) : {} },
  });
  function accept(event: FormEvent) {
    event.preventDefault();
    if (!form?.actions.includes("accept")) return;
    for (const field of form.fields) {
      const value = values[field.id];
      if (field.required && (value === undefined || value === "")) {
        setError(`Choose a value for ${field.label}.`);
        return;
      }
      if (field.input.kind === "choice" && Array.isArray(value)) {
        const { minSelections = Number(field.required), maxSelections = field.input.options.length } = field.input;
        if (value.length < minSelections || value.length > maxSelections) {
          setError(`Choose between ${minSelections} and ${maxSelections} options for ${field.label}.`);
          return;
        }
      }
      if (field.input.kind === "choice" && typeof value === "string" && !value.trim()) {
        setError(`Enter a value for ${field.label}.`);
        return;
      }
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
            <Button type="button" disabled={busy} onClick={() => void submit("approve_conversation_input", { decision: "allow_once" })}>Allow once</Button>
            <Button type="button" variant="outline" disabled={busy} onClick={() => void submit("approve_conversation_input", { decision: "deny" })}>Deny</Button>
          </div>
        </>
      ) : form ? (
        <form onSubmit={accept} className="grid gap-2">
          <p className="text-sm font-medium">{form.message || "Input requested"}</p>
          {savedReason && <p className="text-xs text-muted-foreground">{savedReason}</p>}
          <fieldset className="grid gap-2" disabled={busy}>
            {form.fields.map((field) => <FieldControl key={field.id} field={field} value={values[field.id]}
              update={(value) => setValues((current) => ({ ...current, [field.id]: value }))} />)}
          </fieldset>
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
