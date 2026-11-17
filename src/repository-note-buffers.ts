import { Channel, invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";

export type NoteRevision = {
  id: string;
  checkoutId: string;
  relativePath: string;
  sha256: string;
  baseCommit: string | null;
};
type LineEndings = "none" | "lf" | "crlf" | "cr" | "mixed";
type RepositoryDocument = { revision: NoteRevision; markdown: string; fileIdentity: string; utf8Bom?: boolean; lineEndings?: LineEndings };
export type DocumentOpen = { status: "document"; document: RepositoryDocument }
  | { status: "missing" | "unsupported_encoding"; relativePath?: string };
type Recovery = { message: string; editedPath: string | null; displacedPath: string | null };
type DocumentSave = { status: "saved"; document: RepositoryDocument }
  | { status: "conflict" | "recovery_required"; recovery: Recovery };
type SavedDraft = { revision: string; base: NoteRevision; markdown: string };

export type NoteBuffer = {
  content: string;
  saved: string;
  revision: number | string;
  external?: string;
  format?: { utf8Bom: boolean; lineEndings: LineEndings };
};
type RepositoryBuffer = NoteBuffer & {
  base: NoteRevision;
  fileIdentity?: string;
  draftRevision: string | null;
  incoming?: RepositoryDocument;
  sourceProblem?: string;
  recovery?: Recovery;
};
type NoteState = { notes: Record<string, RepositoryBuffer>; failures: Record<string, string> };
export type NoteCatalogEntry = { path: string; frontmatter: string | null };
export type NoteAttachmentContent = { relativePath: string; mimeType: string; contentBase64: string };
type Watch = { id?: string };

export const normalizeEditorText = (markdown: string) => markdown.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n");

const noteCommands = {
  catalog: "repository_note_catalog",
  open: "open_repository_document",
  save: "save_repository_document",
  readRevision: "read_repository_markdown",
  drafts: "list_repository_markdown_drafts",
  readDraft: "read_repository_markdown_draft",
  saveDraft: "save_repository_markdown_draft",
  discardDraft: "discard_repository_markdown_draft",
  watch: "watch_repository_note",
  unwatch: "unwatch_repository_note",
};

const sourceCommands: typeof noteCommands = {
  catalog: "list_repository_source_files",
  open: "open_repository_source_file",
  save: "save_repository_source_file",
  readRevision: "read_repository_source_revision",
  drafts: "list_repository_source_drafts",
  readDraft: "read_repository_source_draft",
  saveDraft: "save_repository_source_draft",
  discardDraft: "discard_repository_source_draft",
  watch: "watch_repository_source",
  unwatch: "unwatch_repository_source",
};

function sourceProblem(result: DocumentOpen, noun: "note" | "file") {
  return result.status === "unsupported_encoding"
    ? `This ${noun} uses an unsupported text encoding.`
    : `The ${noun} is missing from this checkout.`;
}

function loaded(document: RepositoryDocument, draftRevision: string | null): RepositoryBuffer {
  return {
    base: document.revision, fileIdentity: document.fileIdentity,
    revision: document.revision.sha256.slice(0, 12), content: document.markdown, saved: document.markdown, draftRevision,
    format: document.lineEndings === undefined ? undefined : { utf8Bom: Boolean(document.utf8Bom), lineEndings: document.lineEndings },
  };
}

export function useRepositoryNotes(checkoutId: string, enabled: boolean) {
  return useRepositoryFileBuffers<NoteCatalogEntry>(checkoutId, enabled, noteCommands, "note");
}

export function useRepositorySourceBuffers(checkoutId: string, enabled: boolean) {
  const { notes: files, getNote: getFile, ...buffers } =
    useRepositoryFileBuffers<string>(checkoutId, enabled, sourceCommands, "file");
  return { ...buffers, files, getFile };
}

function useRepositoryFileBuffers<CatalogEntry>(
  checkoutId: string, enabled: boolean, commands: typeof noteCommands, noun: "note" | "file",
) {
  const [state, setState] = useState<NoteState>({ notes: {}, failures: {} });
  const [catalog, setCatalog] = useState<CatalogEntry[]>([]);
  const [draftPaths, setDraftPaths] = useState<string[]>([]);
  const [error, setError] = useState("");
  const current = useRef(state);
  const mounted = useRef(false);
  const catalogRequest = useRef(0);
  const requests = useRef(new Map<string, Promise<string | undefined>>()).current;
  const writes = useRef(new Map<string, Promise<unknown>>()).current;
  const watches = useRef(new Map<string, Watch>()).current;

  const publish = useCallback((next: NoteState) => {
    current.current = next;
    if (mounted.current) setState(next);
  }, []);
  const change = useCallback((path: string, amend: (note: RepositoryBuffer) => RepositoryBuffer) => {
    const { notes, failures } = current.current;
    if (notes[path]) publish({ notes: { ...notes, [path]: amend(notes[path]) }, failures });
  }, [publish]);
  const report = useCallback((failure: unknown) => {
    if (mounted.current) setError(String(failure));
  }, []);
  const refreshCatalog = useCallback(async () => {
    const request = ++catalogRequest.current;
    const entries = await invoke<CatalogEntry[]>(commands.catalog, { checkoutId });
    if (mounted.current && request === catalogRequest.current) setCatalog(entries);
  }, [checkoutId, commands]);
  const serialize = useCallback(<T,>(path: string, operation: () => Promise<T>) => {
    const next = (writes.get(path) ?? Promise.resolve()).catch(() => undefined).then(operation);
    writes.set(path, next);
    const release = () => { if (writes.get(path) === next) writes.delete(path); };
    void next.then(release, release);
    return next;
  }, [writes]);

  const stopWatching = useCallback((path: string) => {
    const record = watches.get(path);
    watches.delete(path);
    if (record?.id) void invoke(commands.unwatch, { watchId: record.id }).catch(report);
  }, [commands, report, watches]);

  useEffect(() => {
    mounted.current = true;
    if (enabled) {
      void refreshCatalog().catch(report);
      void invoke<string[]>(commands.drafts, { checkoutId })
        .then((paths) => { if (mounted.current) setDraftPaths(paths); }, report);
    }
    return () => {
      mounted.current = false;
      catalogRequest.current += 1;
      for (const path of watches.keys()) stopWatching(path);
    };
  }, [checkoutId, commands, enabled, refreshCatalog, report, stopWatching, watches]);

  const load = useCallback((path: string): Promise<string | undefined> => {
    if (current.current.notes[path]) return Promise.resolve(path);
    const pending = requests.get(path);
    if (pending) return pending;
    const request = (async () => {
      const result = await invoke<DocumentOpen>(commands.open, { checkoutId, path });
      const canonical = result.status === "document" ? result.document.revision.relativePath : result.relativePath ?? path;
      const requestedDraft = canonical !== path
        ? await invoke<SavedDraft | null>(commands.readDraft, { checkoutId, path })
        : null;
      const owner = requestedDraft ? path : canonical;
      const draft = requestedDraft ?? await invoke<SavedDraft | null>(commands.readDraft, { checkoutId, path: owner });
      if (current.current.notes[owner]) return owner;
      let buffer: RepositoryBuffer;
      if (draft) {
        const disk = !requestedDraft && result.status === "document" ? result.document : undefined;
        if (disk && disk.markdown === draft.markdown) {
          const discarded = await invoke<boolean>(commands.discardDraft, {
            checkoutId, path: owner, expectedDraftRevision: draft.revision,
          });
          if (discarded) {
            publish({ ...current.current, notes: { ...current.current.notes, [owner]: loaded(disk, null) } });
            return owner;
          }
        }
        const sameVersion = disk?.revision.sha256 === draft.base.sha256;
        const base = sameVersion ? { markdown: disk!.markdown } : await invoke<{ markdown: string } | null>(
          commands.readRevision, { checkoutId, revisionId: draft.base.id },
        );
        if (!base) throw new Error(`The saved base of the recovered ${noun} is unavailable.`);
        buffer = {
          base: draft.base, fileIdentity: sameVersion ? disk?.fileIdentity : undefined,
          saved: normalizeEditorText(base.markdown), content: draft.markdown, revision: draft.base.sha256.slice(0, 12),
          draftRevision: draft.revision,
          ...(!sameVersion && disk ? { external: disk.markdown, incoming: disk } : {}),
          ...(requestedDraft ? { sourceProblem: `The path now refers to a different ${noun}.` }
            : result.status !== "document" ? { sourceProblem: sourceProblem(result, noun) } : {}),
        };
      } else if (result.status === "document") {
        buffer = loaded(result.document, null);
      } else {
        publish({ ...current.current, failures: { ...current.current.failures, [path]: sourceProblem(result, noun) } });
        return canonical;
      }
      publish({ ...current.current, notes: { ...current.current.notes, [owner]: buffer } });
      return owner;
    })().catch((failure: unknown) => {
      publish({ ...current.current, failures: { ...current.current.failures, [path]: String(failure) } });
      return path;
    });
    requests.set(path, request);
    void request.then(() => requests.delete(path));
    return request;
  }, [checkoutId, commands, noun, publish, requests]);

  const readExternal = useCallback((path: string) => serialize(path, async () => {
    const result = await invoke<DocumentOpen>(commands.open, { checkoutId, path });
    change(path, (note) => {
      if (result.status !== "document") return { ...note, sourceProblem: sourceProblem(result, noun), external: undefined, incoming: undefined };
      const disk = result.document;
      if (disk.revision.relativePath !== path) {
        return { ...note, sourceProblem: `The path now refers to a different ${noun}.`, external: undefined, incoming: undefined };
      }
      if (disk.revision.sha256 === note.base.sha256 && (!note.fileIdentity || disk.fileIdentity === note.fileIdentity)) {
        return { ...note, external: undefined, incoming: undefined, sourceProblem: undefined };
      }
      if (note.content === note.saved && note.draftRevision === null) return loaded(disk, null);
      return { ...note, external: disk.markdown, incoming: disk, sourceProblem: undefined };
    });
  }), [change, checkoutId, commands, noun, serialize]);

  const watch = useCallback((path: string) => {
    const note = current.current.notes[path];
    if (!note || watches.has(path) || !mounted.current) return;
    const record: Watch = {};
    const channel = new Channel<{ checkoutId: string; relativePath: string }>();
    channel.onmessage = (event) => {
      if (watches.get(path) === record && event.checkoutId === checkoutId && event.relativePath === path) {
        void readExternal(path).catch(report);
      }
    };
    watches.set(path, record);
    void invoke<string>(commands.watch, { checkoutId, baseRevision: note.base.id, channel })
      .then((id) => {
        if (watches.get(path) !== record) return invoke(commands.unwatch, { watchId: id });
        record.id = id;
        return readExternal(path);
      })
      .catch((failure: unknown) => {
        if (watches.get(path) === record) {
          stopWatching(path);
          report(failure);
        }
      });
  }, [checkoutId, commands, readExternal, report, stopWatching, watches]);

  const persist = useCallback(async (path: string) => {
    const note = current.current.notes[path];
    if (!note) return;
    if (note.content === note.saved && !note.incoming && !note.sourceProblem) {
      if (note.draftRevision) {
        const discarded = await invoke<boolean>(commands.discardDraft, {
          checkoutId, path, expectedDraftRevision: note.draftRevision,
        });
        if (!discarded) throw new Error(`The recovered draft changed. Reopen the ${noun} before replacing it.`);
        change(path, (current) => ({ ...current, draftRevision: null }));
      }
      return;
    }
    const draft = await invoke<SavedDraft>(commands.saveDraft, {
      checkoutId, baseRevision: note.base.id, expectedDraftRevision: note.draftRevision, markdown: note.content,
    });
    change(path, (current) => ({ ...current, draftRevision: draft.revision }));
  }, [change, checkoutId, commands, noun]);

  const update = useCallback((path: string, content: string) => {
    change(path, (note) => ({ ...note, content }));
    void serialize(path, () => persist(path)).catch(report);
  }, [change, persist, report, serialize]);

  const loadLocalRevision = useCallback((path: string, content: string) => serialize(path, async () => {
    const previous = current.current.notes[path];
    if (!previous || previous.sourceProblem) throw new Error(`Open an available ${noun} before loading a revision.`);
    if (previous.content !== previous.saved || previous.incoming || previous.draftRevision) {
      throw new Error(`Save or discard the current ${noun} draft before loading a revision.`);
    }
    if (previous.content === content) return;
    const applied = { ...previous, content };
    change(path, () => applied);
    try { await persist(path); }
    catch (failure) {
      change(path, (note) => note === applied ? previous : note);
      throw failure;
    }
  }), [change, noun, persist, serialize]);

  const save = useCallback((path: string) => serialize(path, async () => {
    const note = current.current.notes[path];
    if (!note) return false;
    if (note.incoming || note.sourceProblem) return false;
    if (note.content === note.saved) return true;
    await persist(path);
    const result = await invoke<DocumentSave>(commands.save, {
      checkoutId, path, expectedRevision: note.base.id, markdown: note.content,
    });
    if (result.status !== "saved") {
      change(path, (current) => ({ ...current, recovery: result.recovery }));
      if (result.status === "conflict") {
        const observed = await invoke<DocumentOpen>(commands.open, { checkoutId, path });
        change(path, (current) => {
          if (observed.status !== "document") return { ...current, sourceProblem: sourceProblem(observed, noun) };
          if (observed.document.revision.relativePath !== path) {
            return { ...current, sourceProblem: `The path now refers to a different ${noun}.` };
          }
          return { ...current, incoming: observed.document, external: observed.document.markdown };
        });
      }
      report([result.recovery.message, result.recovery.editedPath, result.recovery.displacedPath].filter(Boolean).join(" · "));
      return false;
    }
    change(path, (current) => ({ ...loaded(result.document, current.draftRevision), content: current.content }));
    await persist(path);
    return current.current.notes[path]?.content === current.current.notes[path]?.saved;
  }).catch((failure: unknown) => { report(failure); return false; }), [change, checkoutId, commands, noun, persist, report, serialize]);

  const resolve = useCallback((path: string, keepLocal: boolean) => serialize(path, async () => {
    const note = current.current.notes[path];
    if (!note || (keepLocal && note.sourceProblem)) return;
    if (!keepLocal && note.sourceProblem) {
      if (note.draftRevision) {
        const discarded = await invoke<boolean>(commands.discardDraft, {
          checkoutId, path, expectedDraftRevision: note.draftRevision,
        });
        if (!discarded) throw new Error(`The recovered draft changed. Reopen the ${noun} before replacing it.`);
      }
      change(path, (current) => ({
        ...current, content: current.content === note.content ? current.saved : current.content, draftRevision: null,
      }));
      return;
    }
    change(path, (note) => {
      if (note.incoming) return {
        ...loaded(note.incoming, note.draftRevision), recovery: note.recovery,
        content: keepLocal ? note.content : note.incoming.markdown,
      };
      return keepLocal ? note : { ...note, content: note.saved, external: undefined };
    });
    await persist(path);
  }), [change, checkoutId, commands, noun, persist, serialize]);

  const getNote = useCallback((path: string) => current.current.notes[path], []);
  const forget = useCallback((path: string) => {
    stopWatching(path);
    const notes = { ...current.current.notes };
    const failures = { ...current.current.failures };
    delete notes[path];
    delete failures[path];
    publish({ notes, failures });
  }, [publish, stopWatching]);
  return { ...state, catalog, refreshCatalog, draftPaths, error, clearError: () => setError(""), load, save, update, resolve, watch, stopWatching, getNote, forget, loadLocalRevision };
}
