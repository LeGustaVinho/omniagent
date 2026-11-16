import {
  AlertCircle, BookOpen, BookOpenText, FilePenLine, FileText, GitFork, Link2,
  ListTree, Network, PanelRight, Paperclip, RotateCw, Save, Search, SplitSquareHorizontal, Wand2,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  FileDialog, FileDock, FileSearch, FileSidebar, FileTree, FileUnavailable,
  keepOnlyCleanClose, SidebarToggle, useFileDock, useFileSidebar, type FilePanelApi,
} from "@/ide-file-workspace";
import { readNoteSearchTarget } from "@/ide-file-search";
import { deliveryNote, exampleNotes, fileName, findFileText, normalizeRepositoryPath } from "@/ide-knowledge-data";
import {
  anchorLine, getProperties, headings, parseReadingAddress, readingLocalAddress, repairWikiAddress, resolveWiki, rewriteNoteReferences,
  updateProperty, wikiLinks, type NoteLocation, type NoteTarget,
} from "@/knowledge-document";
import { AttachmentPreview, noteAttachments, type NoteAttachment } from "@/knowledge-media";
import type { NoteJump } from "@/knowledge-editor";
import { useRepositoryNotes, type NoteBuffer } from "@/repository-note-buffers";
import { RepositoryNoteMedia } from "@/repository-note-media";
import { NotePropertyEditor } from "@/note-property-editor";
import { KnowledgeLinks, KnowledgeOutline, KnowledgeSearch, type KnowledgeLocation, type KnowledgeOutgoingLink } from "@/knowledge-navigation";
import { useRepositoryNoteIndex } from "@/repository-note-index";
import { RepositoryNoteRename } from "@/repository-note-rename";
import { asNoteLocation, linkPreviewValue, resolveRepositoryLink, suggestRepositoryLinks, useRepositoryLinkPreviews,
  type IndexedLinkLocation, type IndexedLinkResolution, type IndexedNoteDetails, type IndexedNoteRelationships } from "@/repository-note-links";
import type { NoteGraphProjection } from "@/knowledge-graph";
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
  | { kind: "repositoryRename"; path: string; revision: string }
  | { kind: "rewrite"; path: string; destination: string; edits: RewrittenNote[] }
  | { kind: "link"; from: string; link: NoteTarget; matches: NoteLocation[]; sourceLine?: number; manual?: boolean }
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
  const initialTarget = target ? readNoteSearchTarget(target) : undefined;
  const initialPath = initialTarget ? normalizeRepositoryPath(initialTarget.path) : simulated ? deliveryNote : undefined;
  const dock = useFileDock({ checkoutId: checkout.checkoutId, workspace: "knowledge", simulated, initialPath: simulated ? initialPath : undefined });
  const [examples, setNotes] = useState(() => initialNotes(simulated, simulation?.scenario === "recovery"));
  const repository = useRepositoryNotes(checkout.checkoutId, !simulated);
  const notes = simulated ? examples : repository.notes;
  const [sidebar, setSidebar] = useFileSidebar();
  const [display, setDisplay] = useState<"tree" | "search">("tree");
  const [query, setQuery] = useState("");
  const [focusSearch, requestSearchFocus] = useState(0);
  const noteIndex = useRepositoryNoteIndex(checkout.checkoutId, !simulated, query);
  const [graph, setGraph] = useState(false);
  const graphEdges = useQuery({
    queryKey: ["repository-note-edges", checkout.checkoutId, noteIndex.index.dataUpdatedAt],
    enabled: !simulated && graph && noteIndex.index.isSuccess,
    placeholderData: (previous, query) => query?.queryKey[1] === checkout.checkoutId ? previous : undefined,
    queryFn: () => invoke<NoteGraphProjection["edges"]>("repository_note_edges", { checkoutId: checkout.checkoutId }),
  });
  const [inspector, setInspector] = useState<Inspector | null>(window.innerWidth >= 1180 ? "outline" : null);
  const [modes, setModes] = useState<Record<string, NoteMode>>({});
  const [jumps, setJumps] = useState<Record<string, NoteJump>>({});
  const [dialog, setDialog] = useState<KnowledgeDialog>();
  const [destination, setDestination] = useState("");
  const [status, setStatus] = useState(simulation?.scenario === "recovery" ? "Recovered an unsaved Markdown buffer." : "");
  const [problem, setProblem] = useState("");
  const [selection, setSelection] = useState("");
  const [selectedAnchor, setSelectedAnchor] = useState("");
  const lastTarget = useRef(simulated && !initialTarget?.line ? target : undefined);
  const lastOpen = useRef(0);
  const { load: loadNote, watch: watchNote, getNote } = repository;
  const active = dock.path;
  const note = active ? notes[active] : undefined;
  const conflictRecovery = dialog?.kind === "conflict" ? repository.notes[dialog.path]?.recovery : undefined;
  const source = note?.content ?? "";
  const activeMode = active ? modes[active] ?? "live" : "live";
  const showIndexed = !simulated && Boolean(active) && noteIndex.index.isSuccess;
  const outlineIndex = useQuery({
    queryKey: ["repository-indexed-note", checkout.checkoutId, active, noteIndex.index.dataUpdatedAt],
    enabled: showIndexed && inspector === "outline" && !isDirty(note),
    placeholderData: (previous, query) => query?.queryKey[1] === checkout.checkoutId && query.queryKey[2] === active ? previous : undefined,
    queryFn: () => invoke<IndexedNoteDetails | null>("repository_indexed_note", { checkoutId: checkout.checkoutId, path: active }),
  });
  const relationships = useQuery({
    queryKey: ["repository-note-relationships", checkout.checkoutId, active, noteIndex.index.dataUpdatedAt],
    enabled: showIndexed && inspector === "links",
    placeholderData: (previous, query) => query?.queryKey[1] === checkout.checkoutId && query.queryKey[2] === active ? previous : undefined,
    queryFn: () => invoke<IndexedNoteRelationships>("repository_note_relationships", { checkoutId: checkout.checkoutId, path: active }),
  });
  const catalog = useMemo(() => {
    const indexed = new Map((noteIndex.index.data ?? repository.catalog).map(({ path, frontmatter }) => [path, frontmatter ?? ""]));
    for (const path of repository.draftPaths) if (!indexed.has(path)) indexed.set(path, "");
    for (const [path, value] of Object.entries(notes)) {
      if (simulated || indexed.has(path) || isDirty(value)) indexed.set(path, value.content);
    }
    return [...indexed].map(([path, content]) => ({ path, content }));
  }, [notes, noteIndex.index.data, repository.catalog, repository.draftPaths, simulated]);
  const previews = useRepositoryLinkPreviews(checkout.checkoutId, catalog, noteIndex.index.dataUpdatedAt, !simulated);
  const lookupLink = useCallback((link: NoteTarget, from: string) => linkPreviewValue(previews.data, link, from), [previews.data]);
  const matches = simulated ? findFileText(catalog, query, true) : [];
  const outgoing = wikiLinks(source);
  const backlinks = simulated && active ? catalog.flatMap(({ path, content }) => {
    if (path === active) return [];
    const inbound = wikiLinks(content).filter((link) => {
      const resolved = resolveWiki(link, path, catalog);
      return resolved.length === 1 && resolved[0]?.path === active;
    });
    return inbound.length ? [{ path, count: inbound.length }] : [];
  }) : [];
  const previewCatalog = useMemo(() => new Map(noteIndex.index.data?.map((note) => [note.path, note])), [noteIndex.index.data]);
  const previewNote = (path: string) => previewCatalog.get(path);
  const previewTarget = (location: IndexedLinkLocation) => {
    const note = previewNote(location.path);
    return { ...location, title: note?.title, snippet: note?.preview };
  };
  const describeLink = (reference: NoteTarget, resolution: IndexedLinkResolution | undefined,
    line?: number, wiki = true): KnowledgeOutgoingLink | undefined => {
    if (resolution?.status === "attachment") return;
    const view: KnowledgeOutgoingLink["resolution"] = resolution?.status === "resolved"
      ? { kind: "resolved", target: previewTarget(resolution.location) }
      : resolution?.status === "missing_anchor" ? { kind: "missing_anchor", target: previewTarget(resolution.location) }
        : resolution?.status === "ambiguous" ? { kind: "ambiguous", candidates: resolution.candidates.map(previewTarget) }
          : { kind: "missing" };
    return { label: reference.alias || reference.raw, source: { path: active!, line }, rawTarget: reference.raw,
      reference: { ...reference, wiki }, resolution: view };
  };
  const indexedOutgoing = active && inspector === "links" && !simulated ? (
    isDirty(note) ? outgoing.map((reference) => describeLink(reference, lookupLink(reference, active)))
      : relationships.data?.outgoing.map(({ link, resolution }) => {
        const raw = link.target + (link.anchor ? `#${link.anchor}` : "") + (link.alias ? `|${link.alias}` : "");
        const parsed = link.syntax === "markdown" ? parseReadingAddress(readingLocalAddress(active, link.target)) : undefined;
        const reference = link.syntax === "wiki" ? { raw, target: link.target, alias: link.alias ?? undefined, anchor: link.anchor ?? undefined, embed: link.embed }
          : parsed && { ...parsed, alias: link.alias ?? undefined };
        return reference && describeLink(reference, resolution, link.line, link.syntax === "wiki");
      }) ?? []
  ).filter((link): link is KnowledgeOutgoingLink => Boolean(link)) : [];

  const open = useCallback((path: string, line?: number, anchor?: string) => {
    const focus = (file: string, targetLine?: number) => {
      if (targetLine || anchor) setJumps((before) => ({ ...before, [file]: { line: targetLine, anchor, token: (before[file]?.token ?? 0) + 1 } }));
      setGraph(false);
      dock.open(file);
    };
    if (simulated) { focus(path, line); return; }
    const request = ++lastOpen.current;
    void loadNote(path).then((file) => {
      if (!file || request !== lastOpen.current) return;
      const anchorTarget = anchor ? anchorLine(getNote(file)?.content ?? "", anchor) : undefined;
      focus(file, anchorTarget ?? line);
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
        const resolution = lookupLink(embed, path);
        const target = resolution?.status === "resolved" ? resolution.location : undefined;
        if (!target || (visitedDepth.get(target.path) ?? Infinity) <= depth + 1) continue;
        const buffer = notes[target.path];
        if (buffer) inspect(target.path, buffer.content, depth + 1);
        else if (!repository.failures[target.path]) void loadNote(target.path);
      }
    };
    inspect(active, note.content, 0);
  }, [active, loadNote, lookupLink, note, notes, repository.failures, simulated]);

  useEffect(() => {
    if (!target || target === lastTarget.current || !dock.ready) return;
    const location = readNoteSearchTarget(target);
    const next = location && normalizeRepositoryPath(location.path);
    if (!next || !location) return;
    lastTarget.current = target;
    if (location.line) setModes((before) => before[next] === "reading" ? { ...before, [next]: "live" } : before);
    open(next, location.line);
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
  const openLocation = ({ path, line, anchor }: KnowledgeLocation) => open(path, line ?? undefined, anchor ?? undefined);
  const offerLinkRepair = (from: string, link: NoteTarget, matches: NoteLocation[], anchor = "", sourceLine?: number, manual?: boolean) => {
    setSelection(matches.length === 1 ? matches[0]!.path : "");
    setSelectedAnchor(anchor);
    setDialog({ kind: "link", from, link, matches, sourceLine, manual });
  };
  const repairReference = async (proposal: Extract<KnowledgeDialog, { kind: "link" }>) => {
    const current = simulated ? examples[proposal.from] : getNote(proposal.from);
    if (!current) return;
    const anchor = selectedAnchor.trim() || undefined;
    try {
      const markdown = simulated ? repairWikiAddress(current.content, proposal.link.raw, selection, anchor)
        : await invoke<string>("repair_note_wikilink", {
          markdown: current.content, reference: proposal.link.raw, sourceLine: proposal.sourceLine ?? null,
          destination: selection, anchor: anchor ?? null,
        });
      if (!simulated && getNote(proposal.from)?.content !== current.content) {
        setProblem("The note changed. Open the link again to review the current source.");
        return;
      }
      update(proposal.from, markdown);
      setStatus(`Updated reference in ${proposal.from}`);
      closeDialog();
    } catch (failure) { setProblem(String(failure)); }
  };
  const navigateLink = async (from: string, link: NoteTarget) => {
    from = link.sourcePath ?? from;
    const attachment = simulated ? noteAttachments.find(({ name }) => link.target === name) : undefined;
    if (attachment) { setDialog({ kind: "attachment", attachment }); return; }
    if (!simulated) {
      try {
        if (!noteIndex.index.data) await noteIndex.index.refetch();
        const result = await resolveRepositoryLink(checkout.checkoutId, link, from);
        if (result?.status === "resolved") openLocation(result.location);
        else if (result?.status === "attachment") setDialog({ kind: "repositoryAttachment", paths: result.paths, label: link.alias ?? link.target });
        else if (result?.status === "ambiguous") offerLinkRepair(from, link, result.candidates.map(asNoteLocation), undefined, link.sourceLine);
        else if (result?.status === "missing_anchor") {
          const target = asNoteLocation(result.location);
          void loadNote(target.path);
          offerLinkRepair(from, link, [target], link.anchor, link.sourceLine);
        } else offerLinkRepair(from, link, [], undefined, link.sourceLine);
      } catch (failure) {
        setStatus(String(failure));
      }
      return;
    }
    const targets = resolveWiki(link, from, catalog);
    const only = targets[0];
    if (targets.length === 1 && only && (!only.anchor || anchorLine(notes[only.path]?.content ?? "", only.anchor))) {
      navigateLocation(only);
      return;
    }
    offerLinkRepair(from, link, targets);
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
      if (event.key.toLowerCase() === "p") {
        event.preventDefault(); setSidebar(true); setDisplay("search"); requestSearchFocus((before) => before + 1);
      }
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
              error={noteIndex.error ? String(noteIndex.error) : undefined} focusRequest={focusSearch} onOpen={openLocation} />
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
            <button type="button" className="fw-icon" title="Rename note" aria-label="Rename note"
              disabled={!note || (!simulated && (isDirty(note) || Boolean(active && repository.notes[active]?.sourceProblem)))}
              onClick={() => {
                if (!active) return;
                if (simulated) {
                  setDestination(active);
                  setDialog({ kind: "rename", path: active });
                } else {
                  const revision = getNote(active)?.base.id;
                  if (revision) setDialog({ kind: "repositoryRename", path: active, revision });
                }
              }}><FilePenLine size={15} /></button>
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
                const onLink = (link: NoteTarget) => { void navigateLink(path, link); };
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
                        <Editor mode={selectedMode === "source" ? "source" : "live"} path={path} source={entry.content} files={catalog} jump={jumps[path]} analyze
                          suggestLinks={simulated ? undefined : (prefix) => suggestRepositoryLinks(checkout.checkoutId, path, prefix)}
                          onChange={(content) => update(path, content)} onLink={onLink} />
                      </div>}
                      {selectedMode === "reading" && <Reading path={path} content={entry.content} files={catalog} jump={jumps[path]} onLink={onLink}
                        checkoutId={simulated ? undefined : checkout.checkoutId} sampleAttachments={simulated}
                        resolveLinks={simulated ? undefined : lookupLink}
                        previewFor={simulated ? undefined : previewNote}
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
            {!simulated && (graphEdges.error || noteIndex.index.error) ? <p role="alert" className="file-help">{String(graphEdges.error ?? noteIndex.index.error)}</p>
              : <Suspense fallback={<p role="status" className="file-help">Loading note graph…</p>}>
                {simulated ? <NoteGraph files={catalog} selected={active} onOpen={open} />
                  : noteIndex.index.data && graphEdges.data ? <NoteGraph projection={{ notes: noteIndex.index.data, edges: graphEdges.data }} selected={active} onOpen={open} />
                    : <p role="status" className="file-help">Loading note graph…</p>}
              </Suspense>}
          </div>}
          {inspector && !graph && <aside className="note-inspector" aria-label="Note details">
            <div className="note-inspector-views">
              {([[ListTree, "outline", "Outline"], [Link2, "links", "Links"], [FileText, "properties", "Properties"], [Paperclip, "attachments", "Attachments"]] as const)
                .map(([Icon, view, label]) => <button type="button" key={view} className="fw-icon" aria-label={label} aria-pressed={view === inspector} title={label}
                  onClick={() => setInspector(view)}><Icon size={16} /></button>)}
            </div>
            <div className="note-inspector-content">
              {!active || !note ? <p className="file-help">Open a note to see its details.</p> : <>
                {inspector === "outline" && <KnowledgeOutline
                  entries={(isDirty(note) ? headings(source) : outlineIndex.data?.headings ?? headings(source))
                    .map((entry) => ({ ...entry, path: active, anchor: entry.label }))} onOpen={openLocation} />}
                {inspector === "links" && !simulated && <KnowledgeLinks outgoing={indexedOutgoing}
                  backlinks={(relationships.data?.backlinks ?? []).map((link) => ({
                    source: { path: link.path, line: link.line, title: link.title, snippet: previewNote(link.path)?.preview },
                    label: link.label ?? undefined,
                  }))} onOpen={openLocation} pending={showIndexed && relationships.isPending}
                  error={relationships.error ? String(relationships.error) : undefined}
                  onRepair={(entry) => {
                    const resolution = entry.resolution;
                    const link = entry.reference;
                    if (!link) return;
                    const targets = resolution.kind === "ambiguous" ? resolution.candidates : resolution.kind === "missing_anchor" ? [resolution.target] : [];
                    if (targets.length === 1) void loadNote(targets[0]!.path);
                    offerLinkRepair(active, { ...link, raw: entry.rawTarget }, targets.map((target) => ({ path: target.path, anchor: target.anchor ?? undefined })),
                      resolution.kind === "missing_anchor" ? link.anchor : undefined, entry.source.line ?? undefined, !link.wiki);
                  }} />}
                {inspector === "links" && simulated && <>
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
                  {!simulated && outgoing.flatMap((link, index) => lookupLink(link, active)?.status === "attachment" ? [
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
      {dialog?.kind === "repositoryRename" && <RepositoryNoteRename key={dialog.path} checkoutId={checkout.checkoutId}
        path={dialog.path} sourceRevisionId={dialog.revision}
        canApply={!isDirty(notes[dialog.path]) && getNote(dialog.path)?.base.id === dialog.revision && !getNote(dialog.path)?.sourceProblem}
        onClose={closeDialog} onSettled={(moved) => {
          void noteIndex.index.refetch();
          if (!moved) return;
          for (const panel of [...dock.api?.panels ?? []]) {
            if ((panel.params as { path?: string } | undefined)?.path === dialog.path) closePanel(dialog.path, panel.api);
          }
          repository.forget(dialog.path);
          open(moved);
        }} />}
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
          setSelectedAnchor(originalAnchor ?? "");
          if (!simulated && path) void loadNote(path);
        }}>
          <option value="">Select a note</option>
          {(dialog.matches.length > 1 ? dialog.matches : catalog).map(({ path }) => <option key={path} value={path}>{path}</option>)}
        </select></label>
        {selection && <label>Heading or block<input value={selectedAnchor} placeholder="Open note without a specific location"
          onChange={(event) => setSelectedAnchor(event.target.value)} /></label>}
        {selectedAnchor && !anchorLine(notes[selection]?.content ?? "", selectedAnchor.trim()) && <p role="alert">The heading or block is unavailable in this note.</p>}
        {problem && <p role="alert">{problem}</p>}
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn fw-outlined" disabled={!selection || Boolean(selectedAnchor && !anchorLine(notes[selection]?.content ?? "", selectedAnchor.trim()))}
            onClick={() => { navigateLocation({ path: selection, anchor: selectedAnchor.trim() || undefined }); closeDialog(); }}>Open note</button>
          {dialog.manual ? <button type="button" className="fw-btn fw-primary" onClick={() => {
            setModes((before) => ({ ...before, [dialog.from]: "source" }));
            open(dialog.from, dialog.sourceLine);
            closeDialog();
          }}>Edit source link</button> : <button type="button" className="fw-btn fw-primary" disabled={!selection || Boolean(selectedAnchor && !anchorLine(notes[selection]?.content ?? "", selectedAnchor.trim()))}
            onClick={() => { void repairReference(dialog); }}>Update link</button>}
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
