import {
  AlertCircle, BookOpen, BookOpenText, FilePenLine, FileText, GitFork, Link2,
  ListTree, Network, PanelRight, Paperclip, RotateCw, Save, Search, SplitSquareHorizontal, Wand2,
} from "lucide-react";
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  FileDialog, FileDock, FileSearch, FileSidebar, FileTree, FileUnavailable,
  keepOnlyCleanClose, SidebarToggle, useFileDock, useFileSidebar, type FilePanelApi,
} from "@/ide-file-workspace";
import { deliveryNote, exampleNotes, fileName, findFileText, normalizeRepositoryPath } from "@/ide-knowledge-data";
import {
  anchorLine, getProperties, headings, repairWikiAddress, resolveWiki, rewriteNoteReferences,
  updateProperty, wikiLinks, type NoteLocation, type NoteTarget,
} from "@/knowledge-document";
import { AttachmentPreview, noteAttachments, type NoteAttachment } from "@/knowledge-media";
import type { NoteJump } from "@/knowledge-editor";
import { useRepositoryNotes, type NoteBuffer } from "@/repository-note-buffers";
import { noteAttachmentCandidates, RepositoryNoteMedia } from "@/repository-note-media";
import { NotePropertyEditor } from "@/note-property-editor";
import { KnowledgeSearch } from "@/knowledge-navigation";
import { useRepositoryNoteIndex } from "@/repository-note-index";
import type { WorkspaceProps } from "@/workbench-contract";

const Editor = lazy(() => import("@/knowledge-editor").then(({ MarkdownEditor }) => ({ default: MarkdownEditor })));
const Reading = lazy(() => import("@/knowledge-reading").then(({ NoteReading }) => ({ default: NoteReading })));
const NoteGraph = lazy(() => import("@/knowledge-graph"));

type NoteMode = "source" | "live" | "reading";
type Inspector = "outline" | "links" | "properties" | "attachments";

interface Note {
  content: string;
  saved: string;
  revision: number;
  external?: string;
}

interface RewrittenNote {
  path: string;
  before: string;
  after: string;
}

type KnowledgeDialog =
  | { kind: "close"; path: string; api: FilePanelApi }
  | { kind: "conflict"; path: string; external: string }
  | { kind: "rename"; path: string }
  | { kind: "rewrite"; path: string; destination: string; edits: RewrittenNote[] }
  | { kind: "link"; from: string; link: NoteTarget; matches: NoteLocation[] }
  | { kind: "repositoryAttachment"; paths: string[]; label: string }
  | { kind: "attachment"; attachment: NoteAttachment };

function initialNotes(simulated: boolean, recovery: boolean): Record<string, Note> {
  return Object.fromEntries((simulated ? exampleNotes : []).map(({ path, content }) => [path, {
    content: recovery && path === deliveryNote ? content + "\n\n## Recovered buffer\n\nConfirm startup after an interrupted launch.\n" : content,
    saved: content,
    revision: 1,
  }]));
}

function isDirty(note?: NoteBuffer) {
  return Boolean(note && (note.external !== undefined || note.content !== note.saved));
}

export default function KnowledgeWorkspace(props: WorkspaceProps) {
  return <KnowledgeSurface key={`${props.checkout.checkoutId}:${props.simulation?.epoch ?? "real"}`} {...props} />;
}

function KnowledgeSurface({ checkout, target, simulation, startSimulation, openWorkspace }: WorkspaceProps) {
  const simulated = Boolean(simulation);
  const initialPath = target ? normalizeRepositoryPath(target) : simulated ? deliveryNote : undefined;
  const dock = useFileDock({ checkoutId: checkout.checkoutId, workspace: "knowledge", simulated, initialPath: simulated ? initialPath : undefined });
  const [examples, setNotes] = useState(() => initialNotes(simulated, simulation?.scenario === "recovery"));
  const repository = useRepositoryNotes(checkout.checkoutId, !simulated);
  const notes = simulated ? examples : repository.notes;
  const [sidebar, setSidebar] = useFileSidebar();
  const [display, setDisplay] = useState<"tree" | "search">("tree");
  const [query, setQuery] = useState("");
  const noteIndex = useRepositoryNoteIndex(checkout.checkoutId, !simulated, query);
  const [graph, setGraph] = useState(false);
  const [inspector, setInspector] = useState<Inspector | null>(window.innerWidth >= 1180 ? "outline" : null);
  const [modes, setModes] = useState<Record<string, NoteMode>>({});
  const [jumps, setJumps] = useState<Record<string, NoteJump>>({});
  const [dialog, setDialog] = useState<KnowledgeDialog>();
  const [destination, setDestination] = useState("");
  const [status, setStatus] = useState(simulation?.scenario === "recovery" ? "Recovered an unsaved Markdown buffer." : "");
  const [problem, setProblem] = useState("");
  const [selection, setSelection] = useState("");
  const [selectedAnchor, setSelectedAnchor] = useState("");
  const lastTarget = useRef(simulated ? initialPath : undefined);
  const lastOpen = useRef(0);
  const { load: loadNote, watch: watchNote, getNote } = repository;
  const active = dock.path;
  const note = active ? notes[active] : undefined;
  const conflictRecovery = dialog?.kind === "conflict" ? repository.notes[dialog.path]?.recovery : undefined;
  const source = note?.content ?? "";
  const activeMode = active ? modes[active] ?? "live" : "live";
  const catalog = useMemo(() => {
    const indexed = new Map((noteIndex.index.data ?? repository.catalog).map(({ path, frontmatter }) => [path, frontmatter ?? ""]));
    for (const path of repository.draftPaths) if (!indexed.has(path)) indexed.set(path, "");
    for (const [path, value] of Object.entries(notes)) indexed.set(path, value.content);
    return [...indexed].map(([path, content]) => ({ path, content }));
  }, [notes, noteIndex.index.data, repository.catalog, repository.draftPaths]);
  const matches = simulated ? findFileText(catalog, query, true) : [];
  const outgoing = wikiLinks(source);
  const backlinks = active ? catalog.flatMap(({ path, content }) => {
    if (path === active) return [];
    const inbound = wikiLinks(content).filter((link) => {
      const resolved = resolveWiki(link, path, catalog);
      return resolved.length === 1 && resolved[0]?.path === active;
    });
    return inbound.length ? [{ path, count: inbound.length }] : [];
  }) : [];

  const open = useCallback((path: string, line?: number, anchor?: string) => {
    const focus = (file: string) => {
      if (line || anchor) setJumps((before) => ({ ...before, [file]: { line, anchor, token: (before[file]?.token ?? 0) + 1 } }));
      setGraph(false);
      dock.open(file);
    };
    if (simulated) { focus(path); return; }
    const request = ++lastOpen.current;
    void loadNote(path).then((file) => {
      if (!file || request !== lastOpen.current) return;
      const anchorTarget = !line && anchor ? anchorLine(getNote(file)?.content ?? "", anchor) : undefined;
      if (anchorTarget) setJumps((before) => ({ ...before, [file]: { line: anchorTarget, anchor, token: (before[file]?.token ?? 0) + 1 } }));
      else focus(file);
      if (anchorTarget) { setGraph(false); dock.open(file); }
      watchNote(file);
    });
  }, [dock.open, getNote, loadNote, simulated, watchNote]);

  useEffect(() => {
    if (simulated || !dock.ready || !active) return;
    let current = true;
    void loadNote(active).then((file) => {
      if (!current || !file) return;
      if (file !== active) {
        dock.open(file);
        dock.api?.getPanel(`editor:${active}`)?.api.close();
      }
      watchNote(file);
    });
    return () => { current = false; };
  }, [active, dock.api, dock.open, dock.ready, loadNote, simulated, watchNote]);

  useEffect(() => {
    if (simulated || !active || !note) return;
    const visitedDepth = new Map<string, number>();
    const inspect = (path: string, content: string, depth: number) => {
      if (depth >= 3 || (visitedDepth.get(path) ?? Infinity) <= depth) return;
      visitedDepth.set(path, depth);
      for (const embed of wikiLinks(content)) {
        if (!embed.embed) continue;
        const targets = resolveWiki(embed, path, catalog);
        const target = targets.length === 1 ? targets[0] : undefined;
        if (!target || (visitedDepth.get(target.path) ?? Infinity) <= depth + 1) continue;
        const buffer = notes[target.path];
        if (buffer) inspect(target.path, buffer.content, depth + 1);
        else if (!repository.failures[target.path]) void loadNote(target.path);
      }
    };
    inspect(active, note.content, 0);
  }, [active, catalog, loadNote, note, notes, repository.failures, simulated]);

  useEffect(() => {
    if (!target || !dock.ready) return;
    const next = normalizeRepositoryPath(target);
    if (next && next !== lastTarget.current) {
      lastTarget.current = next;
      open(next);
    }
  }, [dock.ready, open, target]);

  const update = (path: string, content: string) => {
    if (!simulated) { repository.update(path, content); return; }
    setNotes((before) => {
      const current = before[path];
      return current && current.content !== content ? { ...before, [path]: { ...current, content } } : before;
    });
  };
  const save = async (path: string) => {
    const external = notes[path]?.external;
    if (external !== undefined) {
      setDialog({ kind: "conflict", path, external });
      return false;
    }
    if (!simulated) {
      const saved = await repository.save(path);
      if (saved) setStatus(`Saved ${path}`);
      else {
        const incoming = getNote(path)?.external;
        if (incoming !== undefined) setDialog({ kind: "conflict", path, external: incoming });
      }
      return saved;
    }
    setNotes((before) => {
      const current = before[path];
      if (!current || !isDirty(current)) return before;
      return { ...before, [path]: { ...current, saved: current.content, revision: current.revision + 1 } };
    });
    setStatus(`Saved ${path}`);
    return true;
  };
  const discard = async (path: string) => {
    if (!simulated) return repository.resolve(path, false);
    const entry = examples[path];
    if (!entry) return;
    const content = entry.external ?? entry.saved;
    setNotes((before) => ({ ...before, [path]: { content, saved: content, revision: entry.revision + Number(entry.external !== undefined) } }));
  };
  const closePanel = (path: string, api: FilePanelApi) => {
    api.close();
    if (!simulated && !dock.api?.panels.some((panel) => (panel.params as { path?: string } | undefined)?.path === path)) {
      repository.stopWatching(path);
    }
  };
  const close = (path: string, api: FilePanelApi) => {
    if (!isDirty(notes[path]) || !dock.api || keepOnlyCleanClose(dock.api, path, api)) closePanel(path, api);
    else setDialog({ kind: "close", path, api });
  };
  const closeDialog = () => { setDialog(undefined); setProblem(""); };

  const navigateLocation = ({ path, anchor }: NoteLocation) => {
    const targetSource = notes[path]?.content ?? "";
    open(path, anchor ? anchorLine(targetSource, anchor) : undefined, anchor);
  };
  const navigateLink = (from: string, link: NoteTarget) => {
    from = link.sourcePath ?? from;
    const attachment = simulated ? noteAttachments.find(({ name }) => link.target === name) : undefined;
    if (attachment) { setDialog({ kind: "attachment", attachment }); return; }
    if (!simulated) {
      const paths = noteAttachmentCandidates(link.target, from, true);
      if (paths.length) { setDialog({ kind: "repositoryAttachment", paths, label: link.alias ?? link.target }); return; }
    }
    const targets = resolveWiki(link, from, catalog);
    const only = targets[0];
    if (targets.length === 1 && only && (!simulated || !only.anchor || anchorLine(notes[only.path]?.content ?? "", only.anchor))) {
      navigateLocation(only);
      return;
    }
    setSelection(only?.path ?? "");
    setSelectedAnchor(only && link.anchor && anchorLine(notes[only.path]?.content ?? "", link.anchor) ? link.anchor : "");
    setDialog({ kind: "link", from, link, matches: targets });
  };

  const previewRename = (path: string) => {
    const to = normalizeRepositoryPath(destination.trim());
    if (!to?.toLowerCase().endsWith(".md")) { setProblem("Enter a relative Markdown path ending in .md."); return; }
    if (catalog.some((file) => file.path.toLowerCase() === to.toLowerCase())) { setProblem("A note already uses that path."); return; }
    const edits = catalog.flatMap((file) => {
      const after = rewriteNoteReferences(file.content, file.path, path, to, catalog);
      return after === file.content ? [] : [{ path: file.path, before: file.content, after }];
    });
    setProblem("");
    setDialog({ kind: "rewrite", path, destination: to, edits });
  };
  const finishRename = (proposal: Extract<KnowledgeDialog, { kind: "rewrite" }>) => {
    const moved = examples[proposal.path];
    if (!moved) return;
    const changed = { ...examples };
    const skipped: string[] = [];
    for (const edit of proposal.edits) {
      const current = changed[edit.path];
      if (!current || current.external !== undefined || current.content !== edit.before) { skipped.push(edit.path); continue; }
      changed[edit.path] = { content: edit.after, saved: edit.after, revision: current.revision + 1 };
    }
    if (moved.external !== undefined || skipped.includes(proposal.path)) {
      setProblem("The source note changed. Close and review a fresh rename preview.");
      return;
    }
    const updated = changed[proposal.path] ?? moved;
    delete changed[proposal.path];
    changed[proposal.destination] = updated;
    setNotes(changed);
    for (const id of [`editor:${proposal.path}`, `reading:${proposal.path}`, `split:${proposal.path}`]) dock.api?.getPanel(id)?.api.close();
    open(proposal.destination);
    setStatus(skipped.length ? `Renamed ${proposal.destination}. ${skipped.length} link rewrites skipped after an external change.`
      : `Renamed ${proposal.destination} and rewrote links in ${proposal.edits.length} notes.`);
    closeDialog();
  };

  return (
    <section className="file-workspace knowledge-workspace" aria-label="Knowledge workspace" onKeyDown={(event) => {
      if (!event.ctrlKey && !event.metaKey) return;
      if (event.key.toLowerCase() === "s" && active && note) { event.preventDefault(); save(active); }
      if (event.key.toLowerCase() === "p") { event.preventDefault(); setSidebar(true); setDisplay("search"); }
    }}>
      {sidebar && <FileSidebar name="Knowledge navigation">
        <div className="file-sidebar-heading">
          <BookOpen size={16} /><h2>Notes</h2>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" aria-label="Show notes" title="Show notes" aria-pressed={display === "tree"} onClick={() => setDisplay("tree")}><ListTree size={15} /></button>
            <button type="button" className="fw-icon" aria-label="Search notes" title="Search notes" aria-pressed={display === "search"} onClick={() => setDisplay("search")}><Search size={15} /></button>
          </div>
        </div>
        <div className="file-sidebar-scroll">
          {display === "search" ? simulated ? <FileSearch query={query} setQuery={setQuery} results={matches} unavailable={false}
            placeholder="Find notes and content" onOpen={open} />
            : <KnowledgeSearch query={query} onQueryChange={setQuery} results={noteIndex.hits} pending={noteIndex.searching}
              error={noteIndex.error ? String(noteIndex.error) : undefined} onOpen={({ path, line, anchor }) => open(path, line ?? undefined, anchor ?? undefined)} />
            : <FileTree paths={catalog.map(({ path }) => path)} active={active} onOpen={open} />}
        </div>
      </FileSidebar>}
      <div className="file-workspace-main">
        <header className="file-workspace-header">
          <SidebarToggle open={sidebar} setOpen={setSidebar} label="Toggle Knowledge navigation" />
          <BookOpenText size={17} /><h1>Knowledge</h1>
          <div className="file-toolbar">
            {!simulated && <button type="button" className="fw-icon" title="Rebuild note index" aria-label="Rebuild note index"
              disabled={noteIndex.index.isFetching} onClick={() => { void noteIndex.index.refetch(); }}><RotateCw size={15} /></button>}
            <button type="button" className="fw-btn note-graph-toggle" aria-pressed={graph} onClick={() => setGraph(!graph)}><Network size={15} />Graph</button>
            <button type="button" className="fw-icon" title="Note details" aria-label="Note details" aria-expanded={Boolean(inspector)} onClick={() => setInspector(inspector ? null : "outline")}><PanelRight size={16} /></button>
          </div>
        </header>
        {dock.error && <p role="alert" className="file-status"><AlertCircle size={15} />{dock.error}</p>}
        {repository.error && <p role="alert" className="file-status"><AlertCircle size={15} />{repository.error}</p>}
        {noteIndex.watchError && <p role="alert" className="file-status"><AlertCircle size={15} />{noteIndex.watchError}</p>}
        {status && <p role="status" className="file-status">{status}</p>}
        {!graph && <div className="note-mode-bar">
          <div className="note-modes" aria-label="Note display modes">
            {([["source", "Source"], ["live", "Live Preview"], ["reading", "Reading"]] as const).map(([mode, label]) => (
              <button key={mode} type="button" className="fw-btn" disabled={!note} aria-pressed={activeMode === mode} onClick={() => {
                if (active) {
                  dock.open(active);
                  setModes((before) => ({ ...before, [active]: mode }));
                }
              }}>{label}</button>
            ))}
          </div>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" title="Save note (Ctrl+S)" aria-label="Save note" disabled={!isDirty(note)}
              onClick={() => { if (active) save(active); }}><Save size={15} /></button>
            <button type="button" className="fw-icon" title="Split Reading view" aria-label="Split Reading view" disabled={!note}
              onClick={() => { if (active) { setModes((before) => before[active] === "reading" ? { ...before, [active]: "live" } : before); dock.split(active, "reading"); } }}><SplitSquareHorizontal size={15} /></button>
            {simulated && <button type="button" className="fw-icon" title="Rename note" aria-label="Rename note" disabled={!note}
              onClick={() => { if (active) { setDestination(active); setDialog({ kind: "rename", path: active }); } }}><FilePenLine size={15} /></button>}
          </div>
        </div>}
        <div className="note-body">
          <div className="file-dock-body" hidden={graph}>
            <FileDock onReady={dock.onReady} context={{
              renderFile: (path, view) => {
                const entry = notes[path];
                if (!entry) {
                  const failure = repository.failures[path];
                  return failure || simulated
                    ? <FileUnavailable title="Note unavailable" detail={failure ?? path} />
                    : <p role="status" className="file-help">Loading {path}…</p>;
                }
                const selectedMode = view === "reading" ? "reading" : modes[path] ?? "live";
                const onLink = (link: NoteTarget) => navigateLink(path, link);
                const sourceProblem = repository.notes[path]?.sourceProblem;
                return (
                  <div className="note-file-view">
                    <div className="ide-file-caption">
                      <span>{path.replaceAll("/", " / ")}</span>
                      <span>{isDirty(entry) ? "Unsaved · " : ""}{simulated ? `r${entry.revision}` : entry.revision}</span>
                    </div>
                    {sourceProblem && <p role="alert" className="file-help">{sourceProblem}</p>}
                    {!simulated && entry.external !== undefined && <button type="button" className="fw-btn fw-outlined"
                      onClick={() => setDialog({ kind: "conflict", path, external: entry.external! })}>Review external change</button>}
                    <Suspense fallback={<p role="status" className="file-help">Loading note view…</p>}>
                      {view === "editor" && <div className="note-editable-view" hidden={selectedMode === "reading"}>
                        <Editor mode={selectedMode === "source" ? "source" : "live"} path={path} source={entry.content} files={catalog} jump={jumps[path]} analyze onChange={(content) => update(path, content)} onLink={onLink} />
                      </div>}
                      {selectedMode === "reading" && <Reading path={path} content={entry.content} files={catalog} jump={jumps[path]} onLink={onLink}
                        checkoutId={simulated ? undefined : checkout.checkoutId} sampleAttachments={simulated}
                        onAttachment={(attachment) => setDialog({ kind: "attachment", attachment })}
                        onRepositoryAttachment={(paths, label) => setDialog({ kind: "repositoryAttachment", paths, label })} />}
                    </Suspense>
                  </div>
                );
              },
              empty: <FileUnavailable title="Open a note"
                detail="Choose a note from the tree or search by title and content."
                onSimulate={simulated ? undefined : () => startSimulation()} />,
              dirty: (path) => isDirty(notes[path]),
              requestClose: close,
            }} />
          </div>
          {graph && <div className="note-graph-host">
            {simulated ? <Suspense fallback={<p role="status" className="file-help">Loading note graph…</p>}>
              <NoteGraph files={catalog} selected={active} onOpen={open} />
            </Suspense> : <FileUnavailable title="Graph integration pending" detail="The graph will use indexed notes from the active checkout." onSimulate={() => startSimulation()} />}
          </div>}
          {inspector && !graph && <aside className="note-inspector" aria-label="Note details">
            <div className="note-inspector-views">
              {([[ListTree, "outline", "Outline"], [Link2, "links", "Links"], [FileText, "properties", "Properties"], [Paperclip, "attachments", "Attachments"]] as const)
                .map(([Icon, view, label]) => <button type="button" key={view} className="fw-icon" aria-label={label} aria-pressed={view === inspector} title={label}
                  onClick={() => setInspector(view)}><Icon size={16} /></button>)}
            </div>
            <div className="note-inspector-content">
              {!active || !note ? <p className="file-help">Open a note to see its details.</p> : <>
                {inspector === "outline" && <section aria-label="Outline">
                  <h3>Outline</h3>
                  {headings(source).map((entry) => <button type="button" key={entry.line} title={entry.label}
                    style={{ paddingLeft: Math.min(entry.level - 1, 4) * 10 + 5 }} onClick={() => open(active, entry.line, entry.anchor)}>{entry.label}</button>)}
                </section>}
                {inspector === "links" && <>
                  <section aria-label="Outgoing links"><h3>Outgoing links</h3>
                    {outgoing.length ? outgoing.map((link, index) => {
                      const found = resolveWiki(link, active, catalog);
                      return <button type="button" key={`${link.raw}:${index}`} onClick={() => navigateLink(active, link)}>
                        {link.alias ?? link.target} {found.length === 1 ? "" : found.length ? " · Ambiguous" : " · Missing"}
                      </button>;
                    }) : <p className="file-help">No outgoing links</p>}
                  </section>
                  <section aria-label="Backlinks"><h3>Backlinks</h3>
                    {backlinks.length ? backlinks.map(({ path, count }) => <button type="button" key={path} onClick={() => open(path)} title={path}>{fileName(path)} · {count}</button>)
                      : <p className="file-help">No backlinks</p>}
                  </section>
                </>}
                {inspector === "properties" && <section aria-label="Properties"><h3>Properties</h3>
                  {getProperties(source).map(({ key, value }, index) => simulated
                    ? <label className="note-property-row" key={`${key}:${index}`}>{key}
                      <input value={value} onChange={(event) => update(active, updateProperty(source, key, event.target.value))} />
                    </label>
                    : <NotePropertyEditor key={`${active}:${key}:${index}`} name={key} value={value}
                      read={() => getNote(active)?.content} update={(content) => update(active, content)} />)}
                  {getProperties(source).length === 0 && <p className="file-help">This note has no frontmatter properties.</p>}
                </section>}
                {inspector === "attachments" && <section aria-label="Attachments"><h3>Attachments</h3>
                  {simulated && noteAttachments.map((attachment) => <button type="button" className="note-attachment-row" key={attachment.name} onClick={() => setDialog({ kind: "attachment", attachment })}>
                    <strong>{attachment.name}</strong><small>{attachment.detail}</small>
                  </button>)}
                  {!simulated && outgoing.flatMap((link, index) => noteAttachmentCandidates(link.target, active, true).length ? [
                    <button type="button" className="note-attachment-row" key={index} onClick={() => navigateLink(active, link)}>
                      {link.alias ?? link.target}
                    </button>,
                  ] : [])}
                </section>}
              </>}
            </div>
          </aside>}
        </div>

        {simulated && active && note && !graph && <footer className="ide-statusbar note-statusbar">
          <button type="button" className="fw-btn" onClick={() => {
            const example = examples[active];
            if (!example) return;
            const external = example.external ?? example.saved + "\n\n> External review added a new readiness note.\n";
            setNotes((before) => ({ ...before, [active]: { ...example, external } }));
            setStatus(`External changes need review in ${active}`);
            setDialog({ kind: "conflict", path: active, external });
          }}>{note.external === undefined ? "Simulate external change" : "Review external change"}</button>
          {active === deliveryNote && simulation?.delivery === "note" && <button type="button" className="fw-btn fw-primary" disabled={isDirty(note)}
            title={isDirty(note) ? "Save this note before planning from its revision" : "Start Plan Mode from the saved note"}
            onClick={() => { simulation.advanceDelivery("start"); openWorkspace("conversations", "plan"); }}><Wand2 size={14} />Plan from note</button>}
          {active === deliveryNote && simulation?.delivery !== "note" && <button type="button" className="fw-btn" onClick={() => openWorkspace("tasks", "release")}><GitFork size={14} />Open linked task</button>}
        </footer>}
      </div>

      {dialog?.kind === "close" && <FileDialog title="Save note before closing?" description={`${dialog.path} has unsaved changes.`} onClose={closeDialog}>
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn" onClick={closeDialog}>Cancel</button>
          <button type="button" className="fw-btn fw-destructive" onClick={() => { void discard(dialog.path).then(() => {
            closePanel(dialog.path, dialog.api); closeDialog();
          }, (failure: unknown) => setProblem(String(failure))); }}>Discard changes</button>
          <button type="button" className="fw-btn fw-primary" onClick={() => { void save(dialog.path).then((saved) => {
            if (saved) { closePanel(dialog.path, dialog.api); closeDialog(); }
          }); }}>Save and close</button>
        </div>
        {problem && <p role="alert">{problem}</p>}
      </FileDialog>}
      {dialog?.kind === "conflict" && <FileDialog title="Note revision conflict" description={`Review the local and external versions of ${dialog.path}.`} onClose={closeDialog}>
        <div className="file-dialog-comparison">
          <div><h3>Local buffer</h3><pre>{notes[dialog.path]?.content}</pre></div>
          <div><h3>External version</h3><pre>{dialog.external}</pre></div>
        </div>
        {conflictRecovery && <div className="grid gap-1" aria-label="Recovery files">
          <p>{conflictRecovery.message}</p>
          {conflictRecovery.editedPath && <p>Edited buffer <code className="break-all">{conflictRecovery.editedPath}</code></p>}
          {conflictRecovery.displacedPath && <p>Displaced file <code className="break-all">{conflictRecovery.displacedPath}</code></p>}
        </div>}
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn" onClick={closeDialog}>Review later</button>
          <button type="button" className="fw-btn fw-outlined" onClick={() => {
            const { path, external } = dialog;
            if (!simulated) {
              void repository.resolve(path, false).then(closeDialog, (failure: unknown) => setProblem(String(failure)));
              return;
            }
            setNotes((before) => ({ ...before, [path]: { content: external, saved: external, revision: (before[path]?.revision ?? 0) + 1 } }));
            setStatus(`Loaded external revision of ${path}`);
            closeDialog();
          }}>Load external version</button>
          <button type="button" className="fw-btn fw-primary" onClick={() => {
            const { path, external } = dialog;
            if (!simulated) {
              void repository.resolve(path, true).then(closeDialog, (failure: unknown) => setProblem(String(failure)));
              return;
            }
            setNotes((before) => {
              const current = before[path];
              return current ? { ...before, [path]: { ...current, external: undefined, saved: external, revision: current.revision + 1 } } : before;
            });
            setStatus(`Kept local edits for reconciliation with ${path}`);
            closeDialog();
          }}>Keep local edits</button>
        </div>
        {problem && <p role="alert">{problem}</p>}
      </FileDialog>}
      {dialog?.kind === "rename" && <FileDialog title="Rename note" description={`Choose a new repository path for ${dialog.path}.`} onClose={closeDialog}>
        <label>New path<input value={destination} onChange={(event) => setDestination(event.target.value)} /></label>
        {problem && <p role="alert">{problem}</p>}
        <div className="file-dialog-footer"><button type="button" className="fw-btn fw-primary" onClick={() => previewRename(dialog.path)}>Preview link rewrites</button></div>
      </FileDialog>}
      {dialog?.kind === "rewrite" && <FileDialog title="Review link rewrites" description={`${dialog.path} → ${dialog.destination}`} onClose={closeDialog}>
        <ul className="note-rewrite-list">{dialog.edits.map((edit) => <li key={edit.path}>
          <details><summary>{edit.path}</summary><div className="file-dialog-comparison"><div><h3>Current</h3><pre>{edit.before}</pre></div><div><h3>Proposed</h3><pre>{edit.after}</pre></div></div></details>
        </li>)}</ul>
        {dialog.edits.length === 0 && <p>No links need to change.</p>}
        {problem && <p role="alert">{problem}</p>}
        <div className="file-dialog-footer">
          {dialog.edits.some((edit) => edit.path !== dialog.path) && <button type="button" className="fw-btn" onClick={() => {
            const affected = dialog.edits.find((edit) => edit.path !== dialog.path);
            if (affected) update(affected.path, (notes[affected.path]?.content ?? "") + "\nExternal edit.\n");
          }}>Simulate concurrent edit</button>}
          <button type="button" className="fw-btn fw-primary" onClick={() => finishRename(dialog)}>Apply reviewed changes</button>
        </div>
      </FileDialog>}
      {dialog?.kind === "link" && <FileDialog title={dialog.matches.length > 1 ? "Choose a note" : "Repair note link"}
        description={`Resolve [[${dialog.link.raw}]] from ${dialog.from}.`} onClose={closeDialog}>
        <label>Target note<select value={selection} onChange={(event) => {
          const path = event.target.value;
          const originalAnchor = dialog.link.anchor;
          setSelection(path);
          setSelectedAnchor(originalAnchor && anchorLine(notes[path]?.content ?? "", originalAnchor) ? originalAnchor : "");
        }}>
          <option value="">Select a note</option>
          {(dialog.matches.length > 1 ? dialog.matches : catalog).map(({ path }) => <option key={path} value={path}>{path}</option>)}
        </select></label>
        {selection && <label>Heading or block<input value={selectedAnchor} placeholder="Open note without a specific location"
          onChange={(event) => setSelectedAnchor(event.target.value)} /></label>}
        {selectedAnchor && !anchorLine(notes[selection]?.content ?? "", selectedAnchor.trim()) && <p role="alert">The heading or block is unavailable in this note.</p>}
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn fw-outlined" disabled={!selection || Boolean(selectedAnchor && !anchorLine(notes[selection]?.content ?? "", selectedAnchor.trim()))}
            onClick={() => { navigateLocation({ path: selection, anchor: selectedAnchor.trim() || undefined }); closeDialog(); }}>Open note</button>
          <button type="button" className="fw-btn fw-primary" disabled={!selection || Boolean(selectedAnchor && !anchorLine(notes[selection]?.content ?? "", selectedAnchor.trim()))} onClick={() => {
            const current = notes[dialog.from];
            if (current) update(dialog.from, repairWikiAddress(current.content, dialog.link.raw, selection, selectedAnchor.trim() || undefined));
            setStatus(`Updated reference in ${dialog.from}`);
            closeDialog();
          }}>Update link</button>
        </div>
      </FileDialog>}
      {dialog?.kind === "attachment" && <FileDialog title={dialog.attachment.name} description={dialog.attachment.detail} onClose={closeDialog}>
        <AttachmentPreview attachment={dialog.attachment} />
      </FileDialog>}
      {dialog?.kind === "repositoryAttachment" && <FileDialog title={dialog.label} description="Repository attachment" onClose={closeDialog}>
        <RepositoryNoteMedia checkoutId={checkout.checkoutId} paths={dialog.paths} title={dialog.label} />
      </FileDialog>}
    </section>
  );
}
