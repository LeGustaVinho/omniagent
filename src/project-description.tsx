import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { resolveWiki, type NoteTarget } from "@/knowledge-document";
import type { NoteCatalogEntry } from "@/repository-note-buffers";
import type { WorkspaceProps } from "@/workbench-contract";

const Editor = lazy(() => import("@/knowledge-editor").then(({ MarkdownEditor }) => ({ default: MarkdownEditor })));
const Reading = lazy(() => import("@/knowledge-reading").then(({ NoteReading }) => ({ default: NoteReading })));

type OwnerKind = "epic" | "work";
type Revision = { id: string; revision: number; markdown: string | null };
type Draft = { revision: string; base: Revision | null; markdown: string };
type SavedState = { saved: Revision | null; draft: Draft | null };
type Conflict = "saved_revision_changed" | "draft_revision_changed";
type SaveResult = { status: "saved"; revision: Revision } | { status: "conflict"; reason: Conflict; state: SavedState };
type Buffer = SavedState & { base: Revision | null; content: string; conflict?: Conflict };
type Mode = "source" | "live" | "reading";

function restore(state: SavedState): Buffer {
  const { saved, draft } = state;
  const base = draft ? draft.base : saved;
  return {
    ...state, base, content: draft?.markdown ?? saved?.markdown ?? "",
    conflict: draft && base?.id !== saved?.id ? "saved_revision_changed" : undefined,
  };
}

export function ProjectDescription(props: { kind: OwnerKind; id: string; links: WorkspaceProps }) {
  const [open, setOpen] = useState(false);
  return open ? <DescriptionEditor {...props} /> : <Button type="button" variant="outline" onClick={() => setOpen(true)}>Open description</Button>;
}

function DescriptionEditor({ kind, id, links }: { kind: OwnerKind; id: string; links: WorkspaceProps }) {
  const { projectId, checkoutId } = links.checkout;
  const args = useMemo(() => ({ projectId, owner: kind === "epic" ? { epic: id } : { work: id } }), [id, kind, projectId]);
  const [buffer, setBuffer] = useState<Buffer>();
  const [mode, setMode] = useState<Mode>("live");
  const [split, setSplit] = useState(false);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const current = useRef<Buffer>(undefined);
  const operations = useRef(Promise.resolve());
  const mounted = useRef(false);
  const catalog = useQuery({
    queryKey: ["project-description-note-catalog", checkoutId],
    queryFn: () => invoke<NoteCatalogEntry[]>("repository_note_catalog", { checkoutId }),
  });
  const files = useMemo(() => (catalog.data ?? []).map(({ path, frontmatter }) => ({ path, content: frontmatter ?? "" })), [catalog.data]);
  const path = `${id}.md`;

  useEffect(() => {
    let alive = true;
    mounted.current = true;
    void invoke<SavedState>("project_description_editor", args).then((state) => {
      if (!alive) return;
      current.current = restore(state);
      setBuffer(current.current);
    }).catch((failure: unknown) => { if (alive) setError(String(failure)); });
    return () => { mounted.current = false; alive = false; };
  }, [args]);

  function publish(next: Buffer) {
    current.current = next;
    if (mounted.current) setBuffer(next);
  }

  function enqueue(operation: () => Promise<void>) {
    operations.current = operations.current.then(operation).catch((failure: unknown) => {
      if (mounted.current) setError(String(failure));
    });
  }

  async function persistDraft() {
    const note = current.current;
    if (!note || note.conflict === "draft_revision_changed") return;
    const savedInEditor = note.base?.id === note.saved?.id && note.content === (note.saved?.markdown ?? "");
    try {
      const draft = await (async () => {
        if (!savedInEditor) return invoke<Draft>("save_project_description_draft", {
          ...args, baseRevision: note.base?.id ?? null, expectedDraftRevision: note.draft?.revision ?? null, markdown: note.content,
        });
        if (note.draft && !await invoke<boolean>("discard_project_description_draft", {
          ...args, expectedDraftRevision: note.draft.revision,
        })) throw new Error("The description draft changed before it could be discarded.");
        return null;
      })();
      publish({ ...current.current!, draft });
    } catch (failure) {
      const latest = await invoke<SavedState>("project_description_editor", args);
      if (latest.draft?.revision === note.draft?.revision) throw failure;
      publish({ ...current.current!, ...latest, conflict: "draft_revision_changed" });
    }
  }

  function edit(content: string) {
    const note = current.current;
    if (!note || note.content === content) return;
    publish({ ...note, content });
    enqueue(persistDraft);
  }

  function save() {
    if (saving) return;
    setSaving(true);
    setError("");
    enqueue(async () => {
      try {
        const note = current.current;
        if (!note || note.conflict) return;
        const result = await invoke<SaveResult>("save_project_description_editor", {
          ...args, expectedRevision: note.base?.id ?? null, expectedDraftRevision: note.draft?.revision ?? null,
          markdown: note.content || null,
        });
        if (result.status === "conflict") {
          const conflict = result.state.draft?.revision !== note.draft?.revision ? "draft_revision_changed" : result.reason;
          publish({ ...current.current!, ...result.state, conflict });
        } else {
          publish({ ...current.current!, base: result.revision, draft: null, saved: result.revision });
        }
      } finally {
        if (mounted.current) setSaving(false);
      }
    });
  }

  function resolve(keepLocal: boolean) {
    setError("");
    enqueue(async () => {
      const note = current.current;
      if (!note?.conflict) return;
      if (keepLocal) {
        publish({ ...note, base: note.saved, conflict: undefined });
        await persistDraft();
      } else if (note.conflict === "draft_revision_changed") {
        publish(restore(note));
      } else {
        if (note.draft && !await invoke<boolean>("discard_project_description_draft", {
          ...args, expectedDraftRevision: note.draft.revision,
        })) {
          const latest = await invoke<SavedState>("project_description_editor", args);
          publish({ ...current.current!, ...latest, conflict: "draft_revision_changed" });
          return;
        }
        const content = current.current?.content;
        publish({ ...restore({ saved: note.saved, draft: null }), content: content === note.content ? note.saved?.markdown ?? "" : content ?? "" });
      }
    });
  }

  function openLink(link: NoteTarget) {
    const targets = resolveWiki(link, link.sourcePath ?? path, files);
    if (targets.length === 1 && targets[0]) links.openWorkspace("knowledge", targets[0].path);
    else setError(targets.length ? "The note reference matches multiple files." : "The referenced note could not be found.");
  }

  return <section className="pipeline-stack" aria-label="Work description">
    <h4 className="font-medium">Description</h4>
    {!buffer && !error && <p role="status">Loading description…</p>}
    {error && <p role="alert">{error}</p>}
    {buffer && <div className="agile-description">
      <div className="flex flex-wrap gap-1 border-b border-border px-1.5 py-1" aria-label="Description views">
        {([["source", "Source"], ["live", "Live Preview"], ["reading", "Reading"]] as const).map(([choice, label]) =>
          <button type="button" key={choice} className="delivery-tab" aria-pressed={mode === choice} onClick={() => setMode(choice)}>{label}</button>)}
        <button type="button" className="delivery-tab" aria-pressed={split} onClick={() => {
          if (!split && mode === "reading") setMode("live");
          setSplit(!split);
        }}>Split Reading view</button>
        <button type="button" className="delivery-tab" disabled={saving || Boolean(buffer.conflict) || buffer.content === (buffer.saved?.markdown ?? "")}
          onClick={save}>Save description</button>
        {buffer.content !== (buffer.saved?.markdown ?? "") && <span className="px-2 text-xs" role="status">Unsaved changes</span>}
      </div>
      {buffer.conflict && <div className="p-2" aria-label="Description conflict">
        <p>{buffer.conflict === "saved_revision_changed" ? "The saved description changed." : "The stored description draft changed."}</p>
        <pre className="whitespace-pre-wrap">{buffer.conflict === "draft_revision_changed" ? buffer.draft?.markdown : buffer.saved?.markdown}</pre>
        <Button type="button" variant="outline" onClick={() => resolve(false)}>
          {buffer.conflict === "draft_revision_changed" ? "Use stored draft" : "Use saved description"}
        </Button>{" "}
        <Button type="button" variant="outline" onClick={() => resolve(true)}>Keep my edit</Button>
      </div>}
      <Suspense fallback={<p className="p-3 text-xs" role="status">Loading Markdown view…</p>}>
        <div className={split && mode !== "reading" ? "agile-description-split" : undefined}>
          <div className="agile-description-editor agile-description-editor-expanded" hidden={mode === "reading"}>
            <Editor mode={mode === "source" ? "source" : "live"} path={path} source={buffer.content} files={files} analyze onChange={edit} onLink={openLink} />
          </div>
          {(split || mode === "reading") && <div className="agile-description-reading agile-description-reading-expanded">
            {buffer.content ? <Reading path={path} content={buffer.content} files={files} onLink={openLink} onAttachment={() => undefined}
              checkoutId={checkoutId} sampleAttachments={false} /> : <p>No description.</p>}
          </div>}
        </div>
      </Suspense>
    </div>}
  </section>;
}
