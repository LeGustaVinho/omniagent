import { invoke } from "@tauri-apps/api/core";
import { useId, useState, type FormEvent } from "react";

export function NotePropertyEditor({ name, value, read, update, replace }: {
  name: string;
  value: string;
  read: () => string | undefined;
  update: (markdown: string) => void;
  replace?: (markdown: string, name: string, value: string) => string;
}) {
  const inputId = useId();
  const [pending, setPending] = useState<string>();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  async function apply(event: FormEvent) {
    event.preventDefault();
    const markdown = read();
    if (saving || pending === undefined || markdown === undefined) return;
    setSaving(true);
    setError("");
    try {
      const edited = replace ? replace(markdown, name, pending)
        : await invoke<string>("replace_note_property", { markdown, key: name, value: pending });
      if (read() !== markdown) {
        setError("The note changed. Review the property and apply it again.");
        return;
      }
      update(edited);
      setPending(undefined);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setSaving(false);
    }
  }

  return <form className="note-property-row" onSubmit={(event) => void apply(event)}>
    <label htmlFor={inputId}>{name}</label>
    <input id={inputId} value={pending ?? value} onChange={(event) => setPending(event.currentTarget.value)} disabled={saving} />
    {pending !== undefined && pending !== value && <button className="fw-btn" type="submit" disabled={saving}>Apply</button>}
    {error && <span role="alert" className="file-help">{error}</span>}
  </form>;
}
