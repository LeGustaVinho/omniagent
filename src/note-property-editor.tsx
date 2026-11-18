import { invoke } from "@tauri-apps/api/core";
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ContextActions, type MenuAction } from "@/components/ui/context-actions";

export function NotePropertyEditor({ name, value, read, update, actions, editable = true, expectedSource, focus }: {
  name: string;
  value: string;
  read: () => string | undefined;
  update: (markdown: string) => void;
  actions?: readonly MenuAction[];
  editable?: boolean;
  expectedSource?: string;
  focus?: boolean;
}) {
  const inputId = useId();
  const [pending, setPending] = useState<{ value: string; source?: string }>();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  async function apply(event: FormEvent) {
    event.preventDefault();
    const markdown = read();
    if (saving || !editable || pending === undefined) return;
    if (markdown === undefined || (expectedSource ?? pending.source) !== markdown) {
      setError("The note changed. Reopen the latest property before applying this edit.");
      return;
    }
    setSaving(true);
    setError("");
    try {
      const edited = await invoke<string>("replace_note_property", { markdown, key: name, value: pending.value });
      if (!mounted.current) return;
      if (read() !== markdown) {
        setError("The note changed. Review the property and apply it again.");
        return;
      }
      update(edited);
      setPending(undefined);
    } catch (failure) {
      if (mounted.current) setError(String(failure));
    } finally {
      if (mounted.current) setSaving(false);
    }
  }

  return <form className="note-property-row" onSubmit={(event) => void apply(event)}>
    <ContextActions label={name} groups={actions ? [[...(editable ? [{ label: "Edit property", disabled: saving, run: () => {
      requestAnimationFrame(() => { input.current?.focus(); input.current?.select(); });
    } }] : []), ...actions]] : []}>
      {editable ? <label htmlFor={inputId}>{name}</label> : <span>{name}</span>}
    </ContextActions>
    {editable ? <input ref={input} id={inputId} value={pending?.value ?? value} autoFocus={focus}
      onChange={(event) => setPending({ value: event.currentTarget.value, source: pending?.source ?? read() })} disabled={saving} />
      : <span className="whitespace-pre-wrap">{value}</span>}
    {editable && pending !== undefined && pending.value !== value && <button className="fw-btn" type="submit" disabled={saving}>Apply</button>}
    {error && <span role="alert" className="file-help">{error}</span>}
  </form>;
}
